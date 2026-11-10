-- Lapsed is completion of an already approved request, never a new decision.
-- The shared projector atomically replays the retained original approval before
-- completion. Raw writes cannot skip that predecessor or replace its decision.
CREATE TRIGGER request_lapsed_insert BEFORE INSERT ON invitation_requests
WHEN NEW.state='lapsed' AND NOT EXISTS (
  SELECT 1 FROM invitation_requests old WHERE old.id=NEW.id
    AND old.state IN ('approved','lapsed')
    AND (old.state='lapsed' OR NEW.projection_revision>old.projection_revision)
    AND old.decided_by IS NEW.decided_by AND old.decided_at IS NEW.decided_at
    AND old.decline_reason IS NEW.decline_reason
    AND old.cancellation_reason IS NEW.cancellation_reason
    AND NEW.decline_reason IS NULL AND NEW.cancellation_reason IS NULL
    AND old.projection_content IS NOT NULL AND NEW.projection_content IS NOT NULL
    AND json_extract(old.projection_content,'$.decision') IS NOT NULL
    AND json_extract(old.projection_content,'$.decision') IS json_extract(NEW.projection_content,'$.decision')
)
BEGIN SELECT RAISE(ABORT,'invalid request lapse'); END;

CREATE TRIGGER request_lapsed_update BEFORE UPDATE ON invitation_requests
WHEN (OLD.state='lapsed' OR NEW.state='lapsed') AND (
  NEW.state!='lapsed' OR OLD.state NOT IN ('approved','lapsed')
  OR NEW.projection_revision<OLD.projection_revision
  OR (OLD.state='approved' AND NEW.projection_revision<=OLD.projection_revision)
  OR OLD.decided_by IS NOT NEW.decided_by OR OLD.decided_at IS NOT NEW.decided_at
  OR OLD.decline_reason IS NOT NEW.decline_reason
  OR OLD.cancellation_reason IS NOT NEW.cancellation_reason
  OR NEW.decline_reason IS NOT NULL OR NEW.cancellation_reason IS NOT NULL
  OR OLD.projection_content IS NULL OR NEW.projection_content IS NULL
  OR json_extract(OLD.projection_content,'$.decision') IS NULL
  OR json_extract(OLD.projection_content,'$.decision') IS NOT json_extract(NEW.projection_content,'$.decision')
  OR (OLD.projection_revision=NEW.projection_revision AND OLD.projection_content IS NOT NEW.projection_content)
)
BEGIN SELECT RAISE(ABORT,'invalid request lapse'); END;
