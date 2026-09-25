//! Invitation request flow views.

use dioxus::prelude::*;
use ghinvite_core::Permission;

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

// ── request status ───────────────────────────────────────────────────────────

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
#[derive(Clone, Copy, PartialEq)]
pub enum DeliveryStatus {
    Approved,
    Planned,
    Submitted,
    Blocked,
    Throttled,
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
    pub fn label(self) -> &'static str {
        match self {
            Self::Approved => "Approved — awaiting dispatch",
            Self::Planned => "Planned — awaiting dispatch",
            Self::Submitted => "Submitted — awaiting GitHub confirmation",
            Self::Blocked => "Blocked — waiting for availability or identity verification",
            Self::Throttled => "Throttled — waiting for GitHub’s rate limit",
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
            Self::Throttled => {
                "GitHub asked us to wait. Delivery will retry after the rate limit clears; check this page again later."
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
    receipts: &[ghinvite_core::delivery::DeliverySnapshot],
    progress: &[ghinvite_core::delivery::RepositoryProgress],
    invitations: &[ghinvite_core::GithubInvitation],
    read_unavailable: bool,
) -> DeliveryPresentation {
    use DeliveryStatus as S;
    use ghinvite_core::{
        InvitationState,
        delivery::{CreateOutcome, DispatchStage},
    };
    let snapshot = receipts
        .iter()
        .find(|r| r.create.command.repo_id == repo.repo_id);
    let outcome = snapshot.map(|r| &r.create.outcome);
    let stage = progress
        .iter()
        .find(|r| r.repo_id == repo.repo_id)
        .map(|r| &r.stage);
    let invitation = invitations.iter().find(|r| r.repo_id == repo.repo_id);
    let lifecycle = snapshot
        .map(|s| s.invitation().state)
        .or_else(|| invitation.map(|r| r.state));
    // Settlement is later evidence; a retained create receipt is historical.
    let status = match (lifecycle, outcome) {
        (Some(InvitationState::Declined), _) => S::Declined,
        (Some(InvitationState::Expired), _) => S::Expired,
        (Some(InvitationState::Cancelled), _) => S::Cancelled,
        (_, Some(CreateOutcome::AlreadyCollaborator)) => S::Collaborator,
        // A 204 (already-collaborator) create has no upstream invitation to
        // accept. A retained create ID, when present, still distinguishes a
        // real invitation.
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
        (_, Some(CreateOutcome::Throttled)) => S::Throttled,
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
