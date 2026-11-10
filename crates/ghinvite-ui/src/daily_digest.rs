//! Daily snapshot shown only after Console scope has filtered its counts.
use chrono::NaiveDate;
use dioxus::prelude::*;
use ghinvite_core::storage::blocked_digest::BlockedDigest;

#[component]
pub fn DailyDeliveryDigest(
    digest: Option<BlockedDigest>,
    requested_day: NaiveDate,
    #[props(default)] unavailable: bool,
) -> Element {
    rsx! {
        section { class:"mac-panel mb-4 p-4", aria_labelledby:"daily-delivery-summary",
            h2 { id:"daily-delivery-summary", class:"text-sm font-semibold", "Daily delivery summary" }
            if unavailable {
                p { role:"alert", class:"mt-2 text-sm", "The delivery summary could not be loaded. Try again later." }
            } else if let Some(digest) = digest {
                p { class:"mt-2 text-sm text-muted", "Observed {digest.observed_at.to_rfc3339()}. Counts reflect that daily snapshot." }
                if digest.stale_for(requested_day) {
                    p { role:"status", class:"mt-2 text-sm", "Today's summary is not ready. Showing the latest available summary from {digest.day}." }
                }
                if digest.counts.is_empty() {
                    p { class:"mt-2 text-sm", "No blocked deliveries in this summary." }
                } else {
                    p { class:"mt-2 text-sm", "These deliveries are waiting for a prerequisite. Review the reason before retrying." }
                    ul { class:"mt-2 list-disc pl-5 text-sm",
                        for blocked in &digest.counts {
                            li { "{blocked.kind.label()} — {blocked.count}: {blocked.reason.description()}" }
                        }
                    }
                }
            } else {
                p { role:"status", class:"mt-2 text-sm", "The first daily delivery summary is not ready yet." }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn missing_stale_and_failed_digests_are_distinct_from_a_recorded_zero() {
        let day: NaiveDate = "2026-10-16".parse().unwrap();
        let absent = crate::testing::render(
            move || rsx! {DailyDeliveryDigest {digest:None,requested_day:day}},
        );
        assert!(absent.contains("first daily delivery summary is not ready"));
        assert!(!absent.contains("No blocked"));
        let failed = crate::testing::render(
            move || rsx! {DailyDeliveryDigest {digest:None,requested_day:day,unavailable:true}},
        );
        assert!(failed.contains("could not be loaded"));
        assert!(!failed.contains("No blocked"));
        let digest = BlockedDigest {
            day: "2026-10-15".parse().unwrap(),
            observed_at: "2026-10-15T00:05:00Z".parse().unwrap(),
            counts: vec![],
        };
        let stale = crate::testing::render(
            move || rsx! {DailyDeliveryDigest {digest:Some(digest.clone()),requested_day:day}},
        );
        assert!(stale.contains("latest available summary from 2026-10-15"));
        assert!(stale.contains("No blocked deliveries in this summary"));
        assert!(stale.contains("Counts reflect that daily snapshot"));
    }
}
