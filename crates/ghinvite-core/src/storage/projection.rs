//! Versioned, bounded admission after-images. Parents are verified identities,
//! not profile snapshots: their owners must restore missing installations/users.
use crate::audit::EventType;
use crate::{InvitationLinkId, InvitationLinkRepo, Permission, RequestId, RequestState};
use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AccountAdmin {
    pub account_id: u64,
    pub user_id: u64,
}

/// Allocate the link ID before first submission; reuse it on retry.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateLink {
    pub version: u32,
    pub link_id: InvitationLinkId,
    pub admin: AccountAdmin,
    pub account_id: u64,
    pub installation_id: u64,
    pub description: String,
    pub internal_note: Option<String>,
    pub expires_at: Option<DateTime<Utc>>,
    pub max_uses: Option<u32>,
    pub permission: Permission,
    pub approval_required: bool,
    pub repos: Vec<InvitationLinkRepo>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct LinkSnapshot {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<LinkMetadata>,
    pub link_id: InvitationLinkId,
    pub creation: CreateLink,
    pub invitation_code: String,
    pub created_at: DateTime<Utc>,
    pub uses: u64,
    pub revision: u64,
    pub revoked_at: Option<DateTime<Utc>>,
    pub revoked_by: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct LinkMetadata {
    pub description: String,
    pub internal_note: Option<String>,
}

impl LinkSnapshot {
    pub fn description(&self) -> &str {
        self.metadata
            .as_ref()
            .map(|m| m.description.as_str())
            .unwrap_or(&self.creation.description)
    }
    pub fn internal_note(&self) -> Option<&str> {
        match &self.metadata {
            Some(m) => m.internal_note.as_deref(),
            None => self.creation.internal_note.as_deref(),
        }
    }
    pub fn as_link(&self) -> crate::InvitationLink {
        crate::InvitationLink {
            id: self.link_id,
            slug: crate::Slug::from_string(self.invitation_code.clone())
                .expect("validated authoritative code"),
            installation_id: self.creation.installation_id,
            account_id: self.creation.account_id,
            created_by: self.creation.admin.user_id,
            created_at: self.created_at,
            expires_at: self.creation.expires_at,
            max_uses: self.creation.max_uses,
            uses_count: self.uses.try_into().unwrap_or(u32::MAX),
            permission: self.creation.permission,
            approval_required: self.creation.approval_required,
            description: self.description().into(),
            internal_note: self.internal_note().map(str::to_owned),
            revoked_at: self.revoked_at,
            revoked_by: self.revoked_by,
            repos: self.creation.repos.clone(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RequestSnapshot {
    pub request_id: RequestId,
    pub link_id: InvitationLinkId,
    pub account_id: u64,
    pub requester_id: u64,
    pub justification: Option<String>,
    pub state: RequestState,
    pub admitted_at: DateTime<Utc>,
    pub decision_deadline: Option<DateTime<Utc>>,
    pub revision: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision: Option<crate::request_lifecycle::TerminalDecision>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ProjectionEnvelope {
    pub version: u32,
    pub transition_id: String,
    pub link: LinkSnapshot,
    pub requests: Vec<RequestSnapshot>,
    pub events: Vec<AuditIntent>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AuditIntent {
    pub event_id: String,
    #[schemars(with = "String")]
    pub kind: EventType,
    pub actor_id: Option<u64>,
    pub target_id: String,
    pub effective_at: DateTime<Utc>,
    pub evaluated_at: DateTime<Utc>,
}

/// Atomic application, independent revisions, content-checked immutable events.
/// Errors leave the entire envelope unapplied. Retry dependency/database errors;
/// invariant errors need repair and redrive with the original envelope.
#[async_trait::async_trait]
pub trait ProjectionStorage: Send + Sync + 'static {
    async fn apply_transition(&self, envelope: &ProjectionEnvelope) -> super::Result<()>;
}

pub mod sql;

/// Test seeding through the projector.
#[cfg(feature = "test-suite")]
pub mod fixture;
