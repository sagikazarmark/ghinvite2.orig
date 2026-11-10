-- Historical administrator-only notes remain private. Only future explicit
-- decisions may populate this separate field; no backfill from decline_reason.
ALTER TABLE invitation_requests ADD COLUMN requester_message TEXT;
CREATE TRIGGER request_message_insert_guard
BEFORE INSERT ON invitation_requests
WHEN NEW.requester_message IS NOT NULL AND (
  NEW.state != 'declined' OR typeof(NEW.requester_message) != 'text'
  OR length(NEW.requester_message) NOT BETWEEN 1 AND 500
  OR trim(NEW.requester_message) != NEW.requester_message
  OR instr(NEW.requester_message,char(0)) != 0)
BEGIN SELECT RAISE(ABORT,'invalid requester message'); END;
CREATE TRIGGER request_message_update_guard
BEFORE UPDATE ON invitation_requests
WHEN (OLD.state != 'pending' AND OLD.requester_message IS NOT NEW.requester_message)
  OR (NEW.requester_message IS NOT NULL AND (
    NEW.state != 'declined' OR typeof(NEW.requester_message) != 'text'
    OR length(NEW.requester_message) NOT BETWEEN 1 AND 500
    OR trim(NEW.requester_message) != NEW.requester_message
    OR instr(NEW.requester_message,char(0)) != 0))
BEGIN SELECT RAISE(ABORT,'immutable or invalid requester message'); END;
