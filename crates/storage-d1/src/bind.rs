//! D1 parameter binding helpers + conflict-error classification.
#[cfg(target_arch = "wasm32")]
pub use wasm_impl::*;

#[cfg(target_arch = "wasm32")]
mod wasm_impl {
    use storage::{ConflictKind, Error};

    /// Convert a worker::Error to a storage::Error by inspecting the message string.
    pub fn classify_d1_error(e: worker::Error) -> Error {
        let msg = e.to_string();
        if msg.contains("UNIQUE constraint failed") {
            if msg.contains("share_links.slug") {
                return Error::Conflict(ConflictKind::DuplicateSlug);
            }
            // DuplicatePendingRequest: partial unique index covers BOTH columns.
            // Check both to avoid misclassifying PK collisions on invitation_requests.id.
            if msg.contains("invitation_requests.share_link_id")
                && msg.contains("invitation_requests.requester_id")
            {
                return Error::Conflict(ConflictKind::DuplicatePendingRequest);
            }
            // DuplicateActiveInstallation: partial unique index on installations.account_id
            if msg.contains("installations.account_id") {
                return Error::Conflict(ConflictKind::DuplicateActiveInstallation);
            }
            return Error::Conflict(ConflictKind::DuplicateId);
        }
        if msg.contains("FOREIGN KEY constraint failed") {
            return Error::Conflict(ConflictKind::ForeignKey);
        }
        Error::Database(msg)
    }
}
