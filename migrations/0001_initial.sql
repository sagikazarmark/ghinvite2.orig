-- ghinvite initial schema. SQLite-portable so the same file is applied to
-- both local sqlx (sqlx-migrate) and Cloudflare D1 (wrangler d1 migrations apply).

CREATE TABLE installations (
  installation_id   INTEGER PRIMARY KEY,
  account_id        INTEGER NOT NULL,
  account_login     TEXT    NOT NULL,
  account_type      TEXT    NOT NULL CHECK (account_type IN ('User', 'Organization')),
  installed_at      TEXT    NOT NULL,
  uninstalled_at    TEXT,
  selected_repos    TEXT    NOT NULL
);

CREATE INDEX idx_installations_account_login ON installations(account_login);

CREATE UNIQUE INDEX idx_installations_active_account
  ON installations(account_id) WHERE uninstalled_at IS NULL;

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
  installation_id      INTEGER NOT NULL REFERENCES installations(installation_id),
  account_id           INTEGER NOT NULL,
  created_by           INTEGER NOT NULL REFERENCES users(user_id),
  created_at           TEXT    NOT NULL,
  expires_at           TEXT,
  max_uses             INTEGER,
  uses_count           INTEGER NOT NULL DEFAULT 0,
  permission           TEXT    NOT NULL,
  approval_required    INTEGER NOT NULL,
  internal_note        TEXT,
  revoked_at           TEXT,
  revoked_by           INTEGER REFERENCES users(user_id),
  description          TEXT    NOT NULL DEFAULT '',
  projection_revision  INTEGER NOT NULL,
  projection_content   TEXT NOT NULL
);

CREATE INDEX idx_invitation_links_account ON invitation_links(account_id);

CREATE TABLE invitation_link_repos (
  invitation_link_id  TEXT    NOT NULL REFERENCES invitation_links(id),
  repo_id             INTEGER NOT NULL,
  repo_full_name      TEXT    NOT NULL,
  PRIMARY KEY (invitation_link_id, repo_id)
);

-- queue_account_id is a derived seek key maintained by the triggers below. The
-- owning link remains the authorization source.
CREATE TABLE invitation_requests (
  id                   TEXT    PRIMARY KEY,
  invitation_link_id   TEXT    NOT NULL REFERENCES invitation_links(id),
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
  queue_account_id     INTEGER
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

CREATE TABLE github_invitations (
  id                     TEXT    PRIMARY KEY,
  invitation_request_id  TEXT    NOT NULL REFERENCES invitation_requests(id),
  repo_id                INTEGER NOT NULL,
  github_invitation_id   INTEGER,
  state                  TEXT    NOT NULL,
  error_message          TEXT,
  created_at             TEXT    NOT NULL,
  updated_at             TEXT    NOT NULL
);

CREATE INDEX idx_github_invitations_request ON github_invitations(invitation_request_id);

CREATE INDEX idx_github_invitations_github_id ON github_invitations(github_invitation_id);

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
  invitation_id  TEXT    PRIMARY KEY NOT NULL,
  command        TEXT    NOT NULL,
  generation     INTEGER NOT NULL DEFAULT 1,
  retryable      INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE delivery_outcomes (
  invitation_id  TEXT PRIMARY KEY NOT NULL,
  request_id     TEXT NOT NULL REFERENCES invitation_requests(id),
  receipt        TEXT NOT NULL
);

CREATE INDEX delivery_outcomes_request ON delivery_outcomes(request_id);

CREATE TRIGGER delivery_outcome_identity BEFORE UPDATE ON delivery_outcomes
WHEN json_extract(OLD.receipt, '$.create.command') != json_extract(NEW.receipt, '$.create.command')
  OR (json_extract(OLD.receipt, '$.revision') = json_extract(NEW.receipt, '$.revision') AND OLD.receipt != NEW.receipt)
BEGIN SELECT RAISE(ABORT, 'delivery outcome conflict'); END;

-- Apply only the initial create lifecycle. Concurrent webhook/cancel updates
-- cannot be overwritten by a delayed create projection.
CREATE TRIGGER delivery_created AFTER UPDATE ON delivery_outcomes
WHEN json_extract(NEW.receipt, '$.create.outcome.kind') IN ('created', 'already_collaborator', 'failed')
BEGIN
  UPDATE github_invitations SET
    state = CASE json_extract(NEW.receipt, '$.create.outcome.kind') WHEN 'created' THEN 'sent' WHEN 'already_collaborator' THEN 'accepted' ELSE 'failed' END,
    github_invitation_id = json_extract(NEW.receipt, '$.create.outcome.upstream_id'),
    updated_at = json_extract(NEW.receipt, '$.create.confirmed_at')
  WHERE id = NEW.invitation_id AND state = 'sending';
END;

CREATE TRIGGER delivery_created_initial AFTER INSERT ON delivery_outcomes
WHEN json_extract(NEW.receipt, '$.create.outcome.kind') IN ('created', 'already_collaborator', 'failed')
BEGIN
  UPDATE github_invitations SET
    state = CASE json_extract(NEW.receipt, '$.create.outcome.kind') WHEN 'created' THEN 'sent' WHEN 'already_collaborator' THEN 'accepted' ELSE 'failed' END,
    github_invitation_id = json_extract(NEW.receipt, '$.create.outcome.upstream_id'),
    updated_at = json_extract(NEW.receipt, '$.create.confirmed_at')
  WHERE id = NEW.invitation_id AND state = 'sending';
END;

-- Defense in depth against an old invocation's delayed SQL write.
CREATE TRIGGER github_invitation_settlement_fence BEFORE UPDATE ON github_invitations
WHEN OLD.state NOT IN ('sending', 'sent') AND EXISTS (SELECT 1 FROM delivery_outcomes s WHERE s.invitation_id = OLD.id AND json_type(s.receipt, '$.settlement') = 'object')
 AND (NEW.state IS NOT (SELECT json_extract(receipt, '$.settlement.state') FROM delivery_outcomes WHERE invitation_id = OLD.id)
   OR NEW.github_invitation_id IS NOT OLD.github_invitation_id
   OR NEW.invitation_request_id IS NOT OLD.invitation_request_id
   OR NEW.repo_id IS NOT OLD.repo_id)
BEGIN SELECT RAISE(ABORT, 'settled invitation writer conflict'); END;

-- Signed-body identity, not the unsigned delivery header. Retain no-match too:
-- an old untracked event must never bind to a subsequently created invitation.
CREATE TABLE member_webhook_receipts (
  payload_sha256  TEXT PRIMARY KEY NOT NULL,
  invitation_id   TEXT REFERENCES github_invitations(id)
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
