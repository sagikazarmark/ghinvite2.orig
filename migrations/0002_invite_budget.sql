-- UI projection only. InviteBudget VO remains authoritative; authenticated
-- projection consumers apply each complete snapshot atomically and monotonically.
CREATE TABLE invite_budget_projection (
  app TEXT NOT NULL CHECK (app IN ('repo','org')),
  account_id INTEGER NOT NULL CHECK (account_id BETWEEN 1 AND 9007199254740991),
  revision INTEGER NOT NULL CHECK (revision BETWEEN 1 AND 9007199254740991),
  snapshot_json TEXT NOT NULL CHECK (json_valid(snapshot_json)),
  PRIMARY KEY(app,account_id)
);
CREATE TABLE invite_budget_snapshot (
  app TEXT NOT NULL CHECK (app IN ('repo','org')),
  account_id INTEGER NOT NULL CHECK (account_id BETWEEN 1 AND 9007199254740991),
  bucket TEXT NOT NULL,
  used_24h INTEGER NOT NULL CHECK (used_24h>=0),
  cap INTEGER NOT NULL CHECK (cap>0),
  closed_until TEXT,
  next_slot_at TEXT,
  projection_revision INTEGER NOT NULL CHECK (projection_revision BETWEEN 1 AND 9007199254740991),
  observed_at TEXT NOT NULL,
  PRIMARY KEY(app,account_id,bucket)
);
