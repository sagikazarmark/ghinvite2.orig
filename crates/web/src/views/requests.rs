//! Approval queue page (Dioxus).

use crate::session::Flash;
use crate::views::layouts::DashboardLayout;
use chrono::{DateTime, Utc};
use dioxus::prelude::*;

#[derive(Clone, PartialEq)]
pub struct PendingRequestRow {
    pub request_id: String,
    pub link_slug: String,
    pub link_id: String,
    pub requester_login: String,
    pub justification: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone, PartialEq, Props)]
pub struct RequestsQueueProps {
    pub signed_in_login: Option<String>,
    pub flash: Option<Flash>,
    pub account_login: String,
    pub rows: Vec<PendingRequestRow>,
}

#[component]
pub fn RequestsQueuePage(props: RequestsQueueProps) -> Element {
    let login = props.account_login.clone();
    rsx! {
        DashboardLayout {
            signed_in_login: props.signed_in_login.clone(),
            title: "Pending requests · {props.account_login}".to_string(),
            account_login: Some(props.account_login.clone()),
            flash: props.flash.clone(),
            children: rsx! {
                header { class: "mb-6", h1 { class: "text-2xl font-bold", "Pending requests" } }
                {if props.rows.is_empty() {
                    rsx! { p { class: "opacity-70", "No pending requests." } }
                } else {
                    rsx! {
                        div { class: "space-y-4",
                            {props.rows.iter().map(|r| {
                                let rid = r.request_id.clone();
                                let just = r.justification.clone();
                                let link_id = r.link_id.clone();
                                let link_slug = r.link_slug.clone();
                                let requester = r.requester_login.clone();
                                let created = r.created_at;
                                rsx! {
                                    div { class: "card bg-base-100 shadow",
                                        div { class: "card-body",
                                            div { class: "flex justify-between items-center",
                                                div {
                                                    p {
                                                        strong { "@{requester}" }
                                                        " requesting access via "
                                                        a { class: "link", href: "/accounts/{login}/links/{link_id}", "{link_slug}" }
                                                    }
                                                    {match just {
                                                        Some(j) if !j.is_empty() => rsx! {
                                                            p { class: "italic opacity-80 mt-1", "\"{j}\"" }
                                                        },
                                                        _ => rsx! {},
                                                    }}
                                                    p { class: "text-xs opacity-60", "{created}" }
                                                }
                                                div { class: "flex gap-2",
                                                    form {
                                                        method: "post",
                                                        action: "/accounts/{login}/requests/{rid}/approve",
                                                        button { r#type: "submit", class: "btn btn-success btn-sm", "Approve" }
                                                    }
                                                    form {
                                                        method: "post",
                                                        action: "/accounts/{login}/requests/{rid}/decline",
                                                        button { r#type: "submit", class: "btn btn-error btn-sm", "Decline" }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            })}
                        }
                    }
                }}
            },
        }
    }
}
