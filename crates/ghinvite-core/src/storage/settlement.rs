//! Atomic, expected-state-fenced GitHub invitation settlement.
use crate::{GithubInvitation, InvitationState, audit::AuditEvent};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Settlement {
    pub expected: GithubInvitation,
    pub state: InvitationState,
    pub event: AuditEvent,
}

pub fn event_type(state: InvitationState) -> super::Result<crate::audit::EventType> {
    use crate::audit::EventType;
    Ok(match state {
        InvitationState::Accepted => EventType::InvitationAccepted,
        InvitationState::Declined => EventType::InvitationDeclined,
        InvitationState::Cancelled => EventType::InvitationCancelled,
        InvitationState::Expired => EventType::InvitationExpired,
        _ => {
            return Err(super::Error::ProjectionInvariant(
                "invalid settlement state".into(),
            ));
        }
    })
}

pub fn encode(value: &Settlement) -> super::Result<String> {
    use crate::audit::TargetKind;
    if value.expected.state != InvitationState::Sent
        || value.expected.github_invitation_id.is_none()
        || value.event.target_kind != TargetKind::GithubInvitation
        || value.event.target_id != value.expected.id.to_string()
        || value.event.event_type != event_type(value.state)?
    {
        return Err(super::Error::ProjectionInvariant(
            "invalid settlement evidence".into(),
        ));
    }
    serde_json::to_string(value).map_err(|e| super::Error::Corrupt(e.to_string()))
}

/// The same statements execute in one native transaction or actual D1 batch.
/// The retained winner makes audit publication independent of caller retries.
pub const STATEMENTS: &[&str] = &[
    "INSERT INTO github_invitation_settlements(invitation_id, content)
     SELECT i.id, ?1 FROM github_invitations i
     JOIN invitation_requests r ON r.id = i.invitation_request_id
     JOIN invitation_links l ON l.id = r.invitation_link_id
     WHERE i.id = json_extract(?1, '$.expected.id') AND i.state = 'sent'
       AND i.github_invitation_id = json_extract(?1, '$.expected.github_invitation_id')
       AND i.invitation_request_id = json_extract(?1, '$.expected.invitation_request_id')
       AND i.repo_id = json_extract(?1, '$.expected.repo_id')
       AND l.account_id = json_extract(?1, '$.event.account_id')
     ON CONFLICT(invitation_id) DO NOTHING",
    "INSERT INTO audit_events(id, account_id, occurred_at, event_type, actor_kind, actor_id, target_kind, target_id, metadata, request_id)
     SELECT json_extract(content, '$.event.id'), json_extract(content, '$.event.account_id'),
       json_extract(content, '$.event.occurred_at'), json_extract(content, '$.event.event_type'),
       json_extract(content, '$.event.actor_kind'), json_extract(content, '$.event.actor_id'),
       json_extract(content, '$.event.target_kind'), json_extract(content, '$.event.target_id'),
       json_extract(content, '$.event.metadata'), json_extract(content, '$.event.request_id')
     FROM github_invitation_settlements WHERE invitation_id = json_extract(?1, '$.expected.id')
     ON CONFLICT(id) DO UPDATE SET account_id = CASE WHEN
       audit_events.account_id IS excluded.account_id AND audit_events.occurred_at IS excluded.occurred_at
       AND audit_events.event_type IS excluded.event_type AND audit_events.actor_kind IS excluded.actor_kind
       AND audit_events.actor_id IS excluded.actor_id AND audit_events.target_kind IS excluded.target_kind
       AND audit_events.target_id IS excluded.target_id AND audit_events.metadata IS excluded.metadata
       AND audit_events.request_id IS excluded.request_id THEN audit_events.account_id ELSE NULL END",
    "UPDATE github_invitations SET
       state = (SELECT json_extract(content, '$.state') FROM github_invitation_settlements WHERE invitation_id = github_invitations.id),
       updated_at = (SELECT json_extract(content, '$.event.occurred_at') FROM github_invitation_settlements WHERE invitation_id = github_invitations.id),
       error_message = NULL
     WHERE id = json_extract(?1, '$.expected.id') AND state = 'sent'
       AND EXISTS (SELECT 1 FROM github_invitation_settlements WHERE invitation_id = github_invitations.id)",
];
