-- Same nanosecond-preserving UTC key as storage::request_history.
CREATE INDEX idx_request_history ON invitation_requests (
    invitation_link_id,
    (substr(created_at,1,19) || '.' || substr(CASE WHEN substr(created_at,20,1) = '.' THEN replace(replace(substr(created_at,21),'+00:00',''),'Z','') ELSE '' END || '000000000',1,9)) DESC,
    id DESC
);
