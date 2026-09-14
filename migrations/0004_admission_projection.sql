-- Additive projection support; NULL revisions identify legacy-owned rows.
ALTER TABLE invitation_links ADD COLUMN projection_revision INTEGER;
ALTER TABLE invitation_links ADD COLUMN projection_content TEXT;
ALTER TABLE invitation_links ADD COLUMN projection_identity TEXT;
ALTER TABLE invitation_requests ADD COLUMN projection_revision INTEGER;
ALTER TABLE invitation_requests ADD COLUMN projection_content TEXT;
ALTER TABLE invitation_requests ADD COLUMN projection_identity TEXT;
ALTER TABLE invitation_requests ADD COLUMN decision_deadline TEXT;
ALTER TABLE audit_events ADD COLUMN projection_event_id TEXT;
ALTER TABLE audit_events ADD COLUMN projection_content TEXT;
ALTER TABLE audit_events ADD COLUMN evaluated_at TEXT;
CREATE UNIQUE INDEX idx_projection_event ON audit_events(projection_event_id);

-- Admission uniqueness belongs to Restate. Reordered snapshots may temporarily
-- show multiple pending rows. Keep the legacy writer's guard until cutover.
DROP INDEX idx_one_pending_per_link_per_user;
CREATE UNIQUE INDEX idx_one_pending_per_link_per_user
  ON invitation_requests(invitation_link_id, requester_id)
  WHERE state = 'pending' AND projection_revision IS NULL;

-- Portable assertion mechanism: failing either named CHECK aborts the SQLx
-- transaction / D1 batch BEFORE commit. The final statement removes the guard.
CREATE TABLE projection_assertions (
  dependency INTEGER CONSTRAINT projection_dependency CHECK (dependency = 1),
  invariant INTEGER CONSTRAINT projection_invariant CHECK (invariant = 1)
);
