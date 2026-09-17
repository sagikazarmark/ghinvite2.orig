-- One immutable winning settlement per GitHub invitation. State and audit are
-- committed with this receipt in a SQLx transaction / D1 batch.
CREATE TABLE github_invitation_settlements (
    invitation_id TEXT PRIMARY KEY REFERENCES github_invitations(id),
    content TEXT NOT NULL
);

-- Defense in depth against an old invocation's delayed SQL write. Deployment
-- cutover must still drain/isolate legacy writers before enabling new ingress.
CREATE TRIGGER github_invitation_settlement_fence BEFORE UPDATE ON github_invitations
WHEN EXISTS (SELECT 1 FROM github_invitation_settlements s WHERE s.invitation_id = OLD.id)
 AND (NEW.state IS NOT (SELECT json_extract(content, '$.state') FROM github_invitation_settlements WHERE invitation_id = OLD.id)
   OR NEW.github_invitation_id IS NOT OLD.github_invitation_id
   OR NEW.invitation_request_id IS NOT OLD.invitation_request_id
   OR NEW.repo_id IS NOT OLD.repo_id)
BEGIN SELECT RAISE(ABORT, 'settled invitation writer conflict'); END;
