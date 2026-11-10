use super::*;
use ghinvite_github::{
    AppSlot, GithubApps, Method,
    mocks::{API_BASE as BASE, Expectation, FixedTokens, MockTransport},
};
use serde_json::{Value, json};
/// Bearer of the org app's (456) first token for installation 2.
const TOKEN: &str = "Bearer ghs_fixed_456_2_1";
fn installation() -> Installation {
    ghinvite_core::account::InstallationFacts {
        app: App::Org,
        account: Account {
            installation_id: 2,
            account_id: 100,
            account_login: "acme".into(),
            account_type: AccountType::Organization,
            selected_repos: ghinvite_core::SelectedRepos::Subset(vec![]),
            installed_at: chrono::Utc::now(),
            uninstalled_at: None,
        },
        suspended_at: None,
        permissions: Default::default(),
    }
    .into_snapshot(1)
}
fn facts() -> Value {
    json!({"id":2,"app_id":456,"account":{"id":100,"login":"acme","type":"Organization"},"created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T00:00:00Z","suspended_at":null,"repository_selection":"all","permissions":{"members":"write","metadata":"read"}})
}
fn get(path: &str, body: Value) -> Expectation {
    Expectation::ok_json(Method::Get, format!("{BASE}{path}"), body)
}
fn org() -> Expectation {
    get(
        "/orgs/acme",
        json!({"id":100,"login":"acme","created_at":"2020-01-01T00:00:00Z"}),
    )
    .require_header("authorization", TOKEN)
}
fn member(role: &str, state: &str) -> Expectation {
    get("/orgs/acme/memberships/current-login",json!({"role":role,"state":state,"user":{"id":7,"login":"current-login"},"organization":{"id":100,"login":"acme"}})).require_header("authorization",TOKEN)
}
struct Fixture {
    transport: MockTransport,
    tokens: FixedTokens,
}
impl Fixture {
    /// Every scripted request was made, and tokens were only ever created
    /// for the members-read scope.
    fn assert_exhausted(&self) {
        self.transport.assert_exhausted();
        assert!(
            self.tokens
                .generation(2, &ghinvite_github::profiles::ORG_READ)
                <= 1
        );
        assert_eq!(
            self.tokens
                .generation(2, &ghinvite_github::profiles::ORG_INVITE),
            0
        );
        assert_eq!(self.tokens.replace_calls(), 0);
    }
}
async fn owner(script: Vec<Expectation>) -> (AccountInstallation, Fixture) {
    let transport = MockTransport::scripted(script);
    let tokens = FixedTokens::new(456);
    let apps = GithubApps::for_tests(
        &transport,
        [
            (AppSlot::Repo, FixedTokens::new(123)),
            (AppSlot::Org, tokens.clone()),
        ],
    );
    let state = crate::test_support::fixture_state()
        .await
        .with_github_apps(crate::test_github::repository_github(apps));
    (AccountInstallation { state }, Fixture { transport, tokens })
}
#[tokio::test]
async fn org_scope_uses_members_read_and_numeric_org_not_repository_enumeration() {
    let (owner, transport) = owner(vec![get("/app/installations/2", facts()), org()]).await;
    assert!(
        matches!(owner.observe(App::Org,&installation().account).await,Observation::Available{repo_ids} if repo_ids.is_empty())
    );
    transport.assert_exhausted();
    assert_eq!(
        transport
            .tokens
            .generation(2, &ghinvite_github::profiles::ORG_READ),
        1
    );
}
#[tokio::test]
async fn installation_and_org_identity_failures_never_report_available() {
    for field in [
        "suspended_at",
        "account_id",
        "account_type",
        "permissions",
        "app_id",
        "missing",
    ] {
        let mut value = facts();
        match field {
            "suspended_at" => value[field] = json!("2026-01-01T00:00:00Z"),
            "account_id" => value["account"]["id"] = json!(999),
            "account_type" => value["account"]["type"] = json!("User"),
            "permissions" => value[field] = json!({}),
            "app_id" => value[field] = json!(123),
            _ => {}
        }
        let response = if field == "missing" {
            Expectation::status(Method::Get, format!("{BASE}/app/installations/2"), 404)
        } else {
            get("/app/installations/2", value)
        };
        let (owner, transport) = owner(vec![response]).await;
        assert!(
            !matches!(
                owner.observe(App::Org, &installation().account).await,
                Observation::Available { .. }
            ),
            "{field}"
        );
        transport.assert_exhausted();
    }
    let (owner, transport) = owner(vec![
        get("/app/installations/2", facts()),
        get(
            "/orgs/acme",
            json!({"id":999,"login":"acme","created_at":"2020-01-01T00:00:00Z"}),
        ),
    ])
    .await;
    assert!(matches!(
        owner.observe(App::Org, &installation().account).await,
        Observation::Unavailable
    ));
    transport.assert_exhausted();
}
#[tokio::test]
async fn fresh_numeric_user_and_active_owner_are_required_with_explicit_nonowner_states() {
    for (role, state, expected) in [
        ("admin", "active", OrgAuthority::Owner),
        ("member", "active", OrgAuthority::NonOwner),
        ("admin", "pending", OrgAuthority::Denied),
        ("member", "pending", OrgAuthority::Denied),
    ] {
        let (owner, transport) = owner(vec![
            get("/app/installations/2", facts()),
            org(),
            get("/user/7", json!({"id":7,"login":"current-login"})),
            member(role, state),
        ])
        .await;
        assert_eq!(owner.org_owner_evidence(&installation(), 7).await, expected);
        transport.assert_exhausted();
    }
}
#[tokio::test]
async fn retained_inactive_org_and_identity_substitutions_cannot_fall_back_to_delegation() {
    let (owner, transport) = owner(vec![]).await;
    let mut value = installation();
    value.suspended_at = Some(chrono::Utc::now());
    assert_eq!(
        owner.org_owner_evidence(&value, 7).await,
        OrgAuthority::Denied
    );
    value.suspended_at = None;
    value.account.uninstalled_at = Some(chrono::Utc::now());
    assert_eq!(
        owner.org_owner_evidence(&value, 7).await,
        OrgAuthority::Denied
    );
    transport.assert_exhausted();
    let (owner, transport) = self::owner(vec![
        get("/app/installations/2", facts()),
        org(),
        get("/user/7", json!({"id":99,"login":"current-login"})),
    ])
    .await;
    assert_eq!(
        owner.org_owner_evidence(&installation(), 7).await,
        OrgAuthority::Denied
    );
    transport.assert_exhausted();
}

#[tokio::test]
async fn missing_membership_is_nonowner_but_failed_or_substituted_evidence_is_not() {
    for (reply, expected) in [
        (
            Expectation::status(
                Method::Get,
                format!("{BASE}/orgs/acme/memberships/current-login"),
                404,
            ),
            OrgAuthority::NonOwner,
        ),
        (
            Expectation::status(
                Method::Get,
                format!("{BASE}/orgs/acme/memberships/current-login"),
                503,
            ),
            OrgAuthority::Unavailable,
        ),
        (
            get(
                "/orgs/acme/memberships/current-login",
                json!({"role":"admin","state":"active","user":{"id":99,"login":"current-login"},"organization":{"id":100,"login":"acme"}}),
            ),
            OrgAuthority::Denied,
        ),
        (
            get(
                "/orgs/acme/memberships/current-login",
                json!({"role":"admin","state":"active","user":{"id":7,"login":"current-login"},"organization":{"id":999,"login":"acme"}}),
            ),
            OrgAuthority::Denied,
        ),
    ] {
        let (owner, transport) = owner(vec![
            get("/app/installations/2", facts()),
            org(),
            get("/user/7", json!({"id":7,"login":"current-login"})),
            reply,
        ])
        .await;
        assert_eq!(owner.org_owner_evidence(&installation(), 7).await, expected);
        transport.assert_exhausted();
    }
}
