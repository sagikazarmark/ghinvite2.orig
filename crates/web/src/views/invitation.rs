//! Invitation flow views: LandingPage, RequestFormPage, PendingPage.

use crate::views::layouts::InvitationLayout;
use chrono::{DateTime, Utc};
use dioxus::prelude::*;
use domain::{Permission, RequestState, ShareLink};

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

// ── LandingPage ──────────────────────────────────────────────────────────────

#[derive(Clone, PartialEq, Props)]
pub struct LandingProps {
    pub slug: String,
    pub link: ShareLink,
    pub signed_in_login: Option<String>,
    pub now: DateTime<Utc>,
}

#[component]
pub fn LandingPage(props: LandingProps) -> Element {
    let slug = props.slug.clone();
    let perm_label = permission_label(props.link.permission);
    let repo_word = if props.link.repos.len() == 1 {
        "repository"
    } else {
        "repositories"
    };

    let repos_view = props.link.repos.iter().map(|r| {
        let name = r.repo_full_name.clone();
        rsx! { li { "{name}" } }
    });

    let expiry_view = match props.link.expires_at {
        None => rsx! {},
        Some(expires_at) => {
            let diff = expires_at.signed_duration_since(props.now);
            let days = diff.num_days();
            let expiry_text = if days <= 0 {
                "Expires today".to_string()
            } else {
                format!(
                    "Expires in {} day{}",
                    days,
                    if days == 1 { "" } else { "s" }
                )
            };
            rsx! {
                p { class: "text-sm opacity-70", "{expiry_text}" }
            }
        }
    };

    let cta_view = match &props.signed_in_login {
        None => {
            let href = format!("/login?return_to=/i/{slug}/request");
            rsx! {
                a {
                    class: "btn btn-primary w-full",
                    href: "{href}",
                    "Sign in with GitHub to request access"
                }
            }
        }
        Some(login) => {
            let login = login.clone();
            let href = format!("/i/{slug}/request");
            rsx! {
                p { class: "text-sm opacity-70 mb-2", "Signed in as @{login}" }
                a {
                    class: "btn btn-primary w-full",
                    href: "{href}",
                    "Continue to request access"
                }
            }
        }
    };

    rsx! {
        InvitationLayout {
            signed_in_login: props.signed_in_login.clone(),
            title: "You've been invited · ghinvite".to_string(),
            account_login: None,
            active_nav: None,
            flash: None,
            children: rsx! {
                h1 { class: "text-xl font-semibold tracking-tight", "Access request" }
                p { class: "mt-2 text-sm text-base-content/70",
                    "This share link requests collaborator access to the following {repo_word}:"
                }
                ul { class: "mt-4 list-inside list-disc space-y-1 rounded-box border border-base-300 bg-base-200 p-4 text-sm", {repos_view} }
                p { class: "mt-4",
                    span { class: "badge badge-neutral", "Permission: {perm_label}" }
                }
                p { class: "mt-3 text-sm text-base-content/70",
                    "GitHub sign-in confirms your identity before any request is sent."
                }
                {expiry_view}
                div { class: "card-actions justify-end mt-6", {cta_view} }
            },
        }
    }
}

// ── RequestFormPage ───────────────────────────────────────────────────────────

#[derive(Clone, PartialEq, Props)]
pub struct RequestFormProps {
    pub slug: String,
    pub link: ShareLink,
    pub signed_in_login: String,
    pub flash: Option<crate::session::Flash>,
    /// Pre-generated ULID from GET handler for double-submit dedup.
    pub request_id: String,
}

#[component]
pub fn RequestFormPage(props: RequestFormProps) -> Element {
    let login = props.signed_in_login.clone();
    let perm_label = permission_label(props.link.permission);
    let repo_count = props.link.repos.len();
    let repo_word = if repo_count == 1 { "repo" } else { "repos" };

    let flash_view = match &props.flash {
        None => rsx! {},
        Some(f) => {
            let alert_class = match f.level {
                crate::session::FlashLevel::Error => "alert alert-error mb-4",
                _ => "alert alert-info mb-4",
            };
            let msg = f.message.clone();
            rsx! {
                div { class: "{alert_class}", span { "{msg}" } }
            }
        }
    };

    let repos_view = props.link.repos.iter().map(|r| {
        let name = r.repo_full_name.clone();
        rsx! { li { "{name}" } }
    });

    let logout_href = "/logout".to_string();
    let slug = props.slug.clone();
    let action = format!("/i/{slug}/request");
    let request_id = props.request_id.clone();

    rsx! {
        InvitationLayout {
            signed_in_login: Some(props.signed_in_login.clone()),
            title: "Request access · ghinvite".to_string(),
            account_login: None,
            active_nav: None,
            flash: None,
            children: rsx! {
                header { class: "mb-4",
                    h1 { class: "text-xl font-semibold tracking-tight", "Request access" }
                    p { class: "mt-1 text-sm text-base-content/70", "Confirm the repositories and include context for the account admins." }
                }
                {flash_view}
                div { class: "alert alert-info mb-4",
                    span {
                        "Signed in as "
                        strong { "@{login}" }
                        ". Not you? "
                        a { href: "{logout_href}", class: "link", "Sign out and sign in again." }
                    }
                }
                p { class: "mb-2",
                    "Requesting "
                    span { class: "badge badge-neutral", "Permission: {perm_label}" }
                    " access to {repo_count} {repo_word}:"
                }
                ul { class: "list-disc list-inside mb-4", {repos_view} }
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
                            class: "textarea textarea-bordered w-full",
                            placeholder: "Why do you need access?",
                            rows: "3",
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
            },
        }
    }
}

// ── PendingPage ───────────────────────────────────────────────────────────────

#[derive(Clone, PartialEq, Props)]
pub struct PendingProps {
    pub slug: String,
    pub request_id: String,
    pub request_state: Option<RequestState>,
    pub signed_in_login: Option<String>,
}

#[component]
pub fn PendingPage(props: PendingProps) -> Element {
    let slug = props.slug.clone();
    let request_id = props.request_id.clone();
    let refresh_href = format!("/i/{slug}/pending/{request_id}");

    let status_view = match props.request_state {
        None => rsx! {
            div { class: "alert alert-warning",
                div {
                    p { "Your request is being processed." }
                    a { class: "link", href: "{refresh_href}", "Check again" }
                }
            }
        },
        Some(RequestState::Pending) => rsx! {
            div { class: "alert alert-warning",
                div {
                    p { "Your request is awaiting admin review." }
                    a { class: "link", href: "{refresh_href}", "Check again" }
                }
            }
        },
        Some(RequestState::Approved) => rsx! {
            div { class: "alert alert-success",
                span {
                    "Approved! Check your GitHub notifications and email for the repository invitation."
                }
            }
        },
        Some(RequestState::Declined) => {
            let back_href = format!("/i/{slug}");
            rsx! {
                div { class: "alert alert-error",
                    span { "Your request was declined." }
                }
                div { class: "mt-4",
                    a { href: "{back_href}", class: "link", "Back to invitation" }
                }
            }
        }
        Some(RequestState::Expired) => {
            let new_request_href = format!("/i/{slug}");
            rsx! {
                div { class: "alert alert-warning",
                    span { "This request has expired." }
                }
                div { class: "mt-4",
                    a { href: "{new_request_href}", class: "link", "Submit a new request" }
                }
            }
        }
        Some(RequestState::Cancelled) => rsx! {
            div { class: "alert alert-warning",
                span { "This request has been cancelled." }
            }
        },
    };

    rsx! {
        InvitationLayout {
            signed_in_login: props.signed_in_login.clone(),
            title: "Request status · ghinvite".to_string(),
            account_login: None,
            active_nav: None,
            flash: None,
            children: rsx! {
                h1 { class: "mb-4 text-xl font-semibold tracking-tight", "Request status" }
                {status_view}
                div { class: "mt-6 flex flex-col gap-2 sm:flex-row",
                    a { class: "btn btn-primary", href: "/console", "Create your own link" }
                    a { class: "btn btn-ghost", href: "/", "Go home" }
                }
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use domain::RequestState;

    #[test]
    fn pending_page_renders_refresh_affordance() {
        let html = crate::views::render::render(|| {
            rsx! {
                PendingPage {
                    slug: "abcdEFGH01234567".to_string(),
                    request_id: "01ARZ3NDEKTSV4RRFFQ69G5FAV".to_string(),
                    request_state: Some(RequestState::Pending),
                    signed_in_login: Some("octocat".to_string()),
                }
            }
        });

        assert!(html.contains("Check again"));
        assert!(html.contains("awaiting admin review"));
        assert!(html.contains("Request status"));
        assert!(!html.contains("card-title"));
        assert!(html.contains("/i/abcdEFGH01234567/pending/01ARZ3NDEKTSV4RRFFQ69G5FAV"));
        assert!(html.contains("Create your own link"));
        assert!(html.contains("href=\"/console\""));
        assert!(html.contains("Go home"));
        assert!(html.contains("href=\"/\""));
    }
}
