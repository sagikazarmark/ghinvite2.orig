//! Dashboard overview page (Dioxus).

use crate::session::Flash;
use crate::views::layouts::DashboardLayout;
use chrono::{DateTime, Utc};
use dioxus::prelude::*;
use domain::ShareLink;

#[derive(Clone, PartialEq, Props)]
pub struct OverviewProps {
    pub signed_in_login: Option<String>,
    pub flash: Option<Flash>,
    pub account_login: String,
    pub account_type: String, // "User" / "Organization"
    pub pending_requests: u64,
    pub active_links: u64,
    pub recent_links: Vec<ShareLink>,
    pub now: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overview_page_renders_console_copy() {
        let html = crate::views::render::render(|| {
            rsx! {
                OverviewPage {
                    signed_in_login: Some("admin".to_string()),
                    flash: None,
                    account_login: "acme".to_string(),
                    account_type: "Organization".to_string(),
                    pending_requests: 2,
                    active_links: 3,
                    recent_links: vec![],
                    now: Utc::now(),
                }
            }
        });

        assert!(html.contains("Console overview"));
        assert!(html.contains("Review queue"));
        assert!(html.contains("Create first share link"));
        assert!(html.contains("Recent share links"));
        assert!(html.contains("mac-panel"));
        assert!(html.contains("compact-table"));
        assert!(!html.contains("text-3xl"));
    }
}

#[component]
pub fn OverviewPage(props: OverviewProps) -> Element {
    let login = props.account_login.clone();
    let recent_links_view = props.recent_links.iter().map(|link| {
        let active = link.is_active(props.now);
        let badge = if active {
            "badge badge-success"
        } else {
            "badge badge-ghost"
        };
        let label = if active { "active" } else { "inactive" };
        let id_str = link.id.to_string();
        let slug_str = link.slug.as_str().to_string();
        rsx! {
            tr { class: "hover:bg-base-200/70",
                td { class: "font-mono text-xs",
                    a {
                        class: "link link-hover",
                        href: "/accounts/{login}/links/{id_str}",
                        "{slug_str}"
                    }
                }
                td { span { class: "{badge} badge-sm", "{label}" } }
                td { class: "text-right tabular-nums", "{link.uses_count}" }
            }
        }
    });

    rsx! {
        DashboardLayout {
            signed_in_login: props.signed_in_login.clone(),
            title: "{props.account_login} · ghinvite".to_string(),
            account_login: Some(props.account_login.clone()),
            active_nav: Some("overview".to_string()),
            flash: props.flash.clone(),
            children: rsx! {
                header { class: "mb-4 flex flex-col gap-2 sm:flex-row sm:items-center sm:justify-between",
                    div {
                        h1 { class: "text-xl font-semibold tracking-tight", "Console overview" }
                        p { class: "mt-0.5 text-sm text-base-content/60", "{props.account_type} account: {props.account_login}" }
                    }
                    a { class: "btn btn-primary btn-sm", href: "/accounts/{login}/links/new", "New share link" }
                }
                section { class: "mac-panel mb-4 overflow-hidden",
                    div { class: "grid divide-y divide-base-300 md:grid-cols-2 md:divide-x md:divide-y-0",
                        a { class: "block p-4 transition-colors hover:bg-base-200/60", href: "/accounts/{login}/requests",
                            p { class: "text-sm font-medium", "Review queue" }
                            p { class: "mt-1 text-sm text-base-content/65", "{props.pending_requests} pending requests need an admin decision." }
                        }
                        a { class: "block p-4 transition-colors hover:bg-base-200/60", href: "/accounts/{login}/links/new",
                            p { class: "text-sm font-medium", "Active access links" }
                            p { class: "mt-1 text-sm text-base-content/65", "{props.active_links} active links can accept collaborator requests." }
                        }
                    }
                }
                section { class: "mac-panel compact-table overflow-hidden",
                    div { class: "flex items-center justify-between border-b border-base-300 px-4 py-3",
                        h2 { class: "text-sm font-semibold", "Recent share links" }
                        a { class: "btn btn-ghost btn-xs h-7 min-h-0", href: "/accounts/{login}/links/new", "Create link" }
                    }
                    {if props.recent_links.is_empty() {
                        rsx! {
                            div { class: "p-5 text-sm text-base-content/70",
                                h3 { class: "font-medium text-base-content", "No share links yet" }
                                p { class: "mt-1", "Create a link to let recipients request collaborator access without manual GitHub invites." }
                                a { class: "btn btn-primary btn-sm mt-4", href: "/accounts/{login}/links/new", "Create first share link" }
                            }
                        }
                    } else {
                        rsx! {
                            div { class: "overflow-x-auto",
                                table { class: "table compact-table table-sm",
                                    thead { tr { th { "Slug" } th { "State" } th { class: "text-right", "Uses" } } }
                                    tbody { {recent_links_view} }
                                }
                            }
                        }
                    }}
                }
            },
        }
    }
}
