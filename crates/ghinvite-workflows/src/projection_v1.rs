//! Internal ordinary service. Retains failing work independently of link
//! exclusivity, including invariant failures awaiting operator repair/redrive.
use crate::admission_v1::InvitationProjectionV1;
use ghinvite_core::storage::projection::{ProjectionEnvelope, ProjectionStorage};
use restate_sdk::context::{Context, ContextSideEffects, RunFuture};
use restate_sdk::endpoint::Builder;
use restate_sdk::errors::{HandlerError, TerminalError};
use restate_sdk::serde::Json;
use std::sync::Arc;

pub struct InvitationProjectionV1Impl {
    storage: Arc<dyn ProjectionStorage>,
}

/// Explicit opt-in alongside `admission_v1::bind`; keep ingress private.
pub fn bind(builder: Builder, storage: Arc<dyn ProjectionStorage>) -> Builder {
    builder.bind(InvitationProjectionV1Impl { storage }.serve())
}

impl InvitationProjectionV1 for InvitationProjectionV1Impl {
    async fn apply_transition(
        &self,
        ctx: Context<'_>,
        Json(envelope): Json<ProjectionEnvelope>,
    ) -> Result<(), TerminalError> {
        ctx.run(|| async {
            self.storage
                .apply_transition(&envelope)
                .await
                .map_err(|error| {
                    // No justification, invitation code, metadata, or profile data.
                    let failure = match &error {
                        ghinvite_core::storage::Error::ProjectionDependency => "dependency",
                        ghinvite_core::storage::Error::ProjectionInvariant(_) => "invariant",
                        _ => "storage",
                    };
                    tracing::error!(transition_id = %envelope.transition_id,
                    link_id = %envelope.link.link_id, failure, "projection pending repair/retry");
                    HandlerError::from(error)
                })
        })
        .name("apply_projection_v1")
        .await
    }
}
