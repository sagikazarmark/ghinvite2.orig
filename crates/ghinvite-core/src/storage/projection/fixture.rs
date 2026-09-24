//! Seed links and requests the way production writes them: as projector
//! envelopes. Tests build domain values, then apply `envelope(..)` with
//! [`ProjectionStorage::apply_transition`](super::ProjectionStorage).
use super::*;
use crate::request_lifecycle::TerminalDecision;
use crate::{InvitationLink, InvitationRequest};

/// A projector envelope carrying `link` and `requests` at `revision`.
///
/// Link and request revisions are independent in production; here both use
/// `revision`, so re-seeding a changed link or request needs a higher one.
/// A pending request without a deadline gets the seven-day policy deadline,
/// because the projector only accepts pending requests that have one.
pub fn envelope(
    link: &InvitationLink,
    requests: &[InvitationRequest],
    revision: u64,
) -> ProjectionEnvelope {
    ProjectionEnvelope {
        transition_id: format!("fixture/{}/{revision}", link.id),
        link: link_snapshot(link, revision),
        requests: requests
            .iter()
            .map(|request| request_snapshot(link, request, revision))
            .collect(),
        events: Vec::new(),
    }
}

pub fn link_snapshot(link: &InvitationLink, revision: u64) -> LinkSnapshot {
    LinkSnapshot {
        metadata: None,
        link_id: link.id,
        creation: CreateLink {
            link_id: link.id,
            admin: AccountAdmin {
                account_id: link.account_id,
                user_id: link.created_by,
            },
            account_id: link.account_id,
            installation_id: link.installation_id,
            description: link.description.clone(),
            internal_note: link.internal_note.clone(),
            expires_at: link.expires_at,
            max_uses: link.max_uses,
            permission: link.permission,
            approval_required: link.approval_required,
            repos: link.repos.clone(),
        },
        created_at: link.created_at,
        uses: link.uses_count.into(),
        revision,
        revoked_at: link.revoked_at,
        revoked_by: link.revoked_by,
    }
}

pub fn request_snapshot(
    link: &InvitationLink,
    request: &InvitationRequest,
    revision: u64,
) -> RequestSnapshot {
    let decision_deadline = match (request.state, request.decision_deadline) {
        (RequestState::Pending, None) => Some(request.created_at + chrono::Duration::days(7)),
        (_, deadline) => deadline,
    };
    RequestSnapshot {
        request_id: request.id,
        link_id: request.invitation_link_id,
        account_id: link.account_id,
        requester_id: request.requester_id,
        justification: request.justification.clone(),
        state: request.state,
        admitted_at: request.created_at,
        decision_deadline,
        revision,
        decision: request.decided_at.map(|effective_at| TerminalDecision {
            decision_id: format!("fixture/{}", request.id),
            decided_by: request.decided_by,
            effective_at,
            evaluated_at: effective_at,
            decline_reason: request.decline_reason.clone(),
        }),
    }
}

/// Request revision [`seed_decision`] applies, above any [`seed_request`] one.
pub const DECISION_REVISION: u64 = 1 << 32;

/// Seed `request` under its stored link, consuming one use like admission.
pub async fn seed_request<S>(s: &S, request: &InvitationRequest) -> crate::storage::Result<()>
where
    S: crate::storage::Storage + ProjectionStorage + ?Sized,
{
    let mut link = s
        .get_invitation_link_by_id(request.invitation_link_id)
        .await?
        .ok_or(crate::storage::Error::NotFound)?;
    link.uses_count += 1;
    let revision = u64::from(link.uses_count) + 1;
    s.apply_transition(&envelope(&link, std::slice::from_ref(request), revision))
        .await
}

/// Replace a seeded request with its decided state.
pub async fn seed_decision<S>(s: &S, request: &InvitationRequest) -> crate::storage::Result<()>
where
    S: crate::storage::Storage + ProjectionStorage + ?Sized,
{
    let link = s
        .get_invitation_link_by_id(request.invitation_link_id)
        .await?
        .ok_or(crate::storage::Error::NotFound)?;
    let mut projected = envelope(&link, std::slice::from_ref(request), 1);
    projected.requests[0].revision = DECISION_REVISION;
    s.apply_transition(&projected).await
}

/// Method-call form of the seeding helpers for any storage that also projects.
#[async_trait::async_trait]
pub trait Seed {
    /// Seed `link` without requests at revision 1.
    async fn seed_link(&self, link: &InvitationLink) -> crate::storage::Result<()>;
    async fn seed_request(&self, request: &InvitationRequest) -> crate::storage::Result<()>;
    async fn seed_decision(&self, request: &InvitationRequest) -> crate::storage::Result<()>;
}

#[async_trait::async_trait]
impl<S> Seed for S
where
    S: crate::storage::Storage + ProjectionStorage + ?Sized,
{
    async fn seed_link(&self, link: &InvitationLink) -> crate::storage::Result<()> {
        self.apply_transition(&envelope(link, &[], 1)).await
    }
    async fn seed_request(&self, request: &InvitationRequest) -> crate::storage::Result<()> {
        seed_request(self, request).await
    }
    async fn seed_decision(&self, request: &InvitationRequest) -> crate::storage::Result<()> {
        seed_decision(self, request).await
    }
}
