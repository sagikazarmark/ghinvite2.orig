//! Invitation link object state keys (ADR 0003 split-key layout).
//!
//! Every record is its own key so lazy state reads only what a command
//! touches; nothing here enumerates or scans history.

use ghinvite_core::request_lifecycle::LifecycleOperationId;
use ghinvite_core::{GithubInvitationId, RequestId};

use super::AdmissionOperationId;

/// The hot link record: guardrails, metadata, revocation, uses, revision.
pub(super) const LINK: &str = "link";
/// The original creation receipt, returned on creation replay.
pub(super) const CREATION: &str = "creation";

pub(super) fn request(id: RequestId) -> String {
    format!("request/{id}")
}

/// The one pending-or-approved request currently blocking a requester.
pub(super) fn blocker(requester_id: u64) -> String {
    format!("blocker/{requester_id}")
}

/// The retained admission outcome for an attempt.
pub(super) fn op(id: &AdmissionOperationId) -> String {
    format!("op/{}", String::from(id.clone()))
}

/// A prepared attempt that has no retained outcome yet.
pub(super) fn attempt(id: &AdmissionOperationId) -> String {
    format!("attempt/{}", String::from(id.clone()))
}

pub(super) fn latest_attempt(requester_id: u64) -> String {
    format!("latest-attempt/{requester_id}")
}

pub(super) fn dispatch(request_id: RequestId) -> String {
    format!("dispatch/{request_id}")
}

pub(super) fn submitted(invitation_id: GithubInvitationId) -> String {
    format!("submitted/{invitation_id}")
}

pub(super) fn consumed(request_id: RequestId) -> String {
    format!("consumed/{request_id}")
}

/// The retained receipt for an account-admin decision command.
pub(super) fn lifecycle_op(id: &LifecycleOperationId) -> String {
    format!("lifecycle-op/{}", String::from(id.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_keep_their_stored_encoding() {
        let request_id: RequestId = "01ARZ3NDEKTSV4RRFFQ69G5FAV".parse().unwrap();
        let invitation_id: GithubInvitationId = "01ARZ3NDEKTSV4RRFFQ69G5FAW".parse().unwrap();
        let admission =
            AdmissionOperationId::try_from("01ARZ3NDEKTSV4RRFFQ69G5FAX".to_owned()).unwrap();
        let lifecycle =
            LifecycleOperationId::try_from("01ARZ3NDEKTSV4RRFFQ69G5FAY".to_owned()).unwrap();
        assert_eq!(LINK, "link");
        assert_eq!(CREATION, "creation");
        assert_eq!(request(request_id), "request/01ARZ3NDEKTSV4RRFFQ69G5FAV");
        assert_eq!(blocker(71), "blocker/71");
        assert_eq!(op(&admission), "op/01ARZ3NDEKTSV4RRFFQ69G5FAX");
        assert_eq!(attempt(&admission), "attempt/01ARZ3NDEKTSV4RRFFQ69G5FAX");
        assert_eq!(latest_attempt(71), "latest-attempt/71");
        assert_eq!(dispatch(request_id), "dispatch/01ARZ3NDEKTSV4RRFFQ69G5FAV");
        assert_eq!(
            submitted(invitation_id),
            "submitted/01ARZ3NDEKTSV4RRFFQ69G5FAW"
        );
        assert_eq!(consumed(request_id), "consumed/01ARZ3NDEKTSV4RRFFQ69G5FAV");
        assert_eq!(
            lifecycle_op(&lifecycle),
            "lifecycle-op/01ARZ3NDEKTSV4RRFFQ69G5FAY"
        );
    }
}
