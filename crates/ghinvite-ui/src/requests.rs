//! Approval queue page (Dioxus).

use crate::flash::Flash;
use crate::layouts::ConsoleLayout;
use chrono::{DateTime, Utc};
use dioxus::prelude::*;

#[derive(Clone, PartialEq)]
pub struct PendingRequestRow {
    pub request_id: String,
    pub link_description: Option<String>,
    pub link_id: String,
    pub requester_login: String,
    pub justification: Option<String>,
    pub created_at: DateTime<Utc>,
    pub decision_deadline: Option<DateTime<Utc>>,
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
        let html = crate::testing::render(|| {
            rsx! {
                RequestsQueuePage {
                    signed_in_login: Some("admin".into()),
                    flash: None,
                    account_login: "acme",
                    now: dt("2026-05-04T13:00:00Z"),
                    rows: vec![PendingRequestRow {
                        request_id: "01ARZ3NDEKTSV4RRFFQ69G5FAV".into(),
                        link_description: Some("Workshop".into()),
                        link_id: "01ARZ3NDEKTSV4RRFFQ69G5FAA".into(),
                        requester_login: "octocat".into(),
                        justification: Some("Need access for launch".into()),
                        created_at: dt("2026-05-04T12:30:00Z"),
                        decision_deadline: None,
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
        assert!(html.contains("Approval authorizes delivery"));
        assert!(html.contains("unavailable repositories may block delivery"));
        assert!(!html.contains("GitHub collaborator invitations"));
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
        let html = crate::testing::render(|| {
            rsx! {
                RequestsQueuePage {
                    signed_in_login: Some("admin".into()),
                    flash: None,
                    account_login: "acme",
                    now: dt("2026-05-04T13:00:00Z"),
                    rows: vec![PendingRequestRow {
                        request_id: "01ARZ3NDEKTSV4RRFFQ69G5FAV".into(),
                        link_description: None,
                        link_id: "".into(),
                        requester_login: "octocat".into(),
                        justification: None,
                        created_at: dt("2026-05-04T12:30:00Z"),
                        decision_deadline: None,
                        permission: None,
                        repos: vec![],
                        expires_at: None,
                        approval_required: None,
                    }],
                }
            }
        });

        assert!(html.contains("(deleted link)"));
        assert!(!html.contains("href=\"/console/accounts/acme/links/\""));
        assert!(html.contains("Repository details unavailable"));
        assert!(
            !html.contains("Approving sends GitHub invitations for the repositories listed here.")
        );
        assert!(
            !html.contains("/console/accounts/acme/requests/01ARZ3NDEKTSV4RRFFQ69G5FAV/approve")
        );
        assert!(
            !html.contains("/console/accounts/acme/requests/01ARZ3NDEKTSV4RRFFQ69G5FAV/decline")
        );
    }
}

#[derive(Clone, PartialEq, Props)]
pub struct RequestsQueueProps {
    pub now: DateTime<Utc>,
    pub signed_in_login: Option<String>,
    pub flash: Option<Flash>,
    pub account_login: String,
    pub rows: Vec<PendingRequestRow>,
    #[props(default)]
    pub next_href: Option<String>,
    #[props(default)]
    pub is_continuation: bool,
}

#[component]
pub fn RequestsQueuePage(props: RequestsQueueProps) -> Element {
    let login = props.account_login.clone();
    let page_size = ghinvite_core::storage::pending_queue::PENDING_PAGE_SIZE;
    rsx! {
        ConsoleLayout {
            signed_in_login: props.signed_in_login.clone(),
            title: "Pending requests · {login}",
            account_login: Some(props.account_login.clone()),
            active_nav: Some("requests".to_string()),
            flash: props.flash.clone(),
            children: rsx! {
                header { class: "mb-6",
                    p { class: "text-sm font-medium text-primary", "Requests" }
                    h1 { class: "text-2xl font-semibold tracking-tight", "Decision queue" }
                    p { class: "mt-1 text-sm text-base-content/65", "Review invitation requests before GitHub invitations are sent." }
                }
                {if props.rows.is_empty() {
                    rsx! {
                        div { class: "mac-panel p-5 text-base-content/70",
                            h2 { class: "font-medium text-base-content",
                                if props.is_continuation { "No more pending requests on this page" } else { "No pending requests" }
                            }
                            p { class: "mt-1 text-sm", "Invitation requests that need an account admin decision will appear here. The queue may change as requests are decided." }
                        }
                    }
                } else {
                    rsx! {
                        div { class: "request-decision-list mac-panel compact-table overflow-hidden",
                            {props.rows.iter().map(|r| {
                                let rid = r.request_id.clone();
                                let approve_operation = ghinvite_core::RequestId::new().to_string();
                                let decline_operation = ghinvite_core::RequestId::new().to_string();
                                let just = r.justification.clone();
                                let link_id = r.link_id.clone();
                                let invitation_code = if r.link_id.is_empty() { "(deleted link)".into() } else { r.link_id.clone() };
                                let description = r.link_description.clone().unwrap_or_else(|| invitation_code.clone());
                                let link_label = if link_id.is_empty() {
                                    rsx! { span { "{invitation_code}" } }
                                } else {
                                    rsx! {
                                        a { class: "link link-hover", href: "/console/accounts/{login}/links/{link_id}", "{description}" }
                                        span { class: "ml-2 text-xs text-muted", "Code: {invitation_code}" }
                                    }
                                };
                                let requester = r.requester_login.clone();
                                let created = r.created_at;
                                let permission = r.permission.clone().unwrap_or_else(|| "unknown permission".into());
                                let repos_available = !r.repos.is_empty();
                                let actions_available = !link_id.is_empty() && repos_available;
                                let deadline = r.decision_deadline.map(|when| when.format("%Y-%m-%d %H:%M:%S UTC").to_string()).unwrap_or_else(|| "Unavailable".into());
                                let expires = r.expires_at.map(|when| when.format("%Y-%m-%d %H:%M:%S UTC").to_string()).unwrap_or_else(|| if link_id.is_empty() { "Unavailable".into() } else { "No expiration".into() });
                                let approval = match r.approval_required {
                                    Some(true) => "Account admin approval required",
                                    Some(false) => "Auto-approved invitation link",
                                    None => "Approval policy unavailable",
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
                                            a { class: "link text-sm", href: "/console/accounts/{login}/requests/{rid}", "Request details" }
                                            p { class: "mt-0.5 text-xs text-muted", "Requested at {created}" }
                                        }
                                        div { class: "min-w-0 space-y-2",
                                            p { class: "text-sm",
                                                "Via "
                                                {link_label}
                                            }
                                            div { class: "flex flex-wrap gap-1.5",
                                                span { class: "badge badge-neutral badge-sm", "Permission: {permission}" }
                                                span { class: "badge badge-ghost badge-sm", "Decision deadline: {deadline}" }
                                                span { class: "badge badge-ghost badge-sm", "Invitation link expiration: {expires}" }
                                                span { class: "badge badge-ghost badge-sm", "{approval}" }
                                            }
                                            p { class: "text-xs text-base-content/65", "The decision deadline shown is the recorded deadline for this request." }
                                            if r.decision_deadline.is_some_and(|deadline| deadline <= props.now) {
                                                p { class: "rounded-box bg-warning px-2 py-1 text-xs font-medium text-warning-content", "Decision deadline passed. Queue updates may be delayed." }
                                            }
                                            div {
                                                p { class: "text-[0.68rem] font-semibold uppercase tracking-wide text-muted", "Repositories" }
                                                {repos}
                                            }
                                            {match just {
                                                Some(j) if !j.is_empty() => rsx! { blockquote { class: "rounded-box bg-base-200 px-3 py-2 text-sm text-base-content/80", "\"{j}\"" } },
                                                _ => rsx! {},
                                            }}
                                            {if repos_available {
                                                rsx! { p { class: "text-xs text-base-content/65", "Approval authorizes delivery for the repositories listed here. Available repositories can proceed; unavailable repositories may block delivery. The original scope and decision deadline stay unchanged." } }
                                            } else {
                                                rsx! { p { class: "text-xs text-base-content/65", "This invitation request cannot be completed until its invitation link details are available." } }
                                            }}
                                        }
                                        {if actions_available {
                                            rsx! {
                                                div { class: "flex gap-2 lg:flex-col lg:items-stretch",
                                                    form { method: "post", action: "/console/accounts/{login}/requests/{rid}/approve",
                                                        crate::csrf::CsrfField {}
                                                        input { r#type: "hidden", name: "link_id", value: "{link_id}" }
                                                        input { r#type: "hidden", name: "operation_id", value: "{approve_operation}" }
                                                        button { r#type: "submit", class: "btn btn-success btn-sm h-8 min-h-0", "Approve request" }
                                                    }
                                                    form { method: "post", action: "/console/accounts/{login}/requests/{rid}/decline",
                                                        crate::csrf::CsrfField {}
                                                        input { r#type: "hidden", name: "link_id", value: "{link_id}" }
                                                        input { r#type: "hidden", name: "operation_id", value: "{decline_operation}" }
                                                        button { r#type: "submit", class: "btn btn-error btn-sm h-8 min-h-0", "Decline request" }
                                                    }
                                                }
                                            }
                                        } else {
                                            rsx! { p { class: "text-sm font-medium text-muted", "Decision unavailable" } }
                                        }}
                                    }
                                }
                            })}
                        }
                    }
                }}
                nav { class: "mt-4 flex items-center gap-4", aria_label: "Request queue pages",
                    if props.is_continuation {
                        a { href: "/console/accounts/{login}/requests", class: "link", "Back to oldest requests" }
                    }
                    if let Some(href) = props.next_href {
                        a { rel: "next", href, class: "btn btn-sm", "Next requests" }
                    }
                    p { class: "text-sm text-base-content/65", "Oldest first · Up to {page_size} requests per page" }
                }
            },
        }
    }
}
