-- Signed-body identity, not the unsigned delivery header. Retain no-match too:
-- an old untracked event must never bind to a subsequently created invitation.
CREATE TABLE member_webhook_receipts (
    payload_sha256 TEXT PRIMARY KEY NOT NULL,
    invitation_id TEXT REFERENCES github_invitations(id)
);
