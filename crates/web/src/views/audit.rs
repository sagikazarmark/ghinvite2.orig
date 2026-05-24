//! Audit Log coming-soon page.

use crate::session::Flash;
use crate::views::layouts::ConsoleLayout;
use dioxus::prelude::*;

#[derive(Clone, PartialEq, Props)]
pub struct AuditLogPageProps {
    pub signed_in_login: Option<String>,
    pub flash: Option<Flash>,
    pub account_login: String,
}

#[component]
pub fn AuditLogPage(props: AuditLogPageProps) -> Element {
    let login = props.account_login.clone();

    rsx! {
        ConsoleLayout {
            signed_in_login: props.signed_in_login.clone(),
            title: "Audit log · {login}",
            account_login: Some(props.account_login.clone()),
            active_nav: Some("audit".to_string()),
            flash: props.flash.clone(),
            children: rsx! {
                header { class: "mb-6",
                    p { class: "text-sm font-medium text-primary", "Audit" }
                    h1 { class: "text-2xl font-semibold tracking-tight", "Audit log" }
                    p { class: "mt-1 text-sm text-base-content/65", "Review account activity for share links, requests, and GitHub invitations." }
                }
                section { class: "mac-panel max-w-3xl p-6",
                    div { class: "max-w-2xl",
                        p { class: "text-xs font-semibold uppercase tracking-[0.18em] text-base-content/45", "Coming soon" }
                        h2 { class: "mt-3 text-lg font-semibold tracking-tight", "Audit log coming soon" }
                        p { class: "mt-2 text-sm leading-6 text-base-content/70",
                            "ghinvite records account activity for share links, requests, and GitHub invitations. Console browsing is not available yet."
                        }
                        a { class: "btn btn-primary btn-sm mt-5", href: "/accounts/{login}", "Back to overview" }
                    }
                }
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audit_log_page_renders_coming_soon_empty_state() {
        let html = crate::views::render::render(|| {
            rsx! {
                AuditLogPage {
                    signed_in_login: Some("admin".to_string()),
                    flash: None,
                    account_login: "acme".to_string(),
                }
            }
        });

        assert!(html.contains("Audit log"));
        assert!(html.contains("<title>Audit log · acme</title>"));
        assert!(html.contains(
            "Review account activity for share links, requests, and GitHub invitations."
        ));
        assert!(html.contains("Audit log coming soon"));
        assert!(html.contains("ghinvite records account activity for share links, requests, and GitHub invitations. Console browsing is not available yet."));
        assert!(html.contains("Back to overview"));
        assert!(html.contains("href=\"/accounts/acme\""));
        assert!(html.contains("mac-panel"));
        assert!(!html.contains("<table"));
        assert!(!html.contains("Filter"));
        assert!(!html.contains("No events"));
    }
}
