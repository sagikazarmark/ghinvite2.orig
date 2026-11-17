//! The repository surface (GE-03) over HTTP: repositories, collaborators,
//! permissions, invitations and their alias, expiry and the invitation limit
//! on the virtual clock, the invitee's side through the admin `act` routes,
//! the event outbox, users and avatars.
#![cfg(not(target_arch = "wasm32"))]

use chrono::{DateTime, Utc};
use octoemu::Emulator;
use octoemu::events::Target;
use octoemu::seed::{
    Access, App, BasePermission, BaseRole, Collaborator, Installation, Membership, Org, OrgRole,
    Repo, RepositoryInvitation, RepositoryRole, Role, Seed, User,
};
use reqwest::{Client, Method, StatusCode};
use serde_json::{Value, json};
use std::time::Duration;

const PUBLIC_KEY: &str = include_str!("fixtures/rsa2048.pub.pem");

const DAY: u64 = 24 * 3600;

fn start_2030() -> DateTime<Utc> {
    "2030-01-01T00:00:00Z".parse().unwrap()
}

/// App 4 (`invite-bot`) installed on acme (5: widgets, big, limits) and on
/// octocat (6: every repository). acme's base permission is read; orgdev
/// is a member, boss an owner. Users u1001… exist for bulk invitations.
fn seed() -> Seed {
    let mut seed = Seed::new()
        .start_time(start_2030())
        .user(User::new(1, "octocat"))
        .user(User::new(3, "hubot"))
        .user(User::new(11, "monalisa"))
        .user(User::new(12, "mallory"))
        .user(User::new(13, "orgdev"))
        .user(User::new(14, "boss"))
        .org(Org::new(2, "acme").default_repository_permission(BasePermission::Read))
        .repo(Repo::new(101, "acme", "widgets").private())
        .repo(Repo::new(102, "acme", "public"))
        .repo(Repo::new(103, "acme", "secret").private())
        .repo(Repo::new(104, "acme", "big").private())
        .repo(Repo::new(105, "acme", "limits").private())
        .repo(Repo::new(201, "octocat", "dotfiles"))
        .repository_role(RepositoryRole::new(
            70,
            "acme",
            "auditor",
            BaseRole::Maintain,
        ))
        .app(
            App::new(4, "invite-bot", "Iv23.fixture", "fixture-client-secret")
                .public_key(PUBLIC_KEY)
                .permission("administration", Access::Write)
                .permission("metadata", Access::Read)
                .permission("members", Access::Write)
                .event("member"),
        )
        .installation(Installation::new(5, 4, "acme").selected([
            "acme/widgets",
            "acme/big",
            "acme/limits",
        ]))
        .installation(Installation::new(6, 4, "octocat"))
        .membership(Membership::new("acme", "orgdev", OrgRole::Member))
        .membership(Membership::new("acme", "boss", OrgRole::Admin))
        .collaborator(Collaborator::new("acme/widgets", "monalisa", Role::Push));
    for n in 1..=130 {
        seed = seed.user(User::new(1000 + n, format!("u{n:03}")));
    }
    seed
}

struct Reply {
    status: StatusCode,
    headers: reqwest::header::HeaderMap,
    body: Value,
}

impl Reply {
    fn link(&self) -> Option<&str> {
        self.headers.get("link").map(|l| l.to_str().unwrap())
    }
}

async fn send(method: Method, url: &str, token: Option<&str>, body: Option<String>) -> Reply {
    let mut request = Client::new().request(method, url);
    if let Some(token) = token {
        request = request.bearer_auth(token);
    }
    if let Some(body) = body {
        request = request
            .header("content-type", "application/json")
            .body(body);
    }
    let response = request.send().await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let text = response.text().await.unwrap();
    let body = if text.is_empty() {
        Value::Null
    } else {
        serde_json::from_str(&text).unwrap_or(Value::String(text))
    };
    Reply {
        status,
        headers,
        body,
    }
}

/// An emulator and an installation token for acme (installation 5).
struct World {
    emulator: Emulator,
    token: String,
}

impl World {
    async fn start() -> Self {
        Self::with(Emulator::start(seed()).await.unwrap()).await
    }

    async fn with(emulator: Emulator) -> Self {
        let token = mint(&emulator, json!({"installation_id": 5})).await;
        Self { emulator, token }
    }

    /// Moves the clock and takes a fresh token (they last an hour).
    async fn advance(&mut self, seconds: u64) {
        self.emulator.advance_clock(Duration::from_secs(seconds));
        self.token = mint(&self.emulator, json!({"installation_id": 5})).await;
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.emulator.api_base())
    }

    async fn get(&self, path: &str) -> Reply {
        send(Method::GET, &self.url(path), Some(&self.token), None).await
    }

    async fn put(&self, path: &str, body: Value) -> Reply {
        send(
            Method::PUT,
            &self.url(path),
            Some(&self.token),
            Some(body.to_string()),
        )
        .await
    }

    async fn patch(&self, path: &str, body: Value) -> Reply {
        send(
            Method::PATCH,
            &self.url(path),
            Some(&self.token),
            Some(body.to_string()),
        )
        .await
    }

    async fn delete(&self, path: &str) -> Reply {
        send(Method::DELETE, &self.url(path), Some(&self.token), None).await
    }

    async fn invite(&self, repo: &str, user: &str, permission: &str) -> Reply {
        self.put(
            &format!("/repos/acme/{repo}/collaborators/{user}"),
            json!({"permission": permission}),
        )
        .await
    }

    async fn act(&self, path: &str) -> Reply {
        send(
            Method::POST,
            &format!("{}/act/{path}", self.emulator.admin_base()),
            None,
            None,
        )
        .await
    }

    async fn permission(&self, repo: &str, user: &str) -> Value {
        let reply = self
            .get(&format!(
                "/repos/acme/{repo}/collaborators/{user}/permission"
            ))
            .await;
        assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
        reply.body
    }
}

async fn mint(emulator: &Emulator, body: Value) -> String {
    let reply = send(
        Method::POST,
        &format!("{}/tokens/installation", emulator.admin_base()),
        None,
        Some(body.to_string()),
    )
    .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
    reply.body["token"].as_str().unwrap().to_owned()
}

#[tokio::test]
async fn repositories_follow_the_token_and_visibility() {
    let world = World::start().await;
    let widgets = world.get("/repos/acme/widgets").await;
    assert_eq!(widgets.status, StatusCode::OK);
    assert_eq!(widgets.body["id"], 101);
    assert_eq!(widgets.body["full_name"], "acme/widgets");
    assert_eq!(widgets.body["private"], true);
    assert_eq!(widgets.body["visibility"], "private");
    assert_eq!(widgets.body["owner"]["type"], "Organization");
    assert!(widgets.headers.contains_key("date"));
    // A public repository outside the installation is still visible …
    let public = world.get("/repos/acme/public").await;
    assert_eq!(public.status, StatusCode::OK);
    assert_eq!(public.body["visibility"], "public");
    // … a private one, or one that does not exist, is not.
    for path in [
        "/repos/acme/secret",
        "/repos/acme/missing",
        "/repos/nobody/x",
    ] {
        assert_eq!(
            world.get(path).await.status,
            StatusCode::NOT_FOUND,
            "{path}"
        );
    }
    // A token narrowed to `big` no longer sees `widgets`.
    let narrowed = mint(
        &world.emulator,
        json!({"installation_id": 5, "repository_ids": [104]}),
    )
    .await;
    let hidden = send(
        Method::GET,
        &world.url("/repos/acme/widgets"),
        Some(&narrowed),
        None,
    )
    .await;
    assert_eq!(hidden.status, StatusCode::NOT_FOUND);

    let call = world.emulator.calls().into_iter().next().unwrap();
    assert_eq!(call.route.as_deref(), Some("GET /repos/{owner}/{repo}"));
    assert!(matches!(
        call.identity,
        Some(octoemu::calls::Identity::Installation {
            installation_id: 5,
            ..
        })
    ));
}

#[tokio::test]
async fn put_creates_an_invitation_then_answers_204() {
    let world = World::start().await;
    let created = world.invite("widgets", "hubot", "triage").await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
    let invitation = &created.body;
    let id = invitation["id"].as_u64().unwrap();
    assert_eq!(invitation["invitee"]["login"], "hubot");
    assert_eq!(invitation["invitee"]["id"], 3);
    assert_eq!(invitation["inviter"]["login"], "invite-bot[bot]");
    assert_eq!(invitation["inviter"]["type"], "Bot");
    assert_eq!(invitation["permissions"], "triage");
    assert_eq!(invitation["expired"], false);
    assert_eq!(invitation["repository"]["id"], 101);
    assert_eq!(invitation["repository"]["full_name"], "acme/widgets");
    assert!(
        invitation["html_url"]
            .as_str()
            .unwrap()
            .ends_with("/acme/widgets/invitations")
    );
    let created_at: DateTime<Utc> = invitation["created_at"].as_str().unwrap().parse().unwrap();
    assert!(created_at >= start_2030());

    // Re-PUT on the pending invitation: the same invitation, new role
    // (assumed).
    let again = world.invite("widgets", "hubot", "admin").await;
    assert_eq!(again.status, StatusCode::CREATED);
    assert_eq!(again.body["id"], id);
    assert_eq!(again.body["permissions"], "admin");
    assert_eq!(again.body["created_at"], invitation["created_at"]);

    // An existing collaborator: 204, and a role change is 204 too.
    let same = world.invite("widgets", "monalisa", "push").await;
    assert_eq!(same.status, StatusCode::NO_CONTENT);
    assert_eq!(same.body, Value::Null);
    assert!(world.emulator.events().is_empty(), "no change, no event");
    let changed = world.invite("widgets", "monalisa", "maintain").await;
    assert_eq!(changed.status, StatusCode::NO_CONTENT);
    assert_eq!(
        world.permission("widgets", "monalisa").await["role_name"],
        "maintain"
    );
    let events = world.emulator.events();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].event, "member");
    assert_eq!(events[0].action.as_deref(), Some("edited"));
    assert_eq!(events[0].payload["member"]["login"], "monalisa");
    assert_eq!(
        events[0].payload["changes"],
        json!({"old_permission": {"from": "write"}, "permission": {"from": "write", "to": "maintain"}})
    );
    assert_eq!(events[0].payload["sender"]["login"], "invite-bot[bot]");

    // An organization member is added directly: 204, no invitation.
    let member = world
        .put("/repos/acme/widgets/collaborators/orgdev", json!({}))
        .await;
    assert_eq!(member.status, StatusCode::NO_CONTENT);
    let permission = world.permission("widgets", "orgdev").await;
    assert_eq!(permission["role_name"], "write", "default push");
    let direct = world
        .get("/repos/acme/widgets/collaborators?affiliation=direct")
        .await;
    let logins: Vec<&str> = direct
        .body
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["login"].as_str().unwrap())
        .collect();
    assert_eq!(logins, ["monalisa", "orgdev"]);
    assert_eq!(world.emulator.events()[1].action.as_deref(), Some("added"));

    // The default permission is push (shown as write).
    let default = world
        .put("/repos/acme/widgets/collaborators/mallory", json!({}))
        .await;
    assert_eq!(default.status, StatusCode::CREATED);
    assert_eq!(default.body["permissions"], "write");
    let empty = send(
        Method::PUT,
        &world.url("/repos/acme/widgets/collaborators/u001"),
        Some(&world.token),
        None,
    )
    .await;
    assert_eq!(empty.status, StatusCode::CREATED);
    assert_eq!(empty.body["permissions"], "write");

    // An unknown user is a 404 (assumed).
    assert_eq!(
        world.invite("widgets", "ghost", "push").await.status,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn a_user_repository_ignores_the_permission() {
    let emulator = Emulator::start(seed()).await.unwrap();
    let token = mint(&emulator, json!({"installation_id": 6})).await;
    let reply = send(
        Method::PUT,
        &format!(
            "{}/repos/octocat/dotfiles/collaborators/hubot",
            emulator.api_base()
        ),
        Some(&token),
        Some(json!({"permission": "admin"}).to_string()),
    )
    .await;
    assert_eq!(reply.status, StatusCode::CREATED);
    assert_eq!(reply.body["permissions"], "write");
    // The owner has admin.
    let owner = send(
        Method::GET,
        &format!(
            "{}/repos/octocat/dotfiles/collaborators/octocat/permission",
            emulator.api_base()
        ),
        Some(&token),
        None,
    )
    .await;
    assert_eq!(owner.body["permission"], "admin");
}

#[tokio::test]
async fn permissions_map_roles_and_take_the_highest_source() {
    let emulator = Emulator::start(
        seed()
            .collaborator(Collaborator::new("acme/big", "u001", Role::Pull))
            .collaborator(Collaborator::new("acme/big", "u002", Role::Triage))
            .collaborator(Collaborator::new("acme/big", "u003", Role::Push))
            .collaborator(Collaborator::new("acme/big", "u004", Role::Maintain))
            .collaborator(Collaborator::new("acme/big", "u005", Role::Admin))
            .collaborator(Collaborator::new(
                "acme/big",
                "u006",
                Role::Custom("auditor".into()),
            ))
            .collaborator(Collaborator::new("acme/big", "boss", Role::Triage)),
    )
    .await
    .unwrap();
    let world = World::with(emulator).await;
    for (user, permission, role_name) in [
        ("u001", "read", "read"),
        ("u002", "read", "triage"),
        ("u003", "write", "write"),
        ("u004", "write", "maintain"),
        ("u005", "admin", "admin"),
        ("u006", "write", "auditor"),
        // Triage directly, admin as an organization owner.
        ("boss", "admin", "admin"),
        // Read through acme's base permission.
        ("orgdev", "read", "read"),
        // No access (assumed shape).
        ("mallory", "none", "none"),
    ] {
        let body = world.permission("big", user).await;
        assert_eq!(body["permission"], permission, "{user}");
        assert_eq!(body["role_name"], role_name, "{user}");
        assert_eq!(body["user"]["login"], user);
        assert_eq!(body["user"]["role_name"], role_name, "{user}");
    }
    let custom = world.permission("big", "u006").await;
    assert_eq!(
        custom["user"]["permissions"],
        json!({"admin": false, "maintain": true, "push": true, "triage": true, "pull": true})
    );
    assert_eq!(
        world
            .get("/repos/acme/big/collaborators/ghost/permission")
            .await
            .status,
        StatusCode::NOT_FOUND
    );

    // A custom role through an invitation and its acceptance.
    let invited = world.invite("big", "u010", "auditor").await;
    assert_eq!(invited.status, StatusCode::CREATED);
    assert_eq!(invited.body["permissions"], "auditor");
    // Pending: no access yet.
    assert_eq!(world.permission("big", "u010").await["permission"], "none");
    let id = invited.body["id"].as_u64().unwrap();
    let accepted = world
        .act(&format!("u010/repository_invitations/{id}/accept"))
        .await;
    assert_eq!(accepted.status, StatusCode::NO_CONTENT, "{}", accepted.body);
    let body = world.permission("big", "u010").await;
    assert_eq!(
        (&body["permission"], &body["role_name"]),
        (&json!("write"), &json!("auditor"))
    );
}

#[tokio::test]
async fn invalid_input_is_refused() {
    let world = World::start().await;
    for permission in [json!("read"), json!("write"), json!("owner"), json!(5)] {
        let reply = world
            .put(
                "/repos/acme/widgets/collaborators/hubot",
                json!({"permission": permission}),
            )
            .await;
        assert_eq!(
            reply.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{permission}"
        );
        assert_eq!(reply.body["message"], "Validation Failed");
        assert_eq!(reply.body["errors"][0]["field"], "permission");
    }
    let malformed = send(
        Method::PUT,
        &world.url("/repos/acme/widgets/collaborators/hubot"),
        Some(&world.token),
        Some("{".into()),
    )
    .await;
    assert_eq!(malformed.status, StatusCode::BAD_REQUEST);
    assert_eq!(malformed.body["message"], "Problems parsing JSON");

    let id = world.invite("widgets", "hubot", "push").await.body["id"]
        .as_u64()
        .unwrap();
    for permissions in ["push", "pull", "auditor"] {
        let reply = world
            .patch(
                &format!("/repos/acme/widgets/invitations/{id}"),
                json!({"permissions": permissions}),
            )
            .await;
        assert_eq!(
            reply.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{permissions}"
        );
        assert_eq!(reply.body["errors"][0]["field"], "permissions");
    }
    let bogus = world
        .get("/repos/acme/widgets/collaborators?affiliation=bogus")
        .await;
    assert_eq!(bogus.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(world.emulator.events().is_empty());
}

#[tokio::test]
async fn collaborators_are_paged_with_their_roles() {
    let mut seed = seed().membership(Membership::new("acme", "u035", OrgRole::Member));
    for n in 1..=35 {
        let role = if n == 7 {
            Role::Custom("auditor".into())
        } else {
            Role::Pull
        };
        seed = seed.collaborator(Collaborator::new("acme/big", format!("u{n:03}"), role));
    }
    let world = World::with(Emulator::start(seed).await.unwrap()).await;

    let first = world
        .get("/repos/acme/big/collaborators?affiliation=direct")
        .await;
    assert_eq!(first.status, StatusCode::OK);
    let rows = first.body.as_array().unwrap();
    assert_eq!(rows.len(), 30, "30 per page by default");
    assert_eq!(rows[0]["login"], "u001");
    assert_eq!(rows[0]["role_name"], "read");
    assert_eq!(rows[0]["permissions"]["pull"], true);
    assert_eq!(rows[0]["permissions"]["push"], false);
    assert_eq!(rows[6]["role_name"], "auditor");
    let origin = world.emulator.api_base();
    assert_eq!(
        first.link().unwrap(),
        format!(
            "<{origin}/repos/acme/big/collaborators?affiliation=direct&page=2>; rel=\"next\", \
             <{origin}/repos/acme/big/collaborators?affiliation=direct&page=2>; rel=\"last\""
        )
    );
    let second = world
        .get("/repos/acme/big/collaborators?affiliation=direct&page=2")
        .await;
    assert_eq!(second.body.as_array().unwrap().len(), 5);
    let capped = world
        .get("/repos/acme/big/collaborators?affiliation=direct&per_page=500")
        .await;
    assert_eq!(capped.body.as_array().unwrap().len(), 35);
    assert_eq!(capped.link(), None);

    // `outside` leaves out organization members; `all` adds everyone with
    // access through the organization (assumed).
    let outside = world
        .get("/repos/acme/big/collaborators?affiliation=outside&per_page=100")
        .await;
    let outside: Vec<&str> = outside
        .body
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["login"].as_str().unwrap())
        .collect();
    assert_eq!(outside.len(), 34);
    assert!(!outside.contains(&"u035"));
    let all = world
        .get("/repos/acme/big/collaborators?per_page=100")
        .await;
    let all: Vec<(String, String)> = all
        .body
        .as_array()
        .unwrap()
        .iter()
        .map(|c| {
            (
                c["login"].as_str().unwrap().into(),
                c["role_name"].as_str().unwrap().into(),
            )
        })
        .collect();
    assert_eq!(all.len(), 37);
    assert!(all.contains(&("boss".into(), "admin".into())));
    assert!(all.contains(&("orgdev".into(), "read".into())));
}

#[tokio::test]
async fn invitation_lists_page_onto_the_repository_id_alias() {
    let mut seed = seed();
    for n in 1..=120 {
        seed = seed.repository_invitation(
            RepositoryInvitation::new(5000 + n, "acme/big", format!("u{n:03}"), Role::Push)
                .inviter("boss"),
        );
    }
    let world = World::with(Emulator::start(seed).await.unwrap()).await;
    let origin = world.emulator.api_base().to_owned();

    let first = world.get("/repos/acme/big/invitations?per_page=100").await;
    assert_eq!(first.status, StatusCode::OK);
    let rows = first.body.as_array().unwrap();
    assert_eq!(rows.len(), 100);
    assert_eq!(rows[0]["id"], 5001);
    assert_eq!(rows[0]["repository"]["id"], 104);
    assert_eq!(rows[0]["repository"]["full_name"], "acme/big");
    assert_eq!(rows[0]["inviter"]["login"], "boss");
    assert_eq!(rows[0]["permissions"], "write");
    let alias = format!("{origin}/repositories/104/invitations?per_page=100");
    assert_eq!(
        first.link().unwrap(),
        format!("<{alias}&page=2>; rel=\"next\", <{alias}&page=2>; rel=\"last\"")
    );

    let second = send(
        Method::GET,
        &format!("{alias}&page=2"),
        Some(&world.token),
        None,
    )
    .await;
    assert_eq!(second.status, StatusCode::OK);
    assert_eq!(second.body.as_array().unwrap().len(), 20);
    assert_eq!(second.body[19]["id"], 5120);
    assert_eq!(
        second.link().unwrap(),
        format!("<{alias}&page=1>; rel=\"prev\", <{alias}&page=1>; rel=\"first\"")
    );
    let default = world.get("/repositories/104/invitations").await;
    assert_eq!(default.body.as_array().unwrap().len(), 30);
    assert!(
        default
            .link()
            .unwrap()
            .contains("/repositories/104/invitations?page=2")
    );

    // The alias follows the same visibility rules.
    for path in [
        "/repositories/103/invitations",
        "/repositories/999/invitations",
        "/repositories/big/invitations",
    ] {
        assert_eq!(
            world.get(path).await.status,
            StatusCode::NOT_FOUND,
            "{path}"
        );
    }
    assert_eq!(
        world.get("/repositories/102/invitations").await.status,
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn invitations_can_be_edited_and_deleted() {
    let world = World::start().await;
    let id = world.invite("widgets", "hubot", "pull").await.body["id"]
        .as_u64()
        .unwrap();
    let other = world.invite("big", "mallory", "pull").await.body["id"]
        .as_u64()
        .unwrap();
    assert_ne!(id, other);

    let patched = world
        .patch(
            &format!("/repos/acme/widgets/invitations/{id}"),
            json!({"permissions": "maintain"}),
        )
        .await;
    assert_eq!(patched.status, StatusCode::OK);
    assert_eq!(patched.body["id"], id);
    assert_eq!(patched.body["permissions"], "maintain");
    let listed = world.get("/repos/acme/widgets/invitations").await;
    assert_eq!(listed.body[0]["permissions"], "maintain");
    // Another repository's invitation is not this one's.
    let wrong = world
        .patch(
            &format!("/repos/acme/widgets/invitations/{other}"),
            json!({"permissions": "read"}),
        )
        .await;
    assert_eq!(wrong.status, StatusCode::NOT_FOUND);
    assert_eq!(
        world
            .delete(&format!("/repos/acme/widgets/invitations/{other}"))
            .await
            .status,
        StatusCode::NOT_FOUND
    );

    let deleted = world
        .delete(&format!("/repos/acme/widgets/invitations/{id}"))
        .await;
    assert_eq!(deleted.status, StatusCode::NO_CONTENT);
    assert_eq!(
        world
            .delete(&format!("/repos/acme/widgets/invitations/{id}"))
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        world.get("/repos/acme/widgets/invitations").await.body,
        json!([])
    );
    // A new invitation never reuses a deleted one's ID.
    let next = world.invite("widgets", "hubot", "pull").await.body["id"]
        .as_u64()
        .unwrap();
    assert!(next > other);
}

#[tokio::test]
async fn invitations_expire_after_seven_days() {
    let mut world = World::start().await;
    let id = world.invite("widgets", "hubot", "push").await.body["id"]
        .as_u64()
        .unwrap();
    world.advance(7 * DAY - 5).await;
    assert_eq!(
        world.get("/repos/acme/widgets/invitations").await.body[0]["expired"],
        false
    );
    world.advance(5).await;
    let listed = world.get("/repos/acme/widgets/invitations").await;
    assert_eq!(listed.body[0]["id"], id);
    assert_eq!(listed.body[0]["expired"], true, "still listed (assumed)");

    let refused = world
        .act(&format!("hubot/repository_invitations/{id}/accept"))
        .await;
    assert_eq!(refused.status, StatusCode::NOT_FOUND);
    assert_eq!(
        refused.body["message"],
        "octoemu: the invitation has expired"
    );
    assert_eq!(
        world.permission("widgets", "hubot").await["permission"],
        "none"
    );

    // Re-PUT replaces it (assumed).
    let renewed = world.invite("widgets", "hubot", "push").await;
    assert_eq!(renewed.status, StatusCode::CREATED);
    assert_ne!(renewed.body["id"], id);
    assert_eq!(renewed.body["expired"], false);
    let listed = world.get("/repos/acme/widgets/invitations").await;
    assert_eq!(listed.body.as_array().unwrap().len(), 1);
    assert_eq!(listed.body[0]["id"], renewed.body["id"]);
}

#[tokio::test]
async fn the_admin_api_expires_one_invitation_and_renames_a_user() {
    let world = World::start().await;
    let first = world.invite("widgets", "hubot", "push").await.body["id"]
        .as_u64()
        .unwrap();
    let second = world.invite("big", "hubot", "pull").await.body["id"]
        .as_u64()
        .unwrap();
    let admin = |path: &str| format!("{}{path}", world.emulator.admin_base());
    let expired = send(
        Method::POST,
        &admin(&format!("/repository_invitations/{first}/expire")),
        None,
        None,
    )
    .await;
    assert_eq!(expired.status, StatusCode::NO_CONTENT);
    // Only that invitation; the clock stays where it was.
    assert_eq!(
        world.get("/repos/acme/widgets/invitations").await.body[0]["expired"],
        true
    );
    assert_eq!(
        world.get("/repos/acme/big/invitations").await.body[0]["expired"],
        false
    );
    let unknown = send(
        Method::POST,
        &admin("/repository_invitations/999999/expire"),
        None,
        None,
    )
    .await;
    assert_eq!(unknown.status, StatusCode::NOT_FOUND);

    // A renamed user keeps their ID and invitations; the old login is free.
    let renamed = send(
        Method::POST,
        &admin("/users/hubot/rename"),
        None,
        Some(json!({"login": "hubot-2"}).to_string()),
    )
    .await;
    assert_eq!(renamed.status, StatusCode::NO_CONTENT, "{}", renamed.body);
    let user = world.get("/users/hubot-2").await;
    assert_eq!(
        (user.body["id"].as_u64(), user.status),
        (Some(3), StatusCode::OK)
    );
    assert_eq!(
        world.get("/users/hubot").await.status,
        StatusCode::NOT_FOUND
    );
    let listed = world.get("/repos/acme/big/invitations").await;
    assert_eq!(listed.body[0]["id"], second);
    assert_eq!(listed.body[0]["invitee"]["login"], "hubot-2");
    let accepted = world
        .act(&format!("hubot-2/repository_invitations/{second}/accept"))
        .await;
    assert_eq!(accepted.status, StatusCode::NO_CONTENT);
    assert_eq!(
        world.permission("big", "hubot-2").await["permission"],
        "read"
    );
    for (login, body, status) in [
        ("nobody", json!({"login": "x"}), StatusCode::NOT_FOUND),
        (
            "hubot-2",
            json!({"login": "octocat"}),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (
            "hubot-2",
            json!({"login": "bad name"}),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (
            "octocat",
            json!({"login": "octo"}),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
    ] {
        let reply = send(
            Method::POST,
            &admin(&format!("/users/{login}/rename")),
            None,
            Some(body.to_string()),
        )
        .await;
        assert_eq!(reply.status, status, "{login}: {}", reply.body);
    }
}

#[tokio::test]
async fn fifty_invitations_per_repository_per_day() {
    let mut world = World::start().await;
    for n in 1..=50 {
        let reply = world.invite("limits", &format!("u{n:03}"), "pull").await;
        assert_eq!(reply.status, StatusCode::CREATED, "invitation {n}");
    }
    let over = world.invite("limits", "u051", "pull").await;
    assert_eq!(over.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(over.body["message"], "Validation Failed");
    // Other repositories have their own budget.
    assert_eq!(
        world.invite("widgets", "u051", "pull").await.status,
        StatusCode::CREATED
    );
    // Deleting one does not give it back; re-PUT on a pending one is free.
    let first = world.get("/repos/acme/limits/invitations").await.body[0]["id"].clone();
    world
        .delete(&format!("/repos/acme/limits/invitations/{first}"))
        .await;
    assert_eq!(
        world.invite("limits", "u052", "pull").await.status,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert_eq!(
        world.invite("limits", "u002", "admin").await.status,
        StatusCode::CREATED
    );

    world.advance(DAY - 1).await;
    assert_eq!(
        world.invite("limits", "u052", "pull").await.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "a sliding 24 hours"
    );
    world.advance(1).await;
    assert_eq!(
        world.invite("limits", "u052", "pull").await.status,
        StatusCode::CREATED
    );
}

#[tokio::test]
async fn the_invitee_accepts_declines_and_leaves_through_the_admin_api() {
    let world = World::start().await;
    let accept = world.invite("widgets", "hubot", "maintain").await.body["id"]
        .as_u64()
        .unwrap();
    let decline = world.invite("widgets", "mallory", "pull").await.body["id"]
        .as_u64()
        .unwrap();

    // Only the invitee can act on an invitation.
    assert_eq!(
        world
            .act(&format!("mallory/repository_invitations/{accept}/accept"))
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    let accepted = world
        .act(&format!("HUBOT/repository_invitations/{accept}/accept"))
        .await;
    assert_eq!(accepted.status, StatusCode::NO_CONTENT);
    let body = world.permission("widgets", "hubot").await;
    assert_eq!(
        (&body["permission"], &body["role_name"]),
        (&json!("write"), &json!("maintain"))
    );
    let declined = world
        .act(&format!("mallory/repository_invitations/{decline}/decline"))
        .await;
    assert_eq!(declined.status, StatusCode::NO_CONTENT);
    assert_eq!(
        world.get("/repos/acme/widgets/invitations").await.body,
        json!([])
    );
    assert_eq!(
        world.permission("widgets", "mallory").await["permission"],
        "none"
    );
    assert_eq!(
        world
            .act(&format!("hubot/repository_invitations/{accept}/accept"))
            .await
            .status,
        StatusCode::NOT_FOUND,
        "gone once accepted"
    );

    // DELETE removes a collaborator; removing a non-collaborator is a 204
    // (assumed).
    let removed = world
        .delete("/repos/acme/widgets/collaborators/monalisa")
        .await;
    assert_eq!(removed.status, StatusCode::NO_CONTENT);
    assert_eq!(
        world.permission("widgets", "monalisa").await["permission"],
        "none"
    );
    let again = world
        .delete("/repos/acme/widgets/collaborators/monalisa")
        .await;
    assert_eq!(again.status, StatusCode::NO_CONTENT);
    let left = world.act("hubot/repos/acme/widgets/leave").await;
    assert_eq!(left.status, StatusCode::NO_CONTENT);
    assert_eq!(
        world.act("hubot/repos/acme/widgets/leave").await.status,
        StatusCode::NOT_FOUND
    );

    // The outbox: added (accepted), removed (by the App), removed (left).
    let events = world.emulator.events();
    let summary: Vec<(u64, &str, &str, &str)> = events
        .iter()
        .map(|e| {
            (
                e.id,
                e.action.as_deref().unwrap(),
                e.payload["member"]["login"].as_str().unwrap(),
                e.payload["sender"]["login"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        summary,
        [
            (1, "added", "hubot", "hubot"),
            (2, "removed", "monalisa", "invite-bot[bot]"),
            (3, "removed", "hubot", "hubot"),
        ]
    );
    let added = &events[0];
    assert_eq!(added.event, "member");
    assert_eq!(
        added.targets,
        [Target {
            app_id: 4,
            installation_id: 5
        }]
    );
    assert_eq!(
        added.payload["changes"],
        json!({"permission": {"to": "write"}, "role_name": {"to": "maintain"}})
    );
    assert_eq!(added.payload["repository"]["id"], 101);
    assert_eq!(added.payload["organization"]["login"], "acme");
    assert!(added.payload.get("installation").is_none());
    assert!(added.at >= start_2030());

    // The same outbox over the admin API.
    let served = send(
        Method::GET,
        &format!("{}/events", world.emulator.admin_base()),
        None,
        None,
    )
    .await;
    assert_eq!(served.status, StatusCode::OK);
    assert_eq!(
        served.body["events"],
        serde_json::to_value(&events).unwrap()
    );
    assert_eq!(served.body["events"][0]["event"], "member");
    assert_eq!(served.body["events"][0]["action"], "added");
}

#[tokio::test]
async fn routes_need_their_permission_and_scope() {
    let world = World::start().await;
    let metadata_only = mint(
        &world.emulator,
        json!({"installation_id": 5, "permissions": {"metadata": "read"}}),
    )
    .await;
    let call = |method: Method, path: &str| {
        let url = world.url(path);
        let token = metadata_only.clone();
        async move { send(method, &url, Some(&token), Some("{}".into())).await }
    };
    for (method, path, accepted) in [
        (
            Method::PUT,
            "/repos/acme/widgets/collaborators/hubot",
            "administration=write",
        ),
        (
            Method::DELETE,
            "/repos/acme/widgets/collaborators/monalisa",
            "administration=write",
        ),
        (
            Method::GET,
            "/repos/acme/widgets/invitations",
            "administration=read",
        ),
        (
            Method::GET,
            "/repositories/101/invitations",
            "administration=read",
        ),
        (
            Method::PATCH,
            "/repos/acme/widgets/invitations/1",
            "administration=write",
        ),
        (
            Method::DELETE,
            "/repos/acme/widgets/invitations/1",
            "administration=write",
        ),
    ] {
        let reply = call(method.clone(), path).await;
        assert_eq!(reply.status, StatusCode::FORBIDDEN, "{method} {path}");
        assert_eq!(
            reply.body["message"],
            "Resource not accessible by integration"
        );
        assert_eq!(reply.headers["x-accepted-github-permissions"], accepted);
    }
    // metadata: read is enough to read permissions and collaborators.
    assert_eq!(
        call(
            Method::GET,
            "/repos/acme/widgets/collaborators/monalisa/permission"
        )
        .await
        .status,
        StatusCode::OK
    );
    assert_eq!(
        call(Method::GET, "/repos/acme/widgets/collaborators")
            .await
            .status,
        StatusCode::OK
    );

    // Private outside the token's repositories: 404; public: 403.
    assert_eq!(
        world
            .put("/repos/acme/secret/collaborators/hubot", json!({}))
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        world
            .patch("/repos/acme/secret/invitations/1", json!({}))
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        world.get("/repos/acme/secret/invitations").await.status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        world
            .get("/repos/acme/secret/collaborators/hubot/permission")
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    let public = world.invite("public", "hubot", "push").await;
    assert_eq!(public.status, StatusCode::FORBIDDEN);
    assert_eq!(
        public.headers["x-accepted-github-permissions"],
        "administration=write"
    );
    // No credential at all.
    let anonymous = send(
        Method::GET,
        &world.url("/repos/acme/widgets/invitations"),
        None,
        None,
    )
    .await;
    assert_eq!(anonymous.status, StatusCode::UNAUTHORIZED);
    assert!(world.emulator.events().is_empty());
}

#[tokio::test]
async fn users_and_avatars() {
    let world = World::start().await;
    let user = send(Method::GET, &world.url("/users/OctoCat"), None, None).await;
    assert_eq!(user.status, StatusCode::OK);
    assert_eq!(user.body["id"], 1);
    assert_eq!(user.body["login"], "octocat");
    assert_eq!(user.body["type"], "User");
    assert_eq!(user.body["created_at"], "2030-01-01T00:00:00Z");
    let avatar_url = user.body["avatar_url"].as_str().unwrap().to_owned();
    assert!(
        avatar_url.starts_with(world.emulator.avatar_base()),
        "{avatar_url}"
    );
    let org = world.get("/users/acme").await;
    assert_eq!(org.body["type"], "Organization");
    assert_eq!(
        world.get("/users/ghost").await.status,
        StatusCode::NOT_FOUND
    );

    let by_id = world.get("/user/3").await;
    assert_eq!(by_id.status, StatusCode::OK);
    assert_eq!(by_id.body["login"], "hubot");
    for path in ["/user/999", "/user/hubot"] {
        assert_eq!(
            world.get(path).await.status,
            StatusCode::NOT_FOUND,
            "{path}"
        );
    }

    let image = Client::new().get(&avatar_url).send().await.unwrap();
    assert_eq!(image.status(), StatusCode::OK);
    assert_eq!(image.headers()["content-type"], "image/png");
    let bytes = image.bytes().await.unwrap();
    assert!(bytes.starts_with(b"\x89PNG\r\n\x1a\n"));
    assert!(bytes.ends_with(b"IEND\xaeB`\x82"));
    let missing = Client::new()
        .get(format!("{}/u/999", world.emulator.avatar_base()))
        .send()
        .await
        .unwrap();
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
    // The invitation's App bot inviter has an avatar too.
    let invitation = world.invite("widgets", "hubot", "push").await;
    let bot = invitation.body["inviter"]["avatar_url"].as_str().unwrap();
    assert_eq!(
        Client::new().get(bot).send().await.unwrap().status(),
        StatusCode::OK
    );
}

#[tokio::test]
async fn refuse_assumed_mode_refuses_the_guesses() {
    let emulator = Emulator::builder(seed())
        .refuse_assumed()
        .start()
        .await
        .unwrap();
    let world = World::with(emulator).await;
    // Documented outcomes work.
    let created = world.invite("widgets", "hubot", "push").await;
    assert_eq!(created.status, StatusCode::CREATED);
    assert_eq!(
        world.invite("widgets", "monalisa", "admin").await.status,
        StatusCode::NO_CONTENT
    );
    let edited = &world.emulator.events()[0];
    assert_eq!(edited.action.as_deref(), Some("edited"));
    assert!(
        edited.payload.get("changes").is_none(),
        "assumed shape left out"
    );

    // A re-PUT on a pending invitation rests on a guess.
    let again = world.invite("widgets", "hubot", "push").await;
    assert_eq!(again.status, StatusCode::NOT_IMPLEMENTED);
    assert_eq!(again.body["behaviour"], "reput-pending-invitation");
    // The status of an invalid permission is documented, its body is not.
    let invalid = world.invite("widgets", "mallory", "read").await;
    assert_eq!(invalid.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(invalid.body["message"], "octoemu: assumed message withheld");
    // Assumed routes and affiliations.
    let by_id = world.get("/user/1").await;
    assert_eq!(by_id.status, StatusCode::NOT_IMPLEMENTED);
    assert_eq!(by_id.body["route"], "GET /user/{account_id}");
    assert_eq!(
        world.get("/repos/acme/widgets/collaborators").await.status,
        StatusCode::NOT_IMPLEMENTED
    );
    assert_eq!(
        world
            .get("/repos/acme/widgets/collaborators?affiliation=direct")
            .await
            .status,
        StatusCode::OK
    );
    assert_eq!(
        world
            .get("/repos/acme/widgets/collaborators/mallory/permission")
            .await
            .body["behaviour"],
        "permission-without-access"
    );
}
