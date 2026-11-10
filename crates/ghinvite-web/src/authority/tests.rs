use super::*;
use ghinvite_core::{
    account::{AccountSettings, SelectedRepos},
    storage::AppInstallationStorage,
    targets::{RepoRole, RepoTarget as Target},
};
use ghinvite_github::{
    AppSlot, InstalledApp, Method, TokenSource,
    mocks::{Expectation, FixedTokens, MockTransport},
};
use ghinvite_storage_sqlx::SqlxStorage;
use serde_json::json;
use std::sync::atomic::{AtomicUsize, Ordering};

struct Limit {
    calls: AtomicUsize,
    max: usize,
}
#[async_trait::async_trait]
impl EdgeRateLimiter for Limit {
    async fn check(
        &self,
        bucket: Bucket,
        key: &str,
    ) -> Result<bool, crate::rate_limit::Unavailable> {
        assert_eq!(bucket, Bucket::Authority);
        assert!(matches!(key, "repo:1" | "org:2"));
        Ok(self.calls.fetch_add(1, Ordering::SeqCst) < self.max)
    }
}
/// The repository app's (ID 1) first token for installation 1.
const REPO_BEARER: &str = "Bearer ghs_fixed_1_1_1";
thread_local! {
    /// The token sources of the last fixture on this test's thread: repo, org.
    static TOKENS: std::cell::RefCell<Option<(FixedTokens, FixedTokens)>> =
        const { std::cell::RefCell::new(None) };
}
/// Tokens created so far for the installation's Console read scope: the
/// repository app's installation 1, or the organization app's installation 2.
fn console_generation(org: bool) -> u64 {
    TOKENS.with(|tokens| {
        let tokens = tokens.borrow();
        let (repo, org_tokens) = tokens.as_ref().expect("fixture tokens");
        if org {
            org_tokens.generation(2, &CONSOLE_ORG_READ)
        } else {
            repo.generation(1, &CONSOLE_REPO_READ)
        }
    })
}
/// Apps `repo` (ID 1) and `org` (ID 2) with fixed tokens, calling
/// `api.github.com` through `mock`.
fn github(mock: &MockTransport) -> crate::state::WebGithub {
    let transport: Arc<dyn ghinvite_github::HttpTransportDyn> = Arc::new(mock.clone());
    let env =
        ghinvite_github::rest::RestEnv::new(transport.clone(), "https://api.github.com", "p1")
            .unwrap();
    let (repo, org) = (FixedTokens::new(1), FixedTokens::new(2));
    TOKENS.with(|tokens| *tokens.borrow_mut() = Some((repo.clone(), org.clone())));
    let apps = [(AppSlot::Repo, repo), (AppSlot::Org, org)].map(|(slot, tokens)| {
        let installed = InstalledApp {
            slot,
            app: ghinvite_github::mocks::app(
                tokens.app_id().get(),
                ghinvite_github::mocks::UNREACHABLE_APP_BASE,
            ),
            user_agent: format!("p1/{slot}"),
        };
        (installed, Arc::new(tokens) as Arc<dyn TokenSource>)
    });
    crate::state::WebGithub {
        transport,
        apps: GithubApps::with_sources(env, apps).unwrap(),
    }
}
fn account() -> Account {
    Account {
        installation_id: 1,
        account_id: 100,
        account_login: "acme".into(),
        account_type: AccountType::Organization,
        installed_at: chrono::Utc::now(),
        uninstalled_at: None,
        selected_repos: SelectedRepos::All,
    }
}
async fn fixture(
    expectations: Vec<Expectation>,
    delegation: bool,
) -> (AppState, Arc<SqlxStorage>, MockTransport, Arc<Limit>) {
    let mut installation = Installation::repo(account());
    installation.installed_by_user_id = Some(9);
    installation.installed_by_login = Some("installer".into());
    fixture_with(expectations, delegation, installation).await
}
async fn fixture_with(
    expectations: Vec<Expectation>,
    delegation: bool,
    installation: Installation,
) -> (AppState, Arc<SqlxStorage>, MockTransport, Arc<Limit>) {
    let storage = Arc::new(SqlxStorage::in_memory().await.unwrap());
    storage.put_app_installation(&installation).await.unwrap();
    storage
        .put_account_settings(&AccountSettings {
            account_id: 100,
            revision: 1,
            repo_admin_delegation: delegation,
            changed_by: 9,
            changed_at: chrono::Utc::now(),
        })
        .await
        .unwrap();
    let mock = MockTransport::scripted(expectations);
    let config = crate::config::WebConfig::for_local_dev_with_secret([3; 32]);
    let limit = Arc::new(Limit {
        calls: AtomicUsize::new(0),
        max: usize::MAX,
    });
    let state = AppState::new(
        storage.clone(),
        github(&mock),
        Arc::new(crate::RestateClient::new("http://127.0.0.1:8080").unwrap()),
        config,
    )
    .with_authority_cache(Arc::new(
        crate::authority_cache::MemoryAuthorityCache::default(),
    ))
    .with_edge_limits(limit.clone());
    (state, storage, mock, limit)
}
fn identity(id: u64, owner: u64) -> Expectation {
    Expectation::ok_json(
        Method::Get,
        format!("https://api.github.com/repos/acme/r{id}"),
        json!({"id":id,"full_name":format!("acme/r{id}"),"owner":{"id":owner,"login":"acme"}}),
    )
    .require_header("authorization", REPO_BEARER)
}
fn permission(id: u64, user: u64, role: &str) -> Expectation {
    Expectation::ok_json(
        Method::Get,
        format!("https://api.github.com/repos/acme/r{id}/collaborators/alice/permission"),
        json!({"user":{"id":user,"login":"alice"},"role_name":role}),
    )
}
fn page(ids: &[u64]) -> Expectation {
    Expectation::ok_json(
        Method::Get,
        "https://api.github.com/installation/repositories?per_page=100",
        json!({"total_count":ids.len(),"repositories":ids.iter().map(|id|json!({"id":id,"full_name":format!("acme/r{id}"),"owner":{"id":100,"login":"acme"}})).collect::<Vec<_>>()}),
    )
}
fn cardinality(total: u64) -> Expectation {
    let ids: Vec<_> = (1..=total.min(100)).collect();
    let mut first = page(&ids);
    let mut body: serde_json::Value = serde_json::from_slice(&first.response.body).unwrap();
    body["total_count"] = json!(total);
    first.response.body = serde_json::to_vec(&body).unwrap();
    if total > 100 {
        first.response.headers.insert(
            "link".into(),
            "<https://api.github.com/installation/repositories?per_page=100&page=2>; rel=\"next\""
                .into(),
        );
    }
    first
}
fn targets(ids: &[u64]) -> Targets {
    Targets {
        repos: ids
            .iter()
            .map(|id| Target {
                repo_id: *id,
                repo_full_name: format!("acme/r{id}"),
                grant: RepoRole::Write,
            })
            .collect(),
        ..Default::default()
    }
}
async fn delegate(state: &AppState) -> AuthorityView {
    AuthorityView::open(state, 8, "alice", &account())
        .await
        .unwrap()
        .unwrap()
}

#[tokio::test]
async fn delegation_off_and_installer_pending_grant_nothing_without_github() {
    let (state, _, mock, _) = fixture(vec![], false).await;
    assert!(
        AuthorityView::open(&state, 8, "alice", &account())
            .await
            .unwrap()
            .is_none()
    );
    let owner = AuthorityView::open(&state, 9, "installer", &account())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(owner.role(), Role::InstallingOwner);
    assert_eq!(
        owner.can_read_link(&targets(&[1, 2])).await.unwrap(),
        Access::Allowed
    );
    assert!(owner.console_scope().await.unwrap().scope.is_account());
    let (state, _, _, _) = fixture_with(vec![], false, Installation::repo(account())).await;
    assert_eq!(
        AuthorityView::open(&state, 8, "alice", &account())
            .await
            .unwrap_err(),
        AuthorityError::PendingSetup
    );
    mock.assert_exhausted();
}
#[tokio::test]
async fn every_target_admin_is_required_and_cached_across_sessions() {
    let (state, _, mock, limit) = fixture(
        vec![
            cardinality(2000),
            identity(1, 100),
            permission(1, 8, "admin"),
            identity(2, 100),
            permission(2, 8, "write"),
        ],
        true,
    )
    .await;
    let view = delegate(&state).await;
    assert_eq!(
        view.can_read_link(&targets(&[1, 2])).await.unwrap(),
        Access::Denied
    );
    assert_eq!(
        view.can_mutate_targets(&targets(&[1])).await.unwrap(),
        Access::Allowed
    );
    assert_eq!(
        view.can_mutate_targets(&targets(&[2])).await.unwrap(),
        Access::Denied
    );
    let second = delegate(&state).await;
    assert_eq!(
        second.can_read_link(&targets(&[1])).await.unwrap(),
        Access::Allowed
    );
    assert_eq!(
        second.can_mutate_targets(&targets(&[1, 2])).await.unwrap(),
        Access::Denied
    );
    assert_eq!(limit.calls.load(Ordering::SeqCst), 5);
    // Only the repository app's Console scope (metadata: read), one token.
    assert_eq!(console_generation(false), 1);
    assert_eq!(console_generation(true), 0);
    mock.assert_exhausted();
}
#[tokio::test]
async fn numeric_user_repository_and_owner_mismatch_never_grants() {
    for (identity_id, owner, user) in [(1, 101, 8), (1, 100, 99), (2, 100, 8)] {
        let mut response = identity(1, owner);
        response.response.body = serde_json::to_vec(
            &json!({"id":identity_id,"full_name":"acme/r1","owner":{"id":owner,"login":"acme"}}),
        )
        .unwrap();
        let mut script = vec![cardinality(2000), response];
        if owner == 100 && identity_id == 1 {
            script.push(permission(1, user, "admin"))
        }
        let (state, _, mock, _) = fixture(script, true).await;
        assert_eq!(
            delegate(&state)
                .await
                .can_mutate_targets(&targets(&[1]))
                .await
                .unwrap(),
            Access::Denied
        );
        mock.assert_exhausted();
    }
}
#[tokio::test]
async fn org_active_owner_replaces_installer_and_pending_owner_never_grants() {
    for (state_name, role, expected) in [
        ("pending", "admin", None),
        ("active", "admin", Some(Role::AccountAdmin)),
        ("active", "member", Some(Role::Delegated)),
    ] {
        let membership = Expectation::ok_json(
            Method::Get,
            "https://api.github.com/orgs/acme/memberships/alice",
            json!({"state":state_name,"role":role,"user":{"id":8,"login":"alice"},"organization":{"id":100,"login":"acme"}}),
        );
        let mut script = vec![membership];
        if expected == Some(Role::Delegated) {
            script.extend([cardinality(2000)]);
        }
        let (state, storage, mock, _) = fixture(script, true).await;
        let mut org = Installation::repo(account());
        org.app = App::Org;
        org.account.installation_id = 2;
        storage.put_app_installation(&org).await.unwrap();
        assert_eq!(
            AuthorityView::open(&state, 8, "alice", &account())
                .await
                .unwrap()
                .map(|v| v.role()),
            expected
        );
        mock.assert_exhausted();
    }
}
#[tokio::test]
async fn fifty_outbound_reads_bound_the_view_then_cache_hits_still_work() {
    let mut script = vec![cardinality(2000)];
    for id in 1..=24 {
        script.push(identity(id, 100));
        script.push(permission(id, 8, "admin"));
    }
    script.push(identity(25, 100));
    let (state, _, mock, limit) = fixture(script, true).await;
    let view = delegate(&state).await;
    assert_eq!(
        view.can_read_link(&targets(&(1..=24).collect::<Vec<_>>()))
            .await
            .unwrap(),
        Access::Allowed
    );
    assert_eq!(
        view.can_read_link(&targets(&[25])).await.unwrap(),
        Access::Pending
    );
    assert_eq!(
        view.can_read_link(&targets(&[1])).await.unwrap(),
        Access::Allowed
    );
    assert_eq!(limit.calls.load(Ordering::SeqCst), 50);
    mock.assert_exhausted();
}
#[tokio::test]
async fn global_rate_exhaustion_is_pending_and_upstream_failure_is_unavailable() {
    let (mut state, _, mock, _) = fixture(vec![], true).await;
    state.edge_limits = Arc::new(Limit {
        calls: AtomicUsize::new(0),
        max: 0,
    });
    assert_eq!(
        AuthorityView::open(&state, 8, "alice", &account())
            .await
            .unwrap_err(),
        AuthorityError::Busy
    );
    mock.assert_exhausted();
    let (state, _, mock, _) = fixture(
        vec![
            cardinality(2000),
            Expectation::status(Method::Get, "https://api.github.com/repos/acme/r1", 500),
        ],
        true,
    )
    .await;
    assert_eq!(
        delegate(&state)
            .await
            .can_read_link(&targets(&[1]))
            .await
            .unwrap_err(),
        AuthorityError::Unavailable
    );
    mock.assert_exhausted();
}
#[tokio::test]
async fn search_first_has_no_permission_reads_and_selected_ids_bypass_twenty_cap() {
    let rows:Vec<_>=(1..=25).map(|id|json!({"id":id,"full_name":format!("acme/r{id}"),"owner":{"id":100,"login":"acme"}})).collect();
    let page = Expectation::ok_json(
        Method::Get,
        "https://api.github.com/installation/repositories?per_page=100",
        json!({"total_count":25,"repositories":rows}),
    );
    let (state, _, mock, limit) = fixture(
        vec![page, identity(25, 100), permission(25, 8, "admin")],
        true,
    )
    .await;
    let view = delegate(&state).await;
    assert!(
        view.search_repositories("")
            .await
            .unwrap()
            .repositories
            .is_empty()
    );
    assert_eq!(limit.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        view.search_repositories("r")
            .await
            .unwrap()
            .repositories
            .len(),
        20
    );
    assert_eq!(limit.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        view.repositories_by_ids(&[25]).await.unwrap(),
        vec![RepoTarget {
            id: 25,
            full_name: "acme/r25".into()
        }]
    );
    assert_eq!(limit.calls.load(Ordering::SeqCst), 3);
    mock.assert_exhausted();
}
#[tokio::test]
async fn cache_partition_is_enforced_by_consumer() {
    let (state, _, mock, _) = fixture(
        vec![
            cardinality(2000),
            identity(1, 100),
            permission(1, 8, "write"),
        ],
        true,
    )
    .await;
    let evidence = serde_json::to_value(RepositoryEvidence {
        account: 100,
        id: 1,
        name: "acme/r1".into(),
        user: 8,
        allowed: true,
    })
    .unwrap();
    for key in [
        "authz:org:1:8:1",
        "authz:repo:2:8:1",
        "authz:repo:1:9:1",
        "authz:repo:1:8:2",
    ] {
        state
            .authority_cache
            .put(
                key,
                CacheEntry {
                    observed_at: chrono::Utc::now().timestamp(),
                    value: evidence.clone(),
                },
            )
            .await
            .unwrap();
    }
    assert_eq!(
        delegate(&state)
            .await
            .can_read_link(&targets(&[1]))
            .await
            .unwrap(),
        Access::Denied
    );
    mock.assert_exhausted();
}
#[tokio::test]
async fn personal_owner_creation_checks_availability_without_collaborator_permission() {
    let mut personal = account();
    personal.account_type = AccountType::User;
    let (state, _, mock, _) = fixture_with(
        vec![page(&[1]), identity(1, 100)],
        false,
        Installation::repo(personal.clone()),
    )
    .await;
    assert!(
        AuthorityView::open(&state, 8, "alice", &personal)
            .await
            .unwrap()
            .is_none()
    );
    let owner = AuthorityView::open(&state, 100, "acme", &personal)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(owner.role(), Role::PersonalOwner);
    assert_eq!(
        owner.can_create_targets(&targets(&[1])).await.unwrap(),
        Access::Allowed
    );
    mock.assert_exhausted();
}

#[tokio::test]
async fn personal_history_remains_readable_and_mutable_but_cannot_create_after_uninstall() {
    let mut personal = account();
    personal.account_type = AccountType::User;
    personal.uninstalled_at = Some(chrono::Utc::now());
    let (state, _, mock, _) =
        fixture_with(vec![], false, Installation::repo(personal.clone())).await;
    let owner = AuthorityView::open(&state, 100, "acme", &personal)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        owner.can_read_link(&targets(&[1])).await.unwrap(),
        Access::Allowed
    );
    assert_eq!(
        owner.can_mutate_targets(&targets(&[1])).await.unwrap(),
        Access::Allowed
    );
    assert_eq!(
        owner.can_create_targets(&targets(&[1])).await.unwrap(),
        Access::Denied
    );
    mock.assert_exhausted();
}
#[tokio::test]
async fn public_repository_outside_installation_selection_never_authorizes_mutation() {
    let (state, _, mock, _) = fixture(vec![page(&[2])], true).await;
    let view = delegate(&state).await;
    assert_eq!(
        view.can_create_targets(&targets(&[1])).await.unwrap(),
        Access::Denied
    );
    assert_eq!(
        view.repositories_by_ids(&[1]).await.unwrap_err(),
        AuthorityError::InvalidSelection
    );
    mock.assert_exhausted();
}
#[tokio::test]
async fn read_401_is_retried_with_a_new_token_and_metered_once() {
    let (state, _, mock, limit) = fixture(
        vec![
            cardinality(2000),
            Expectation::status(Method::Get, "https://api.github.com/repos/acme/r1", 401)
                .require_header("authorization", REPO_BEARER),
            identity(1, 100).require_header("authorization", "Bearer ghs_fixed_1_1_2"),
            permission(1, 8, "admin"),
        ],
        true,
    )
    .await;
    assert_eq!(
        delegate(&state)
            .await
            .can_read_link(&targets(&[1]))
            .await
            .unwrap(),
        Access::Allowed
    );
    // The read budget is spent before the read, once: the 401 retry inside
    // `GithubApp::read` is not another reservation (GM-05).
    assert_eq!(limit.calls.load(Ordering::SeqCst), 3);
    assert_eq!(console_generation(false), 2);
    mock.assert_exhausted();
}
struct StaleCache(CacheEntry);
#[async_trait::async_trait]
impl AuthorityCache for StaleCache {
    async fn get(
        &self,
        _: &str,
    ) -> Result<Option<CacheEntry>, crate::authority_cache::CacheUnavailable> {
        Ok(Some(self.0.clone()))
    }
    async fn put(
        &self,
        _: &str,
        _: CacheEntry,
    ) -> Result<(), crate::authority_cache::CacheUnavailable> {
        Ok(())
    }
}
#[tokio::test]
async fn expired_and_future_cache_entries_cannot_grant_even_if_adapter_returns_them() {
    for offset in [-300, 60] {
        let (mut state, _, mock, _) = fixture(
            vec![
                cardinality(2000),
                identity(1, 100),
                permission(1, 8, "write"),
            ],
            true,
        )
        .await;
        state.authority_cache = Arc::new(StaleCache(CacheEntry {
            observed_at: chrono::Utc::now().timestamp() + offset,
            value: serde_json::to_value(RepositoryEvidence {
                account: 100,
                id: 1,
                name: "acme/r1".into(),
                user: 8,
                allowed: true,
            })
            .unwrap(),
        }));
        assert_eq!(
            delegate(&state)
                .await
                .can_read_link(&targets(&[1]))
                .await
                .unwrap(),
            Access::Denied
        );
        mock.assert_exhausted();
    }
}
#[tokio::test]
async fn missing_cache_backend_fails_closed_before_github_read() {
    let (mut state, _, mock, _) = fixture(vec![], true).await;
    state.authority_cache = Arc::new(crate::authority_cache::Unconfigured);
    assert_eq!(
        AuthorityView::open(&state, 8, "alice", &account())
            .await
            .unwrap_err(),
        AuthorityError::Unavailable
    );
    mock.assert_exhausted();
}
#[tokio::test]
async fn every_list_page_is_metered_and_complete_cache_has_zero_read_cost() {
    let mut first = page(&[1]);
    first.response.body=serde_json::to_vec(&json!({"total_count":2,"repositories":[{"id":1,"full_name":"acme/r1","owner":{"id":100,"login":"acme"}}]})).unwrap();
    first.response.headers.insert(
        "link".into(),
        "<https://api.github.com/installation/repositories?per_page=100&page=2>; rel=\"next\""
            .into(),
    );
    let mut second = page(&[2]);
    second.url = "https://api.github.com/installation/repositories?per_page=100&page=2".into();
    second.response.body=serde_json::to_vec(&json!({"total_count":2,"repositories":[{"id":2,"full_name":"acme/r2","owner":{"id":100,"login":"acme"}}]})).unwrap();
    let (state, _, mock, limit) = fixture(vec![first, second], true).await;
    assert_eq!(
        delegate(&state)
            .await
            .search_repositories("r")
            .await
            .unwrap()
            .repositories
            .len(),
        2
    );
    assert_eq!(
        delegate(&state)
            .await
            .search_repositories("r")
            .await
            .unwrap()
            .repositories
            .len(),
        2
    );
    assert_eq!(limit.calls.load(Ordering::SeqCst), 2);
    mock.assert_exhausted();
}
#[tokio::test]
async fn thirty_links_only_check_twelve_distinct_targets_and_warm_views_read_nothing() {
    use ghinvite_core::storage::projection::fixture::Seed;
    let mut script = vec![cardinality(2000)];
    for id in 1..=12 {
        script.push(identity(id, 100));
        script.push(permission(id, 8, "admin"));
    }
    let (state, storage, mock, limit) = fixture(script, true).await;
    use ghinvite_core::storage::RecordStorage;
    storage
        .upsert_user(&ghinvite_core::User {
            user_id: 9,
            login: "installer".into(),
            avatar_url: None,
            github_created_at: None,
            last_seen_at: chrono::Utc::now(),
        })
        .await
        .unwrap();
    for i in 0..30 {
        let link = ghinvite_core::InvitationLink {
            batch_source: None,
            access_lifetime_seconds: None,
            email_domains: vec![],
            id: ghinvite_core::InvitationLinkId::new(),
            organization_installation_id: None,
            installation_id: 1,
            account_id: 100,
            created_by: 9,
            created_at: chrono::Utc::now(),
            expires_at: None,
            max_uses: None,
            invitation_lifetime_seconds: 604800,
            abuse_threshold_per_hour: None,
            budget_share_pct: 50,
            uses_count: 0,
            secret: ghinvite_core::invitation_link::LinkSecretState {
                secret_generation: 1,
                sha256: ghinvite_core::SecretHash::from_bytes([i + 1; 32]),
            },
            approval_required: true,
            description: format!("Link {i}"),
            internal_note: None,
            revoked_at: None,
            revoked_by: None,
            pause: None,
            targets: targets(&[u64::from(i % 12 + 1)]),
        };
        storage.seed_link(&link).await.unwrap();
    }
    let scope = delegate(&state).await.console_scope().await.unwrap();
    assert!(!scope.pending);
    assert_eq!(scope.authorized_repositories, 12);
    assert!(!scope.scope.is_account());
    assert_eq!(
        delegate(&state)
            .await
            .console_scope()
            .await
            .unwrap()
            .authorized_repositories,
        12
    );
    assert_eq!(limit.calls.load(Ordering::SeqCst), 25);
    mock.assert_exhausted();
}

#[tokio::test]
async fn partial_pages_and_mixed_unknown_ids_never_authorize_a_shorter_selection() {
    let mut first = page(&[1]);
    first.response.body = serde_json::to_vec(&json!({"total_count":2,"repositories":[{"id":1,"full_name":"acme/r1","owner":{"id":100,"login":"acme"}}]})).unwrap();
    first.response.headers.insert(
        "link".into(),
        "<https://api.github.com/installation/repositories?per_page=100&page=2>; rel=\"next\""
            .into(),
    );
    let (mut state, _, mock, _) = fixture(vec![first], true).await;
    state.edge_limits = Arc::new(Limit {
        calls: AtomicUsize::new(0),
        max: 1,
    });
    let view = delegate(&state).await;
    let choices = view.search_repositories("r").await.unwrap();
    assert!(choices.pending);
    assert_eq!(choices.repositories.len(), 1);
    assert_eq!(
        view.repositories_by_ids(&[1]).await.unwrap_err(),
        AuthorityError::Busy
    );
    mock.assert_exhausted();
    let (state, _, mock, _) = fixture(vec![page(&[1])], true).await;
    assert_eq!(
        delegate(&state)
            .await
            .repositories_by_ids(&[1, 999])
            .await
            .unwrap_err(),
        AuthorityError::InvalidSelection
    );
    mock.assert_exhausted();
}

#[tokio::test]
async fn inconsistent_first_page_cannot_grant_delegated_view() {
    for body in [
        json!({"total_count":2,"repositories":[{"id":1,"full_name":"acme/r1","owner":{"id":100,"login":"acme"}}]}),
        json!({"total_count":2,"repositories":[{"id":1,"full_name":"acme/r1","owner":{"id":100,"login":"acme"}},{"id":1,"full_name":"acme/r1","owner":{"id":100,"login":"acme"}}]}),
        json!({"total_count":5001,"repositories":[]}),
    ] {
        let (state, _, mock, _) = fixture(
            vec![Expectation::ok_json(
                Method::Get,
                "https://api.github.com/installation/repositories?per_page=100",
                body,
            )],
            true,
        )
        .await;
        assert_eq!(
            AuthorityView::open(&state, 8, "alice", &account())
                .await
                .unwrap_err(),
            AuthorityError::Unavailable
        );
        mock.assert_exhausted();
    }
}

#[tokio::test]
async fn failed_form_review_preserves_only_known_delegated_choices_without_granting_authority() {
    let (state, _, mock, limit) = fixture(vec![page(&[1, 2, 3])], true).await;
    let view = delegate(&state).await;
    let choices = view.repositories_for_review(&[2, 999]).await.unwrap();
    assert!(!choices.pending);
    assert_eq!(
        choices.repositories,
        vec![RepoTarget {
            id: 2,
            full_name: "acme/r2".into()
        }]
    );
    assert!(
        view.repositories_for_review(&[])
            .await
            .unwrap()
            .repositories
            .is_empty()
    );
    assert_eq!(limit.calls.load(Ordering::SeqCst), 1);
    mock.assert_exhausted();
}

#[tokio::test]
async fn installation_cardinality_gates_all_delegation_with_one_cached_page() {
    for total in [5000, 5001] {
        let mut script = vec![cardinality(total)];
        if total == 5000 {
            script.extend([identity(1, 100), permission(1, 8, "admin")]);
        }
        let (state, _, mock, limit) = fixture(script, true).await;
        let view = AuthorityView::open(&state, 8, "alice", &account())
            .await
            .unwrap();
        assert_eq!(view.is_some(), total == 5000);
        assert_eq!(
            limit.calls.load(Ordering::SeqCst),
            1,
            "only first-page metadata, never enumerate the installation"
        );
        if let Some(view) = view {
            assert_eq!(
                view.can_read_link(&targets(&[1])).await.unwrap(),
                Access::Allowed
            );
        }
        let reads = limit.calls.load(Ordering::SeqCst);
        assert_eq!(
            AuthorityView::open(&state, 8, "alice", &account())
                .await
                .unwrap()
                .is_some(),
            total == 5000
        );
        assert_eq!(
            limit.calls.load(Ordering::SeqCst),
            reads,
            "five-minute metadata shared by views"
        );
        let owner = AuthorityView::open(&state, 9, "installer", &account())
            .await
            .unwrap()
            .unwrap();
        assert!(owner.console_scope().await.unwrap().scope.is_account());
        assert_eq!(
            limit.calls.load(Ordering::SeqCst),
            reads,
            "owners retain access regardless of delegation ceiling"
        );
        mock.assert_exhausted();
    }
}

#[tokio::test]
async fn cardinality_failure_never_returns_a_delegated_view_or_reads_permissions() {
    for status in [404, 403, 429, 500] {
        let (state, _, mock, limit) = fixture(
            vec![Expectation::status(
                Method::Get,
                "https://api.github.com/installation/repositories?per_page=100",
                status,
            )],
            true,
        )
        .await;
        assert_eq!(
            AuthorityView::open(&state, 8, "alice", &account())
                .await
                .unwrap_err(),
            AuthorityError::Unavailable
        );
        assert_eq!(limit.calls.load(Ordering::SeqCst), 1);
        mock.assert_exhausted();
    }
}

#[tokio::test]
async fn cardinality_cache_expiry_observes_installation_growth_before_granting() {
    for age in [-300, 60] {
        let (mut state, _, mock, limit) = fixture(vec![cardinality(5001)], true).await;
        let mut cached: serde_json::Value =
            serde_json::from_slice(&cardinality(5000).response.body).unwrap();
        cached["next"] = json!("/installation/repositories?per_page=100&page=2");
        state.authority_cache = Arc::new(StaleCache(CacheEntry {
            observed_at: chrono::Utc::now().timestamp() + age,
            value: cached,
        }));
        assert!(
            AuthorityView::open(&state, 8, "alice", &account())
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(limit.calls.load(Ordering::SeqCst), 1);
        mock.assert_exhausted();
    }
}

#[path = "tests/organization.rs"]
mod organization;

mod bulk;

mod request_preview;

mod golden;
