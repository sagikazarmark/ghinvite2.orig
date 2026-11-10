-- See ID-47: fresh initial schema namespaces organization delivery identities.
-- Full immutable typed receipts are retained separately from their read columns.
ALTER TABLE projection_assertions ADD COLUMN unchanged INTEGER
    CONSTRAINT projection_changed CHECK(unchanged = 1);
CREATE TABLE delivery_steps (
    delivery_id TEXT NOT NULL REFERENCES deliveries(id),
    step TEXT NOT NULL CHECK(step GLOB 'team:[0-9]*'),
    receipt TEXT NOT NULL CHECK(json_valid(receipt)),
    outcome TEXT NOT NULL,
    reason TEXT,
    updated_at TEXT NOT NULL,
    projection_revision INTEGER NOT NULL CHECK(projection_revision BETWEEN 1 AND 9007199254740991),
    CHECK(step = 'team:' || json_extract(receipt,'$.create.team_id')),
    CHECK(outcome = json_extract(receipt,'$.create.outcome.kind')),
    CHECK(reason IS json_extract(receipt,'$.create.outcome.reason')),
    CHECK(projection_revision = json_extract(receipt,'$.revision')),
    PRIMARY KEY(delivery_id,step)
);
CREATE UNIQUE INDEX organization_attempt_identity ON delivery_attempts(step,json_extract(command,'$.create.invitation_id'));
CREATE TRIGGER organization_step_parent BEFORE INSERT ON delivery_steps
WHEN NOT EXISTS(SELECT 1 FROM deliveries WHERE id=NEW.delivery_id AND kind='org')
BEGIN SELECT RAISE(ABORT,'organization step parent conflict'); END;
CREATE TRIGGER organization_step_immutable BEFORE UPDATE ON delivery_steps
WHEN NEW.delivery_id!=OLD.delivery_id OR NEW.step!=OLD.step
 OR NEW.projection_revision<OLD.projection_revision
 OR (NEW.projection_revision=OLD.projection_revision AND NEW.receipt!=OLD.receipt)
 OR (json_type(OLD.receipt,'$.create.confirmed_at')!='null' AND json_extract(OLD.receipt,'$.create')!=json_extract(NEW.receipt,'$.create'))
 OR (json_type(OLD.receipt,'$.settlement')='object' AND json_extract(OLD.receipt,'$.settlement') IS NOT json_extract(NEW.receipt,'$.settlement'))
 OR (json_type(OLD.receipt,'$.tracking_stopped')!='null' AND json_extract(OLD.receipt,'$.tracking_stopped') IS NOT json_extract(NEW.receipt,'$.tracking_stopped'))
BEGIN SELECT RAISE(ABORT,'organization step receipt conflict'); END;
