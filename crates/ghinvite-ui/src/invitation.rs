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
pub const PENDING_REFRESH_SECONDS: u32 = 20;

#[component]
pub fn IdentityConfirmation(login: String, return_to: String) -> Element {
    rsx! {
        div { class: "alert alert-info",
            div {
                "Signed in as "
                strong { "@{login}" }
                ". Not you? "
                form { method: "post", action: "/logout",
                    crate::csrf::CsrfField {}
                    input { r#type: "hidden", name: "return_to", value: "{return_to}" }
                    button { r#type: "submit", class: "link", "Sign out and sign in again." }
                }
            }
        }
    }
}

#[component]
pub fn AccessSummary(
    permission: Permission,
    repos: Vec<ghinvite_core::InvitationLinkRepo>,
    approval_required: bool,
) -> Element {
    let label = permission_label(permission);
    rsx! {
        div { class: "space-y-3",
            span { class: "badge badge-neutral", "Permission: {label}" }
            ul { class: "space-y-2",
                for repo in &repos {
                    li { class: "rounded-box bg-base-200 px-3 py-2 break-all",
                        span { class: "font-mono text-sm", "{repo.repo_full_name}" }
                    }
                }
            }
            p { class: "text-sm leading-6 text-base-content/70",
                if approval_required {
                    "Account admins review your request before access is approved."
                } else {
                    "Requests are automatically approved by this invitation link's policy. Approval does not guarantee GitHub invitation delivery."
                }
            }
        }
    }
}

#[component]
pub fn JustificationField(
    value: String,
    #[props(default)] readonly: bool,
    error: Option<String>,
    max_bytes: Option<usize>,
) -> Element {
    rsx! {
        div { class: "form-control gap-2",
            label { class: "label", r#for: "justification", span { class: "label-text font-medium", "Justification" } }
            textarea {
                id: "justification", name: "justification",
                class: "textarea textarea-bordered min-h-28 w-full",
                placeholder: "Share useful context for the account admins.", rows: "4",
                readonly, aria_invalid: if error.is_some() { "true" } else { "false" },
                aria_describedby: if error.is_some() { "justification-help justification-error" } else { "justification-help" },
                "{value}"
            }
            p { id: "justification-help", class: "text-sm text-base-content/70",
                "Optional, visible to account admins."
                if let Some(limit) = max_bytes {
                    " Maximum {limit} UTF-8 bytes; non-ASCII characters may use multiple bytes."
                }
            }
            if let Some(error) = &error {
                p { id: "justification-error", class: "text-sm text-error", "{error}" }
            }
        }
    }
}

#[derive(Clone, PartialEq, Props)]
pub struct RequestPageProps {
    pub slug: String,
    pub link: InvitationLink,
    pub signed_in_login: String,
    pub flash: Option<crate::flash::Flash>,
    pub request_id: String,
    #[props(default)]
    pub justification: String,
    pub current_status: Option<RequestState>,
    pub retry_notice: Option<RequestState>,
    #[props(default)]
    pub delivery: Vec<ghinvite_core::delivery::CreateReceipt>,
    #[props(default)]
    pub delivery_progress: Vec<ghinvite_core::delivery::RepositoryProgress>,
    #[props(default)]
    pub legacy_delivery: Vec<ghinvite_core::GithubInvitation>,
    #[props(default)]
    pub delivery_unavailable: bool,
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
                    "Your request is approved. Repository delivery is tracked separately below."
                }
                ul { class: "space-y-4", aria_label: "Repository delivery",
                    for repo in &props.link.repos {
                        DeliveryRow {
                            row: delivery_presentation(
                                repo,
                                &props.delivery,
                                &props.delivery_progress,
                                &props.legacy_delivery,
                                props.delivery_unavailable),
                            login: login.clone(),
                        }
                    }
                }
                a { class: "btn btn-primary", href: "{check_href}", "Check again" }
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
                IdentityConfirmation { login: login.clone(), return_to: action.clone() }
                AccessSummary {
                    permission: props.link.permission, repos: props.link.repos.clone(), approval_required: props.link.approval_required,
                }
                form {
                    method: "post",
                    action: "{action}",
                    class: "space-y-4",
                    crate::csrf::CsrfField {}
                    input {
                        r#type: "hidden",
                        name: "request_id",
                        value: "{request_id}",
                    }
                    JustificationField { value: props.justification.clone(), error: None }
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

#[derive(Clone, Copy, PartialEq)]
pub enum DeliveryStatus {
    Approved,
    Planned,
    Submitted,
    Blocked,
    Unknown,
    Created,
    Sent,
    Collaborator,
    Failed,
    Accepted,
    Declined,
    Expired,
    Cancelled,
    Unavailable,
}

impl DeliveryStatus {
    fn label(self) -> &'static str {
        match self {
            Self::Approved => "Approved — awaiting dispatch",
            Self::Planned => "Planned — awaiting dispatch",
            Self::Submitted => "Submitted — awaiting GitHub confirmation",
            Self::Blocked => "Blocked — waiting for availability or identity verification",
            Self::Unknown => "GitHub outcome unknown — awaiting reconciliation",
            Self::Created => "GitHub invitation created — awaiting acceptance",
            Self::Sent => "GitHub invitation sent — awaiting acceptance",
            Self::Collaborator => "Already a collaborator",
            Self::Failed => "GitHub rejected delivery",
            Self::Accepted => "Repository access accepted",
            Self::Declined => "GitHub invitation declined",
            Self::Expired => "GitHub invitation expired",
            Self::Cancelled => "GitHub invitation cancelled",
            Self::Unavailable => "Delivery status unavailable",
        }
    }

    fn guidance(self) -> &'static str {
        match self {
            Self::Approved | Self::Planned | Self::Submitted => {
                "Wait, then check again for GitHub confirmation. Approval alone does not mean an invitation has been sent. If this persists, contact an account admin."
            }
            Self::Blocked => {
                "Delivery is waiting for a prerequisite. Wait, then check again; if it stays blocked, contact an account admin."
            }
            Self::Unknown => {
                "GitHub may already have sent an invitation. Check your GitHub notifications or email, then check this page again. If the outcome stays unknown, contact an account admin; do not submit another request to resend it."
            }
            Self::Created | Self::Sent => {
                "Finish accepting repository access on GitHub. Check your GitHub notifications or invitation email. After accepting, check this page again; updates may take time. If you cannot find or accept it, contact an account admin."
            }
            Self::Collaborator => {
                "GitHub confirmed you already had repository access; no invitation needs accepting. If you cannot open the repository, contact an account admin."
            }
            Self::Accepted => {
                "Your GitHub invitation was accepted; no further acceptance is needed. If you cannot open the repository, contact an account admin."
            }
            Self::Declined | Self::Expired | Self::Cancelled => {
                "This GitHub invitation is no longer awaiting acceptance. If you still need access, contact an account admin."
            }
            Self::Failed => {
                "GitHub definitively rejected this delivery. Contact an account admin to resolve access."
            }
            Self::Unavailable => {
                "Missing status does not mean delivery failed. Check your GitHub notifications or email and check this page again later. If status stays unavailable, contact an account admin."
            }
        }
    }
}

/// Requester-safe view data: no retained commands, reasons or diagnostics.
#[derive(Clone, PartialEq)]
pub struct DeliveryPresentation {
    pub repo: String,
    pub status: DeliveryStatus,
    pub observation_unavailable: bool,
}

pub fn delivery_presentation(
    repo: &ghinvite_core::InvitationLinkRepo,
    receipts: &[ghinvite_core::delivery::CreateReceipt],
    progress: &[ghinvite_core::delivery::RepositoryProgress],
    invitations: &[ghinvite_core::GithubInvitation],
    read_unavailable: bool,
) -> DeliveryPresentation {
    use DeliveryStatus as S;
    use ghinvite_core::{
        InvitationState,
        delivery::{CreateOutcome, DispatchStage},
    };
    let outcome = receipts
        .iter()
        .find(|r| r.command.repo_id == repo.repo_id)
        .map(|r| &r.outcome);
    let stage = progress
        .iter()
        .find(|r| r.repo_id == repo.repo_id)
        .map(|r| &r.stage);
    let invitation = invitations.iter().find(|r| r.repo_id == repo.repo_id);
    let lifecycle = invitation.map(|r| r.state);
    // Settlement is later evidence; a retained create receipt is historical.
    let status = match (lifecycle, outcome) {
        (Some(InvitationState::Declined), _) => S::Declined,
        (Some(InvitationState::Expired), _) => S::Expired,
        (Some(InvitationState::Cancelled), _) => S::Cancelled,
        (_, Some(CreateOutcome::AlreadyCollaborator)) => S::Collaborator,
        // Legacy 204 responses have no upstream invitation to accept. A retained
        // create ID, when present, still distinguishes a real invitation.
        (Some(InvitationState::Accepted), None)
            if invitation.is_some_and(|r| r.github_invitation_id.is_none()) =>
        {
            S::Collaborator
        }
        (Some(InvitationState::Accepted), _) => S::Accepted,
        (_, Some(CreateOutcome::Created { .. })) => S::Created,
        (Some(InvitationState::Sent), _) => S::Sent,
        (_, Some(CreateOutcome::Failed { .. })) => S::Failed,
        (_, Some(CreateOutcome::Blocked { .. })) => S::Blocked,
        (_, Some(CreateOutcome::OutcomeUnknown)) => S::Unknown,
        (Some(InvitationState::Failed), None) => S::Failed,
        _ => match stage {
            Some(DispatchStage::Approved) => S::Approved,
            Some(DispatchStage::Planned) => S::Planned,
            Some(DispatchStage::Submitted) => S::Submitted,
            None => S::Unavailable,
        },
    };
    DeliveryPresentation {
        repo: repo.repo_full_name.clone(),
        status,
        observation_unavailable: read_unavailable || (lifecycle.is_none() && status == S::Created),
    }
}

#[component]
pub fn DeliveryRow(row: DeliveryPresentation, login: String) -> Element {
    use DeliveryStatus as S;
    let label = row.status.label();
    let guidance = row.status.guidance();
    // Only offer repository URLs for GitHub-compatible owner/name segments.
    let repository_url = row
        .repo
        .split_once('/')
        .filter(|(owner, name)| {
            [*owner, *name].iter().all(|part| {
                !part.is_empty()
                    && !matches!(*part, "." | "..")
                    && part
                        .bytes()
                        .all(|c| c.is_ascii_alphanumeric() || b"-_.".contains(&c))
            })
        })
        .map(|_| format!("https://github.com/{}", row.repo));
    let (repository_action, show_notifications) = match row.status {
        S::Created | S::Sent => (Some(("Accept on GitHub", "/invitations")), true),
        S::Accepted | S::Collaborator => (Some(("Open repository", "")), false),
        S::Unknown | S::Unavailable => (None, true),
        _ => (None, false),
    };
    rsx! {
        li { class: "rounded-box bg-base-200 p-4 space-y-3 break-words",
            h3 { class: "font-mono text-sm break-all", "{row.repo}" }
            p { class: "font-semibold", "{label}" }
            p { class: "text-sm leading-6", "{guidance}" }
            if row.observation_unavailable {
                p { class: "text-sm", "Latest status updates are unavailable. Any known outcome above is retained history; the invitation may have changed since. Missing status does not mean delivery failed." }
            }
            if repository_action.is_some() || show_notifications {
                p { class: "text-sm", "Use GitHub signed in as @{login}." }
                div { class: "flex flex-wrap gap-3",
                    if let (Some(url), Some((label, suffix))) = (repository_url, repository_action) {
                        a { class: "link", href: "{url}{suffix}", "{label}" }
                    }
                    if show_notifications {
                        a { class: "link", href: "https://github.com/notifications", "GitHub notifications" }
                    }
                }
            }
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
        assert!(html.contains("action=\"/logout\""));
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
        assert!(html.contains("Repository delivery is tracked separately"));
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
#[dioxus::prelude::component]
pub fn InvitationUnavailablePage(
    signed_in_login: Option<String>,
    retry_href: String,
) -> dioxus::prelude::Element {
    dioxus::prelude::rsx! {
        crate::layouts::InvitationLayout {
            signed_in_login,
            title: "Temporarily unavailable · ghinvite",
            account_login: None,
            active_nav: None,
            flash: None,
            children: dioxus::prelude::rsx! {
                section { class: "space-y-4 p-6",
                    h1 { class: "text-xl font-semibold", "Invitation request flow is temporarily unavailable" }
                    p { "We could not load this page. Try again in a moment." }
                    a { class: "btn btn-primary", href: retry_href, "Try again" }
                    a { class: "btn btn-ghost", href: "/", "Go home" }
                }
            },
        }
    }
}
