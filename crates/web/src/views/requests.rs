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
    pub permission: Option<String>,
    pub repos: Vec<String>,
    pub expires_at: Option<DateTime<Utc>>,
    pub approval_required: Option<bool>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{DateTime, Utc};

    fn dt(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    #[test]
    fn requests_queue_renders_link_context_before_actions() {
        let html = crate::views::render::render(|| {
            rsx! {
                RequestsQueuePage {
                    signed_in_login: Some("admin".into()),
                    flash: None,
                    account_login: "acme",
                    rows: vec![PendingRequestRow {
                        request_id: "01ARZ3NDEKTSV4RRFFQ69G5FAV".into(),
                        link_slug: "QueueSlug0000006".into(),
                        link_id: "01ARZ3NDEKTSV4RRFFQ69G5FAA".into(),
                        requester_login: "octocat".into(),
                        justification: Some("Need access for launch".into()),
                        created_at: dt("2026-05-04T12:30:00Z"),
                        permission: Some("push".into()),
                        repos: vec!["acme/api".into(), "acme/web".into()],
                        expires_at: Some(dt("2026-06-03T12:00:00Z")),
                        approval_required: Some(true),
                    }],
                }
            }
        });

        assert!(html.contains("push"));
        assert!(html.contains("acme/api"));
        assert!(html.contains("acme/web"));
        assert!(html.contains("Approving sends GitHub collaborator invitations"));
        assert!(html.contains("Need access for launch"));
    }

    #[test]
    fn requests_queue_renders_missing_link_context_without_broken_link() {
        let html = crate::views::render::render(|| {
            rsx! {
                RequestsQueuePage {
                    signed_in_login: Some("admin".into()),
                    flash: None,
                    account_login: "acme",
                    rows: vec![PendingRequestRow {
                        request_id: "01ARZ3NDEKTSV4RRFFQ69G5FAV".into(),
                        link_slug: "(deleted link)".into(),
                        link_id: "".into(),
                        requester_login: "octocat".into(),
                        justification: None,
                        created_at: dt("2026-05-04T12:30:00Z"),
                        permission: None,
                        repos: vec![],
                        expires_at: None,
                        approval_required: None,
                    }],
                }
            }
        });

        assert!(html.contains("(deleted link)"));
        assert!(!html.contains("href=\"/accounts/acme/links/\""));
        assert!(html.contains("Repository details unavailable"));
        assert!(!html.contains(
            "Approving sends GitHub collaborator invitations for the repositories listed here."
        ));
        assert!(!html.contains("/accounts/acme/requests/01ARZ3NDEKTSV4RRFFQ69G5FAV/approve"));
        assert!(!html.contains("/accounts/acme/requests/01ARZ3NDEKTSV4RRFFQ69G5FAV/decline"));
    }
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
            active_nav: Some("requests".to_string()),
            flash: props.flash.clone(),
            children: rsx! {
                header { class: "mb-6", h1 { class: "text-2xl font-bold", "Pending requests" } }
                {if props.rows.is_empty() {
                    rsx! {
                        div { class: "rounded-box bg-base-100 p-6 text-base-content/70",
                            h2 { class: "font-semibold text-base-content", "No pending requests" }
                            p { class: "mt-1 text-sm", "Requests that need an admin decision will appear here before GitHub invitations are sent." }
                        }
                    }
                } else {
                    rsx! {
                        div { class: "space-y-4",
                            {props.rows.iter().map(|r| {
                                let rid = r.request_id.clone();
                                let just = r.justification.clone();
                                let link_id = r.link_id.clone();
                                let link_slug = r.link_slug.clone();
                                let link_label = if link_id.is_empty() {
                                    rsx! { span { "{link_slug}" } }
                                } else {
                                    rsx! { a { class: "link", href: "/accounts/{login}/links/{link_id}", "{link_slug}" } }
                                };
                                let requester = r.requester_login.clone();
                                let created = r.created_at;
                                let permission = r.permission.clone().unwrap_or_else(|| "unknown permission".into());
                                let repos_available = !r.repos.is_empty();
                                let actions_available = !link_id.is_empty() && repos_available;
                                let expires = r
                                    .expires_at
                                    .map(|when| when.format("%Y-%m-%d").to_string())
                                    .unwrap_or_else(|| "No expiration".into());
                                let approval = match r.approval_required {
                                    Some(true) => "Admin approval required",
                                    Some(false) => "Auto-approved link",
                                    None => "Approval mode unavailable",
                                };
                                let repos = if r.repos.is_empty() {
                                    rsx! { p { class: "text-sm text-base-content/70", "Repository details unavailable" } }
                                } else {
                                    rsx! {
                                        ul { class: "list-disc list-inside text-sm",
                                            {r.repos.iter().map(|repo| {
                                                let repo = repo.clone();
                                                rsx! { li { "{repo}" } }
                                            })}
                                        }
                                    }
                                };
                                rsx! {
                                    div { class: "card bg-base-100 shadow",
                                        div { class: "card-body gap-4",
                                            div { class: "flex flex-col gap-4 md:flex-row md:justify-between md:items-start",
                                                div { class: "space-y-3",
                                                    div {
                                                        p {
                                                            strong { "@{requester}" }
                                                            " requested access via "
                                                            {link_label}
                                                        }
                                                        p { class: "text-xs text-base-content/60", "Requested at {created}" }
                                                    }
                                                    div { class: "flex flex-wrap gap-2",
                                                        span { class: "badge badge-neutral", "Permission: {permission}" }
                                                        span { class: "badge badge-ghost", "Expires: {expires}" }
                                                        span { class: "badge badge-ghost", "{approval}" }
                                                    }
                                                    div {
                                                        h3 { class: "font-semibold text-sm", "Repositories" }
                                                        {repos}
                                                    }
                                                    {match just {
                                                        Some(j) if !j.is_empty() => rsx! {
                                                            blockquote { class: "text-sm italic text-base-content/80", "\"{j}\"" }
                                                        },
                                                        _ => rsx! {},
                                                    }}
                                                    {if repos_available {
                                                        rsx! {
                                                            p { class: "text-sm text-base-content/70",
                                                                "Approving sends GitHub collaborator invitations for the repositories listed here."
                                                            }
                                                        }
                                                    } else {
                                                        rsx! {
                                                            p { class: "text-sm text-base-content/70",
                                                                "This request cannot be completed until its share link details are available."
                                                            }
                                                        }
                                                    }}
                                                }
                                                {if actions_available {
                                                    rsx! {
                                                        div { class: "flex gap-2 md:flex-col md:items-stretch",
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
                                                } else {
                                                    rsx! {
                                                        p { class: "text-sm font-medium text-base-content/60", "Decision unavailable" }
                                                    }
                                                }}
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
