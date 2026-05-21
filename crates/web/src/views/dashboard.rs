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
            flash: props.flash.clone(),
            children: rsx! {
                header {
                    class: "mb-6",
                    h1 { class: "text-3xl font-bold", "{props.account_login}" }
                    p { class: "text-sm opacity-70", "{props.account_type}" }
                }
                div {
                    class: "grid grid-cols-1 md:grid-cols-2 gap-4 mb-8",
                    a {
                        class: "card bg-base-100 shadow",
                        href: "/accounts/{login}/requests",
                        div {
                            class: "card-body",
                            h2 { class: "card-title", "Pending requests" }
                            p { class: "text-4xl", "{props.pending_requests}" }
                            p { class: "text-sm text-base-content/70", "Review people waiting for repository access." }
                        }
                    }
                    a {
                        class: "card bg-base-100 shadow",
                        href: "/accounts/{login}/links/new",
                        div {
                            class: "card-body",
                            h2 { class: "card-title", "Active links" }
                            p { class: "text-4xl", "{props.active_links}" }
                            p { class: "text-sm text-base-content/70", "Create another controlled access URL." }
                        }
                    }
                }
                section {
                    h2 { class: "text-xl font-semibold mb-3", "Recent links" }
                    {if props.recent_links.is_empty() {
                        rsx! {
                            div { class: "rounded-box bg-base-100 p-6 text-base-content/70",
                                h3 { class: "font-semibold text-base-content", "No share links yet" }
                                p { class: "mt-1 text-sm", "Create a link to let recipients request GitHub collaborator access without sending invites by hand." }
                                a { class: "btn btn-primary btn-sm mt-4", href: "/accounts/{login}/links/new", "Create first link" }
                            }
                        }
                    } else {
                        rsx! {
                            div { class: "overflow-x-auto",
                                table { class: "table",
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
