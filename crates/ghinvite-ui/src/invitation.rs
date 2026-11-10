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

pub(crate) fn repo_grant_label(grant: ghinvite_core::targets::RepoRole) -> &'static str {
    permission_label(grant.into())
}

#[component]
pub fn AccessSummary(targets: ghinvite_core::targets::Targets, approval_required: bool) -> Element {
    rsx! {
        div { class: "space-y-3",
            ul { class: "space-y-2",
                if let Some(org) = &targets.organization {
                    li { class: "rounded-box bg-base-200 px-3 py-2 break-all", "Organization: {org.org_login} — Member" }
                }
                for team in &targets.teams {
                    li { class: "rounded-box bg-base-200 px-3 py-2 break-all", "Team: {team.team_slug} — Member" }
                }
                for repo in &targets.repos {
                    li { class: "rounded-box bg-base-200 px-3 py-2 break-all",
                        span { class: "font-mono text-sm", "{repo.repo_full_name}" }
                        span { class: "ml-2 badge badge-neutral", "{repo_grant_label(repo.grant)}" }
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
    max_chars: Option<usize>,
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
                if let Some(limit) = max_chars {
                    " Maximum {limit} characters."
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
    OutsideCollaboratorsRestricted,
    IpAllowList,
    Throttled,
    LinkShareExhausted,
    BudgetThrottled,
    Unknown,
    Created,
    Sent,
    Collaborator,
    Upgraded,
    AlreadyPending,
    SkippedCustomRole,
    Held,
    Aborted,
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
            Self::OutsideCollaboratorsRestricted => "Blocked — outside collaborators restricted",
            Self::IpAllowList => "Blocked — GitHub IP allow list",
            Self::Throttled => "Throttled — waiting for GitHub’s rate limit",
            Self::LinkShareExhausted => {
                "Waiting — this link has used its share of today's invitations"
            }
            Self::BudgetThrottled => "Waiting for invitation budget capacity",
            Self::Unknown => "GitHub outcome unknown — awaiting reconciliation",
            Self::Created => "GitHub invitation created — awaiting acceptance",
            Self::Sent => "GitHub invitation sent — awaiting acceptance",
            Self::Collaborator => "Already a collaborator",
            Self::Upgraded => "Repository access upgraded",
            Self::AlreadyPending => "Existing GitHub invitation — awaiting acceptance",
            Self::SkippedCustomRole => "Custom repository role — admin review needed",
            Self::Held => "Delivery held for review",
            Self::Aborted => "Delivery stopped",
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
            Self::OutsideCollaboratorsRestricted => {
                "Ask an organization or enterprise owner to review the policy for inviting outside collaborators. No invitation was sent."
            }
            Self::IpAllowList => {
                "Ask an account admin to review the GitHub IP allow list. This service has no fixed outbound IP address, so organizations requiring fixed egress are currently unsupported. No invitation was sent."
            }
            Self::Throttled => {
                "GitHub asked us to wait. Delivery will retry after the rate limit clears; check this page again later."
            }
            Self::LinkShareExhausted => {
                "This link has used its share of today's invitations. Delivery will retry when its share is available again; you do not need to submit another request."
            }
            Self::BudgetThrottled => {
                "Delivery is waiting for capacity or spacing in the invitation budget. It will retry when capacity is available; you do not need to submit another request."
            }
            Self::Unknown => {
                "GitHub may already have sent an invitation. Check your GitHub notifications or email, then check this page again. If the outcome stays unknown, contact an account admin; do not submit another request to resend it."
            }
            Self::Created | Self::Sent | Self::AlreadyPending => {
                "Finish accepting repository access on GitHub. Check your GitHub notifications or invitation email. After accepting, check this page again; updates may take time. If you cannot find or accept it, contact an account admin."
            }
            Self::Collaborator | Self::Upgraded => {
                "GitHub confirmed you already had repository access; no invitation needs accepting. If you cannot open the repository, contact an account admin."
            }
            Self::Accepted => {
                "Your GitHub invitation was accepted; no further acceptance is needed. If you cannot open the repository, contact an account admin."
            }
            Self::Declined | Self::Expired | Self::Cancelled => {
                "This GitHub invitation is no longer awaiting acceptance. If you still need access, contact an account admin."
            }
            Self::SkippedCustomRole | Self::Held | Self::Aborted => {
                "No new invitation was sent. Contact an account admin to review repository access."
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
    pub expiry: crate::invitation_expiry::InvitationExpiry,
    pub repo: String,
    pub status: DeliveryStatus,
    pub observation_unavailable: bool,
}

pub fn delivery_presentation(
    repo: &ghinvite_core::InvitationLinkRepo,
    receipts: &[ghinvite_core::delivery::DeliverySnapshot],
    progress: &[ghinvite_core::delivery::RepositoryProgress],
    read_unavailable: bool,
) -> DeliveryPresentation {
    use DeliveryStatus as S;
    use ghinvite_core::{
        InvitationState,
        delivery::{BlockedReason, CreateOutcome, DispatchStage},
    };
    let snapshot = receipts
        .iter()
        .find(|r| r.create.command.repo_id == repo.repo_id);
    let outcome = snapshot.map(|r| &r.create.outcome);
    let stage = progress
        .iter()
        .find(|r| r.repo_id == repo.repo_id)
        .map(|r| &r.stage);
    let lifecycle = snapshot.and_then(|s| s.settlement.as_ref().map(|s| s.state));
    // Settlement is later evidence; a retained create receipt is historical.
    let status = match (lifecycle, outcome) {
        (Some(InvitationState::Declined), _) => S::Declined,
        (Some(InvitationState::Expired), _) => S::Expired,
        (Some(InvitationState::Cancelled), _) => S::Cancelled,
        (_, Some(CreateOutcome::AlreadySatisfied { .. })) => S::Collaborator,
        (_, Some(CreateOutcome::Upgraded { .. })) => S::Upgraded,
        (_, Some(CreateOutcome::SkippedCustomRole { .. })) => S::SkippedCustomRole,
        (_, Some(CreateOutcome::Held { .. })) => S::Held,
        (_, Some(CreateOutcome::Aborted { .. })) => S::Aborted,
        (_, Some(CreateOutcome::Planned)) => S::Planned,
        (_, Some(CreateOutcome::Submitted)) => S::Submitted,
        (Some(InvitationState::Accepted), _) => S::Accepted,
        (_, Some(CreateOutcome::Created { .. })) => S::Created,
        (_, Some(CreateOutcome::AlreadyPending { .. })) => S::AlreadyPending,
        (Some(InvitationState::Sent), _) => S::Sent,
        (_, Some(CreateOutcome::Failed { .. })) => S::Failed,
        (
            _,
            Some(CreateOutcome::Blocked {
                reason: BlockedReason::OutsideCollaboratorsRestricted,
            }),
        ) => S::OutsideCollaboratorsRestricted,
        (
            _,
            Some(CreateOutcome::Blocked {
                reason: BlockedReason::IpAllowList,
            }),
        ) => S::IpAllowList,
        (_, Some(CreateOutcome::Blocked { .. })) => S::Blocked,
        (
            _,
            Some(CreateOutcome::Throttled {
                reason: Some(ghinvite_core::budget::RetryReason::LinkShare),
                ..
            }),
        ) => S::LinkShareExhausted,
        (
            _,
            Some(CreateOutcome::Throttled {
                reason: Some(_), ..
            }),
        ) => S::BudgetThrottled,
        (_, Some(CreateOutcome::Throttled { .. })) => S::Throttled,
        (_, Some(CreateOutcome::OutcomeUnknown)) => S::Unknown,
        _ => match stage {
            Some(DispatchStage::Approved) => S::Approved,
            Some(DispatchStage::Planned) => S::Planned,
            Some(DispatchStage::Submitted) => S::Submitted,
            None => S::Unavailable,
        },
    };
    let expiry = if snapshot.is_some_and(|s| s.tracking_stopped.is_none())
        && matches!(status, S::Created | S::Sent | S::AlreadyPending)
    {
        crate::invitation_expiry::InvitationExpiry::pending(
            snapshot.and_then(|s| s.invitation_expires_at()),
        )
    } else {
        crate::invitation_expiry::InvitationExpiry::NotApplicable
    };
    DeliveryPresentation {
        expiry,
        repo: repo.repo_full_name.clone(),
        status,
        observation_unavailable: read_unavailable,
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
            crate::invitation_expiry::ExpiryNotice { expiry: row.expiry.clone() }
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
mod target_tests {
    use super::*;
    use ghinvite_core::targets::{RepoRole, RepoTarget, Targets};

    #[test]
    fn share_exhaustion_guidance_does_not_claim_github_refused_a_write() {
        let html = crate::testing::render(|| {
            rsx! {
                DeliveryRow {
                    row: DeliveryPresentation { expiry: Default::default(), repo: "acme/api".into(), status: DeliveryStatus::LinkShareExhausted, observation_unavailable: false },
                    login: "alice",
                }
            }
        });
        assert!(html.contains("this link has used its share of today"));
        assert!(html.contains("you do not need to submit another request"));
        assert!(!html.contains("GitHub asked us to wait"));
        assert!(!html.contains("Accept on GitHub"));
    }

    #[test]
    fn policy_refusals_render_fixed_actionable_guidance() {
        for (status, hint) in [
            (
                DeliveryStatus::OutsideCollaboratorsRestricted,
                "review the policy",
            ),
            (DeliveryStatus::IpAllowList, "no fixed outbound IP address"),
        ] {
            let html = crate::testing::render(move || {
                rsx! {
                    DeliveryRow { row: DeliveryPresentation { expiry: Default::default(), repo: "acme/api".into(), status, observation_unavailable: false }, login: "alice" }
                }
            });
            assert!(html.contains(hint));
            assert!(html.contains("No invitation was sent"));
            assert!(!html.contains("Accept on GitHub"));
        }
    }

    #[test]
    fn access_summary_keeps_each_repositorys_own_grant() {
        let html = crate::testing::render(|| {
            rsx! {
                AccessSummary {
                    targets: Targets { repos: vec![
                        RepoTarget { repo_id: 10, repo_full_name: "acme/read-only".into(), grant: RepoRole::Read },
                        RepoTarget { repo_id: 11, repo_full_name: "acme/admin".into(), grant: RepoRole::Admin },
                    ], ..Default::default() },
                    approval_required: true,
                }
            }
        });
        let items: Vec<_> = html.split("</li>").collect();
        assert!(items[0].contains("acme/read-only"));
        assert!(items[0].contains("Read (pull)"));
        assert!(!items[0].contains("Admin"));
        assert!(items[1].contains("acme/admin"));
        assert!(items[1].contains("Admin"));
        assert!(!items[1].contains("Read (pull)"));
    }
    #[test]
    fn repository_expiry_is_source_bound_and_missing_historical_policy_is_unavailable() {
        use crate::invitation_expiry::InvitationExpiry;
        use ghinvite_core::delivery::{
            CreateOutcome, CreateReceipt, DeliverySnapshot, InvitationTiming,
        };
        let at = chrono::DateTime::parse_from_rfc3339("2026-10-17T12:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let command=serde_json::from_value(serde_json::json!({"link_id":ghinvite_core::InvitationLinkId::new(),"request_id":ghinvite_core::RequestId::new(),"approval_id":"approval","account_id":100,"installation_id":1,"requester_id":8,"repo_id":10,"repo_full_name":"acme/api","permission":"pull","approved_at":at})).unwrap();
        let mut snapshot: DeliverySnapshot = CreateReceipt {
            audit_version: ghinvite_core::delivery_audit::DeliveryAuditVersion::CanonicalV2,
            command,
            outcome: CreateOutcome::Created { upstream_id: 99 },
            revision: 1,
            confirmed_at: Some(at),
        }
        .into();
        let repo = ghinvite_core::InvitationLinkRepo {
            repo_id: 10,
            repo_full_name: "acme/api".into(),
        };
        assert_eq!(
            delivery_presentation(&repo, &[snapshot.clone()], &[], false).expiry,
            InvitationExpiry::Unavailable
        );
        let mut timing = InvitationTiming::observed(99, at, at, at, true).unwrap();
        timing.owned_lifetime_seconds = Some(3600);
        snapshot.retain_invitation_timing(timing);
        assert_eq!(
            delivery_presentation(&repo, &[snapshot.clone()], &[], false).expiry,
            InvitationExpiry::Known(at + chrono::Duration::hours(1))
        );
        snapshot.create.outcome = CreateOutcome::AlreadyPending {
            upstream_id: 99,
            permission: RepoRole::Read,
        };
        assert_eq!(
            delivery_presentation(&repo, &[snapshot.clone()], &[], false).expiry,
            InvitationExpiry::Known(at + chrono::Duration::days(7))
        );
        snapshot.create.outcome = CreateOutcome::OutcomeUnknown;
        assert_eq!(
            delivery_presentation(&repo, &[snapshot], &[], false).expiry,
            InvitationExpiry::NotApplicable
        );
    }
}
