//! Internal projector, keyed by link ID. The link object sends each transition
//! without waiting, so admission never blocks on SQL; the per-key queue applies
//! one link's transitions in the order they were sent. A failing transition,
//! including an invariant failure awaiting operator repair/redrive, holds back
//! only that link's later transitions.
use ghinvite_core::storage::projection::{ProjectionEnvelope, ProjectionStorage};
use restate_sdk::context::{ContextSideEffects, ObjectContext, RunFuture};
use restate_sdk::endpoint::Builder;
use restate_sdk::errors::{HandlerError, TerminalError};
use restate_sdk::serde::Json;
use std::sync::Arc;

/// Internal durable projection consumer, keyed by link ID so each link's
/// transitions apply in send order; bind via [`bind`].
pub struct InvitationProjection {
    storage: Arc<dyn ProjectionStorage>,
}

/// Bind alongside `admission::bind`; keep ingress private.
pub fn bind(builder: Builder, storage: Arc<dyn ProjectionStorage>) -> Builder {
    builder.bind(InvitationProjection { storage })
}

#[restate_sdk::object]
impl InvitationProjection {
    #[handler]
    async fn apply_transition(
        &self,
        ctx: ObjectContext<'_>,
        Json(envelope): Json<ProjectionEnvelope>,
    ) -> Result<(), TerminalError> {
        // Ordering holds per key; a transition keyed elsewhere would bypass it.
        if ctx.key() != envelope.link.link_id.to_string() {
            return Err(TerminalError::new_with_code(400, "projection key mismatch"));
        }
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
        .name("apply_projection")
        .await
    }
}
