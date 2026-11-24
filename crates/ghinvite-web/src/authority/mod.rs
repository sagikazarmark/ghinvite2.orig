//! Request-local Console authority, derived only from installation credentials.
//! A candidate delegated view carries no account-wide grant. Only its checked
//! scope or an Allowed target decision may authorize a protected response.
use crate::{
    authority_cache::{AuthorityCache, CacheEntry},
    rate_limit::{Bucket, EdgeRateLimiter},
    state::{AppState, WebStorage},
};
use ghinvite_core::{
    account::{Account, AccountType, App, Installation},
    storage::console_scope::{ConsoleScope, RepoTarget},
    targets::Targets,
};
use ghinvite_github::{
    GithubApp, GithubApps, TokenCallError,
    endpoints::{self, authority as reads},
    error::{ApiError, CallError},
    profiles::{CONSOLE_ORG_READ, CONSOLE_REPO_READ},
    rest::Rest,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex},
};

pub mod bulk_review;
mod organization;
mod request_preview;

const MAX_READS: usize = 50;
const MAX_REPOSITORIES: usize = 5000;
use ghinvite_ui::link_form::{
    DELEGATED_SEARCH_LIMIT, REPOSITORY_PICKER_PAGE_SIZE, RepositoryPicker,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    PersonalOwner,
    InstallingOwner,
    AccountAdmin,
    Delegated,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    Allowed,
    Denied,
    Pending,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum AuthorityError {
    #[error("Repository selection is invalid.")]
    InvalidSelection,
    #[error("Not found.")]
    Denied,
    #[error("Finishing setup. Try again shortly.")]
    PendingSetup,
    #[error("Authority is temporarily unavailable.")]
    Unavailable,
    #[error("Authority configuration is unavailable.")]
    Configuration,
    #[error("GitHub is busy. Try again shortly.")]
    Busy,
}
pub struct ScopeResult {
    pub scope: ConsoleScope,
    pub pending: bool,
    pub authorized_repositories: usize,
}
#[derive(Clone, Debug)]
pub struct RepositoryChoices {
    pub repositories: Vec<RepoTarget>,
    pub pending: bool,
}
/// One page of the create form's repository picker. Display evidence only:
/// nothing here authorizes a selection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RepositoryWindow {
    /// The submitted selection the installation still exposes, in
    /// installation order. Unknown ids are dropped, never substituted.
    pub selected: Vec<RepoTarget>,
    /// This page's matches, in installation order.
    pub listed: Vec<RepoTarget>,
    /// Matches across every page.
    pub matches: usize,
    /// The 1-based page listed, clamped to `1..=pages`.
    pub page: usize,
    pub pages: usize,
    /// A delegated caller must search before anything is listed.
    pub search_required: bool,
    /// A delegated search matched more than it may list.
    pub truncated: bool,
    pub pending: bool,
}
#[derive(Default)]
struct ViewState {
    reads: usize,
    memo: BTreeMap<String, CacheEntry>,
}
#[derive(Clone)]
pub struct AuthorityView {
    organization_delivery_enabled: bool,
    storage: Arc<dyn WebStorage>,
    cache: Arc<dyn AuthorityCache>,
    limiter: Arc<dyn EdgeRateLimiter>,
    apps: GithubApps,
    account: Account,
    repo: Option<Installation>,
    org: Option<Installation>,
    user_id: u64,
    login: String,
    role: Role,
    state: Arc<Mutex<ViewState>>,
}
impl std::fmt::Debug for AuthorityView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthorityView")
            .field("role", &self.role)
            .finish_non_exhaustive()
    }
}

#[derive(Clone)]
enum Query {
    Identity(String),
    Permission(String, String),
    Membership(String, String),
    Page(Option<reads::RepositoryCursor>),
    Organization(String),
    Teams(String, Option<endpoints::authority_teams::TeamCursor>),
    Team(String, String),
}
impl AuthorityView {
    pub async fn open(
        state: &AppState,
        user_id: u64,
        login: &str,
        account: &Account,
    ) -> Result<Option<Self>, AuthorityError> {
        if user_id == 0 || account.account_id == 0 || login.is_empty() {
            return Ok(None);
        }
        let repo = state
            .storage
            .active_app_installation(App::Repo, account.account_id)
            .await
            .map_err(|_| AuthorityError::Unavailable)?;
        let org = state
            .storage
            .active_app_installation(App::Org, account.account_id)
            .await
            .map_err(|_| AuthorityError::Unavailable)?;
        if repo.is_none()
            && org.is_none()
            && !(account.account_type == AccountType::User && user_id == account.account_id)
        {
            return Ok(None);
        }
        for installation in repo.iter().chain(org.iter()) {
            if installation.account.account_id != account.account_id
                || installation.account.account_type != account.account_type
                || !installation
                    .account
                    .account_login
                    .eq_ignore_ascii_case(&account.account_login)
            {
                return Err(AuthorityError::Unavailable);
            }
        }
        if state.github_apps.slots().next().is_none() {
            return Err(AuthorityError::Configuration);
        }
        for installation in repo.iter().chain(org.iter()) {
            if crate::installed_apps::configured(state, installation.app).is_none() {
                return Err(AuthorityError::Configuration);
            }
        }
        let mut view = Self {
            organization_delivery_enabled: state.config.organization_delivery_enabled,
            storage: state.storage.clone(),
            cache: state.authority_cache.clone(),
            limiter: state.edge_limits.clone(),
            apps: state.github_apps.clone(),
            account: account.clone(),
            repo,
            org,
            user_id,
            login: login.to_owned(),
            role: Role::Delegated,
            state: Arc::new(Mutex::new(ViewState::default())),
        };
        if account.account_type == AccountType::User {
            if user_id != account.account_id {
                return Ok(None);
            }
            if view.repo.as_ref().is_some_and(unavailable) {
                return Err(AuthorityError::Unavailable);
            }
            view.role = Role::PersonalOwner;
            return Ok(Some(view));
        }
        if let Some(org) = &view.org {
            if unavailable(org) {
                return Err(AuthorityError::Unavailable);
            }
            let key = format!(
                "authz:org:{}:{}:membership",
                org.account.installation_id, user_id
            );
            let value = view
                .cached_query(
                    &key,
                    org,
                    Query::Membership(account.account_login.clone(), login.to_owned()),
                )
                .await?;
            if let Some(value) = value {
                let membership: reads::Membership = decode(value)?;
                if membership.user.id != user_id || membership.organization.id != account.account_id
                {
                    return Ok(None);
                }
                // A pending owner invitation never becomes a delegation grant.
                if membership.state != "active" {
                    return Ok(None);
                }
                if membership.role == "admin" {
                    view.role = Role::AccountAdmin;
                    return Ok(Some(view));
                }
            }
        } else if let Some(repo) = &view.repo {
            if unavailable(repo) {
                return Err(AuthorityError::Unavailable);
            }
            let Some(installer) = repo.installed_by_user_id else {
                return Err(AuthorityError::PendingSetup);
            };
            if installer == user_id {
                view.role = Role::InstallingOwner;
                return Ok(Some(view));
            }
        }
        let delegation = view
            .storage
            .account_settings(account.account_id)
            .await
            .map_err(|_| AuthorityError::Unavailable)?
            .is_some_and(|settings| settings.repo_admin_delegation);
        if delegation && view.repo.as_ref().is_some_and(|repo| !unavailable(repo)) {
            // OQ-11 is an installation-size limit, independent of how many
            // repositories happen to occur in projected invitation links.
            if view.delegation_size_supported().await? {
                Ok(Some(view))
            } else {
                Ok(None)
            }
        } else {
            Ok(None)
        }
    }
    /// One bounded metadata page establishes the installation cardinality. It
    /// shares the picker's five-minute cache but never follows its next cursor.
    /// Neither the repository rows nor the count grant per-target authority.
    async fn delegation_size_supported(&self) -> Result<bool, AuthorityError> {
        let installation = self.repo.as_ref().ok_or(AuthorityError::Unavailable)?;
        let key = format!(
            "authz:repo:{}:0:page-0",
            installation.account.installation_id
        );
        let value = self
            .cached_query(&key, installation, Query::Page(None))
            .await?
            .ok_or(AuthorityError::Unavailable)?;
        let page: reads::RepositoryPage = decode(value)?;
        let count = page.repositories.len() as u64;
        if count > 100
            || count > page.total_count
            || (page.next.is_none() && count != page.total_count)
            || (page.next.is_some() && (count == 0 || count == page.total_count))
        {
            return Err(AuthorityError::Unavailable);
        }
        let mut ids = BTreeSet::new();
        for repo in &page.repositories {
            if repo.id == 0
                || !ids.insert(repo.id)
                || repo.owner.id != self.account.account_id
                || !repo.full_name.split_once('/').is_some_and(|(owner, name)| {
                    owner.eq_ignore_ascii_case(&self.account.account_login)
                        && !name.is_empty()
                        && !name.contains('/')
                })
            {
                return Err(AuthorityError::Unavailable);
            }
        }
        Ok(page.total_count <= MAX_REPOSITORIES as u64)
    }
    pub fn role(&self) -> Role {
        self.role
    }
    pub fn is_owner(&self) -> bool {
        self.role != Role::Delegated
    }
    /// Display-only canonical installation state; not permission evidence.
    /// None means no active installation, Some(true) means suspended.
    pub fn installation_status(&self, app: App) -> Option<bool> {
        match app {
            App::Repo => self.repo.as_ref(),
            App::Org => self.org.as_ref(),
        }
        .map(|installation| installation.suspended_at.is_some())
    }
    pub fn has_repository_installation(&self) -> bool {
        self.repo.is_some()
    }
    pub fn account_id(&self) -> u64 {
        self.account.account_id
    }

    pub async fn can_read_link(&self, targets: &Targets) -> Result<Access, AuthorityError> {
        if targets.organization.is_some() {
            return self.organization_access(targets, false).await;
        }
        if self.is_owner() {
            return Ok(Access::Allowed);
        }
        self.check_targets(targets, true).await
    }
    /// Authority over an existing, account-bound link. Installation selection
    /// controls new creation, not the ability to approve history or stop links.
    /// A delivery independently checks whether its installation/target is usable.
    pub async fn can_mutate_targets(&self, targets: &Targets) -> Result<Access, AuthorityError> {
        if targets.validate().is_err() {
            return Ok(Access::Denied);
        }
        if targets.organization.is_some() {
            if self.organization_access(targets, false).await? != Access::Allowed {
                return Ok(Access::Denied);
            }
            if targets.repos.is_empty() {
                return Ok(Access::Allowed);
            }
        }
        if matches!(self.role, Role::PersonalOwner | Role::AccountAdmin) {
            return Ok(Access::Allowed);
        }
        self.check_targets(targets, true).await
    }
    /// New creation requires current installation selection in addition to the
    /// authority checks used for existing links.
    pub async fn can_create_targets(&self, targets: &Targets) -> Result<Access, AuthorityError> {
        if targets.validate().is_err() {
            return Ok(Access::Denied);
        }
        if targets.organization.is_some() {
            let access = self.organization_access(targets, true).await?;
            if access != Access::Allowed {
                return Ok(access);
            }
            if targets.repos.is_empty() {
                return Ok(Access::Allowed);
            }
        }
        if self.repo.as_ref().is_none_or(unavailable) {
            return Ok(Access::Denied);
        }
        let ids: Vec<_> = targets.repos.iter().map(|repo| repo.repo_id).collect();
        let selected = match self.repositories_by_ids(&ids).await {
            Err(AuthorityError::Busy) => return Ok(Access::Pending),
            Err(AuthorityError::Denied | AuthorityError::InvalidSelection) => {
                return Ok(Access::Denied);
            }
            other => other?,
        };
        Ok(
            if targets.repos.iter().all(|target| {
                selected.iter().any(|repo| {
                    repo.id == target.repo_id
                        && repo.full_name.eq_ignore_ascii_case(&target.repo_full_name)
                })
            }) {
                Access::Allowed
            } else {
                Access::Denied
            },
        )
    }
    async fn check_targets(
        &self,
        targets: &Targets,
        require_admin: bool,
    ) -> Result<Access, AuthorityError> {
        if targets.validate().is_err()
            || targets.organization.is_some()
            || !targets.teams.is_empty()
            || targets.repos.is_empty()
        {
            return Ok(Access::Denied);
        }
        let mut pending = false;
        for target in &targets.repos {
            match self
                .repository_access(target.repo_id, &target.repo_full_name, require_admin)
                .await?
            {
                Access::Denied => return Ok(Access::Denied),
                Access::Pending => pending = true,
                Access::Allowed => {}
            }
        }
        Ok(if pending {
            Access::Pending
        } else {
            Access::Allowed
        })
    }
    pub async fn console_scope(&self) -> Result<ScopeResult, AuthorityError> {
        if self.is_owner() {
            return Ok(ScopeResult {
                scope: ConsoleScope::account(self.account.account_id)
                    .map_err(|_| AuthorityError::Unavailable)?,
                pending: false,
                authorized_repositories: 0,
            });
        }
        let mut after = None;
        let mut allowed = BTreeSet::new();
        let mut pending = false;
        let mut count = 0;
        loop {
            let page = self
                .storage
                .distinct_console_repo_targets(self.account.account_id, after, 501)
                .await
                .map_err(|_| AuthorityError::Unavailable)?;
            for target in page.rows {
                count += 1;
                if count > MAX_REPOSITORIES {
                    return Ok(ScopeResult {
                        scope: ConsoleScope::repo_admin(self.account.account_id, [])
                            .map_err(|_| AuthorityError::Unavailable)?,
                        pending: false,
                        authorized_repositories: 0,
                    });
                }
                match self
                    .repository_access(target.id, &target.full_name, true)
                    .await?
                {
                    Access::Allowed => {
                        allowed.insert(target.id);
                    }
                    Access::Denied => {}
                    Access::Pending => pending = true,
                }
            }
            match page.next {
                Some(next) if after.is_none_or(|previous| next > previous) => after = Some(next),
                None => break,
                _ => return Err(AuthorityError::Unavailable),
            }
        }
        Ok(ScopeResult {
            authorized_repositories: allowed.len(),
            scope: ConsoleScope::repo_admin(self.account.account_id, allowed)
                .map_err(|_| AuthorityError::Unavailable)?,
            pending,
        })
    }
    async fn repository_access(
        &self,
        id: u64,
        name: &str,
        admin: bool,
    ) -> Result<Access, AuthorityError> {
        let Some(installation) = self.repo.as_ref().filter(|i| !unavailable(i)) else {
            return Ok(Access::Denied);
        };
        let Some((owner, repo)) = name.split_once('/') else {
            return Ok(Access::Denied);
        };
        if id == 0
            || repo.is_empty()
            || repo.contains('/')
            || !owner.eq_ignore_ascii_case(&self.account.account_login)
        {
            return Ok(Access::Denied);
        };
        let key = format!(
            "authz:repo:{}:{}:{id}",
            installation.account.installation_id,
            if admin { self.user_id } else { 0 }
        );
        if let Some(value) = self.cache_read(&key).await? {
            let evidence: RepositoryEvidence = decode(value)?;
            if evidence.account == self.account.account_id
                && evidence.id == id
                && evidence.name.eq_ignore_ascii_case(name)
                && evidence.user == if admin { self.user_id } else { 0 }
            {
                return Ok(if evidence.allowed {
                    Access::Allowed
                } else {
                    Access::Denied
                });
            }
        }
        let observed_at = chrono::Utc::now().timestamp();
        let result = self
            .repository_evidence(installation, id, name, admin)
            .await;
        let allowed = match result {
            Err(AuthorityError::Busy) => return Ok(Access::Pending),
            other => other?,
        };
        self.cache_write(
            &key,
            serde_json::to_value(RepositoryEvidence {
                account: self.account.account_id,
                id,
                name: name.to_owned(),
                user: if admin { self.user_id } else { 0 },
                allowed,
            })
            .map_err(|_| AuthorityError::Unavailable)?,
            observed_at,
        )
        .await?;
        Ok(if allowed {
            Access::Allowed
        } else {
            Access::Denied
        })
    }
    async fn repository_evidence(
        &self,
        installation: &Installation,
        id: u64,
        name: &str,
        admin: bool,
    ) -> Result<bool, AuthorityError> {
        let Some(value) = self
            .query(installation, Query::Identity(name.to_owned()))
            .await?
        else {
            return Ok(false);
        };
        let identity: reads::RepositoryIdentity = decode(value)?;
        if identity.id != id
            || identity.owner.id != self.account.account_id
            || !identity.full_name.eq_ignore_ascii_case(name)
        {
            return Ok(false);
        };
        if !admin {
            return Ok(true);
        }
        let Some(value) = self
            .query(
                installation,
                Query::Permission(name.to_owned(), self.login.clone()),
            )
            .await?
        else {
            return Ok(false);
        };
        let evidence: PermissionEvidence = decode(value)?;
        Ok(evidence.user == self.user_id && evidence.admin)
    }
    /// Resolve posted numeric selections independently of the twenty-result picker.
    /// Denied selections are concealed by the HTTP adapter; exhaustion is
    /// an unavailable decision, never a shorter accepted selection.
    pub async fn repositories_by_ids(
        &self,
        ids: &[u64],
    ) -> Result<Vec<RepoTarget>, AuthorityError> {
        if ids.is_empty() || ids.len() > 100 || ids.contains(&0) {
            return Err(AuthorityError::InvalidSelection);
        }
        let choices = self.installation_repositories().await?;
        if choices.pending {
            return Err(AuthorityError::Busy);
        }
        let wanted: BTreeSet<_> = ids.iter().copied().collect();
        let selected: Vec<_> = choices
            .repositories
            .into_iter()
            .filter(|repo| wanted.contains(&repo.id))
            .collect();
        if selected.len() != wanted.len() {
            return Err(AuthorityError::InvalidSelection);
        }
        for repo in &selected {
            match self
                .repository_access(
                    repo.id,
                    &repo.full_name,
                    matches!(self.role, Role::Delegated | Role::InstallingOwner),
                )
                .await?
            {
                Access::Allowed => {}
                Access::Denied => return Err(AuthorityError::Denied),
                Access::Pending => return Err(AuthorityError::Busy),
            }
        }
        Ok(selected)
    }
    /// Delegated callers see at most twenty name matches, without permission
    /// reads. Selection and submission separately call can_mutate_targets.
    ///
    /// Failed-form recovery uses repositories_for_review to preserve selections
    /// outside the search window; neither method authorizes a mutation.
    pub async fn search_repositories(
        &self,
        query: &str,
    ) -> Result<RepositoryChoices, AuthorityError> {
        if self.role == Role::Delegated && query.trim().is_empty() {
            return Ok(RepositoryChoices {
                repositories: vec![],
                pending: false,
            });
        }
        let mut choices = self.installation_repositories().await?;
        let needle = query.trim().to_lowercase();
        choices
            .repositories
            .retain(|repo| repo.full_name.to_lowercase().contains(&needle));
        if self.role == Role::Delegated {
            choices.repositories.truncate(20)
        }
        Ok(choices)
    }
    /// Display evidence only: retain known submitted choices after validation
    /// fails. Missing IDs are not substituted, and no permission is inferred.
    pub async fn repositories_for_review(
        &self,
        ids: &[u64],
    ) -> Result<RepositoryChoices, AuthorityError> {
        if ids.len() > 100 {
            return Err(AuthorityError::InvalidSelection);
        }
        let mut choices = self.installation_repositories().await?;
        if self.role == Role::Delegated {
            let selected: BTreeSet<_> = ids.iter().copied().collect();
            choices
                .repositories
                .retain(|repo| selected.contains(&repo.id));
        }
        Ok(choices)
    }
    /// One page of the create form's repository picker: the installation's
    /// repositories matching `query`, [`REPOSITORY_PICKER_PAGE_SIZE`] to a
    /// page, plus the `selected` ones wherever they are.
    ///
    /// The filter runs over the cached installation listing, never GitHub's
    /// search API. An unfiltered page whose selections it already holds reads
    /// only the listing pages up to it, so opening the form costs one listing
    /// read however large the installation is; a search, or a selection
    /// beyond that prefix, reads (or reuses) the whole cached listing.
    /// Delegated callers keep the search-first rule of
    /// [`Self::search_repositories`]: nothing without a query, at most
    /// [`DELEGATED_SEARCH_LIMIT`] matches, no paging and no permission reads.
    pub async fn repository_picker(
        &self,
        query: &str,
        page: usize,
        selected: &[u64],
    ) -> Result<RepositoryWindow, AuthorityError> {
        let query = query.trim();
        let delegated = self.role == Role::Delegated;
        let wanted: BTreeSet<_> = selected.iter().copied().collect();
        let page = page.max(1);
        if delegated && query.is_empty() && wanted.is_empty() {
            return Ok(RepositoryWindow {
                selected: vec![],
                listed: vec![],
                matches: 0,
                page: 1,
                pages: 1,
                search_required: true,
                truncated: false,
                pending: false,
            });
        }
        let prefix = (!delegated && query.is_empty())
            .then(|| page.saturating_mul(REPOSITORY_PICKER_PAGE_SIZE));
        let (mut choices, mut total) = self.installation_repository_listing(prefix).await?;
        if prefix.is_some()
            && !choices.pending
            && choices.repositories.len() < total
            && !wanted
                .iter()
                .all(|id| choices.repositories.iter().any(|repo| repo.id == *id))
        {
            (choices, total) = self.installation_repository_listing(None).await?;
        }
        let selected = choices
            .repositories
            .iter()
            .filter(|repo| wanted.contains(&repo.id))
            .cloned()
            .collect();
        let mut matches: Vec<_> = choices
            .repositories
            .into_iter()
            .filter(|repo| RepositoryPicker::matches_query(query, &repo.full_name))
            .collect();
        if delegated {
            let truncated = !query.is_empty() && matches.len() > DELEGATED_SEARCH_LIMIT;
            if query.is_empty() {
                matches.clear();
            }
            matches.truncate(DELEGATED_SEARCH_LIMIT);
            return Ok(RepositoryWindow {
                selected,
                matches: matches.len(),
                listed: matches,
                page: 1,
                pages: 1,
                search_required: query.is_empty(),
                truncated,
                pending: choices.pending,
            });
        }
        // A prefix holds the first pages only; GitHub's count says how many
        // the unfiltered listing has in all.
        let count = if prefix.is_some() {
            total.max(matches.len())
        } else {
            matches.len()
        };
        let pages = count.div_ceil(REPOSITORY_PICKER_PAGE_SIZE).max(1);
        let page = page.min(pages);
        let listed = matches
            .into_iter()
            .skip((page - 1) * REPOSITORY_PICKER_PAGE_SIZE)
            .take(REPOSITORY_PICKER_PAGE_SIZE)
            .collect();
        Ok(RepositoryWindow {
            selected,
            listed,
            matches: count,
            page,
            pages,
            search_required: false,
            truncated: false,
            pending: choices.pending,
        })
    }
    async fn installation_repositories(&self) -> Result<RepositoryChoices, AuthorityError> {
        Ok(self.installation_repository_listing(None).await?.0)
    }
    /// The installation's repositories with GitHub's total count. With a
    /// `prefix`, the walk stops at the first listing page that reaches that
    /// many repositories: a display prefix, never a complete selection.
    async fn installation_repository_listing(
        &self,
        prefix: Option<usize>,
    ) -> Result<(RepositoryChoices, usize), AuthorityError> {
        let Some(installation) = self.repo.as_ref().filter(|i| !unavailable(i)) else {
            return Ok((
                RepositoryChoices {
                    repositories: vec![],
                    pending: false,
                },
                0,
            ));
        };
        let mut cursor = None;
        let mut seen_cursors = Vec::new();
        let mut seen_ids = BTreeSet::new();
        let mut total = None;
        let mut all = Vec::new();
        let mut pending = false;
        for page_number in 0..1000 {
            let key = format!(
                "authz:repo:{}:0:page-{page_number}",
                installation.account.installation_id
            );
            let value = match self
                .cached_query(&key, installation, Query::Page(cursor.clone()))
                .await
            {
                Err(AuthorityError::Busy) => {
                    pending = true;
                    break;
                }
                other => other?,
            };
            let Some(value) = value else {
                return Err(AuthorityError::Unavailable);
            };
            let page: reads::RepositoryPage = decode(value)?;
            if total.is_some_and(|total| total != page.total_count) {
                return Err(AuthorityError::Unavailable);
            };
            total = Some(page.total_count);
            if page.total_count
                > if self.role == Role::Delegated {
                    MAX_REPOSITORIES as u64
                } else {
                    100_000
                }
            {
                return Err(AuthorityError::Unavailable);
            }
            for repo in page.repositories {
                if repo.id == 0
                    || repo.owner.id != self.account.account_id
                    || !seen_ids.insert(repo.id)
                    || !repo.full_name.split_once('/').is_some_and(|(owner, _)| {
                        owner.eq_ignore_ascii_case(&self.account.account_login)
                    })
                {
                    return Err(AuthorityError::Unavailable);
                }
                all.push(RepoTarget {
                    id: repo.id,
                    full_name: repo.full_name,
                });
            }
            match page.next {
                Some(_) if prefix.is_some_and(|prefix| all.len() >= prefix) => {
                    return Ok((
                        RepositoryChoices {
                            repositories: all,
                            pending: false,
                        },
                        usize::try_from(page.total_count).unwrap_or(usize::MAX),
                    ));
                }
                Some(next) => {
                    if seen_cursors.contains(&next) {
                        return Err(AuthorityError::Unavailable);
                    };
                    seen_cursors.push(next.clone());
                    cursor = Some(next)
                }
                None => {
                    if total != Some(all.len() as u64) {
                        return Err(AuthorityError::Unavailable);
                    };
                    break;
                }
            }
        }
        if cursor.is_some() && total != Some(all.len() as u64) && !pending {
            return Err(AuthorityError::Unavailable);
        }
        let total = all.len();
        Ok((
            RepositoryChoices {
                repositories: all,
                pending,
            },
            total,
        ))
    }
    async fn reserve(&self, installation: &Installation) -> Result<(), AuthorityError> {
        {
            let mut state = self.state.lock().map_err(|_| AuthorityError::Unavailable)?;
            if state.reads >= MAX_READS {
                return Err(AuthorityError::Busy);
            }
            state.reads += 1;
        }
        let key = format!(
            "{}:{}",
            installation.app.as_str(),
            installation.account.installation_id
        );
        match self.limiter.check(Bucket::Authority, &key).await {
            Ok(true) => Ok(()),
            Ok(false) => Err(AuthorityError::Busy),
            Err(_) => Err(AuthorityError::Unavailable),
        }
    }
    /// The installation's app and its Console read scope.
    fn reader(
        &self,
        installation: &Installation,
    ) -> Result<(GithubApp, &'static octoauth::TokenScope), AuthorityError> {
        let app = self
            .apps
            .app(crate::installed_apps::slot(installation.app))
            .map_err(|_| AuthorityError::Unavailable)?;
        let scope: &'static octoauth::TokenScope = if installation.app == App::Org {
            &CONSOLE_ORG_READ
        } else {
            &CONSOLE_REPO_READ
        };
        Ok((app, scope))
    }
    async fn query(
        &self,
        installation: &Installation,
        query: Query,
    ) -> Result<Option<serde_json::Value>, AuthorityError> {
        let (app, scope) = self.reader(installation)?;
        // Spend the read budget once, before the read: `read` re-runs its
        // closure after a 401, which must not spend it twice.
        self.reserve(installation).await?;
        let result = app
            .read(installation.account.installation_id, scope, |handle| {
                let query = query.clone();
                async move {
                    execute(&handle.rest(), query)
                        .await
                        .map_err(CallError::<std::convert::Infallible>::Api)
                }
            })
            .await;
        match result {
            Ok(value) => Ok(Some(value)),
            Err(TokenCallError::Call(CallError::Api(ApiError::NotFound))) => Ok(None),
            Err(_) => Err(AuthorityError::Unavailable),
        }
    }
    async fn cached_query(
        &self,
        key: &str,
        installation: &Installation,
        query: Query,
    ) -> Result<Option<serde_json::Value>, AuthorityError> {
        if let Some(value) = self.cache_read(key).await? {
            return decode(value);
        }
        let observed_at = chrono::Utc::now().timestamp();
        let value = self.query(installation, query).await?;
        self.cache_write(
            key,
            serde_json::to_value(&value).map_err(|_| AuthorityError::Unavailable)?,
            observed_at,
        )
        .await?;
        Ok(value)
    }
    async fn cache_read(&self, key: &str) -> Result<Option<serde_json::Value>, AuthorityError> {
        let now = chrono::Utc::now().timestamp();
        if let Some(entry) = self
            .state
            .lock()
            .map_err(|_| AuthorityError::Unavailable)?
            .memo
            .get(key)
            .filter(|entry| entry.is_fresh_at(now))
            .cloned()
        {
            return Ok(Some(entry.value));
        }
        let entry = self
            .cache
            .get(key)
            .await
            .map_err(|_| AuthorityError::Unavailable)?;
        if let Some(entry) = entry.filter(|entry| entry.is_fresh_at(now)) {
            self.state
                .lock()
                .map_err(|_| AuthorityError::Unavailable)?
                .memo
                .insert(key.into(), entry.clone());
            Ok(Some(entry.value))
        } else {
            Ok(None)
        }
    }
    async fn cache_write(
        &self,
        key: &str,
        value: serde_json::Value,
        observed_at: i64,
    ) -> Result<(), AuthorityError> {
        let entry = CacheEntry { observed_at, value };
        self.cache
            .put(key, entry.clone())
            .await
            .map_err(|_| AuthorityError::Unavailable)?;
        self.state
            .lock()
            .map_err(|_| AuthorityError::Unavailable)?
            .memo
            .insert(key.into(), entry);
        Ok(())
    }
}
#[derive(Serialize, Deserialize)]
struct RepositoryEvidence {
    account: u64,
    id: u64,
    name: String,
    user: u64,
    allowed: bool,
}
#[derive(Serialize, Deserialize)]
struct PermissionEvidence {
    user: u64,
    admin: bool,
}
fn unavailable(installation: &Installation) -> bool {
    installation.suspended_at.is_some() || installation.account.uninstalled_at.is_some()
}
fn decode<T: serde::de::DeserializeOwned>(value: serde_json::Value) -> Result<T, AuthorityError> {
    serde_json::from_value(value).map_err(|_| AuthorityError::Unavailable)
}
async fn execute(rest: &Rest<'_>, query: Query) -> Result<serde_json::Value, ApiError> {
    let value = match query {
        Query::Identity(name) => {
            let (owner, name) = name
                .split_once('/')
                .ok_or(ApiError::Decode { what: "repository" })?;
            serde_json::to_value(
                reads::repository_identity(rest, endpoints::Repository { owner, name }).await?,
            )
        }
        Query::Permission(name, login) => {
            let (owner, name) = name
                .split_once('/')
                .ok_or(ApiError::Decode { what: "repository" })?;
            let permission = endpoints::collaborator_permission(
                rest,
                endpoints::Repository { owner, name },
                &login,
            )
            .await?;
            serde_json::to_value(PermissionEvidence {
                user: permission.user.id,
                admin: permission.role == ghinvite_github::RepoRole::Admin,
            })
        }
        Query::Membership(org, login) => {
            serde_json::to_value(reads::organization_membership(rest, &org, &login).await?)
        }
        Query::Organization(org) => {
            let org = endpoints::organization::get_organization(rest, &org).await?;
            Ok(serde_json::json!({"id":org.id,"login":org.login}))
        }
        Query::Teams(org, cursor) => serde_json::to_value(
            endpoints::authority_teams::team_page(rest, &org, cursor.as_ref()).await?,
        ),
        Query::Team(org, slug) => serde_json::to_value(
            endpoints::authority_teams::team_identity(rest, &org, &slug).await?,
        ),
        Query::Page(cursor) => {
            serde_json::to_value(reads::installation_repository_page(rest, cursor.as_ref()).await?)
        }
    };
    value.map_err(|_| ApiError::Decode { what: "authority" })
}
#[cfg(test)]
mod tests;
