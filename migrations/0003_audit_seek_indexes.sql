-- UTC RFC3339 timestamps from SQLx and D1 have variable fractions. Index an
-- exact, nanosecond-padded comparison key, not rounded SQLite date functions.
-- Handles both +00:00 and Z without rewriting historical event data.
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
