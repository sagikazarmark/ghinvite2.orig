-- ghinvite v1 initial schema. SQLite-portable so the same file is applied to
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

CREATE TABLE share_links (
  id                 TEXT    PRIMARY KEY,
  slug               TEXT    NOT NULL UNIQUE,
  installation_id    INTEGER NOT NULL REFERENCES installations(installation_id),
  account_id         INTEGER NOT NULL,
  created_by         INTEGER NOT NULL REFERENCES users(user_id),
  created_at         TEXT    NOT NULL,
  expires_at         TEXT,
  max_uses           INTEGER,
  uses_count         INTEGER NOT NULL DEFAULT 0,
  permission         TEXT    NOT NULL,
  approval_required  INTEGER NOT NULL,
  internal_note      TEXT,
  revoked_at         TEXT,
  revoked_by         INTEGER REFERENCES users(user_id)
);

CREATE INDEX idx_share_links_account ON share_links(account_id);

CREATE TABLE share_link_repos (
  share_link_id   TEXT    NOT NULL REFERENCES share_links(id),
  repo_id         INTEGER NOT NULL,
  repo_full_name  TEXT    NOT NULL,
  PRIMARY KEY (share_link_id, repo_id)
);

CREATE TABLE invitation_requests (
  id              TEXT    PRIMARY KEY,
  share_link_id   TEXT    NOT NULL REFERENCES share_links(id),
  requester_id    INTEGER NOT NULL REFERENCES users(user_id),
  justification   TEXT,
  state           TEXT    NOT NULL,
  decided_by      INTEGER REFERENCES users(user_id),
  decided_at      TEXT,
  decline_reason  TEXT,
  created_at      TEXT    NOT NULL
);

CREATE UNIQUE INDEX idx_one_pending_per_link_per_user
  ON invitation_requests(share_link_id, requester_id) WHERE state = 'pending';

CREATE INDEX idx_requests_link ON invitation_requests(share_link_id);

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
  id           TEXT    PRIMARY KEY,
  account_id   INTEGER NOT NULL,
  occurred_at  TEXT    NOT NULL,
  event_type   TEXT    NOT NULL,
  actor_kind   TEXT    NOT NULL,
  actor_id     INTEGER,
  target_kind  TEXT    NOT NULL,
  target_id    TEXT    NOT NULL,
  metadata     TEXT,
  request_id   TEXT
);

CREATE INDEX idx_audit_account_time ON audit_events(account_id, occurred_at);

CREATE INDEX idx_audit_target ON audit_events(target_kind, target_id);
