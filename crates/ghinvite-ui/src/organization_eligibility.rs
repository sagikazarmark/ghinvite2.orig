//! Advisory facts only. Neither cached facts nor warnings grant authority.
use dioxus::prelude::*;
pub use ghinvite_core::org_eligibility::{BasePermission, Facts, Seats};
use ghinvite_core::{org_eligibility::Warning, targets::Targets};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub enum Eligibility {
    #[default]
    NotApplicable,
    Unknown,
    Known(Facts),
}

#[component]
pub fn Panel(facts: Eligibility, targets: Targets) -> Element {
    if matches!(facts, Eligibility::NotApplicable) {
        return rsx! {};
    }
    let (observed, details, warnings) = match &facts {
        Eligibility::Known(facts) => {
            let details = vec![
                facts
                    .seats
                    .map(|s| format!("Seats: {} of {} used.", s.used, s.capacity))
                    .unwrap_or_else(|| "Available seats: unknown.".into()),
                match facts.two_factor_required {
                    Some(true) => "Two-factor authentication: required.".into(),
                    Some(false) => {
                        "Two-factor authentication: not required by the organization.".into()
                    }
                    None => "Two-factor authentication requirement: unknown.".into(),
                },
                facts
                    .base_permission
                    .map(|p| {
                        format!(
                            "Default member repository access: {}.",
                            match p {
                                ghinvite_core::org_eligibility::BasePermission::None => "none",
                                ghinvite_core::org_eligibility::BasePermission::Read => "read",
                                ghinvite_core::org_eligibility::BasePermission::Write => "write",
                                ghinvite_core::org_eligibility::BasePermission::Admin => "admin",
                            }
                        )
                    })
                    .unwrap_or_else(|| "Default member repository access: unknown.".into()),
                match facts.managed_enterprise {
                    Some(true) => "Enterprise-managed users: enabled.".into(),
                    Some(false) => "Enterprise-managed users: not enabled.".into(),
                    None => "Enterprise-managed user policy: unknown.".into(),
                },
            ];
            (
                Some(facts.observed_at.to_rfc3339()),
                details,
                facts.warnings(&targets),
            )
        }
        _ => (
            None,
            vec!["Organization conditions are unknown. Reload to check again.".into()],
            vec![],
        ),
    };
    rsx! {
        section { class:"mac-panel p-4 space-y-2", aria_label:"Organization eligibility",
            h2 { class:"font-semibold", "Organization eligibility" }
            p { "These conditions may affect delivery. They do not prevent link creation or guarantee that access can be granted." }
            if let Some(observed) = observed {p {class:"text-sm","Last checked: {observed}. Conditions can change. Reload to check again."}}
            ul { for detail in details {li {"{detail}"}} }
            ul { class:"space-y-2", for warning in warnings {
                li { class:"text-warning", match warning {
                    Warning::NoSeat => rsx! {"No free organization seat was reported. Ask an organization owner to review capacity."},
                    Warning::TwoFactorRequired => rsx! {"Invitees need two-factor authentication to accept organization membership."},
                    Warning::ManagedEnterprise => rsx! {"Enterprise-managed users are unsupported. Review the organization identity policy."},
                    Warning::BasePermissionAlreadySufficient {repo_id} => {
                        let repo = targets.repos.iter().find(|r|r.repo_id==repo_id).map(|r|r.repo_full_name.as_str()).unwrap_or("Selected repository");
                        rsx! {"{repo}: existing organization members already have this access; this link only affects non-members. This does not establish whether a requester is a member."}
                    }
                }}
            }}
            p { class:"text-sm", "Outside-collaborator restrictions and team-management policies are checked when available. Unknown conditions may be discovered during delivery." }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ghinvite_core::{
        org_eligibility::{BasePermission, Seats},
        targets::{RepoRole, RepoTarget},
    };
    fn html(facts: Eligibility, targets: Targets) -> String {
        dioxus_ssr::render_element(rsx! {Panel {facts,targets}})
    }
    #[test]
    fn known_conditions_preserve_unknown_fields_and_follow_selected_grants() {
        let facts = Facts {
            account_id: 100,
            installation_id: 2,
            observed_at: "2026-05-04T12:00:00Z".parse().unwrap(),
            seats: Some(Seats {
                capacity: 10,
                used: 10,
            }),
            two_factor_required: Some(true),
            base_permission: Some(BasePermission::Read),
            managed_enterprise: None,
        };
        let targets = Targets {
            repos: vec![
                RepoTarget {
                    repo_id: 10,
                    repo_full_name: "acme/read".into(),
                    grant: RepoRole::Read,
                },
                RepoTarget {
                    repo_id: 11,
                    repo_full_name: "acme/write".into(),
                    grant: RepoRole::Write,
                },
            ],
            ..Default::default()
        };
        let rendered = html(Eligibility::Known(facts), targets);
        assert!(rendered.contains("acme/read: existing organization members"));
        assert!(!rendered.contains("acme/write: existing organization members"));
        assert!(rendered.contains("No free organization seat"));
        assert!(rendered.contains("Enterprise-managed user policy: unknown"));
        assert!(rendered.contains("does not establish whether a requester is a member"));
        assert!(!rendered.contains("disabled"));
    }
    #[test]
    fn unavailable_facts_never_render_a_healthy_verdict() {
        let rendered = html(Eligibility::Unknown, Targets::default());
        assert!(rendered.contains("Organization conditions are unknown"));
        assert!(!rendered.contains("No free organization seat"));
        assert_eq!(html(Eligibility::NotApplicable, Targets::default()), "");
    }
}
