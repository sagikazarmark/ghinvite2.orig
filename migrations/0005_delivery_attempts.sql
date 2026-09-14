-- Restate owns create receipts. This immutable fence additionally guards the
-- HTTP run closure, whose result acknowledgement can be lost before journaling.
CREATE TABLE delivery_attempts (
    invitation_id TEXT PRIMARY KEY NOT NULL,
    command TEXT NOT NULL,
    generation INTEGER NOT NULL DEFAULT 1,
    retryable INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE delivery_outcomes (
    invitation_id TEXT PRIMARY KEY NOT NULL,
    request_id TEXT NOT NULL REFERENCES invitation_requests(id),
    receipt TEXT NOT NULL
);
CREATE INDEX delivery_outcomes_request ON delivery_outcomes(request_id);

CREATE TRIGGER delivery_outcome_identity BEFORE UPDATE ON delivery_outcomes
WHEN json_extract(OLD.receipt, '$.command') != json_extract(NEW.receipt, '$.command')
  OR (json_extract(OLD.receipt, '$.revision') = json_extract(NEW.receipt, '$.revision') AND OLD.receipt != NEW.receipt)
BEGIN SELECT RAISE(ABORT, 'delivery outcome conflict'); END;

-- Apply only the initial create lifecycle. Concurrent webhook/cancel updates
-- cannot be overwritten by a delayed create projection.
CREATE TRIGGER delivery_created AFTER UPDATE ON delivery_outcomes
WHEN json_extract(NEW.receipt, '$.outcome.kind') IN ('created', 'already_collaborator', 'failed')
BEGIN
  UPDATE github_invitations SET
    state = CASE json_extract(NEW.receipt, '$.outcome.kind') WHEN 'created' THEN 'sent' WHEN 'already_collaborator' THEN 'accepted' ELSE 'failed' END,
    github_invitation_id = json_extract(NEW.receipt, '$.outcome.upstream_id')
  WHERE id = NEW.invitation_id AND state = 'sending';
END;

CREATE TRIGGER delivery_created_initial AFTER INSERT ON delivery_outcomes
WHEN json_extract(NEW.receipt, '$.outcome.kind') IN ('created', 'already_collaborator', 'failed')
BEGIN
  UPDATE github_invitations SET
    state = CASE json_extract(NEW.receipt, '$.outcome.kind') WHEN 'created' THEN 'sent' WHEN 'already_collaborator' THEN 'accepted' ELSE 'failed' END,
    github_invitation_id = json_extract(NEW.receipt, '$.outcome.upstream_id')
  WHERE id = NEW.invitation_id AND state = 'sending';
END;
