//! The temporary stub-compatibility shim (GE-07; deleted in GE-09): the
//! `compat` seed reached with octoauth and ghinvite's fixture key, and every
//! `/__octoemu/compat/` route.
#![cfg(not(target_arch = "wasm32"))]

use octoauth::{Access as Level, App as AppClient, Permissions, RepositoryPermission, SigningKey};
use octoemu::{Emulator, compat};
use reqwest::{Client, Method, StatusCode};
use serde_json::{Value, json};

/// ghinvite's `test-fixtures/github-app-rsa.pem`, byte for byte.
const PRIVATE_KEY: &str = include_str!("fixtures/rsa2048.pem");

struct World {
    emulator: Emulator,
    client: Client,
    token: String,
}

struct Reply {
    status: StatusCode,
    headers: reqwest::header::HeaderMap,
    body: Value,
}

impl World {
    async fn start() -> Self {
        Self::with(compat::seed(), compat::REPO_INSTALLATION_ID).await
    }

    async fn with(seed: octoemu::seed::Seed, installation: u64) -> Self {
        let emulator = Emulator::start(seed).await.unwrap();
        let app = AppClient::builder(
            compat::REPO_APP_ID,
            SigningKey::from_pem(PRIVATE_KEY).unwrap(),
        )
        .client_id(compat::REPO_CLIENT_ID)
        .base_url(emulator.api_base())
        .build()
        .unwrap();
        let permissions = Permissions::builder()
            .grant(RepositoryPermission::Administration, Level::Write)
            .grant(RepositoryPermission::Metadata, Level::Read)
            .build()
            .unwrap();
        let token = app
            .installation(installation)
            .access_token(&permissions)
            .await
            .unwrap()
            .expose_secret()
            .to_owned();
        Self {
            emulator,
            client: Client::builder().no_proxy().build().unwrap(),
            token,
        }
    }

    async fn send(&self, method: Method, path: &str, body: Option<Value>, token: bool) -> Reply {
        let mut request = self
            .client
            .request(method, format!("{}{path}", self.emulator.api_base()));
        if token {
            request = request.bearer_auth(&self.token);
        }
        if let Some(body) = body {
            request = request
                .header("content-type", "application/json")
                .body(body.to_string());
        }
        let response = request.send().await.unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let text = response.text().await.unwrap();
        Reply {
            status,
            headers,
            body: serde_json::from_str(&text).unwrap_or(Value::String(text)),
        }
    }

    async fn api(&self, method: Method, path: &str, body: Option<Value>) -> Reply {
        self.send(method, path, body, true).await
    }

    async fn compat(&self, method: Method, route: &str, body: Option<Value>) -> Reply {
        self.send(method, &format!("/__octoemu/compat/{route}"), body, false)
            .await
    }

    async fn put(&self, repo: &str, user: &str) -> Reply {
        self.api(
            Method::PUT,
            &format!("/repos/acme/{repo}/collaborators/{user}"),
            Some(json!({"permission": "push"})),
        )
        .await
    }

    async fn permission(&self, repo: &str, user: &str) -> Value {
        self.api(
            Method::GET,
            &format!("/repos/acme/{repo}/collaborators/{user}/permission"),
            None,
        )
        .await
        .body
    }
}

#[tokio::test]
async fn the_seed_is_the_stubs_world() {
    let world = World::start().await;
    let installation = world.emulator.state();
    assert_eq!(installation["world"]["repos"].as_array().unwrap().len(), 2);
    let repo = world.api(Method::GET, "/repos/acme/api", None).await;
    assert_eq!(
        (repo.body["id"].as_u64(), repo.body["owner"]["id"].as_u64()),
        (Some(10), Some(100))
    );
    let repos = world
        .api(Method::GET, "/installation/repositories", None)
        .await;
    let ids: Vec<u64> = repos.body["repositories"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["id"].as_u64().unwrap())
        .collect();
    assert_eq!(ids, [10, 11]);
    assert_eq!(
        world.permission("web", "creator").await["role_name"],
        "admin"
    );
    assert_eq!(world.permission("api", "alice").await["permission"], "none");
    let alice = world.api(Method::GET, "/users/alice", None).await;
    assert_eq!(alice.body["id"], 8);
    // Any installation ID can be the repository App's.
    let nine = World::with(compat::seed_with_installation(9), 9).await;
    assert!(nine.token.starts_with("ghs_123_"));
}

#[tokio::test]
async fn identity_renames_addresses_and_grants() {
    let world = World::start().await;
    let invited = world.put("api", "alice").await;
    assert_eq!(invited.status, 201);
    let set = world
        .compat(
            Method::POST,
            "identity",
            Some(json!({"login": "alice", "addressed_id": 8, "role_name": "write"})),
        )
        .await;
    assert_eq!(set.status, 204);
    let permission = world.permission("api", "alice").await;
    assert_eq!(
        (
            permission["role_name"].as_str(),
            permission["permission"].as_str()
        ),
        (Some("write"), Some("write"))
    );
    // The pending invitation was accepted.
    let invitations = world
        .api(Method::GET, "/repos/acme/api/invitations", None)
        .await;
    assert_eq!(invitations.body, json!([]));

    // A login now held by someone else: /user/8 and /users/renamed disagree,
    // as the stub answered.
    world
        .compat(
            Method::POST,
            "identity",
            Some(json!({"login": "renamed", "addressed_id": 99})),
        )
        .await;
    let by_id = world.send(Method::GET, "/user/8", None, false).await;
    assert_eq!(
        (by_id.body["id"].as_u64(), by_id.body["login"].as_str()),
        (Some(8), Some("renamed"))
    );
    let by_login = world.send(Method::GET, "/users/renamed", None, false).await;
    assert_eq!(by_login.body["id"], 99);
    assert_eq!(
        world.permission("api", "renamed").await["role_name"],
        "none"
    );

    // User 8 renamed: the login is theirs again, writes reach them.
    world
        .compat(
            Method::POST,
            "identity",
            Some(json!({"login": "renamed", "addressed_id": 8})),
        )
        .await;
    assert_eq!(world.put("web", "renamed").await.status, 201);
    let invitations = world
        .api(Method::GET, "/repos/acme/web/invitations", None)
        .await;
    assert_eq!(invitations.body[0]["invitee"]["id"], 8);

    let calls = world.emulator.calls();
    assert!(
        calls
            .iter()
            .any(|c| c.path == "/user/8" && c.fault.is_some())
    );
}

#[tokio::test]
async fn outcomes_shape_collaborator_writes() {
    let world = World::start().await;
    let set = |outcome: &'static str, repo: &'static str| {
        let world = &world;
        async move {
            let reply = world
                .compat(
                    Method::POST,
                    "outcomes",
                    Some(
                        json!({"owner": "acme", "repo": repo, "user": "alice", "outcome": outcome}),
                    ),
                )
                .await;
            assert_eq!(reply.status, 204, "{outcome}");
        }
    };
    set("transient_once", "api").await;
    assert_eq!(world.put("api", "alice").await.status, 502);
    assert_eq!(world.put("api", "alice").await.status, 201);

    set("throttled_once", "web").await;
    let throttled = world.put("web", "alice").await;
    assert_eq!(throttled.status, 403);
    assert_eq!(throttled.headers["retry-after"], "1");
    assert_eq!(
        throttled.body["message"],
        "You have exceeded a secondary rate limit"
    );
    assert_eq!(world.put("web", "alice").await.status, 201);

    set("terminal_failure", "api").await;
    assert_eq!(world.put("api", "alice").await.status, 422);
    assert_eq!(world.put("api", "alice").await.status, 422);

    // Replacing the triple's outcome replaces its rule.
    set("access_lost_once", "api").await;
    assert_eq!(world.put("api", "alice").await.status, 403);
    assert_eq!(world.put("api", "alice").await.status, 201);

    set("already_collaborator", "api").await;
    assert_eq!(world.put("api", "alice").await.status, 204);
    assert_eq!(world.put("api", "alice").await.status, 204);

    let reset = world.compat(Method::DELETE, "reset", None).await;
    assert_eq!(reset.status, 200);
    set("created_response_lost", "api").await;
    assert_eq!(world.put("api", "alice").await.status, 502);
    let invitations = world
        .api(Method::GET, "/repos/acme/api/invitations", None)
        .await;
    assert_eq!(invitations.body.as_array().unwrap().len(), 1);

    set("created_then_declined", "web").await;
    assert_eq!(world.put("web", "alice").await.status, 502);
    let invitations = world
        .api(Method::GET, "/repos/acme/web/invitations", None)
        .await;
    assert_eq!(invitations.body, json!([]));

    let ledger = world.compat(Method::GET, "calls", None).await.body;
    let puts: Vec<(String, Value, u64)> = ledger["requests"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["method"] == "PUT")
        .map(|c| {
            (
                c["path"].as_str().unwrap().to_owned(),
                c["permission"].clone(),
                c["status"].as_u64().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        puts,
        [
            (
                "/repos/acme/api/collaborators/alice".to_owned(),
                json!("push"),
                201
            ),
            (
                "/repos/acme/web/collaborators/alice".to_owned(),
                json!("push"),
                502
            ),
        ],
        "the lost write is logged with the status its handler chose"
    );
    assert_eq!(
        ledger["count"],
        ledger["requests"].as_array().unwrap().len()
    );
    // Reads, invitation lists and ledger-only routes: only the stub's set.
    assert!(
        ledger["requests"]
            .as_array()
            .unwrap()
            .iter()
            .all(|c| !c["path"].as_str().unwrap().contains('?'))
    );
}

#[tokio::test]
async fn installation_identity_repositories_expiry_and_reset() {
    let world = World::start().await;
    let jwt_installation = |world: &World| {
        let base = world.emulator.api_base().to_owned();
        async move {
            let app = AppClient::builder(
                compat::REPO_APP_ID,
                SigningKey::from_pem(PRIVATE_KEY).unwrap(),
            )
            .client_id(compat::REPO_CLIENT_ID)
            .base_url(&base)
            .build()
            .unwrap();
            app.installation(compat::REPO_INSTALLATION_ID)
                .get()
                .await
                .unwrap()
        }
    };
    let set = world
        .compat(
            Method::POST,
            "installation-identity",
            Some(json!({"id": 200, "login": "acme", "type": "Organization"})),
        )
        .await;
    assert_eq!(set.status, 204);
    assert_eq!(jwt_installation(&world).await.account.id, 200);
    world
        .compat(
            Method::POST,
            "installation-identity",
            Some(json!({"id": 100, "login": "acme", "type": "Organization"})),
        )
        .await;
    assert_eq!(jwt_installation(&world).await.account.id, 100);

    let set = world
        .compat(
            Method::POST,
            "installation-repositories",
            Some(json!([10, 12])),
        )
        .await;
    assert_eq!(set.status, 204);
    let repos = world
        .api(Method::GET, "/installation/repositories", None)
        .await;
    let names: Vec<&str> = repos.body["repositories"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["full_name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["acme/api", "acme/repo-12"]);

    let invited = world.put("api", "alice").await;
    let id = invited.body["id"].as_u64().unwrap();
    let expire = |expired: bool| {
        world.compat(
            Method::POST,
            "invitation-expired",
            Some(json!({"owner": "acme", "repo": "api", "id": id, "expired": expired})),
        )
    };
    assert_eq!(expire(true).await.status, 204);
    let listed = world
        .api(Method::GET, "/repos/acme/api/invitations", None)
        .await;
    assert_eq!(listed.body[0]["expired"], true);
    expire(false).await;
    let listed = world
        .api(Method::GET, "/repos/acme/api/invitations", None)
        .await;
    assert_eq!(listed.body[0]["expired"], false);
    let missing = world
        .compat(
            Method::POST,
            "invitation-expired",
            Some(json!({"owner": "acme", "repo": "api", "id": 999_999, "expired": true})),
        )
        .await;
    assert_eq!(missing.status, 404);

    world
        .compat(
            Method::POST,
            "identity",
            Some(json!({"login": "someone", "addressed_id": 8, "role_name": "admin"})),
        )
        .await;
    assert_eq!(
        world.compat(Method::DELETE, "reset", None).await.status,
        200
    );
    assert_eq!(
        world.compat(Method::GET, "calls", None).await.body["count"],
        0
    );
    let alice = world.send(Method::GET, "/users/alice", None, false).await;
    assert_eq!(alice.body["id"], 8);
    assert_eq!(world.permission("api", "alice").await["role_name"], "none");
    assert_eq!(
        world
            .api(Method::GET, "/repos/acme/api/invitations", None)
            .await
            .body,
        json!([])
    );
    let repos = world
        .api(Method::GET, "/installation/repositories", None)
        .await;
    assert_eq!(repos.body["total_count"], 3);
    // Invitation IDs are never reused.
    let again = world.put("api", "alice").await;
    assert!(again.body["id"].as_u64().unwrap() > id);
}
