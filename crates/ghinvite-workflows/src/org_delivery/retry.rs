//! Retry policy is separate from unverified GitHub error-text classification.
//! A typed positive capacity refusal is daily; EMU is unsupported and never
//! becomes an automatic read/write loop merely because it is nonterminal.
use super::*;
pub(super) const NO_SEAT_SECONDS: u64 = 24 * 60 * 60;
pub(super) fn unsupported(value: &Retained, step: &str) -> bool {
    if matches!(
        value.snapshot.create.outcome,
        OrganizationCreateOutcome::Blocked {
            reason: OrganizationBlockedReason::ManagedEnterprise
        }
    ) {
        return true;
    }
    step.strip_prefix("team:")
        .and_then(|id| id.parse::<u64>().ok())
        .and_then(|id| value.snapshot.teams.get(&id))
        .is_some_and(|team| {
            matches!(
                team.create.outcome,
                TeamCreateOutcome::Blocked {
                    reason: OrganizationBlockedReason::ManagedEnterprise
                }
            )
        })
}
pub(super) fn no_seat(value: &Retained) -> bool {
    matches!(
        value.snapshot.create.outcome,
        OrganizationCreateOutcome::Blocked {
            reason: OrganizationBlockedReason::NoSeat
        }
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    fn retained() -> Retained {
        let command = OrganizationCreateCommand {
            batch: None,
            budget_attribution: Default::default(),
            access_lifetime_seconds: None,
            invitation_id: Some(ghinvite_core::GithubInvitationId::new()),
            link_id: Some(ghinvite_core::InvitationLinkId::new()),
            request_id: ghinvite_core::RequestId::new(),
            approval_id: "approval".into(),
            account_id: 100,
            installation_id: 2,
            requester_id: 8,
            organization: ghinvite_core::targets::OrganizationTarget {
                org_id: 100,
                org_login: "acme".into(),
                grant: ghinvite_core::targets::MemberGrant::Member,
            },
            teams: vec![ghinvite_core::targets::TeamTarget {
                team_id: 5,
                team_slug: "team-5".into(),
                grant: ghinvite_core::targets::MemberGrant::Member,
            }],
            delivery_generation: 1,
            approved_at: Utc::now(),
            budget_share_pct: 50,
        };
        Retained {
            snapshot: OrganizationDeliverySnapshot::planned(command).unwrap(),
            steps: BTreeMap::new(),
            last_observed_at: None,
            retry_not_before: None,
            audit: None,
        }
    }
    #[test]
    fn unsupported_org_stops_dependent_steps_but_team_refusal_is_independent() {
        let mut value = retained();
        value.snapshot.create.outcome = OrganizationCreateOutcome::Blocked {
            reason: OrganizationBlockedReason::ManagedEnterprise,
        };
        assert!(!value.snapshot.needs_tracking());
        assert!(unsupported(&value, "org"));
        assert!(unsupported(&value, "team:5"));
        value.snapshot.create.outcome = OrganizationCreateOutcome::AlreadySatisfied {
            role: OrganizationRole::Member,
        };
        value.snapshot.teams.get_mut(&5).unwrap().create.outcome = TeamCreateOutcome::Blocked {
            reason: OrganizationBlockedReason::ManagedEnterprise,
        };
        assert!(!unsupported(&value, "org"));
        assert!(unsupported(&value, "team:5"));
        value.snapshot.create.outcome = OrganizationCreateOutcome::Blocked {
            reason: OrganizationBlockedReason::NoSeat,
        };
        assert!(no_seat(&value));
        assert!(!unsupported(&value, "org"));
        assert_eq!(NO_SEAT_SECONDS, 86400);
    }
}
