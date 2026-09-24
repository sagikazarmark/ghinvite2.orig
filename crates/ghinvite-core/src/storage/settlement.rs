//! Validation of terminal lifecycle audit kinds; arbitration belongs to the owner.
pub fn event_type(state: crate::InvitationState) -> super::Result<crate::audit::EventType> {
    use crate::{InvitationState, audit::EventType};
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
