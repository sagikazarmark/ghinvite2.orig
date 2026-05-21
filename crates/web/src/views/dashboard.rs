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
            tr {
                td {
                    a {
                        class: "link",
                        href: "/accounts/{login}/links/{id_str}",
                        "{slug_str}"
                    }
                }
                td { span { class: "{badge}", "{label}" } }
                td { "{link.uses_count}" }
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
                header { class: "mb-6 flex flex-col gap-3 sm:flex-row sm:items-end sm:justify-between",
                    div {
                        p { class: "text-sm font-medium text-primary", "{props.account_type}" }
                        h1 { class: "text-2xl font-semibold tracking-tight", "Console overview" }
                        p { class: "mt-1 text-sm text-base-content/65", "Manage collaborator access for {props.account_login}." }
                    }
                    a { class: "btn btn-primary btn-sm", href: "/accounts/{login}/links/new", "New share link" }
                }
                div { class: "grid grid-cols-1 gap-4 md:grid-cols-2",
                    a { class: "card border border-base-300 bg-base-100 shadow-sm transition-colors hover:border-primary/40",
                        href: "/accounts/{login}/requests",
                        div { class: "card-body gap-2",
                            p { class: "text-xs font-medium uppercase tracking-wide text-base-content/50", "Review queue" }
                            div { class: "flex items-baseline gap-3",
                                p { class: "text-3xl font-semibold", "{props.pending_requests}" }
                                p { class: "text-sm text-base-content/65", "pending requests" }
                            }
                            p { class: "text-sm text-base-content/70", "Review people waiting for repository access before invitations are sent." }
                        }
                    }
                    a { class: "card border border-base-300 bg-base-100 shadow-sm transition-colors hover:border-primary/40",
                        href: "/accounts/{login}/links/new",
                        div { class: "card-body gap-2",
                            p { class: "text-xs font-medium uppercase tracking-wide text-base-content/50", "Active access links" }
                            div { class: "flex items-baseline gap-3",
                                p { class: "text-3xl font-semibold", "{props.active_links}" }
                                p { class: "text-sm text-base-content/65", "active links" }
                            }
                            p { class: "text-sm text-base-content/70", "Create controlled URLs for GitHub collaborator requests." }
                        }
                    }
                }
                section { class: "mt-6 rounded-box border border-base-300 bg-base-100 shadow-sm",
                    div { class: "flex items-center justify-between border-b border-base-300 px-5 py-4",
                        h2 { class: "text-base font-semibold", "Recent share links" }
                        a { class: "btn btn-ghost btn-sm", href: "/accounts/{login}/links/new", "Create link" }
                    }
                    {if props.recent_links.is_empty() {
                        rsx! {
                            div { class: "p-6 text-sm text-base-content/70",
                                h3 { class: "font-medium text-base-content", "No share links yet" }
                                p { class: "mt-1", "Create a link to let recipients request collaborator access without manual GitHub invites." }
                                a { class: "btn btn-primary btn-sm mt-4", href: "/accounts/{login}/links/new", "Create first share link" }
                            }
                        }
                    } else {
                        rsx! {
                            div { class: "overflow-x-auto",
                                table { class: "table table-sm",
                                    thead { tr { th { "Slug" } th { "State" } th { "Uses" } } }
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
