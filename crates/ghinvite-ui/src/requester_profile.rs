//! Advisory requester profile facts, never authorization or risk scores.
use chrono::{DateTime, Utc};
use dioxus::prelude::*;

pub fn account_age(created_at: Option<DateTime<Utc>>, now: DateTime<Utc>) -> Option<String> {
    let created = created_at.filter(|at| *at <= now)?;
    let days = now.signed_duration_since(created).num_days();
    let age = if days == 0 {
        "less than one day".to_owned()
    } else if days == 1 {
        "1 day".to_owned()
    } else {
        format!("{days} days")
    };
    Some(format!(
        "GitHub account age: {age} (created {})",
        created.format("%Y-%m-%d")
    ))
}

#[component]
pub fn RequesterProfile(
    label: String,
    #[props(default)] avatar_href: Option<String>,
    #[props(default)] github_created_at: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> Element {
    let initials = label
        .trim_start_matches('@')
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(2)
        .collect::<String>()
        .to_uppercase();
    let initials = if initials.is_empty() {
        "?".to_owned()
    } else {
        initials
    };
    rsx! {
        div { class: "flex items-center gap-2",
            span { class: "relative inline-flex h-10 w-10 shrink-0 items-center justify-center overflow-hidden rounded-full bg-base-200 text-sm", aria_hidden: "true",
                "{initials}"
                if let Some(href) = avatar_href {
                    img { class: "absolute inset-0 h-10 w-10 object-cover", src: href, alt: "", width: "40", height: "40", loading: "lazy", decoding: "async", referrerpolicy: "no-referrer" }
                }
            }
            div {
                p { class: "break-words text-sm font-medium", "{label}" }
                if let Some(age) = account_age(github_created_at, now) {
                    p { class: "text-xs text-muted", "{age}" }
                } else {
                    p { class: "text-xs text-muted", "GitHub account age unavailable" }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn profile_age_is_only_an_available_nonfuture_github_fact() {
        let now: DateTime<Utc> = "2026-10-17T12:00:00Z".parse().unwrap();
        assert!(account_age(None, now).is_none());
        assert!(account_age(Some(now + chrono::Duration::seconds(1)), now).is_none());
        assert!(
            account_age(Some(now - chrono::Duration::days(400)), now)
                .unwrap()
                .contains("400 days")
        );
        let html = crate::testing::render(move || {
            rsx! {
                RequesterProfile { label: "@<script>", avatar_href: Some("/console/accounts/acme/requests/01/avatars".into()), github_created_at: Some(now - chrono::Duration::days(400)), now }
            }
        });
        assert!(html.contains("400 days"));
        assert!(html.contains("referrerpolicy=\"no-referrer\""));
        assert!(html.contains("loading=\"lazy\""));
        assert!(!html.contains("<script>"));
        assert!(!html.contains("https://"));
        let missing =
            crate::testing::render(move || rsx! { RequesterProfile { label: "@octocat", now } });
        assert!(missing.contains("GitHub account age unavailable"));
        assert!(missing.contains("OC"));
        assert!(!missing.contains("<img"));
    }
}
