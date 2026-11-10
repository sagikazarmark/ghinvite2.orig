//! Informational, on-demand effective access. Never a decision or write grant.
use chrono::{DateTime, Utc};
use dioxus::prelude::*;
use ghinvite_core::{
    bulk_preview::{MembershipRole, MembershipState, ObservedAccess, PreviewTarget, TargetPreview},
    targets::{EffectiveRole, Targets},
};

fn observed_label(observed: &ObservedAccess) -> String {
    match observed {
        ObservedAccess::None
        | ObservedAccess::Repository {
            role: EffectiveRole::None,
        } => "No existing access observed".into(),
        ObservedAccess::Unavailable => "Unavailable — try again shortly".into(),
        ObservedAccess::Repository {
            role: EffectiveRole::Custom(name),
        } => format!(
            "Custom repository role: {} (comparison unavailable)",
            name.as_str()
        ),
        ObservedAccess::Repository { role } => format!(
            "Repository role: {}",
            match role {
                EffectiveRole::Read => "read",
                EffectiveRole::Triage => "triage",
                EffectiveRole::Write => "write",
                EffectiveRole::Maintain => "maintain",
                EffectiveRole::Admin => "admin",
                _ => "unavailable",
            }
        ),
        ObservedAccess::Membership { state, role } => format!(
            "{} membership · {}",
            match state {
                MembershipState::Active => "Active",
                MembershipState::Pending => "Pending",
            },
            match role {
                MembershipRole::Member => "member",
                MembershipRole::Admin => "owner",
                MembershipRole::Maintainer => "maintainer",
            }
        ),
    }
}

#[component]
pub fn RequestAccessPreview(
    targets: Targets,
    href: String,
    #[props(default)] previews: Option<Vec<TargetPreview>>,
    #[props(default)] observed_at: Option<DateTime<Utc>>,
) -> Element {
    let rows: Vec<_> = targets
        .repos
        .iter()
        .map(|r| {
            (
                PreviewTarget::Repository { repo_id: r.repo_id },
                r.repo_full_name.clone(),
            )
        })
        .chain(targets.organization.iter().map(|o| {
            (
                PreviewTarget::Organization { org_id: o.org_id },
                format!("Organization {}", o.org_login),
            )
        }))
        .chain(targets.teams.iter().map(|t| {
            (
                PreviewTarget::Team { team_id: t.team_id },
                format!("Team {}", t.team_slug),
            )
        }))
        .map(|(id, name)| {
            let label = match &previews {
                None => "Not checked yet".into(),
                Some(rows) => rows
                    .iter()
                    .find(|r| r.target == id)
                    .map(|r| observed_label(&r.observed))
                    .unwrap_or_else(|| "Unavailable — try again shortly".into()),
            };
            (name, label)
        })
        .collect();
    rsx! {
        section { class: "mac-panel p-4 mt-4 space-y-2", aria_label: "Effective access preview",
            h2 { class: "font-semibold", "Effective access preview" }
            p { "Informational only. GitHub access is cached for up to five minutes. Approval authorizes delivery; each delivery checks current access again before any write." }
            if let Some(at) = observed_at { p { class: "text-sm", "Oldest evidence checked: {at.to_rfc3339()}" } }
            ul { for (name, label) in rows { li { strong { "{name}: " } "{label}" } } }
            a { class: "link", href, if previews.is_some() { "Check effective access again" } else { "Preview effective access" } }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ghinvite_core::{
        bulk_preview::PreviewAction,
        targets::{MemberGrant, OrganizationTarget, RepoRole, RepoTarget, TeamTarget},
    };
    fn targets() -> Targets {
        Targets {
            repos: vec![RepoTarget {
                repo_id: 1,
                repo_full_name: "acme/api".into(),
                grant: RepoRole::Write,
            }],
            organization: Some(OrganizationTarget {
                org_id: 100,
                org_login: "acme".into(),
                grant: MemberGrant::Member,
            }),
            teams: vec![TeamTarget {
                team_id: 5,
                team_slug: "build".into(),
                grant: MemberGrant::Member,
            }],
        }
    }
    #[test]
    fn ordinary_preview_preserves_each_target_and_requires_explicit_read() {
        let html = crate::testing::render(
            || rsx! { RequestAccessPreview { targets: targets(), href: "/request?preview=1" } },
        );
        assert_eq!(html.matches("Not checked yet").count(), 3);
        assert!(html.contains("acme/api"));
        assert!(html.contains("Organization acme"));
        assert!(html.contains("Team build"));
        assert!(html.contains("href=\"/request?preview=1\""));
        assert!(html.contains("each delivery checks current access again before any write"));
    }
    #[test]
    fn preview_keeps_pending_custom_and_unavailable_distinct() {
        let html = crate::testing::render(|| {
            rsx! { RequestAccessPreview {
                targets: targets(), href: "/request?preview=1", observed_at: "2026-01-01T00:00:00Z".parse::<DateTime<Utc>>().unwrap(),
                previews: vec![
                    TargetPreview { target:PreviewTarget::Repository{repo_id:1}, observed:ObservedAccess::Repository{role:EffectiveRole::Custom(ghinvite_core::targets::CustomRoleName::new("release").unwrap())},action:PreviewAction::CustomRole },
                    TargetPreview { target:PreviewTarget::Team{team_id:5}, observed:ObservedAccess::Membership{state:MembershipState::Pending,role:MembershipRole::Maintainer},action:PreviewAction::Pending },
                ],
            } }
        });
        assert!(html.contains("Custom repository role: release (comparison unavailable)"));
        assert!(html.contains("Pending membership · maintainer"));
        assert!(html.contains("Unavailable"));
        assert!(html.contains("Oldest evidence checked:"));
        assert!(!html.contains("No existing access observed"));
    }
}
