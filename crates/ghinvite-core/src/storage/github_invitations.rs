//! GitHub invitation rows and member-webhook routing, shared by SQLite and D1.
//! Rows decode as [`crate::GithubInvitation`].
use crate::GithubInvitationId;
use serde::Deserialize;

macro_rules! select {
    ($filter:literal) => {
        concat!(
            "SELECT id, invitation_request_id, repo_id, github_invitation_id, state, error_message, created_at, updated_at FROM github_invitations ",
            $filter
        )
    };
}

/// Bind ID, request ID, repository ID, nullable upstream ID, state, nullable
/// error message, created and updated times.
pub const INSERT: &str = "INSERT INTO github_invitations (id, invitation_request_id, repo_id, github_invitation_id, state, error_message, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)";
pub const GET: &str = select!("WHERE id = ?1");
pub const BY_GITHUB_ID: &str = select!("WHERE github_invitation_id = ?1");
pub const FOR_REQUEST: &str = select!("WHERE invitation_request_id = ?1 ORDER BY id");

pub const PENDING_FOR_ACCOUNT: &str = "SELECT g.id, g.invitation_request_id, g.repo_id,
    g.github_invitation_id, g.state, g.error_message, g.created_at, g.updated_at
    FROM github_invitations g
    JOIN invitation_requests r ON r.id = g.invitation_request_id
    JOIN invitation_links l ON l.id = r.invitation_link_id
    WHERE l.account_id = ?1 AND g.state IN ('sending', 'sent')
    ORDER BY g.created_at";

/// Include terminal history: member events carry no invitation ID or event time,
/// so a historical duplicate must not be reassigned to a later request. Two rows
/// suffice to detect ambiguity without loading an account's invitation history.
pub const MEMBER_CANDIDATES: &str = "SELECT g.id, g.invitation_request_id,
    g.repo_id, g.github_invitation_id, g.state, g.error_message, g.created_at, g.updated_at
    FROM github_invitations g
    JOIN invitation_requests r ON r.id = g.invitation_request_id
    JOIN invitation_links l ON l.id = r.invitation_link_id
    WHERE l.account_id = ?1 AND g.repo_id = ?2 AND r.requester_id = ?3
    AND NOT EXISTS (
      SELECT 1 FROM invitation_links scope WHERE scope.account_id = ?1
      AND scope.uses_count > (SELECT COUNT(*) FROM invitation_requests seen WHERE seen.invitation_link_id = scope.id)
    )
    AND NOT EXISTS (
      SELECT 1 FROM invitation_requests pending
      JOIN invitation_links scope ON scope.id = pending.invitation_link_id
      JOIN invitation_link_repos repo ON repo.invitation_link_id = scope.id
      WHERE scope.account_id = ?1 AND pending.requester_id = ?3 AND repo.repo_id = ?2
      AND pending.state = 'approved'
      AND NOT EXISTS (SELECT 1 FROM github_invitations known WHERE known.invitation_request_id = pending.id AND known.repo_id = ?2)
    ) LIMIT 2";

/// Bind body hash and nullable invitation ID; the first binding is retained.
pub const BIND_MEMBER_WEBHOOK: &str = "INSERT INTO member_webhook_receipts(payload_sha256, invitation_id) VALUES (?1, ?2) ON CONFLICT DO NOTHING";
pub const MEMBER_WEBHOOK_BINDING: &str =
    "SELECT invitation_id FROM member_webhook_receipts WHERE payload_sha256 = ?1";

#[derive(Debug, Deserialize)]
pub struct MemberWebhookBinding {
    pub invitation_id: Option<GithubInvitationId>,
}
