-- Exact private-owner proof permits a new current-generation epoch. Original
-- approval, source and historical completion events remain immutable.
CREATE TABLE request_reissues (
 request_id TEXT NOT NULL REFERENCES invitation_requests(id),
 operation_id TEXT NOT NULL,
 before_revision INTEGER NOT NULL CHECK(before_revision>0 AND before_revision<9007199254740991),
 after_revision INTEGER NOT NULL CHECK(after_revision=before_revision+1),
 before_content TEXT NOT NULL CHECK(json_valid(before_content)),
 after_content TEXT NOT NULL CHECK(json_valid(after_content)),
 content TEXT NOT NULL CHECK(json_valid(content)),
 PRIMARY KEY(request_id,operation_id), UNIQUE(request_id,after_revision)
);
CREATE TRIGGER request_reissues_immutable_update BEFORE UPDATE ON request_reissues
BEGIN SELECT RAISE(ABORT,'immutable reissue proof'); END;
CREATE TRIGGER request_reissues_immutable_delete BEFORE DELETE ON request_reissues
BEGIN SELECT RAISE(ABORT,'immutable reissue proof'); END;
DROP TRIGGER request_lapsed_update;
CREATE TRIGGER request_lapsed_update BEFORE UPDATE ON invitation_requests
WHEN (OLD.state='lapsed' OR NEW.state='lapsed') AND (
 (NEW.state!='lapsed' AND NOT (OLD.state='lapsed' AND NEW.state='approved' AND EXISTS (
   SELECT 1 FROM request_reissues p WHERE p.request_id=OLD.id
   AND p.after_revision=NEW.projection_revision AND p.before_revision>=OLD.projection_revision
   AND p.after_content IS NEW.projection_content
   AND (p.before_revision>OLD.projection_revision OR p.before_content IS OLD.projection_content))))
 OR OLD.state NOT IN ('approved','lapsed')
 OR NEW.projection_revision<OLD.projection_revision
 OR (OLD.state='approved' AND NEW.state='lapsed' AND NEW.projection_revision<=OLD.projection_revision)
 OR OLD.decided_by IS NOT NEW.decided_by OR OLD.decided_at IS NOT NEW.decided_at
 OR OLD.decline_reason IS NOT NEW.decline_reason OR OLD.cancellation_reason IS NOT NEW.cancellation_reason
 OR NEW.decline_reason IS NOT NULL OR NEW.cancellation_reason IS NOT NULL
 OR OLD.projection_content IS NULL OR NEW.projection_content IS NULL
 OR json_extract(OLD.projection_content,'$.decision') IS NULL
 OR json_extract(OLD.projection_content,'$.decision') IS NOT json_extract(NEW.projection_content,'$.decision')
 OR (OLD.projection_revision=NEW.projection_revision AND OLD.projection_content IS NOT NEW.projection_content)
)
BEGIN SELECT RAISE(ABORT,'invalid request lapse'); END;
-- BEFORE INSERT also runs for a stale ON CONFLICT replay. Such a replay must
-- reach the monotonic UPSERT without trying to lapse the newer approved epoch.
DROP TRIGGER request_lapsed_insert;
CREATE TRIGGER request_lapsed_insert BEFORE INSERT ON invitation_requests
WHEN NEW.state='lapsed' AND NOT EXISTS (
 SELECT 1 FROM invitation_requests old WHERE old.id=NEW.id
 AND old.state IN ('approved','lapsed')
 AND (old.state='lapsed' OR NEW.projection_revision!=old.projection_revision)
 AND old.decided_by IS NEW.decided_by AND old.decided_at IS NEW.decided_at
 AND old.decline_reason IS NEW.decline_reason AND old.cancellation_reason IS NEW.cancellation_reason
 AND NEW.decline_reason IS NULL AND NEW.cancellation_reason IS NULL
 AND old.projection_content IS NOT NULL AND NEW.projection_content IS NOT NULL
 AND json_extract(old.projection_content,'$.decision') IS NOT NULL
 AND json_extract(old.projection_content,'$.decision') IS json_extract(NEW.projection_content,'$.decision')
)
BEGIN SELECT RAISE(ABORT,'invalid request lapse'); END;
