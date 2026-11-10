//! Advisory reads for an already authorized request. Identity failures are
//! unavailable evidence, never evidence of absent access or approval authority.
use super::{
    bulk_review::{PreviewQuery, PreviewReader},
    *,
};
use chrono::{DateTime, Utc};
use ghinvite_core::bulk_preview::{ObservedAccess, PreviewAction, PreviewTarget, TargetPreview};

#[derive(Clone)]
pub(crate) struct RequestPreview {
    pub targets: Vec<TargetPreview>,
    pub observed_at: Option<DateTime<Utc>>,
}

impl AuthorityView {
    pub(crate) async fn request_preview(
        &self,
        targets: &Targets,
        requester_id: u64,
        login: &str,
    ) -> RequestPreview {
        let mut result = RequestPreview {
            targets: vec![],
            observed_at: None,
        };
        // This view and its clones share the authority request's bounded read
        // counter. In particular, a preview cannot reset the 50-read allowance.
        let reader = PreviewReader { authority: self };
        let valid = targets.validate().is_ok()
            && requester_id > 0
            && requester_id <= ghinvite_core::storage::integer::MAX_EXACT
            && super::bulk_review::valid_login(login);
        for repo in &targets.repos {
            let target = PreviewTarget::Repository {
                repo_id: repo.repo_id,
            };
            let verified = async {
                let installation = self.repo.as_ref().filter(|i| !unavailable(i))?;
                let user_at = reader
                    .verify_user(installation, requester_id, login)
                    .await?;
                let read = reader
                    .read(
                        installation,
                        PreviewQuery::RepositoryIdentity {
                            full_name: repo.repo_full_name.clone(),
                        },
                    )
                    .await
                    .ok()?;
                let identity: reads::RepositoryIdentity =
                    serde_json::from_value(read.value?).ok()?;
                if identity.id != repo.repo_id
                    || identity.owner.id != self.account.account_id
                    || !identity
                        .full_name
                        .eq_ignore_ascii_case(&repo.repo_full_name)
                {
                    return None;
                }
                Some(user_at.min(read.observed_at))
            };
            let evidence = if valid { verified.await } else { None };
            let subset = Targets {
                repos: vec![repo.clone()],
                ..Default::default()
            };
            reader
                .append_preview(&mut result, target, &subset, requester_id, login, evidence)
                .await;
        }
        if let Some(org) = &targets.organization {
            let verified = async {
                let installation = self.org.as_ref().filter(|i| !unavailable(i))?;
                if org.org_id != self.account.account_id {
                    return None;
                }
                let user_at = reader
                    .verify_user(installation, requester_id, login)
                    .await?;
                let read = reader
                    .read(
                        installation,
                        PreviewQuery::OrganizationIdentity {
                            org: org.org_login.clone(),
                        },
                    )
                    .await
                    .ok()?;
                let identity: ghinvite_github::payloads::GhUser =
                    serde_json::from_value(read.value?).ok()?;
                if identity.id != org.org_id || !identity.login.eq_ignore_ascii_case(&org.org_login)
                {
                    return None;
                }
                Some(user_at.min(read.observed_at))
            };
            let evidence = if valid { verified.await } else { None };
            let subset = Targets {
                organization: Some(org.clone()),
                ..Default::default()
            };
            reader
                .append_preview(
                    &mut result,
                    PreviewTarget::Organization { org_id: org.org_id },
                    &subset,
                    requester_id,
                    login,
                    evidence,
                )
                .await;
            for team in &targets.teams {
                let verified = async {
                    let at = evidence?;
                    let installation = self.org.as_ref()?;
                    let read = reader
                        .read(
                            installation,
                            PreviewQuery::TeamIdentity {
                                org: org.org_login.clone(),
                                slug: team.team_slug.clone(),
                            },
                        )
                        .await
                        .ok()?;
                    let identity: endpoints::authority_teams::TeamIdentity =
                        serde_json::from_value(read.value?).ok()?;
                    if identity.team.id != team.team_id
                        || identity.organization.id != org.org_id
                        || !identity.team.slug.eq_ignore_ascii_case(&team.team_slug)
                    {
                        return None;
                    }
                    Some(at.min(read.observed_at))
                }
                .await;
                let subset = Targets {
                    organization: Some(org.clone()),
                    teams: vec![team.clone()],
                    ..Default::default()
                };
                reader
                    .append_preview(
                        &mut result,
                        PreviewTarget::Team {
                            team_id: team.team_id,
                        },
                        &subset,
                        requester_id,
                        login,
                        verified,
                    )
                    .await;
            }
        }
        if result.observed_at.is_some_and(|at| {
            let age = Utc::now().signed_duration_since(at).num_seconds();
            !(0..crate::authority_cache::TTL_SECONDS).contains(&age)
        }) {
            // An unusually slow sequence must not present expired composite
            // identity evidence as a fresh role observation.
            for row in &mut result.targets {
                row.observed = ObservedAccess::Unavailable;
                row.action = PreviewAction::Unavailable;
            }
            result.observed_at = None;
        }
        result
    }
}

impl PreviewReader<'_> {
    async fn verify_user(
        &self,
        installation: &Installation,
        id: u64,
        login: &str,
    ) -> Option<DateTime<Utc>> {
        let read = self
            .read(
                installation,
                PreviewQuery::User {
                    login: login.into(),
                },
            )
            .await
            .ok()?;
        let user: ghinvite_github::payloads::GhUser = serde_json::from_value(read.value?).ok()?;
        (user.id == id && user.login.eq_ignore_ascii_case(login)).then_some(read.observed_at)
    }
    async fn append_preview(
        &self,
        result: &mut RequestPreview,
        target: PreviewTarget,
        targets: &Targets,
        id: u64,
        login: &str,
        verified: Option<DateTime<Utc>>,
    ) {
        let mut preview = TargetPreview {
            target: target.clone(),
            observed: ObservedAccess::Unavailable,
            action: PreviewAction::Unavailable,
        };
        if let Some(verified) = verified {
            let (rows, at) = self.previews(targets, id, login, Some(&target)).await;
            if let Some(row) = rows
                .into_iter()
                .find(|r| r.target == target && r.validate(targets).is_ok())
            {
                if row.observed != ObservedAccess::Unavailable {
                    let at = verified.min(at);
                    result.observed_at = Some(result.observed_at.map_or(at, |old| old.min(at)));
                }
                preview = row;
            }
        }
        result.targets.push(preview);
    }
}
