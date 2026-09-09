//! Invitation request flow views.

use crate::layouts::InvitationLayout;
use dioxus::prelude::*;
use ghinvite_core::{InvitationLink, Permission, RequestState};

// ── helpers ──────────────────────────────────────────────────────────────────

fn permission_label(p: Permission) -> &'static str {
    match p {
        Permission::Pull => "Read (pull)",
        Permission::Triage => "Triage",
        Permission::Push => "Write (push)",
        Permission::Maintain => "Maintain",
        Permission::Admin => "Admin",
    }
}

// ── RequestPage ──────────────────────────────────────────────────────────────

/// How often the pending invitation request page reloads itself while waiting
/// for an account-admin decision. Manual "Check again" remains as fallback.
const PENDING_REFRESH_SECONDS: u32 = 20;

#[derive(Clone, PartialEq, Props)]
pub struct RequestPageProps {
    pub slug: String,
    pub link: InvitationLink,
    pub signed_in_login: String,
    pub flash: Option<crate::flash::Flash>,
    pub request_id: String,
    pub current_status: Option<RequestState>,
    pub retry_notice: Option<RequestState>,
}

fn retry_notice_copy(state: RequestState) -> Option<&'static str> {
    match state {
        RequestState::Declined => {
            Some("Your previous request was declined. You can submit a new request.")
        }
        RequestState::Expired => {
            Some("Your previous request expired. You can submit a new request.")
        }
        RequestState::Cancelled => {
            Some("Your previous request was cancelled. You can submit a new request.")
        }
        RequestState::Pending | RequestState::Approved => None,
    }
}

#[component]
pub fn RequestPage(props: RequestPageProps) -> Element {
    let slug = props.slug.clone();
    let login = props.signed_in_login.clone();
    let perm_label = permission_label(props.link.permission);
    let action = format!("/i/{slug}");
    let check_href = format!("/i/{slug}");
    let request_id = props.request_id.clone();
    let effective_retry_notice = props
        .retry_notice
        .and_then(retry_notice_copy)
        .or_else(|| props.current_status.and_then(retry_notice_copy));

    let flash_view = match &props.flash {
        None => rsx! {},
        Some(f) => {
            let alert_class = match f.level {
                crate::flash::FlashLevel::Error => "alert alert-error mb-5",
                _ => "alert alert-info mb-5",
            };
            let msg = f.message.clone();
            rsx! {
                div { class: "{alert_class}", span { "{msg}" } }
            }
        }
    };

    let retry_notice_view = match effective_retry_notice {
        None => rsx! {},
        Some(copy) => rsx! {
            div { class: "alert alert-warning mb-5",
                span { "{copy}" }
            }
        },
    };

    let repos_view = props.link.repos.iter().map(|r| {
        let name = r.repo_full_name.clone();
        rsx! {
            li { class: "flex items-center justify-between gap-3 rounded-box bg-base-200 px-3 py-2",
                span { class: "font-mono text-sm", "{name}" }
            }
        }
    });

    let content = match props.current_status {
        Some(RequestState::Pending) => rsx! {
            div { class: "space-y-5",
                div { class: "badge badge-warning badge-lg", "Awaiting review" }
                h1 { class: "text-2xl font-semibold tracking-tight", "Awaiting review" }
                p { class: "text-sm leading-6 text-base-content/70",
                    "The account admins have your request. You can check this page again for updates."
                }
                a { class: "btn btn-primary", href: "{check_href}", "Check again" }
            }
        },
        Some(RequestState::Approved) => rsx! {
            div { class: "space-y-5",
                div { class: "badge badge-success badge-lg", "Approved" }
                h1 { class: "text-2xl font-semibold tracking-tight", "Approved" }
                p { class: "text-sm leading-6 text-base-content/70",
                    "Watch your GitHub notifications and email for the repository invitation from GitHub."
                }
            }
        },
        Some(RequestState::Declined)
        | Some(RequestState::Expired)
        | Some(RequestState::Cancelled)
        | None => rsx! {
            div { class: "space-y-5",
                header { class: "space-y-2",
                    p { class: "text-xs font-semibold uppercase tracking-[0.2em] text-primary", "Repository request" }
                    h1 { class: "text-2xl font-semibold tracking-tight", "Request repository access" }
                    p { class: "text-sm leading-6 text-base-content/70",
                        "Review the repositories and submit a request as @{login}."
                    }
                }
                {flash_view}
                {retry_notice_view}
                div { class: "alert alert-info",
                    span {
                        "Signed in as "
                        strong { "@{login}" }
                        ". Not you? "
                        a { href: "/logout", class: "link", "Sign out and sign in again." }
                    }
                }
                div { class: "space-y-3",
                    div { class: "flex flex-wrap items-center gap-2",
                        span { class: "badge badge-neutral", "Permission: {perm_label}" }
                    }
                    ul { class: "space-y-2", {repos_view} }
                }
                form {
                    method: "post",
                    action: "{action}",
                    class: "space-y-4",
                    input {
                        r#type: "hidden",
                        name: "request_id",
                        value: "{request_id}",
                    }
                    div { class: "form-control gap-2",
                        label { class: "label", r#for: "justification", span { class: "label-text font-medium", "Justification" } }
                        textarea {
                            id: "justification",
                            name: "justification",
                            class: "textarea textarea-bordered min-h-28 w-full",
                            placeholder: "Share useful context for the account admins.",
                            rows: "4",
                        }
                        p { class: "text-sm text-base-content/70", "Optional, visible to account admins." }
                    }
                    div { class: "card-actions justify-end",
                        button {
                            r#type: "submit",
                            class: "btn btn-primary",
                            "Submit request"
                        }
                    }
                }
            }
        },
    };

    let refresh_seconds = match props.current_status {
        Some(RequestState::Pending) => Some(PENDING_REFRESH_SECONDS),
        _ => None,
    };

    rsx! {
        InvitationLayout {
            signed_in_login: Some(props.signed_in_login.clone()),
            title: "Request repository access · ghinvite".to_string(),
            account_login: None,
            active_nav: None,
            flash: None,
            refresh_seconds,
            children: rsx! { {content} },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{DateTime, Utc};
    use ghinvite_core::RequestState;
    use ghinvite_core::{InvitationLink, InvitationLinkId, InvitationLinkRepo, Permission, Slug};

    fn dt(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn sample_link() -> InvitationLink {
        InvitationLink {
            id: InvitationLinkId::new(),
            slug: Slug::from_string("abcdEFGH01234567".to_string()).unwrap(),
            installation_id: 1,
            account_id: 9001,
            created_by: 701,
            created_at: dt("2026-05-04T12:00:00Z"),
            expires_at: Some(dt("2026-06-03T12:00:00Z")),
            max_uses: Some(5),
            uses_count: 2,
            permission: Permission::Push,
            approval_required: true,
            description: "AI coding workshop".into(),
            internal_note: Some("Only admins should see this note".into()),
            revoked_at: None,
            revoked_by: None,
            repos: vec![
                InvitationLinkRepo {
                    repo_id: 10,
                    repo_full_name: "acme/api".into(),
                },
                InvitationLinkRepo {
                    repo_id: 11,
                    repo_full_name: "acme/web".into(),
                },
            ],
        }
    }

    #[test]
    fn request_page_renders_merged_form() {
        let link = sample_link();
        let html = crate::testing::render(move || {
            rsx! {
                RequestPage {
                    slug: "abcdEFGH01234567".to_string(),
                    link: link.clone(),
                    signed_in_login: "octocat".to_string(),
                    flash: None,
                    request_id: "01ARZ3NDEKTSV4RRFFQ69G5FAV".to_string(),
                    current_status: None,
                    retry_notice: None,
                }
            }
        });

        assert!(html.contains("Request repository access"));
        assert!(html.contains("Review the repositories and submit a request as @octocat."));
        assert!(html.contains("acme/api"));
        assert!(html.contains("acme/web"));
        assert!(html.contains("Permission: Write (push)"));
        assert!(html.contains("Signed in as"));
        assert!(html.contains("Not you?"));
        assert!(html.contains("href=\"/logout\""));
        assert!(html.contains("name=\"request_id\""));
        assert!(html.contains("action=\"/i/abcdEFGH01234567\""));
        assert!(html.contains("Submit request"));
        assert!(!html.contains("AI coding workshop"));
        assert!(!html.contains("Only admins should see this note"));
        assert!(!html.contains("collaborator access"));
    }

    #[test]
    fn request_page_renders_pending_status_without_form() {
        let link = sample_link();
        let html = crate::testing::render(move || {
            rsx! {
                RequestPage {
                    slug: "abcdEFGH01234567".to_string(),
                    link: link.clone(),
                    signed_in_login: "octocat".to_string(),
                    flash: None,
                    request_id: "01ARZ3NDEKTSV4RRFFQ69G5FAV".to_string(),
                    current_status: Some(RequestState::Pending),
                    retry_notice: None,
                }
            }
        });

        assert!(html.contains("Awaiting review"));
        assert!(html.contains("account admins have your request"));
        assert!(html.contains("href=\"/i/abcdEFGH01234567\""));
        assert!(!html.contains("textarea"));
        assert!(!html.contains("Submit request"));
        assert!(!html.contains("Create your own invitation link"));
    }

    fn render_request_page_with_status(current_status: Option<RequestState>) -> String {
        let link = sample_link();
        crate::testing::render(move || {
            rsx! {
                RequestPage {
                    slug: "abcdEFGH01234567".to_string(),
                    link: link.clone(),
                    signed_in_login: "octocat".to_string(),
                    flash: None,
                    request_id: "01ARZ3NDEKTSV4RRFFQ69G5FAV".to_string(),
                    current_status,
                    retry_notice: None,
                }
            }
        })
    }

    #[test]
    fn request_page_pending_status_auto_refreshes_and_keeps_manual_fallback() {
        let html = render_request_page_with_status(Some(RequestState::Pending));

        assert!(html.contains("<meta http-equiv=\"refresh\" content=\"20\""));
        assert!(html.contains("Check again"));
        assert!(html.contains("href=\"/i/abcdEFGH01234567\""));
    }

    #[test]
    fn request_page_non_pending_states_do_not_auto_refresh() {
        let states = [
            None,
            Some(RequestState::Approved),
            Some(RequestState::Declined),
            Some(RequestState::Expired),
            Some(RequestState::Cancelled),
        ];

        for state in states {
            let html = render_request_page_with_status(state);
            assert!(
                !html.contains("http-equiv=\"refresh\""),
                "unexpected meta refresh for status {state:?}"
            );
        }
    }

    #[test]
    fn request_page_renders_approved_status_without_form() {
        let link = sample_link();
        let html = crate::testing::render(move || {
            rsx! {
                RequestPage {
                    slug: "abcdEFGH01234567".to_string(),
                    link: link.clone(),
                    signed_in_login: "octocat".to_string(),
                    flash: None,
                    request_id: "01ARZ3NDEKTSV4RRFFQ69G5FAV".to_string(),
                    current_status: Some(RequestState::Approved),
                    retry_notice: None,
                }
            }
        });

        assert!(html.contains("Approved"));
        assert!(html.contains("GitHub notifications and email"));
        assert!(!html.contains("textarea"));
        assert!(!html.contains("Submit request"));
    }

    #[test]
    fn request_page_renders_retry_notice_with_form() {
        let link = sample_link();
        let html = crate::testing::render(move || {
            rsx! {
                RequestPage {
                    slug: "abcdEFGH01234567".to_string(),
                    link: link.clone(),
                    signed_in_login: "octocat".to_string(),
                    flash: None,
                    request_id: "01ARZ3NDEKTSV4RRFFQ69G5FAV".to_string(),
                    current_status: None,
                    retry_notice: Some(RequestState::Declined),
                }
            }
        });

        assert!(html.contains("Your previous request was declined."));
        assert!(html.contains("You can submit a new request."));
        assert!(html.contains("Submit request"));
        assert!(html.contains("Justification"));
    }

    #[test]
    fn request_page_renders_declined_status_as_retryable_form() {
        let link = sample_link();
        let html = crate::testing::render(move || {
            rsx! {
                RequestPage {
                    slug: "abcdEFGH01234567".to_string(),
                    link: link.clone(),
                    signed_in_login: "octocat".to_string(),
                    flash: None,
                    request_id: "01ARZ3NDEKTSV4RRFFQ69G5FAV".to_string(),
                    current_status: Some(RequestState::Declined),
                    retry_notice: None,
                }
            }
        });

        assert!(html.contains("Your previous request was declined."));
        assert!(html.contains("You can submit a new request."));
        assert!(html.contains("Submit request"));
        assert!(html.contains("Justification"));
        assert!(html.contains("action=\"/i/abcdEFGH01234567\""));
    }
}
