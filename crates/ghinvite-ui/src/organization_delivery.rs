//! Organization and team receipt presentation. Dispatch and missing projections
//! never establish membership. Every row belongs to one immutable generation.
use dioxus::prelude::*;
use ghinvite_core::{
    InvitationState,
    delivery::DispatchStage,
    org_delivery::{
        MembershipState, OrganizationBlockedReason, OrganizationCreateOutcome,
        OrganizationDeliverySnapshot, OrganizationRole, TeamCreateOutcome, TeamRole,
    },
    request_lifecycle::OrganizationProgress,
    targets::Targets,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OrganizationDeliveryView {
    pub expiry: crate::invitation_expiry::InvitationExpiry,
    pub organization: String,
    pub generation: Option<u64>,
    pub status: String,
    pub teams: Vec<TeamDeliveryView>,
    pub unavailable: bool,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TeamDeliveryView {
    pub expiry: crate::invitation_expiry::InvitationExpiry,
    pub name: String,
    pub status: String,
}

/// Inputs are scoped to the authenticated request by the caller/read port.
/// A receipt for a different immutable target set cannot supply this view.
pub fn presentation(
    targets: &Targets,
    snapshots: &[OrganizationDeliverySnapshot],
    progress: Option<&OrganizationProgress>,
    unavailable: bool,
) -> Option<OrganizationDeliveryView> {
    let org = targets.organization.as_ref()?;
    let mut expected_teams: Vec<_> = targets.teams.iter().map(|t| t.team_id).collect();
    expected_teams.sort_unstable();
    let progress = progress.filter(|p| {
        let mut ids = p.team_ids.clone();
        ids.sort_unstable();
        p.org_id == org.org_id && ids == expected_teams
    });
    let latest = snapshots
        .iter()
        .filter(|s| {
            s.create.command.organization == *org
                && s.create.command.teams == targets.teams
                && progress
                    .is_none_or(|p| p.delivery_generation == s.create.command.delivery_generation)
        })
        .max_by_key(|s| s.create.command.delivery_generation);
    let pending = || {
        match progress.map(|p| &p.stage) {
            Some(DispatchStage::Planned) => "Delivery planned. No GitHub outcome recorded yet.",
            Some(DispatchStage::Submitted) => "Delivery submitted. No GitHub outcome recorded yet.",
            Some(DispatchStage::Approved) => "Request approved. No GitHub outcome recorded yet.",
            None => "No projected delivery outcome yet. Dispatch or projection may be delayed.",
        }
        .to_owned()
    };
    let status = latest.map(|s| {
        if s.tracking_stopped.is_some() {
            return "Tracking stopped. This does not establish that the GitHub invitation was cancelled or expired.".into();
        }
        if let Some(settlement) = &s.settlement {
            return lifecycle(settlement.state).into();
        }
        organization_status(&s.create.outcome)
    }).unwrap_or_else(pending);
    let expiry = latest
        .filter(|s| {
            s.settlement.is_none()
                && s.tracking_stopped.is_none()
                && s.create.outcome.upstream_id().is_some()
        })
        .map(|s| crate::invitation_expiry::InvitationExpiry::pending(s.invitation_expires_at()))
        .unwrap_or_default();
    let teams = targets.teams.iter().map(|team| TeamDeliveryView {
        expiry: latest.and_then(|s| s.teams.get(&team.team_id).map(|t| (s,t))).filter(|(_,t)| t.settlement.is_none() && t.tracking_stopped.is_none()).map(|(s,t)| match &t.create.outcome {
            TeamCreateOutcome::Created { via: ghinvite_core::org_delivery::TeamCreation::OrganizationInvitation { upstream_id } } => crate::invitation_expiry::InvitationExpiry::pending(s.create.outcome.upstream_id().filter(|id| id == upstream_id).and_then(|_| s.invitation_expires_at())),
            TeamCreateOutcome::Created { via: ghinvite_core::org_delivery::TeamCreation::PendingMembership } | TeamCreateOutcome::AlreadySatisfied { state: MembershipState::Pending, .. } => crate::invitation_expiry::InvitationExpiry::Unavailable,
            _ => crate::invitation_expiry::InvitationExpiry::NotApplicable,
        }).unwrap_or_default(),
        name: team.team_slug.clone(),
        status: latest.and_then(|s| s.teams.get(&team.team_id)).map(|s| {
            if s.tracking_stopped.is_some() {
                "Tracking stopped. Current team access is not established.".into()
            } else if let Some(settlement) = &s.settlement {
                lifecycle(settlement.state).into()
            } else {
                team_status(&s.create.outcome)
            }
        }).unwrap_or_else(|| "No projected team outcome yet. Organization membership does not establish team access.".into()),
    }).collect();
    Some(OrganizationDeliveryView {
        expiry,
        organization: org.org_login.clone(),
        generation: progress
            .map(|p| p.delivery_generation)
            .or_else(|| latest.map(|s| s.create.command.delivery_generation)),
        status,
        teams,
        unavailable,
    })
}

fn lifecycle(state: InvitationState) -> &'static str {
    match state {
        InvitationState::Accepted => "Accepted — verified access was observed.",
        InvitationState::Declined => "Declined.",
        InvitationState::Expired => "GitHub invitation expiry was observed.",
        InvitationState::Cancelled => "Invitation cancellation was confirmed.",
        InvitationState::Sent => "Invitation pending acceptance on GitHub.",
        _ => "No final access outcome has been verified.",
    }
}
fn blocked(reason: OrganizationBlockedReason) -> &'static str {
    use OrganizationBlockedReason::*;
    match reason {
        InstallationUnavailable => {
            "Blocked: the organization app installation is unavailable. Ask an owner to review its installation."
        }
        InstallationPermissions => {
            "Blocked: the organization app needs its required permissions approved."
        }
        RequesterIdentity => "Blocked: the requester identity could not be verified.",
        TargetUnavailable => {
            "Blocked: the organization or team is unavailable. Ask an owner to review the target."
        }
        MintForbidden => {
            "Blocked: GitHub refused organization app credentials. Ask an owner to review its installation."
        }
        IpAllowList => {
            "Blocked: the GitHub IP allow list prevents access. Fixed outbound IP requirements are unsupported."
        }
        NoSeat => {
            "Blocked: no organization seat is available. Ask an organization owner to review seats."
        }
        IdpSyncedTeam => {
            "Blocked: this team is managed by an identity provider. Request access through that provider."
        }
        ManagedEnterprise => {
            "Blocked: managed enterprise policy prevents this invitation. Ask an enterprise owner to review access."
        }
    }
}
fn throttled(
    until: &chrono::DateTime<chrono::Utc>,
    reason: Option<ghinvite_core::budget::RetryReason>,
) -> String {
    if reason == Some(ghinvite_core::budget::RetryReason::LinkShare) {
        format!(
            "This link has used its share of today's invitations. Retry scheduled no earlier than {}.",
            until.to_rfc3339()
        )
    } else {
        format!(
            "Waiting to retry no earlier than {}. No access is established by this wait.",
            until.to_rfc3339()
        )
    }
}
fn organization_status(outcome: &OrganizationCreateOutcome) -> String {
    use OrganizationCreateOutcome::*;
    match outcome {
        Planned => "Delivery planned. No GitHub outcome recorded yet.".into(),
        Submitted => "Delivery submitted. No GitHub outcome recorded yet.".into(),
        Held { .. } => "Delivery is on hold.".into(),
        Blocked { reason } => blocked(*reason).into(),
        Throttled { until, reason } => throttled(until, *reason),
        OutcomeUnknown => "Outcome unknown. GitHub may have applied the invitation; verification is pending.".into(),
        Created { .. } => "Organization invitation created — pending acceptance on GitHub.".into(),
        AlreadyPending { .. } => "An existing organization invitation is pending. It is tracked without claiming ownership.".into(),
        AlreadySatisfied { role: OrganizationRole::Member } => "Active organization membership was verified.".into(),
        AlreadySatisfied { role: OrganizationRole::Admin } => "Organization owner access was verified and preserved.".into(),
        Aborted { .. } => "Delivery was stopped before completion.".into(),
        Failed { .. } => "Delivery failed. Ask an organization owner to review this request.".into(),
    }
}
fn team_status(outcome: &TeamCreateOutcome) -> String {
    use TeamCreateOutcome::*;
    match outcome {
        Planned => "Team delivery planned. No GitHub outcome recorded yet.".into(),
        Submitted => "Team delivery submitted. No GitHub outcome recorded yet.".into(),
        Held { .. } => "Team delivery is on hold.".into(),
        Blocked { reason } => blocked(*reason).into(),
        Throttled { until, reason } => throttled(until, *reason),
        OutcomeUnknown => "Team outcome unknown. Verification is pending.".into(),
        AlreadySatisfied {
            state: MembershipState::Pending,
            ..
        } => "Existing team membership is pending acceptance; active access has not been verified."
            .into(),
        AlreadySatisfied {
            role: TeamRole::Member,
            state: MembershipState::Active,
        } => "Active team membership was verified.".into(),
        AlreadySatisfied {
            role: TeamRole::Maintainer,
            state: MembershipState::Active,
        } => "Active team maintainer access was verified and preserved.".into(),
        Created { .. } => {
            "Team invitation pending acceptance. Active team access has not been verified.".into()
        }
        Upgraded => {
            "Team membership was upgraded; check GitHub for the current access state.".into()
        }
        Aborted { .. } => "Team delivery was stopped before completion.".into(),
        Failed { .. } => {
            "Team delivery failed. Ask an organization owner to review this team.".into()
        }
    }
}

#[component]
pub fn OrganizationDelivery(
    view: OrganizationDeliveryView,
    #[props(default)] historical: bool,
) -> Element {
    rsx! {
        section { class: "space-y-3", aria_label: "Organization and team delivery",
            h2 { class: "font-semibold", if historical { "Recorded organization delivery history: {view.organization}" } else { "Organization: {view.organization}" } }
            p { class: "text-sm", "Projected delivery history may be delayed. Recorded access observations do not guarantee current membership." }
            if view.unavailable { p { role: "alert", "Organization delivery updates are unavailable. Any outcome below is retained history; missing status does not mean delivery failed." } }
            if let Some(generation) = view.generation { p { class: "text-sm", if historical { "Recorded delivery generation: {generation}" } else { "Delivery generation: {generation}" } } }
            p { "{view.status}" }
            crate::invitation_expiry::ExpiryNotice { expiry: view.expiry.clone() }
            ul { class: "space-y-2", aria_label: "Team delivery",
                for team in view.teams { li { h3 { class: "font-medium", "Team: {team.name}" } p { "{team.status}" } crate::invitation_expiry::ExpiryNotice { expiry: team.expiry.clone() } } }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ghinvite_core::{
        GithubInvitationId, InvitationLinkId, RequestId, org_delivery::OrganizationCreateCommand,
    };
    fn fixture(generation: u64) -> (Targets, OrganizationDeliverySnapshot) {
        let targets: Targets = serde_json::from_value(serde_json::json!({
            "organization":{"org_id":9001,"org_login":"acme","grant":"member"},
            "teams":[{"team_id":7,"team_slug":"learners","grant":"member"}],"repos":[]
        }))
        .unwrap();
        let command = OrganizationCreateCommand {
            batch: None,
            budget_attribution: Default::default(),
            access_lifetime_seconds: None,
            invitation_id: Some(GithubInvitationId::new()),
            link_id: Some(InvitationLinkId::new()),
            request_id: RequestId::new(),
            approval_id: "approval".into(),
            account_id: 9001,
            installation_id: 88,
            requester_id: 42,
            organization: targets.organization.clone().unwrap(),
            teams: targets.teams.clone(),
            delivery_generation: generation,
            approved_at: chrono::Utc::now(),
            budget_share_pct: 50,
        };
        (
            targets,
            OrganizationDeliverySnapshot::planned(command).unwrap(),
        )
    }
    #[test]
    fn submission_and_missing_projection_never_claim_membership() {
        let (targets, _) = fixture(1);
        let progress = OrganizationProgress {
            delivery_generation: 1,
            org_id: 9001,
            team_ids: vec![7],
            stage: DispatchStage::Submitted,
        };
        let view = presentation(&targets, &[], Some(&progress), false).unwrap();
        assert!(view.status.contains("submitted"));
        assert!(view.status.contains("No GitHub outcome"));
        assert!(
            view.teams[0]
                .status
                .contains("does not establish team access")
        );
        let (_, mut old) = fixture(1);
        old.create.outcome = OrganizationCreateOutcome::AlreadySatisfied {
            role: OrganizationRole::Admin,
        };
        let next = OrganizationProgress {
            delivery_generation: 2,
            ..progress.clone()
        };
        let lagged = presentation(&targets, &[old], Some(&next), false).unwrap();
        assert_eq!(lagged.generation, Some(2));
        assert!(lagged.status.contains("No GitHub outcome"));
        assert!(!lagged.status.contains("owner access was verified"));
        let wrong = OrganizationProgress {
            org_id: 9002,
            ..progress
        };
        assert!(
            presentation(&targets, &[], Some(&wrong), false)
                .unwrap()
                .status
                .starts_with("No projected")
        );
    }
    #[test]
    fn latest_generation_and_independent_team_receipts_preserve_uncertainty() {
        let (targets, mut old) = fixture(1);
        old.create.outcome = OrganizationCreateOutcome::AlreadySatisfied {
            role: OrganizationRole::Admin,
        };
        let mut current = old.clone();
        current.create.command.delivery_generation = 2;
        current.create.outcome = OrganizationCreateOutcome::OutcomeUnknown;
        current.teams.get_mut(&7).unwrap().create.outcome = TeamCreateOutcome::AlreadySatisfied {
            role: TeamRole::Maintainer,
            state: MembershipState::Pending,
        };
        let view = presentation(&targets, &[current.clone(), old], None, true).unwrap();
        assert_eq!(view.generation, Some(2));
        assert!(view.status.starts_with("Outcome unknown"));
        assert!(
            view.teams[0]
                .status
                .contains("active access has not been verified")
        );
        let html =
            crate::testing::render(move || rsx! { OrganizationDelivery { view: view.clone() } });
        assert!(html.contains("updates are unavailable"));
        assert!(!html.contains("owner access was verified"));
        current.create.command.teams.clear();
        assert_eq!(
            presentation(&targets, &[current], None, false)
                .unwrap()
                .generation,
            None
        );
    }
    #[test]
    fn foreign_pending_higher_roles_and_positive_policy_reasons_remain_distinct() {
        let (targets, mut snapshot) = fixture(1);
        snapshot.create.outcome = OrganizationCreateOutcome::AlreadyPending {
            upstream_id: 3,
            role: ghinvite_core::org_delivery::OrganizationInvitationRole::Admin,
        };
        snapshot.teams.get_mut(&7).unwrap().create.outcome = TeamCreateOutcome::AlreadySatisfied {
            role: TeamRole::Maintainer,
            state: MembershipState::Active,
        };
        let view = presentation(&targets, &[snapshot.clone()], None, false).unwrap();
        assert!(view.status.contains("without claiming ownership"));
        assert!(
            view.teams[0]
                .status
                .contains("maintainer access was verified and preserved")
        );
        snapshot.create.outcome = OrganizationCreateOutcome::Blocked {
            reason: OrganizationBlockedReason::NoSeat,
        };
        snapshot.teams.get_mut(&7).unwrap().create.outcome = TeamCreateOutcome::Blocked {
            reason: OrganizationBlockedReason::IdpSyncedTeam,
        };
        let view = presentation(&targets, &[snapshot], None, false).unwrap();
        assert!(view.status.contains("no organization seat"));
        assert!(view.teams[0].status.contains("identity provider"));
    }
    #[test]
    fn expiry_shared_team_requires_exact_invitation_and_independent_membership_has_no_clock() {
        use crate::invitation_expiry::InvitationExpiry;
        use ghinvite_core::delivery::InvitationTiming;
        let (targets, mut snapshot) = fixture(1);
        let at = snapshot.create.command.approved_at;
        snapshot.create.outcome = OrganizationCreateOutcome::Created {
            upstream_id: 99,
            created_at: at,
        };
        snapshot.create.confirmed_at = Some(at);
        let mut timing = InvitationTiming::observed(99, at, at, at, true).unwrap();
        timing.owned_lifetime_seconds = Some(3600);
        snapshot.invitation_timing = Some(timing);
        snapshot.teams.get_mut(&7).unwrap().create.outcome = TeamCreateOutcome::Created {
            via: ghinvite_core::org_delivery::TeamCreation::OrganizationInvitation {
                upstream_id: 99,
            },
        };
        let view = presentation(&targets, &[snapshot.clone()], None, false).unwrap();
        assert_eq!(
            view.expiry,
            InvitationExpiry::Known(at + chrono::Duration::hours(1))
        );
        assert_eq!(view.teams[0].expiry, view.expiry);
        snapshot.teams.get_mut(&7).unwrap().create.outcome = TeamCreateOutcome::Created {
            via: ghinvite_core::org_delivery::TeamCreation::OrganizationInvitation {
                upstream_id: 100,
            },
        };
        assert_eq!(
            presentation(&targets, &[snapshot.clone()], None, false)
                .unwrap()
                .teams[0]
                .expiry,
            InvitationExpiry::Unavailable
        );
        snapshot.teams.get_mut(&7).unwrap().create.outcome = TeamCreateOutcome::Created {
            via: ghinvite_core::org_delivery::TeamCreation::PendingMembership,
        };
        assert_eq!(
            presentation(&targets, &[snapshot.clone()], None, false)
                .unwrap()
                .teams[0]
                .expiry,
            InvitationExpiry::Unavailable
        );
        snapshot.invitation_timing = None;
        assert_eq!(
            presentation(&targets, &[snapshot], None, false)
                .unwrap()
                .expiry,
            InvitationExpiry::Unavailable
        );
    }
}
