-- Request cancellation is a local terminal decision, separate from a declined
-- request and from cancellation of an upstream GitHub invitation.
ALTER TABLE invitation_requests ADD COLUMN cancellation_reason TEXT
    CHECK(cancellation_reason IS NULL OR cancellation_reason = 'link_revoked');
CREATE TRIGGER request_cancellation_insert BEFORE INSERT ON invitation_requests
WHEN (NEW.state = 'cancelled') != (NEW.cancellation_reason IS NOT NULL)
BEGIN SELECT RAISE(ABORT,'invalid request cancellation'); END;
CREATE TRIGGER request_cancellation_update BEFORE UPDATE ON invitation_requests
WHEN (NEW.state = 'cancelled') != (NEW.cancellation_reason IS NOT NULL)
BEGIN SELECT RAISE(ABORT,'invalid request cancellation'); END;
