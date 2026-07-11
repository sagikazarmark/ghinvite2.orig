//! Audit-event emission helper. Every handler that performs a state
//! transition calls [`emit`] once after the underlying write succeeds.

use crate::error::Result;
use crate::state::AppState;
use chrono::Utc;
use ghinvite_core::AuditEventId;
use ghinvite_core::audit::{ActorKind, AuditEvent, EventType, TargetKind};

/// Description of who triggered the change. Mapped to the `actor_kind` /
/// `actor_id` columns. The `User` variant carries the user_id; `System` and
/// `Github` carry no id.
#[derive(Clone, Copy, Debug)]
pub enum Actor {
    User(u64),
    System,
    Github,
}

impl Actor {
    fn split(self) -> (ActorKind, Option<u64>) {
        match self {
            Actor::User(id) => (ActorKind::User, Some(id)),
            Actor::System => (ActorKind::System, None),
            Actor::Github => (ActorKind::Github, None),
        }
    }
}

/// Description of what the change was about. `id` is the natural string id
/// for the target type (ulid for invitation_links, requests, github_invitations;
/// stringified u64 for installations).
#[derive(Clone, Debug)]
pub struct Target {
    pub kind: TargetKind,
    pub id: String,
}

impl Target {
    pub fn installation(installation_id: u64) -> Self {
        Self {
            kind: TargetKind::Installation,
            id: installation_id.to_string(),
        }
    }

    pub fn invitation_link(link_id: ghinvite_core::InvitationLinkId) -> Self {
        Self {
            kind: TargetKind::InvitationLink,
            id: link_id.to_string(),
        }
    }

    pub fn request(request_id: ghinvite_core::RequestId) -> Self {
        Self {
            kind: TargetKind::InvitationRequest,
            id: request_id.to_string(),
        }
    }

    pub fn github_invitation(id: ghinvite_core::GithubInvitationId) -> Self {
        Self {
            kind: TargetKind::GithubInvitation,
            id: id.to_string(),
        }
    }
}

/// Emit an audit event. Should be wrapped in `ctx.run("audit", async {...})`
/// at the call site so Restate captures it as a durable step.
pub async fn emit(
    state: &AppState,
    account_id: u64,
    event_type: EventType,
    actor: Actor,
    target: Target,
    metadata: serde_json::Value,
    request_id: Option<String>,
) -> Result<()> {
    let (actor_kind, actor_id) = actor.split();
    let event = AuditEvent {
        id: AuditEventId::new(),
        account_id,
        occurred_at: Utc::now(),
        event_type,
        actor_kind,
        actor_id,
        target_kind: target.kind,
        target_id: target.id,
        metadata,
        request_id,
    };
    state.storage.audit(&event).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::fixture_state;

    #[tokio::test]
    async fn emit_writes_event_to_storage() {
        let state = fixture_state().await;
        let result = emit(
            &state,
            42,
            EventType::InvitationLinkCreated,
            Actor::User(7),
            Target::invitation_link(ghinvite_core::InvitationLinkId::new()),
            serde_json::json!({"slug": "abcdef"}),
            Some("inv-1".into()),
        )
        .await;
        assert!(result.is_ok(), "emit should succeed: {result:?}");
    }

    #[test]
    fn actor_split_user_carries_id() {
        let (k, id) = Actor::User(7).split();
        assert_eq!(k, ActorKind::User);
        assert_eq!(id, Some(7));
    }

    #[test]
    fn actor_split_system_carries_no_id() {
        let (k, id) = Actor::System.split();
        assert_eq!(k, ActorKind::System);
        assert_eq!(id, None);
    }

    #[test]
    fn target_invitation_link_uses_ulid_string() {
        let id = ghinvite_core::InvitationLinkId::new();
        let t = Target::invitation_link(id);
        assert_eq!(t.kind, TargetKind::InvitationLink);
        assert_eq!(t.id, id.to_string());
    }
}
