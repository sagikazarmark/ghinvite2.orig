//! GM-00 (PD-9): AuthorityCache entries as production writes them. On Workers
//! these bytes live in KV across deploys, and several embed gh-toolkit types
//! with gh-toolkit's own serde derives (cursors, pages, identities, users).
use super::*;
use crate::authority_cache::{CacheUnavailable, MemoryAuthorityCache};
use ghinvite_core::{
    bulk_preview::ObservedAccess,
    targets::{MemberGrant, OrganizationTarget, TeamTarget},
};
use serde::de::DeserializeOwned;
use serde_json::Value;

/// Records every entry the authority writes, exactly as handed to the adapter.
#[derive(Default)]
struct Recording {
    inner: MemoryAuthorityCache,
    puts: Mutex<BTreeMap<String, CacheEntry>>,
}
#[async_trait::async_trait]
impl AuthorityCache for Recording {
    async fn get(&self, key: &str) -> Result<Option<CacheEntry>, CacheUnavailable> {
        self.inner.get(key).await
    }
    async fn put(&self, key: &str, value: CacheEntry) -> Result<(), CacheUnavailable> {
        self.inner.put(key, value.clone()).await?;
        self.puts.lock().unwrap().insert(key.into(), value);
        Ok(())
    }
}
fn record(state: &mut AppState) -> Arc<Recording> {
    let recording = Arc::new(Recording::default());
    state.authority_cache = recording.clone();
    recording
}
/// Observation times come from the wall clock; everything else is fixed.
fn normalized(recording: &Recording) -> BTreeMap<String, CacheEntry> {
    let mut entries = recording.puts.lock().unwrap().clone();
    for (key, entry) in entries.iter_mut() {
        assert!(entry.observed_at > 0);
        entry.observed_at = 0;
        if key.starts_with("authz:bulk:") {
            let at = entry.value["observed_at"].as_str().unwrap();
            chrono::DateTime::parse_from_rfc3339(at).unwrap();
            entry.value["observed_at"] = json!("1970-01-01T00:00:00Z");
        }
    }
    entries
}
fn same<T: Serialize + DeserializeOwned>(value: &Value) {
    let typed: T = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(&serde_json::to_value(typed).unwrap(), value);
}
/// `{"id","login"}` written by a `json!` literal and read as gh-toolkit's `GhUser`.
fn org_literal(value: &Value) {
    let user: ghinvite_github::payloads::GhUser = serde_json::from_value(value.clone()).unwrap();
    assert_eq!((user.id, user.login.as_str()), (100, "acme"));
}
/// Decode each entry with the type its reader uses and re-encode it.
fn reread(key: &str, value: &Value) -> &'static str {
    use endpoints::authority_teams::{TeamIdentity, TeamPage};
    if key.starts_with("authz:bulk:") {
        same::<bulk_review::Read>(value);
        let inner = &value["value"];
        return if inner.get("full_name").is_some() {
            same::<reads::RepositoryIdentity>(inner);
            "bulk:repository"
        } else if inner.get("organization").is_some() {
            same::<TeamIdentity>(inner);
            "bulk:team"
        } else if inner.get("kind").is_some() {
            same::<ObservedAccess>(inner);
            "bulk:observed"
        } else if inner.get("type").is_some() {
            same::<ghinvite_github::payloads::GhUser>(inner);
            "bulk:user"
        } else {
            org_literal(inner);
            "bulk:organization"
        };
    }
    if key.ends_with(":membership") {
        same::<reads::Membership>(value);
        "membership"
    } else if key.contains(":page-") {
        same::<reads::RepositoryPage>(value);
        "repository_page"
    } else if key.contains(":teams-") {
        same::<TeamPage>(value);
        "team_page"
    } else if key.ends_with(":identity") {
        org_literal(value);
        "organization"
    } else if key.contains(":team-") {
        same::<TeamIdentity>(value);
        "team"
    } else {
        same::<RepositoryEvidence>(value);
        "repository_evidence"
    }
}
fn check(name: &str, recording: &Recording, kinds: &[&str]) {
    let entries = normalized(recording);
    let fixture = crate::golden::text(name, &serde_json::to_string_pretty(&entries).unwrap());
    let decoded: BTreeMap<String, CacheEntry> = serde_json::from_str(&fixture).unwrap();
    assert_eq!(serde_json::to_string_pretty(&decoded).unwrap(), fixture);
    let mut seen: Vec<_> = decoded
        .iter()
        .map(|(key, entry)| reread(key, &entry.value))
        .collect();
    seen.sort_unstable();
    seen.dedup();
    let mut kinds = kinds.to_vec();
    kinds.sort_unstable();
    assert_eq!(seen, kinds);
}
fn user_json(id: u64, login: &str) -> Value {
    json!({"id":id,"login":login,"type":"User","avatar_url":format!("https://avatars.githubusercontent.com/u/{id}?v=4")})
}
fn org_json() -> Value {
    json!({"id":100,"login":"acme","type":"Organization","avatar_url":"https://avatars.githubusercontent.com/u/100?v=4"})
}
fn repo_json(id: u64) -> Value {
    json!({"id":id,"full_name":format!("acme/r{id}"),"private":true,"owner":org_json()})
}
fn get(url: &str, body: Value) -> Expectation {
    Expectation::ok_json(Method::Get, format!("https://api.github.com{url}"), body)
}
fn next(mut page: Expectation, url: &str) -> Expectation {
    page.response.headers.insert(
        "link".into(),
        format!("<https://api.github.com{url}>; rel=\"next\""),
    );
    page
}
fn org_installation() -> Installation {
    let mut installation = Installation::repo(account());
    installation.app = App::Org;
    installation.account.installation_id = 2;
    installation
}
fn org_and_team(team: u64, slug: &str) -> Targets {
    Targets {
        organization: Some(OrganizationTarget {
            org_id: 100,
            org_login: "acme".into(),
            grant: MemberGrant::Member,
        }),
        teams: vec![TeamTarget {
            team_id: team,
            team_slug: slug.into(),
            grant: MemberGrant::Member,
        }],
        repos: vec![],
    }
}

/// Organization owner: membership, organization identity, both cursor-bearing
/// pages (teams and repositories), a team identity and repository evidence.
#[tokio::test]
async fn golden_authority_cache_console_entries() {
    let teams = next(
        get(
            "/orgs/acme/teams?per_page=100",
            json!([
                {"id":5,"slug":"build","name":"Build","ldap_dn":null},
                {"id":6,"slug":"synced","name":"Synced","ldap_dn":"cn=synced,dc=acme"}
            ]),
        ),
        "/orgs/acme/teams?per_page=100&page=2",
    );
    let repositories = next(
        get(
            "/installation/repositories?per_page=100",
            json!({"total_count":2,"repositories":[repo_json(1)]}),
        ),
        "/installation/repositories?per_page=100&page=2",
    );
    let script = vec![
        get(
            "/orgs/acme/memberships/alice",
            json!({"role":"admin","state":"active","user":user_json(8,"alice"),"organization":org_json()}),
        ),
        get(
            "/orgs/acme",
            json!({"id":100,"login":"acme","created_at":"2020-01-01T00:00:00Z"}),
        ),
        teams,
        get(
            "/orgs/acme/teams?per_page=100&page=2",
            json!([{"id":101,"slug":"team-101","name":"Last team"}]),
        ),
        get(
            "/orgs/acme/teams/team-101",
            json!({"id":101,"slug":"team-101","name":"Last team","ldap_dn":"","organization":org_json()}),
        ),
        repositories,
        get(
            "/installation/repositories?per_page=100&page=2",
            json!({"total_count":2,"repositories":[repo_json(2)]}),
        ),
        get("/repos/acme/r1", repo_json(1)),
    ];
    let (mut state, storage, mock, _) = fixture_with(script, false, org_installation()).await;
    storage
        .put_app_installation(&Installation::repo(account()))
        .await
        .unwrap();
    state.config.organization_delivery_enabled = true;
    let recording = record(&mut state);
    let view = AuthorityView::open(&state, 8, "alice", &account())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(view.role(), Role::AccountAdmin);
    let ghinvite_ui::link_form::OrganizationOffer::Available(choices) =
        view.organization_offer().await.unwrap()
    else {
        panic!("organization choices")
    };
    assert_eq!(choices.teams.len(), 3);
    let mut target = org_and_team(101, "team-101");
    target.repos = targets(&[1]).repos;
    assert_eq!(
        view.can_create_targets(&target).await.unwrap(),
        Access::Allowed
    );
    mock.assert_exhausted();
    check(
        "authority_cache_console.json",
        &recording,
        &[
            "membership",
            "organization",
            "repository_evidence",
            "repository_page",
            "team",
            "team_page",
        ],
    );
}

/// Request previews (`authz:bulk:v1:*`): user, repository, organization and
/// team identities plus the observed access derived from gh-toolkit roles.
#[tokio::test]
async fn golden_authority_cache_preview_entries() {
    let user = || {
        get(
            "/users/alice",
            json!({"id":8,"login":"alice","type":"User","created_at":"2015-01-01T00:00:00Z",
                "avatar_url":"https://avatars.githubusercontent.com/u/8?v=4"}),
        )
    };
    let (mut state, _, mock, _) = fixture(
        vec![
            user(),
            get("/repos/acme/r1", repo_json(1)),
            permission(1, 8, "read"),
        ],
        false,
    )
    .await;
    let recording = record(&mut state);
    let preview = AuthorityView::open(&state, 9, "installer", &account())
        .await
        .unwrap()
        .unwrap()
        .request_preview(&targets(&[1]), 8, "alice")
        .await;
    assert_eq!(preview.targets.len(), 1);
    mock.assert_exhausted();
    check(
        "authority_cache_preview_repository.json",
        &recording,
        &["bulk:observed", "bulk:repository", "bulk:user"],
    );

    let membership = |login: &str, id: u64, role: &str| {
        get(
            &format!("/orgs/acme/memberships/{login}"),
            json!({"role":role,"state":"active","user":user_json(id,login),"organization":org_json()}),
        )
    };
    let (mut state, _, mock, _) = fixture_with(
        vec![
            membership("installer", 9, "admin"),
            user(),
            get(
                "/orgs/acme",
                json!({"id":100,"login":"acme","created_at":"2020-01-01T00:00:00Z"}),
            ),
            membership("alice", 8, "member"),
            get(
                "/orgs/acme/teams/build",
                json!({"id":5,"slug":"build","name":"Build","organization":org_json()}),
            ),
            get(
                "/orgs/acme/teams/build/memberships/alice",
                json!({"role":"maintainer","state":"pending"}),
            ),
        ],
        false,
        org_installation(),
    )
    .await;
    let recording = record(&mut state);
    let preview = AuthorityView::open(&state, 9, "installer", &account())
        .await
        .unwrap()
        .unwrap()
        .request_preview(&org_and_team(5, "build"), 8, "alice")
        .await;
    assert_eq!(preview.targets.len(), 2);
    mock.assert_exhausted();
    check(
        "authority_cache_preview_organization.json",
        &recording,
        &[
            "bulk:observed",
            "bulk:organization",
            "bulk:team",
            "bulk:user",
            "membership",
        ],
    );
}
