-- Account-local seek index. The owning link remains the authorization source;
-- this derived key only lets SQLite seek without scanning other accounts.
ALTER TABLE invitation_requests ADD COLUMN queue_account_id INTEGER;
UPDATE invitation_requests SET queue_account_id = (
  SELECT account_id FROM invitation_links WHERE id = invitation_link_id
);
CREATE TRIGGER requests_queue_insert AFTER INSERT ON invitation_requests BEGIN
  UPDATE invitation_requests SET queue_account_id = (
    SELECT account_id FROM invitation_links WHERE id = NEW.invitation_link_id
  ) WHERE id = NEW.id;
END;
CREATE TRIGGER requests_queue_move AFTER UPDATE OF invitation_link_id ON invitation_requests BEGIN
  UPDATE invitation_requests SET queue_account_id = (
    SELECT account_id FROM invitation_links WHERE id = NEW.invitation_link_id
  ) WHERE id = NEW.id;
END;
CREATE TRIGGER links_queue_account AFTER UPDATE OF account_id ON invitation_links BEGIN
  UPDATE invitation_requests SET queue_account_id = NEW.account_id WHERE invitation_link_id = NEW.id;
END;
CREATE TRIGGER links_queue_restore AFTER INSERT ON invitation_links BEGIN
  UPDATE invitation_requests SET queue_account_id = NEW.account_id WHERE invitation_link_id = NEW.id;
END;
-- Normalize both writers' UTC RFC3339 representations, preserving nanoseconds.
CREATE INDEX idx_pending_queue_seek ON invitation_requests(
  queue_account_id,
  (substr(created_at,1,19) || '.' || substr(CASE WHEN substr(created_at,20,1) = '.' THEN replace(replace(substr(created_at,21),'+00:00',''),'Z','') ELSE '' END || '000000000',1,9) || '/' || id)
) WHERE state = 'pending';
