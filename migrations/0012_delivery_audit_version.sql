-- Absent version is the original audit contract. It can never be upgraded by
-- projection, even while a receipt is unconfirmed. New delivery generations
-- choose their version before their first retained snapshot.
CREATE TRIGGER delivery_audit_version_insert BEFORE INSERT ON delivery_outcomes
WHEN json_type(NEW.receipt,'$.create.audit_version') IS NOT NULL
 AND (json_type(NEW.receipt,'$.create.audit_version') IS NOT 'text'
      OR json_extract(NEW.receipt,'$.create.audit_version') NOT IN ('legacy','canonical_v1'))
BEGIN SELECT RAISE(ABORT,'projection_invariant: invalid delivery audit version'); END;
CREATE TRIGGER delivery_audit_version_update BEFORE UPDATE ON delivery_outcomes
WHEN (json_type(NEW.receipt,'$.create.audit_version') IS NOT NULL
 AND (json_type(NEW.receipt,'$.create.audit_version') IS NOT 'text'
      OR json_extract(NEW.receipt,'$.create.audit_version') NOT IN ('legacy','canonical_v1')))
 OR COALESCE(json_extract(OLD.receipt,'$.create.audit_version'),'legacy')
    IS NOT COALESCE(json_extract(NEW.receipt,'$.create.audit_version'),'legacy')
BEGIN SELECT RAISE(ABORT,'projection_invariant: delivery audit version conflict'); END;
