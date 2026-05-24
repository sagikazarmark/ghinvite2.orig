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
        assert!(html.contains("<title>Pending requests · acme</title>"));
        assert!(!html.contains("{props.account_login}"));
        assert!(html.contains("acme/api"));
        assert!(html.contains("acme/web"));
        assert!(html.contains("Approving sends GitHub collaborator invitations"));
        assert!(html.contains("Need access for launch"));
        assert!(html.contains("Decision queue"));
        assert!(html.contains("Approve request"));
        assert!(html.contains("Decline request"));
        assert!(html.contains("request-decision-list"));
        assert!(html.contains("mac-panel"));
        assert!(html.contains("compact-table"));
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
            title: "Pending requests · {login}",
            account_login: Some(props.account_login.clone()),
            active_nav: Some("requests".to_string()),
            flash: props.flash.clone(),
            children: rsx! {
                header { class: "mb-6",
                    p { class: "text-sm font-medium text-primary", "Requests" }
                    h1 { class: "text-2xl font-semibold tracking-tight", "Decision queue" }
                    p { class: "mt-1 text-sm text-base-content/65", "Review access requests before GitHub invitations are sent." }
                }
                {if props.rows.is_empty() {
                    rsx! {
                        div { class: "mac-panel p-5 text-base-content/70",
                            h2 { class: "font-medium text-base-content", "No pending requests" }
                            p { class: "mt-1 text-sm", "Requests that need an admin decision will appear here." }
                        }
                    }
                } else {
                    rsx! {
                        div { class: "request-decision-list mac-panel compact-table overflow-hidden",
                            {props.rows.iter().map(|r| {
                                let rid = r.request_id.clone();
                                let just = r.justification.clone();
                                let link_id = r.link_id.clone();
                                let link_slug = r.link_slug.clone();
                                let link_label = if link_id.is_empty() {
                                    rsx! { span { "{link_slug}" } }
                                } else {
                                    rsx! { a { class: "link link-hover", href: "/accounts/{login}/links/{link_id}", "{link_slug}" } }
                                };
                                let requester = r.requester_login.clone();
                                let created = r.created_at;
                                let permission = r.permission.clone().unwrap_or_else(|| "unknown permission".into());
                                let repos_available = !r.repos.is_empty();
                                let actions_available = !link_id.is_empty() && repos_available;
                                let expires = r.expires_at.map(|when| when.format("%Y-%m-%d").to_string()).unwrap_or_else(|| "No expiration".into());
                                let approval = match r.approval_required {
                                    Some(true) => "Admin approval required",
                                    Some(false) => "Auto-approved link",
                                    None => "Approval mode unavailable",
                                };
                                let repos = if r.repos.is_empty() {
                                    rsx! { p { class: "text-sm text-base-content/65", "Repository details unavailable" } }
                                } else {
                                    rsx! {
                                        ul { class: "mt-1 flex flex-wrap gap-1.5 text-sm",
                                            {r.repos.iter().map(|repo| {
                                                let repo = repo.clone();
                                                rsx! { li { class: "badge badge-ghost badge-sm", "{repo}" } }
                                            })}
                                        }
                                    }
                                };
                                rsx! {
                                    div { class: "grid gap-3 border-b border-base-300 px-4 py-3 last:border-b-0 lg:grid-cols-[minmax(10rem,14rem)_minmax(0,1fr)_auto] lg:items-start",
                                        div { class: "min-w-0",
                                            p { class: "truncate text-sm font-medium", "@{requester}" }
                                            p { class: "mt-0.5 text-xs text-base-content/55", "Requested at {created}" }
                                        }
                                        div { class: "min-w-0 space-y-2",
                                            p { class: "text-sm",
                                                "Via "
                                                {link_label}
                                            }
                                            div { class: "flex flex-wrap gap-1.5",
                                                span { class: "badge badge-neutral badge-sm", "Permission: {permission}" }
                                                span { class: "badge badge-ghost badge-sm", "Expires: {expires}" }
                                                span { class: "badge badge-ghost badge-sm", "{approval}" }
                                            }
                                            div {
                                                p { class: "text-[0.68rem] font-semibold uppercase tracking-wide text-base-content/45", "Repositories" }
                                                {repos}
                                            }
                                            {match just {
                                                Some(j) if !j.is_empty() => rsx! { blockquote { class: "rounded-box bg-base-200 px-3 py-2 text-sm text-base-content/80", "\"{j}\"" } },
                                                _ => rsx! {},
                                            }}
                                            {if repos_available {
                                                rsx! { p { class: "text-xs text-base-content/65", "Approving sends GitHub collaborator invitations for the repositories listed here." } }
                                            } else {
                                                rsx! { p { class: "text-xs text-base-content/65", "This request cannot be completed until its share link details are available." } }
                                            }}
                                        }
                                        {if actions_available {
                                            rsx! {
                                                div { class: "flex gap-2 lg:flex-col lg:items-stretch",
                                                    form { method: "post", action: "/accounts/{login}/requests/{rid}/approve",
                                                        button { r#type: "submit", class: "btn btn-success btn-sm h-8 min-h-0", "Approve request" }
                                                    }
                                                    form { method: "post", action: "/accounts/{login}/requests/{rid}/decline",
                                                        button { r#type: "submit", class: "btn btn-error btn-sm h-8 min-h-0", "Decline request" }
                                                    }
                                                }
                                            }
                                        } else {
                                            rsx! { p { class: "text-sm font-medium text-base-content/60", "Decision unavailable" } }
                                        }}
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
