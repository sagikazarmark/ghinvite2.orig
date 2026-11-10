-- ghinvite initial schema. SQLite-portable so the same file is applied to
-- both local sqlx (sqlx-migrate) and Cloudflare D1 (wrangler d1 migrations apply).

CREATE TABLE installations (
  app               TEXT NOT NULL DEFAULT 'repo' CHECK(app IN ('repo','org')),
  installation_id   INTEGER NOT NULL CHECK(installation_id BETWEEN 1 AND 9007199254740991),
  account_id        INTEGER NOT NULL CHECK(account_id BETWEEN 1 AND 9007199254740991),
  account_login     TEXT    NOT NULL,
  account_type      TEXT    NOT NULL CHECK (account_type IN ('User', 'Organization')),
  installed_at      TEXT    NOT NULL,
  uninstalled_at    TEXT,
  selected_repos    TEXT    NOT NULL,
  suspended_at      TEXT,
  permissions_json  TEXT NOT NULL DEFAULT '{}' CHECK(json_valid(permissions_json)),
  installed_by_user_id INTEGER CHECK(installed_by_user_id BETWEEN 1 AND 9007199254740991),
  installed_by_login TEXT,
  projection_revision INTEGER NOT NULL DEFAULT 0 CHECK(projection_revision BETWEEN 0 AND 9007199254740991),
  projection_content TEXT CHECK(projection_content IS NULL OR json_valid(projection_content)),
  CHECK((installed_by_user_id IS NULL) = (installed_by_login IS NULL)),
  PRIMARY KEY(app,installation_id)
);

CREATE UNIQUE INDEX idx_installations_scoped_account ON installations(app,installation_id,account_id);

CREATE INDEX idx_installations_account_login ON installations(app,account_login,installed_at DESC,installation_id DESC);

CREATE UNIQUE INDEX idx_installations_active_account
  ON installations(app,account_id) WHERE uninstalled_at IS NULL;

CREATE TABLE account_settings (
  account_id INTEGER PRIMARY KEY CHECK(account_id BETWEEN 1 AND 9007199254740991),
  repo_admin_delegation INTEGER NOT NULL DEFAULT 0 CHECK(repo_admin_delegation IN (0,1)),
  changed_by INTEGER NOT NULL CHECK(changed_by BETWEEN 1 AND 9007199254740991),
  changed_at TEXT NOT NULL,
  projection_revision INTEGER NOT NULL DEFAULT 0 CHECK(projection_revision BETWEEN 0 AND 9007199254740991),
  projection_content TEXT CHECK(projection_content IS NULL OR json_valid(projection_content))
);

CREATE TABLE users (
  user_id      INTEGER PRIMARY KEY,
  login        TEXT NOT NULL,
  avatar_url   TEXT,
  last_seen_at TEXT NOT NULL
);

-- Links and requests are written only by the projector; projection_revision is
-- the snapshot revision it last applied, so a stale snapshot cannot regress a row.
CREATE TABLE invitation_links (
  id                   TEXT    PRIMARY KEY,
  installation_app     TEXT NOT NULL DEFAULT 'repo' CHECK(installation_app IN ('repo','org')),
  organization_installation_app TEXT NOT NULL DEFAULT 'org' CHECK(organization_installation_app='org'),
  organization_installation_id INTEGER CHECK(organization_installation_id BETWEEN 1 AND 9007199254740991),
  installation_id      INTEGER NOT NULL,
  account_id           INTEGER NOT NULL,
  created_by           INTEGER NOT NULL REFERENCES users(user_id),
  created_at           TEXT    NOT NULL,
  expires_at           TEXT,
  max_uses             INTEGER,
  uses_count           INTEGER NOT NULL DEFAULT 0,
  approval_required    INTEGER NOT NULL,
  internal_note        TEXT,
  revoked_at           TEXT,
  revoked_by           INTEGER REFERENCES users(user_id),
  description          TEXT    NOT NULL DEFAULT '',
  state TEXT NOT NULL DEFAULT 'active' CHECK(state IN ('active','paused','revoked')),
  paused_at TEXT,
  paused_by INTEGER,
  pause_reason TEXT,
  abuse_window_json TEXT,
  revoke_cascade_json TEXT NOT NULL DEFAULT '{}',
  email_domains_json TEXT,
  invitation_lifetime_secs INTEGER,
  access_lifetime_secs INTEGER,
  budget_share_pct INTEGER CHECK(budget_share_pct BETWEEN 1 AND 100),
  abuse_threshold INTEGER,
  secret_generation INTEGER NOT NULL CHECK(secret_generation BETWEEN 1 AND 4294967295),
  secret_sha256 TEXT NOT NULL CHECK(length(secret_sha256)=64 AND secret_sha256 NOT GLOB '*[^0-9a-f]*'),
  source TEXT NOT NULL DEFAULT 'console' CHECK(source IN ('console','batch')),
  projection_revision  INTEGER NOT NULL,
  projection_content   TEXT NOT NULL,
  FOREIGN KEY(installation_app,installation_id,account_id) REFERENCES installations(app,installation_id,account_id),
  FOREIGN KEY(organization_installation_app,organization_installation_id,account_id) REFERENCES installations(app,installation_id,account_id)
);

CREATE INDEX idx_invitation_links_account ON invitation_links(account_id);

CREATE TABLE invitation_link_targets (
  link_id TEXT NOT NULL REFERENCES invitation_links(id),
  kind TEXT NOT NULL CHECK(kind IN ('repo','org','team')),
  target_id INTEGER NOT NULL CHECK(target_id BETWEEN 1 AND 9007199254740991),
  display_name TEXT NOT NULL,
  grant TEXT NOT NULL,
  PRIMARY KEY(link_id,kind,target_id),
  CHECK((kind='repo' AND grant IN ('read','triage','write','maintain','admin')) OR (kind IN ('org','team') AND grant='member'))
);
CREATE INDEX idx_link_targets_repo_seek ON invitation_link_targets(kind,target_id,link_id);

CREATE TABLE link_secrets (
  secret_sha256 TEXT PRIMARY KEY NOT NULL CHECK(length(secret_sha256)=64 AND secret_sha256 NOT GLOB '*[^0-9a-f]*'),
  link_id TEXT NOT NULL,
  secret_generation INTEGER NOT NULL CHECK(secret_generation BETWEEN 1 AND 4294967295),
  created_at TEXT NOT NULL,
  retired_at TEXT,
  UNIQUE(link_id,secret_generation)
);
CREATE INDEX link_secrets_link ON link_secrets(link_id,secret_generation);

-- queue_account_id is a derived seek key maintained by the triggers below. The
-- owning link remains the authorization source.
CREATE TABLE invitation_requests (
  id                   TEXT    PRIMARY KEY,
  invitation_link_id   TEXT    REFERENCES invitation_links(id),
  requester_id         INTEGER NOT NULL REFERENCES users(user_id),
  justification        TEXT,
  state                TEXT    NOT NULL,
  decided_by           INTEGER REFERENCES users(user_id),
  decided_at           TEXT,
  decline_reason       TEXT,
  created_at           TEXT    NOT NULL,
  projection_revision  INTEGER NOT NULL,
  projection_content   TEXT,
  decision_deadline    TEXT,
  queue_account_id     INTEGER,
  source TEXT NOT NULL DEFAULT 'link',
  batch_id TEXT,
  domain_evidence_json TEXT
);

CREATE INDEX idx_requests_link ON invitation_requests(invitation_link_id);

-- UTC RFC3339 timestamps from SQLx and D1 have variable fractions. Expression
-- indexes below use an exact, nanosecond-padded comparison key, not rounded
-- SQLite date functions, and handle both +00:00 and Z. Keep each expression
-- identical to its storage::{pending_queue,request_history,audit_read} query.

CREATE INDEX idx_pending_queue_seek ON invitation_requests(
  queue_account_id,
  (substr(created_at,1,19) || '.' || substr(CASE WHEN substr(created_at,20,1) = '.' THEN replace(replace(substr(created_at,21),'+00:00',''),'Z','') ELSE '' END || '000000000',1,9) || '/' || id)
) WHERE state = 'pending';

CREATE INDEX idx_request_history ON invitation_requests (
  invitation_link_id,
  (substr(created_at,1,19) || '.' || substr(CASE WHEN substr(created_at,20,1) = '.' THEN replace(replace(substr(created_at,21),'+00:00',''),'Z','') ELSE '' END || '000000000',1,9) || '/' || id) DESC
);

CREATE TRIGGER requests_queue_insert AFTER INSERT ON invitation_requests BEGIN
  UPDATE invitation_requests SET queue_account_id = (
    SELECT account_id FROM invitation_links WHERE id = NEW.invitation_link_id
  ) WHERE id = NEW.id;
END;

CREATE TRIGGER requests_queue_move AFTER UPDATE OF invitation_link_id ON invitation_requests BEGIN
  UPDATE invitation_requests SET queue_account_id = (
    SELECT account_id FROM invitation_links WHERE id = NEW.invitation_link_id
  ) WHERE id = NEW.id;
END;

CREATE TRIGGER links_queue_account AFTER UPDATE OF account_id ON invitation_links BEGIN
  UPDATE invitation_requests SET queue_account_id = NEW.account_id WHERE invitation_link_id = NEW.id;
END;

CREATE TRIGGER links_queue_restore AFTER INSERT ON invitation_links BEGIN
  UPDATE invitation_requests SET queue_account_id = NEW.account_id WHERE invitation_link_id = NEW.id;
END;

CREATE TABLE deliveries (
  id TEXT PRIMARY KEY NOT NULL,
  -- Transitional ULID lookup bridge; canonical identity is always id.
  invitation_id TEXT UNIQUE NOT NULL,
  request_id TEXT NOT NULL REFERENCES invitation_requests(id),
  batch_id TEXT,
  kind TEXT NOT NULL DEFAULT 'repo' CHECK(kind IN ('repo','org','email')),
  target_id INTEGER NOT NULL CHECK(target_id BETWEEN 1 AND 9007199254740991),
  delivery_generation INTEGER NOT NULL CHECK(delivery_generation BETWEEN 1 AND 9007199254740991),
  create_outcome TEXT NOT NULL DEFAULT 'planned',
  outcome_reason TEXT,
  effective_role_before TEXT,
  upstream_invitation_id INTEGER CHECK(upstream_invitation_id BETWEEN 1 AND 9007199254740991),
  tracked_foreign INTEGER NOT NULL DEFAULT 0 CHECK(tracked_foreign IN (0,1)),
  tracking_stopped_reason TEXT,
  settlement TEXT,
  settlement_reason TEXT,
  final_result TEXT,
  cascade_intent TEXT,
  held INTEGER NOT NULL DEFAULT 0 CHECK(held IN (0,1)),
  sent_at TEXT,
  own_expires_at TEXT,
  unknown_since TEXT,
  last_observed_at TEXT,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  projection_revision INTEGER NOT NULL DEFAULT 0,
  CHECK(id = CASE kind WHEN 'org' THEN 'org:' ELSE '' END || request_id || ':' || target_id || ':' || delivery_generation)
);
CREATE INDEX idx_deliveries_request ON deliveries(request_id);
CREATE INDEX idx_deliveries_upstream ON deliveries(upstream_invitation_id);
CREATE INDEX idx_deliveries_route ON deliveries(kind,target_id,settlement);

CREATE TABLE audit_events (
  id                   TEXT    PRIMARY KEY,
  account_id           INTEGER NOT NULL,
  occurred_at          TEXT    NOT NULL,
  event_type           TEXT    NOT NULL,
  actor_kind           TEXT    NOT NULL,
  actor_id             INTEGER,
  target_kind          TEXT    NOT NULL,
  target_id            TEXT    NOT NULL,
  metadata             TEXT,
  request_id           TEXT,
  projection_event_id  TEXT,
  projection_content   TEXT,
  evaluated_at         TEXT
);

CREATE INDEX idx_audit_account_time ON audit_events(account_id, occurred_at);

CREATE INDEX idx_audit_target ON audit_events(target_kind, target_id);

CREATE UNIQUE INDEX idx_projection_event ON audit_events(projection_event_id);

CREATE INDEX idx_audit_account_order ON audit_events(
  account_id,
  (substr(occurred_at,1,19) || '.' || substr(CASE WHEN substr(occurred_at,20,1) = '.' THEN replace(replace(substr(occurred_at,21),'+00:00',''),'Z','') ELSE '' END || '000000000',1,9)) DESC,
  id DESC
);

CREATE INDEX idx_audit_account_event_order ON audit_events(
  account_id, event_type,
  (substr(occurred_at,1,19) || '.' || substr(CASE WHEN substr(occurred_at,20,1) = '.' THEN replace(replace(substr(occurred_at,21),'+00:00',''),'Z','') ELSE '' END || '000000000',1,9)) DESC,
  id DESC
);

-- Portable assertion mechanism: failing either named CHECK aborts the SQLx
-- transaction / D1 batch BEFORE commit. The final statement removes the guard.
CREATE TABLE projection_assertions (
  dependency INTEGER CONSTRAINT projection_dependency CHECK (dependency = 1),
  invariant INTEGER CONSTRAINT projection_invariant CHECK (invariant = 1)
);

-- Restate owns create receipts. This immutable fence additionally guards the
-- HTTP run closure, whose result acknowledgement can be lost before journaling.
CREATE TABLE delivery_attempts (
  delivery_key TEXT NOT NULL,
  step TEXT NOT NULL,
  command TEXT NOT NULL CHECK(json_valid(command)),
  attempt_generation INTEGER NOT NULL DEFAULT 1 CHECK(attempt_generation BETWEEN 1 AND 9007199254740991),
  retryable INTEGER NOT NULL DEFAULT 0 CHECK(retryable IN (0,1)),
  PRIMARY KEY(delivery_key,step)
);
CREATE UNIQUE INDEX delivery_attempt_identity ON delivery_attempts(step,json_extract(command,'$.invitation_id'));
CREATE TABLE delivery_outcomes (
  delivery_id TEXT PRIMARY KEY NOT NULL REFERENCES deliveries(id),
  request_id TEXT NOT NULL REFERENCES invitation_requests(id),
  receipt TEXT NOT NULL CHECK(json_valid(receipt))
);
CREATE INDEX delivery_outcomes_request ON delivery_outcomes(request_id);
CREATE TRIGGER delivery_outcome_identity BEFORE UPDATE ON delivery_outcomes
WHEN json_extract(OLD.receipt,'$.create.command') != json_extract(NEW.receipt,'$.create.command')
 OR (json_extract(OLD.receipt,'$.revision')=json_extract(NEW.receipt,'$.revision') AND OLD.receipt != NEW.receipt)
 OR (json_extract(OLD.receipt,'$.create.confirmed_at') IS NOT NULL AND json_extract(OLD.receipt,'$.create') != json_extract(NEW.receipt,'$.create'))
BEGIN SELECT RAISE(ABORT,'delivery outcome conflict'); END;
CREATE TRIGGER delivery_settlement_fence BEFORE UPDATE ON deliveries
WHEN OLD.settlement IN ('accepted','declined','expired','cancelled') AND (
 NEW.settlement IS NOT OLD.settlement OR NEW.upstream_invitation_id IS NOT OLD.upstream_invitation_id
 OR NEW.request_id IS NOT OLD.request_id OR NEW.target_id IS NOT OLD.target_id OR NEW.delivery_generation IS NOT OLD.delivery_generation)
BEGIN SELECT RAISE(ABORT,'settled invitation writer conflict'); END;
CREATE TRIGGER delivery_tracking_stop_fence BEFORE UPDATE ON deliveries
WHEN OLD.tracking_stopped_reason IS NOT NULL AND NEW.tracking_stopped_reason IS NOT OLD.tracking_stopped_reason
BEGIN SELECT RAISE(ABORT,'foreign tracking stop conflict'); END;
CREATE TABLE webhook_receipts (
  payload_sha256 TEXT PRIMARY KEY NOT NULL,
  app TEXT NOT NULL CHECK(app IN ('repo','org')),
  event TEXT NOT NULL,
  action TEXT,
  matched_delivery_id TEXT REFERENCES deliveries(id)
);

-- Opaque, encrypted browser continuations. Never session authentication records.
CREATE TABLE attempt_continuations (
  scope       TEXT    NOT NULL,
  id          TEXT    NOT NULL,
  binding     TEXT    NOT NULL,
  payload     TEXT    NOT NULL,
  expires_at  INTEGER NOT NULL,
  PRIMARY KEY (scope, id),
  UNIQUE (scope, binding)
);

CREATE INDEX attempt_continuations_expiry ON attempt_continuations(expires_at);

-- Only irreversible hashes of stable session-lineage IDs, never cookies/tokens.
CREATE TABLE signin_starts (
  budget_key TEXT NOT NULL CHECK(length(budget_key)=64 AND budget_key NOT GLOB '*[^0-9a-f]*'),
  reservation_id TEXT NOT NULL,
  started_ms INTEGER NOT NULL CHECK(started_ms BETWEEN -9007199254740991 AND 9007199254740991),
  PRIMARY KEY(budget_key,reservation_id)
);
CREATE INDEX signin_starts_window ON signin_starts(budget_key,started_ms);
CREATE INDEX signin_starts_expiry ON signin_starts(started_ms);
