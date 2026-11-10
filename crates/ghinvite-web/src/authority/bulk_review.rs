//! Read-only bulk resolution and advisory per-target review. No reservation or
//! delivery command is sent here; every uncached GitHub read uses the authority
//! read-only broker, cache partition and existing edge/read budget.
use super::*;
use crate::RestateClient;
use chrono::{DateTime, Utc};
use ghinvite_core::{
    budget::{BucketId, BudgetSnapshot},
    bulk::{Entry, Manifest, Resolution, RowEvidence},
    bulk_preview::{
        MembershipRole, MembershipState, ObservedAccess, PreviewAction, PreviewTarget,
        TargetPreview,
    },
    invitation_link::AccountAdmin,
    targets::EffectiveRole,
};
use sha2::{Digest, Sha256};
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ReviewError {
    #[error("The batch is invalid.")]
    Invalid,
    #[error("Not found.")]
    Denied,
    #[error("Review is temporarily unavailable. Try again shortly.")]
    Unavailable,
}
pub struct BulkReviewService {
    authority: AuthorityView,
    restate: Arc<RestateClient>,
}
#[derive(Clone, Serialize)]
#[serde(tag = "kind")]
pub(super) enum PreviewQuery {
    RepositoryIdentity {
        full_name: String,
    },
    OrganizationIdentity {
        org: String,
    },
    TeamIdentity {
        org: String,
        slug: String,
    },
    User {
        login: String,
    },
    Repo {
        repo_id: u64,
        full_name: String,
        login: String,
        user_id: u64,
    },
    Org {
        org: String,
        login: String,
        user_id: u64,
        org_id: u64,
    },
    Team {
        org_id: u64,
        team_id: u64,
        user_id: u64,
        org: String,
        slug: String,
        login: String,
    },
}
#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Read {
    pub(super) observed_at: DateTime<Utc>,
    pub(super) value: Option<serde_json::Value>,
}
impl BulkReviewService {
    pub fn new(authority: AuthorityView, restate: Arc<RestateClient>) -> Self {
        Self { authority, restate }
    }
    pub async fn review(
        &self,
        manifest: Manifest,
        admin: &AccountAdmin,
    ) -> Result<Vec<RowEvidence>, ReviewError> {
        // Normalization enforces the 200-row bound before any lookup or budget RPC.
        let manifest = manifest.normalize().map_err(|_| ReviewError::Invalid)?;
        let view = &self.authority;
        if admin.account_id != view.account.account_id
            || admin.user_id != view.user_id
            || manifest.admin.account_id != admin.account_id
        {
            return Err(ReviewError::Denied);
        }
        if view
            .creation_installations(&manifest.targets)
            .map_err(|_| ReviewError::Denied)?
            != (
                manifest.installation_id,
                manifest.organization_installation_id,
            )
        {
            return Err(ReviewError::Denied);
        }
        if view
            .can_create_targets(&manifest.targets)
            .await
            .map_err(|_| ReviewError::Unavailable)?
            != Access::Allowed
        {
            return Err(ReviewError::Denied);
        }
        let budget = self.budget(&manifest).await?;
        let lookup_installation = if manifest.targets.repos.is_empty() {
            view.org.as_ref()
        } else {
            view.repo.as_ref()
        }
        .ok_or(ReviewError::Denied)?;
        let mut handles: BTreeMap<String, (Resolution, DateTime<Utc>)> = BTreeMap::new();
        let mut users: BTreeMap<u64, (Vec<TargetPreview>, DateTime<Utc>)> = BTreeMap::new();
        let mut rows = Vec::with_capacity(manifest.entries.len());
        for (index, entry) in manifest.entries.iter().enumerate() {
            let (resolution, mut observed_at) = match entry {
                Entry::Email { .. } => (Resolution::Email, Utc::now()),
                Entry::Handle { login } => {
                    let key = login.to_ascii_lowercase();
                    if let Some(old) = handles.get(&key) {
                        old.clone()
                    } else {
                        let reader = PreviewReader { authority: view };
                        let resolved = match reader
                            .read(
                                lookup_installation,
                                PreviewQuery::User {
                                    login: login.clone(),
                                },
                            )
                            .await
                        {
                            Ok(Read {
                                observed_at,
                                value: None,
                            }) => (Resolution::NotFound, observed_at),
                            Ok(Read {
                                observed_at,
                                value: Some(value),
                            }) => {
                                match serde_json::from_value::<ghinvite_github::payloads::GhUser>(
                                    value,
                                ) {
                                    Ok(user)
                                        if user.id > 0
                                            && user.id
                                                <= ghinvite_core::storage::integer::MAX_EXACT
                                            && valid_login(&user.login) =>
                                    {
                                        (
                                            Resolution::Found {
                                                user_id: user.id,
                                                login: user.login,
                                            },
                                            observed_at,
                                        )
                                    }
                                    _ => (Resolution::Unavailable, observed_at),
                                }
                            }
                            Err(_) => (Resolution::Unavailable, Utc::now()),
                        };
                        handles.insert(key, resolved.clone());
                        resolved
                    }
                }
            };
            let previews = if let Resolution::Found { user_id, login } = &resolution {
                if let Some((previews, at)) = users.get(user_id) {
                    observed_at = observed_at.min(*at);
                    previews.clone()
                } else {
                    let (previews, at) = PreviewReader { authority: view }
                        .previews(&manifest.targets, *user_id, login, None)
                        .await;
                    observed_at = observed_at.min(at);
                    users.insert(*user_id, (previews.clone(), at));
                    previews
                }
            } else {
                Vec::new()
            };
            let all_targets_satisfied = matches!(resolution, Resolution::Found { .. })
                && TargetPreview::complete(&previews, &manifest.targets)
                && previews
                    .iter()
                    .all(|p| p.action == PreviewAction::Satisfied);
            rows.push(RowEvidence {
                row: (index + 1) as u16,
                resolution,
                previews,
                all_targets_satisfied,
                observed_at,
                next_budget_slot: if all_targets_satisfied { None } else { budget },
            });
        }
        Ok(rows)
    }
    async fn budget(&self, manifest: &Manifest) -> Result<Option<DateTime<Utc>>, ReviewError> {
        let mut next = None;
        for app in [App::Repo, App::Org] {
            if (app == App::Repo && manifest.targets.repos.is_empty())
                || (app == App::Org && manifest.targets.organization.is_none())
            {
                continue;
            }
            let snapshot: BudgetSnapshot = self
                .restate
                .call(
                    "InviteBudget",
                    &format!("{}:{}", app.as_str(), manifest.admin.account_id),
                    "snapshot",
                    &(),
                )
                .await
                .map_err(|_| ReviewError::Unavailable)?;
            let now = Utc::now();
            if snapshot.app != app
                || snapshot.account_id != manifest.admin.account_id
                || snapshot.observed_at > now
                || now
                    .signed_duration_since(snapshot.observed_at)
                    .num_seconds()
                    > 60
            {
                return Err(ReviewError::Unavailable);
            }
            for at in snapshot
                .next_content_write_at
                .into_iter()
                .chain(snapshot.content_closed_until)
            {
                if at > now {
                    next = Some(next.map_or(at, |old: DateTime<Utc>| old.max(at)));
                }
            }
            for bucket in snapshot.buckets {
                let relevant = match bucket.bucket {
                    BucketId::Org => app == App::Org,
                    BucketId::Repo { repo_id } => {
                        app == App::Repo
                            && manifest.targets.repos.iter().any(|r| r.repo_id == repo_id)
                    }
                    BucketId::Content => true,
                };
                if relevant {
                    for at in bucket.next_slot_at.into_iter().chain(bucket.closed_until) {
                        if at > now {
                            next = Some(next.map_or(at, |old| old.max(at)));
                        }
                    }
                }
            }
        }
        Ok(next)
    }
}
pub(super) struct PreviewReader<'a> {
    pub(super) authority: &'a AuthorityView,
}
impl PreviewReader<'_> {
    pub(super) async fn read(
        &self,
        installation: &Installation,
        query: PreviewQuery,
    ) -> Result<Read, AuthorityError> {
        let fingerprint = hex::encode(Sha256::digest(
            serde_json::to_vec(&query).map_err(|_| AuthorityError::Unavailable)?,
        ));
        let key = format!(
            "authz:bulk:v1:{}:{}:{fingerprint}",
            installation.app.as_str(),
            installation.account.installation_id
        );
        if let Some(cached) = self.authority.cache_read(&key).await? {
            let read: Read = decode(cached)?;
            let age = Utc::now()
                .signed_duration_since(read.observed_at)
                .num_seconds();
            if (0..crate::authority_cache::TTL_SECONDS).contains(&age) {
                return Ok(read);
            }
        }
        let (app, scope) = self.authority.reader(installation)?;
        // Once, before the read: a 401 re-runs the closure.
        self.authority.reserve(installation).await?;
        let observed_at = Utc::now();
        let result = app
            .read(installation.account.installation_id, scope, |handle| {
                let query = query.clone();
                async move {
                    execute_preview(&handle.rest(), query)
                        .await
                        .map_err(CallError::<std::convert::Infallible>::Api)
                }
            })
            .await;
        let value = match result {
            Ok(value) => Some(value),
            Err(TokenCallError::Call(CallError::Api(ApiError::NotFound))) => None,
            Err(_) => return Err(AuthorityError::Unavailable),
        };
        let read = Read { observed_at, value };
        self.authority
            .cache_write(
                &key,
                serde_json::to_value(&read).map_err(|_| AuthorityError::Unavailable)?,
                observed_at.timestamp(),
            )
            .await?;
        Ok(read)
    }
    pub(super) async fn previews(
        &self,
        targets: &Targets,
        user_id: u64,
        login: &str,
        only: Option<&PreviewTarget>,
    ) -> (Vec<TargetPreview>, DateTime<Utc>) {
        let mut result = Vec::new();
        let mut at = Utc::now();
        let mut queries = Vec::new();
        if let Some(installation) = self.authority.repo.as_ref() {
            for repo in &targets.repos {
                queries.push((
                    installation,
                    PreviewTarget::Repository {
                        repo_id: repo.repo_id,
                    },
                    PreviewQuery::Repo {
                        repo_id: repo.repo_id,
                        full_name: repo.repo_full_name.clone(),
                        login: login.into(),
                        user_id,
                    },
                ));
            }
        }
        if let (Some(installation), Some(org)) =
            (self.authority.org.as_ref(), targets.organization.as_ref())
        {
            queries.push((
                installation,
                PreviewTarget::Organization { org_id: org.org_id },
                PreviewQuery::Org {
                    org: org.org_login.clone(),
                    org_id: org.org_id,
                    login: login.into(),
                    user_id,
                },
            ));
            for team in &targets.teams {
                queries.push((
                    installation,
                    PreviewTarget::Team {
                        team_id: team.team_id,
                    },
                    PreviewQuery::Team {
                        org_id: org.org_id,
                        team_id: team.team_id,
                        user_id,
                        org: org.org_login.clone(),
                        slug: team.team_slug.clone(),
                        login: login.into(),
                    },
                ));
            }
        }
        for (installation, target, query) in queries {
            if only.is_some_and(|selected| selected != &target) {
                continue;
            }
            let observed = match self.read(installation, query).await {
                Ok(read) => {
                    at = at.min(read.observed_at);
                    match read.value {
                        None => ObservedAccess::None,
                        Some(v) => serde_json::from_value(v).unwrap_or(ObservedAccess::Unavailable),
                    }
                }
                Err(_) => ObservedAccess::Unavailable,
            };
            let action = match &observed {
                ObservedAccess::None => PreviewAction::WillInvite,
                ObservedAccess::Unavailable => PreviewAction::Unavailable,
                ObservedAccess::Membership {
                    state: MembershipState::Pending,
                    ..
                } => PreviewAction::Pending,
                ObservedAccess::Membership { .. } => PreviewAction::Satisfied,
                ObservedAccess::Repository { role } => {
                    let grant = match target {
                        PreviewTarget::Repository { repo_id } => targets
                            .repos
                            .iter()
                            .find(|r| r.repo_id == repo_id)
                            .map(|r| r.grant),
                        _ => None,
                    };
                    match role {
                        EffectiveRole::Custom(_) => PreviewAction::CustomRole,
                        EffectiveRole::None => PreviewAction::WillInvite,
                        _ if grant.is_some_and(|g| role.satisfies(g)) => PreviewAction::Satisfied,
                        _ => PreviewAction::WillRaise,
                    }
                }
            };
            result.push(TargetPreview {
                target,
                observed,
                action,
            });
        }
        (result, at)
    }
}
pub(super) fn valid_login(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 100
        && !s
            .chars()
            .any(|c| c.is_control() || c.is_whitespace() || matches!(c, '/' | '\\' | '@'))
}
async fn execute_preview(
    rest: &Rest<'_>,
    query: PreviewQuery,
) -> Result<serde_json::Value, ApiError> {
    let invalid = || ApiError::Decode {
        what: "bulk preview",
    };
    let observed = match query {
        PreviewQuery::RepositoryIdentity { full_name } => {
            let (owner, name) = full_name.split_once('/').ok_or_else(invalid)?;
            return serde_json::to_value(
                endpoints::authority::repository_identity(
                    rest,
                    endpoints::Repository { owner, name },
                )
                .await?,
            )
            .map_err(|_| invalid());
        }
        PreviewQuery::OrganizationIdentity { org } => {
            let org = endpoints::organization::get_organization(rest, &org).await?;
            return Ok(serde_json::json!({"id": org.id, "login": org.login}));
        }
        PreviewQuery::TeamIdentity { org, slug } => {
            return serde_json::to_value(
                endpoints::authority_teams::team_identity(rest, &org, &slug).await?,
            )
            .map_err(|_| invalid());
        }
        PreviewQuery::User { login } => {
            return serde_json::to_value(endpoints::get_user_by_login(rest, &login).await?)
                .map_err(|_| invalid());
        }
        PreviewQuery::Repo {
            full_name,
            login,
            user_id,
            ..
        } => {
            let (owner, name) = full_name.split_once('/').ok_or_else(invalid)?;
            let permission = endpoints::collaborator_permission(
                rest,
                endpoints::Repository { owner, name },
                &login,
            )
            .await?;
            if permission.user.id != user_id {
                return Err(invalid());
            }
            let role = match permission.role {
                ghinvite_github::RepoRole::None => EffectiveRole::None,
                ghinvite_github::RepoRole::Read => EffectiveRole::Read,
                ghinvite_github::RepoRole::Triage => EffectiveRole::Triage,
                ghinvite_github::RepoRole::Write => EffectiveRole::Write,
                ghinvite_github::RepoRole::Maintain => EffectiveRole::Maintain,
                ghinvite_github::RepoRole::Admin => EffectiveRole::Admin,
                ghinvite_github::RepoRole::Custom(name) => EffectiveRole::Custom(
                    ghinvite_core::targets::CustomRoleName::new(
                        octoscrub::scrub(&name).into_owned(),
                    )
                    .map_err(|_| invalid())?,
                ),
                _ => return Err(invalid()),
            };
            ObservedAccess::Repository { role }
        }
        PreviewQuery::Org {
            org,
            login,
            user_id,
            org_id,
        } => {
            let membership =
                endpoints::organization::organization_membership(rest, &org, &login).await?;
            if membership.user.id != user_id || membership.organization.id != org_id {
                return Err(invalid());
            }
            ObservedAccess::Membership {
                state: membership_state(membership.state)?,
                role: match membership.role {
                    ghinvite_github::roles::OrgRole::Member => MembershipRole::Member,
                    ghinvite_github::roles::OrgRole::Admin => MembershipRole::Admin,
                    _ => return Err(invalid()),
                },
            }
        }
        PreviewQuery::Team {
            org, slug, login, ..
        } => {
            let membership =
                endpoints::organization::team_membership(rest, &org, &slug, &login).await?;
            ObservedAccess::Membership {
                state: membership_state(membership.state)?,
                role: match membership.role {
                    ghinvite_github::roles::TeamRole::Member => MembershipRole::Member,
                    ghinvite_github::roles::TeamRole::Maintainer => MembershipRole::Maintainer,
                    _ => return Err(invalid()),
                },
            }
        }
    };
    serde_json::to_value(observed).map_err(|_| invalid())
}
fn membership_state(
    state: ghinvite_github::roles::MembershipState,
) -> Result<MembershipState, ApiError> {
    Ok(match state {
        ghinvite_github::roles::MembershipState::Active => MembershipState::Active,
        ghinvite_github::roles::MembershipState::Pending => MembershipState::Pending,
        _ => {
            return Err(ApiError::Decode {
                what: "bulk preview membership",
            });
        }
    })
}
