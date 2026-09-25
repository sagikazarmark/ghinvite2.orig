//! Bounded admission after-images. Parents are verified identities,
//! not profile snapshots: their owners must restore missing installations/users.
use crate::RequestState;
use crate::audit::EventType;
use crate::invitation_link::LinkSnapshot;
use crate::request_lifecycle::RequestSnapshot;
use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProjectionEnvelope {
    pub transition_id: String,
    pub link: LinkSnapshot,
    pub events: Vec<AuditIntent>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RequestProjectionEnvelope {
    pub request: RequestSnapshot,
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
    async fn apply_request(&self, envelope: &RequestProjectionEnvelope) -> super::Result<()>;
}

pub mod request_sql;
pub mod sql;

/// Test seeding through the projector.
#[cfg(feature = "test-suite")]
pub mod fixture;
