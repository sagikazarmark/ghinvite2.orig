//! Advisory invitation deadlines; dates never establish a lifecycle outcome.
use chrono::{DateTime, Utc};
use dioxus::prelude::*;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum InvitationExpiry {
    #[default]
    NotApplicable,
    Unavailable,
    Known(DateTime<Utc>),
}
impl InvitationExpiry {
    pub fn pending(at: Option<DateTime<Utc>>) -> Self {
        at.map_or(Self::Unavailable, Self::Known)
    }
}
#[component]
pub fn ExpiryNotice(expiry: InvitationExpiry) -> Element {
    match expiry {
        InvitationExpiry::NotApplicable => rsx! {},
        InvitationExpiry::Unavailable => rsx! {
            p { class: "text-sm", "Invitation expiry is unavailable because verified timing was not recorded. Check GitHub for the current invitation." }
        },
        InvitationExpiry::Known(at) => rsx! {
            p { class: "text-sm", "Expires at " time { datetime: at.to_rfc3339(), "{at.to_rfc3339()}" } ". This is an expected deadline; passing it does not confirm expiry or cancellation." }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn expiry_notice_is_advisory_and_missing_clock_is_explicit() {
        let at = DateTime::parse_from_rfc3339("2026-10-17T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let html = crate::testing::render(
            move || rsx! { ExpiryNotice { expiry: InvitationExpiry::Known(at) } },
        );
        assert!(html.contains("Expires at"));
        assert!(html.contains("does not confirm expiry or cancellation"));
        let html = crate::testing::render(
            || rsx! { ExpiryNotice { expiry: InvitationExpiry::Unavailable } },
        );
        assert!(html.contains("expiry is unavailable"));
        assert!(!html.contains("Expires at"));
    }
}
