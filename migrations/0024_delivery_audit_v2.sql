-- New canonical generations omit the retired local invitation ULID. Historical
-- Legacy/V1 receipts retain the exact original property, value and audit version.
-- Existing immutable command/confirmed-receipt guards remain in force.
DROP TRIGGER delivery_audit_version_insert;
DROP TRIGGER delivery_audit_version_update;
CREATE TRIGGER delivery_audit_version_insert BEFORE INSERT ON delivery_outcomes
WHEN (json_type(NEW.receipt,'$.create.audit_version') IS NOT NULL
 AND (json_type(NEW.receipt,'$.create.audit_version') IS NOT 'text'
 OR json_extract(NEW.receipt,'$.create.audit_version') NOT IN ('legacy','canonical_v1','canonical_v2')))
 OR (COALESCE(json_extract(NEW.receipt,'$.create.audit_version'),'legacy') = 'canonical_v2'
 AND json_type(NEW.receipt,'$.create.command.invitation_id') IS NOT NULL)
 OR (COALESCE(json_extract(NEW.receipt,'$.create.audit_version'),'legacy') != 'canonical_v2'
 AND (json_type(NEW.receipt,'$.create.command.invitation_id') IS NOT 'text'
 OR length(json_extract(NEW.receipt,'$.create.command.invitation_id')) != 26
 OR substr(json_extract(NEW.receipt,'$.create.command.invitation_id'),1,1) NOT GLOB '[0-7]'
 OR json_extract(NEW.receipt,'$.create.command.invitation_id') GLOB '*[^0123456789ABCDEFGHJKMNPQRSTVWXYZ]*'))
BEGIN SELECT RAISE(ABORT,'projection_invariant: delivery audit version/identity conflict'); END;
CREATE TRIGGER delivery_audit_version_update BEFORE UPDATE ON delivery_outcomes
WHEN (json_type(NEW.receipt,'$.create.audit_version') IS NOT NULL
 AND (json_type(NEW.receipt,'$.create.audit_version') IS NOT 'text'
 OR json_extract(NEW.receipt,'$.create.audit_version') NOT IN ('legacy','canonical_v1','canonical_v2')))
 OR (COALESCE(json_extract(NEW.receipt,'$.create.audit_version'),'legacy') = 'canonical_v2'
 AND json_type(NEW.receipt,'$.create.command.invitation_id') IS NOT NULL)
 OR (COALESCE(json_extract(NEW.receipt,'$.create.audit_version'),'legacy') != 'canonical_v2'
 AND (json_type(NEW.receipt,'$.create.command.invitation_id') IS NOT 'text'
 OR length(json_extract(NEW.receipt,'$.create.command.invitation_id')) != 26
 OR substr(json_extract(NEW.receipt,'$.create.command.invitation_id'),1,1) NOT GLOB '[0-7]'
 OR json_extract(NEW.receipt,'$.create.command.invitation_id') GLOB '*[^0123456789ABCDEFGHJKMNPQRSTVWXYZ]*'))
 OR COALESCE(json_extract(OLD.receipt,'$.create.audit_version'),'legacy') IS NOT COALESCE(json_extract(NEW.receipt,'$.create.audit_version'),'legacy')
BEGIN SELECT RAISE(ABORT,'projection_invariant: delivery audit version/identity conflict'); END;

-- Preserve legacy one-to-one event scope through retained immutable receipts,
-- without a second identity column on new canonical delivery rows.
CREATE UNIQUE INDEX delivery_legacy_invitation_identity
ON delivery_outcomes(json_extract(receipt,'$.create.command.invitation_id'))
WHERE json_type(receipt,'$.create.command.invitation_id')='text';
