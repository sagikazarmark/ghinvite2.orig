//! The organization surface (GE-06) over HTTP: the organization, members
//! and memberships, invitations by user and by email with teams, failed
//! invitations and expiry on the clock, the invitation budget, seats,
//! teams and team memberships, team repository grants, the user's side
//! (accept through the real user route, the admin `act` routes), the
//! `organization` and `membership` events (delivered), permission and
//! scope refusals and refuse-assumed mode.
#![cfg(not(target_arch = "wasm32"))]

use axum::body::Bytes;
use axum::extract::State;
use chrono::{DateTime, Utc};
use octoemu::Emulator;
use octoemu::seed::{
    Access, App, BasePermission, Email, Installation, InvitationRole, Membership, Org,
    OrgInvitation, OrgRole, Plan, Repo, Role, Seed, Team, TeamMembership, TeamRole, User, Webhook,
};
use octoemu::webhooks::Dispatch;
use reqwest::{Client, Method, StatusCode};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const PUBLIC_KEY: &str = include_str!("fixtures/rsa2048.pub.pem");
const DAY: u64 = 24 * 3600;
const WAIT: Duration = Duration::from_secs(10);

fn start_2030() -> DateTime<Utc> {
    "2030-01-01T00:00:00Z".parse().unwrap()
}

/// App 4 (`org-bot`, members write, organization events) installed on acme
/// (5) and on widgetco (6). acme: created two years before the clock,
/// free plan; boss owns it, dev is a member; teams core (10, write on
/// acme/widgets) and core-child (11, nested), docs (12, secret). Users
/// invitee (with a verified address), outsider, u001… exist.
fn seed() -> Seed {
    let mut seed = Seed::new()
        .start_time(start_2030())
        .user(User::new(1, "boss"))
        .user(User::new(2, "dev"))
        .user(
            User::new(3, "invitee")
                .email_address(
                    Email::new("invitee@example.test")
                        .primary(octoemu::seed::EmailVisibility::Private),
                )
                .email_address(Email::new("unverified@example.test").unverified()),
        )
        .user(User::new(4, "outsider"))
        .org(
            Org::new(100, "acme")
                .name("Acme")
                .created_at("2028-01-01T00:00:00Z".parse().unwrap())
                .default_repository_permission(BasePermission::None)
                .two_factor_required(),
        )
        .org(Org::new(200, "widgetco"))
        .repo(Repo::new(1000, "acme", "widgets").private())
        .team(
            Team::new(10, "acme", "core")
                .name("Core")
                .repository("acme/widgets", Role::Push),
        )
        .team(Team::new(11, "acme", "core-child").parent("core"))
        .team(Team::new(12, "acme", "docs").privacy(octoemu::seed::TeamPrivacy::Secret))
        .app(
            App::new(4, "org-bot", "Iv23.org", "fixture-client-secret")
                .public_key(PUBLIC_KEY)
                .permission("members", Access::Write)
                .permission("metadata", Access::Read)
                .permission("organization_plan", Access::Read)
                .permission("organization_administration", Access::Read)
                .event("organization")
                .event("membership"),
        )
        .installation(Installation::new(5, 4, "acme"))
        .installation(Installation::new(6, 4, "widgetco"))
        .membership(Membership::new("acme", "boss", OrgRole::Admin))
        .membership(Membership::new("acme", "dev", OrgRole::Member))
        .team_membership(TeamMembership::new(
            "acme/core",
            "dev",
            TeamRole::Maintainer,
        ));
    for n in 1..=40 {
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

async fn send(method: Method, url: &str, token: Option<&str>, body: Option<Value>) -> Reply {
    let mut request = Client::new().request(method, url);
    if let Some(token) = token {
        request = request.bearer_auth(token);
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

async fn mint(emulator: &Emulator, request: Value) -> String {
    let reply = send(
        Method::POST,
        &format!("{}/tokens/installation", emulator.admin_base()),
        None,
        Some(request),
    )
    .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
    reply.body["token"].as_str().unwrap().to_owned()
}

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

    async fn post(&self, path: &str, body: Value) -> Reply {
        send(Method::POST, &self.url(path), Some(&self.token), Some(body)).await
    }

    async fn put(&self, path: &str, body: Value) -> Reply {
        send(Method::PUT, &self.url(path), Some(&self.token), Some(body)).await
    }

    async fn delete(&self, path: &str) -> Reply {
        send(Method::DELETE, &self.url(path), Some(&self.token), None).await
    }

    async fn act(&self, login: &str, org: &str, action: &str) -> StatusCode {
        send(
            Method::POST,
            &format!(
                "{}/act/{login}/orgs/{org}/{action}",
                self.emulator.admin_base()
            ),
            None,
            None,
        )
        .await
        .status
    }

    fn events(&self) -> Vec<(String, String)> {
        self.emulator
            .events()
            .into_iter()
            .map(|e| (e.event, e.action.unwrap_or_default()))
            .collect()
    }
}

#[tokio::test]
async fn the_organization_shows_plan_and_settings_by_permission() {
    let world = World::start().await;
    let full = world.get("/orgs/ACME").await;
    assert_eq!(full.status, StatusCode::OK);
    assert_eq!(full.body["login"], "acme");
    assert_eq!(full.body["id"], 100);
    assert_eq!(full.body["type"], "Organization");
    assert_eq!(full.body["created_at"], "2028-01-01T00:00:00Z");
    assert_eq!(full.body["plan"]["name"], "free");
    assert_eq!(full.body["plan"]["filled_seats"], 2);
    assert_eq!(full.body["two_factor_requirement_enabled"], true);
    assert_eq!(full.body["default_repository_permission"], "none");

    // A token without organization_plan / organization_administration.
    let narrow = mint(
        &world.emulator,
        json!({"installation_id": 5, "permissions": {"members": "read", "metadata": "read"}}),
    )
    .await;
    let bare = send(Method::GET, &world.url("/orgs/acme"), Some(&narrow), None).await;
    assert_eq!(bare.status, StatusCode::OK);
    assert!(bare.body.get("plan").is_none(), "{}", bare.body);
    assert!(bare.body.get("two_factor_requirement_enabled").is_none());
    assert_eq!(bare.body["created_at"], "2028-01-01T00:00:00Z");

    // Another installation's token sees the public fields only.
    let other = mint(&world.emulator, json!({"installation_id": 6})).await;
    let public = send(Method::GET, &world.url("/orgs/acme"), Some(&other), None).await;
    assert_eq!(public.status, StatusCode::OK);
    assert!(public.body.get("plan").is_none());
    assert_eq!(
        world.get("/orgs/nobody").await.status,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn invitations_by_user_and_email_with_teams() {
    let world = World::start().await;
    let created = world
        .post(
            "/orgs/acme/invitations",
            json!({"invitee_id": 3, "role": "direct_member", "team_ids": [10, 12]}),
        )
        .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
    let id = created.body["id"].as_u64().unwrap();
    assert_eq!(created.body["login"], "invitee");
    assert_eq!(created.body["email"], Value::Null);
    assert_eq!(created.body["role"], "direct_member");
    assert_eq!(created.body["team_count"], 2);
    assert_eq!(created.body["inviter"]["login"], "org-bot[bot]");
    assert_eq!(created.body["created_at"], "2030-01-01T00:00:00Z");
    assert_eq!(created.body["failed_at"], Value::Null);

    let email = world
        .post(
            "/orgs/acme/invitations",
            json!({"email": "someone@example.test", "role": "admin"}),
        )
        .await;
    assert_eq!(email.status, StatusCode::CREATED, "{}", email.body);
    assert_eq!(email.body["login"], Value::Null);
    assert_eq!(email.body["email"], "someone@example.test");
    assert_eq!(email.body["role"], "admin");
    assert_eq!(email.body["team_count"], 0);

    let list = world.get("/orgs/acme/invitations").await;
    assert_eq!(list.status, StatusCode::OK);
    let ids: Vec<u64> = list
        .body
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["id"].as_u64().unwrap())
        .collect();
    assert_eq!(ids, [id, id + 1]);
    let admins = world.get("/orgs/acme/invitations?role=admin").await;
    assert_eq!(admins.body.as_array().unwrap().len(), 1);

    let teams = world
        .get(&format!("/orgs/acme/invitations/{id}/teams"))
        .await;
    let slugs: Vec<&str> = teams
        .body
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["slug"].as_str().unwrap())
        .collect();
    assert_eq!(slugs, ["core", "docs"]);
    assert_eq!(
        world.get("/orgs/acme/invitations/999/teams").await.status,
        StatusCode::NOT_FOUND
    );

    // The invitee's membership and team memberships are pending.
    let membership = world.get("/orgs/acme/memberships/invitee").await;
    assert_eq!(membership.status, StatusCode::OK);
    assert_eq!(membership.body["state"], "pending");
    assert_eq!(membership.body["role"], "member");
    assert_eq!(membership.body["user"]["id"], 3);
    assert_eq!(membership.body["organization"]["id"], 100);
    let team = world.get("/orgs/acme/teams/core/memberships/invitee").await;
    assert_eq!(team.status, StatusCode::OK, "{}", team.body);
    assert_eq!(
        (&team.body["state"], &team.body["role"]),
        (&json!("pending"), &json!("member"))
    );

    // organization.member_invited, twice, from the App's bot.
    let events = world.emulator.events();
    assert_eq!(events.len(), 2);
    let invited = &events[0];
    assert_eq!(
        (invited.event.as_str(), invited.action.as_deref()),
        ("organization", Some("member_invited"))
    );
    assert_eq!(invited.payload["invitation"]["id"], id);
    assert_eq!(invited.payload["user"]["login"], "invitee");
    assert_eq!(invited.payload["organization"]["login"], "acme");
    assert_eq!(invited.payload["sender"]["login"], "org-bot[bot]");
    assert_eq!(invited.targets.len(), 1);
    assert_eq!(invited.targets[0].installation_id, 5);
    assert_eq!(events[1].payload["user"], Value::Null);

    // Cancel the email invitation.
    let cancelled = world
        .delete(&format!("/orgs/acme/invitations/{}", id + 1))
        .await;
    assert_eq!(cancelled.status, StatusCode::NO_CONTENT);
    assert_eq!(
        world
            .delete(&format!("/orgs/acme/invitations/{}", id + 1))
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        world
            .get("/orgs/acme/invitations")
            .await
            .body
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert!(
        world
            .get("/orgs/acme/failed_invitations")
            .await
            .body
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn invalid_invitations_are_422s() {
    let world = World::start().await;
    for body in [
        json!({"invitee_id": 2}),
        json!({"invitee_id": 3, "role": "owner"}),
        json!({"invitee_id": 3, "email": "a@example.test"}),
        json!({"role": "direct_member"}),
        json!({"invitee_id": 3, "team_ids": [999]}),
        json!({"invitee_id": 99_999}),
        json!({"invitee_id": 3, "role": "reinstate"}),
    ] {
        let reply = world.post("/orgs/acme/invitations", body.clone()).await;
        assert_eq!(reply.status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
        assert_eq!(reply.body["message"], "Validation Failed");
    }
    let member = world
        .post("/orgs/acme/invitations", json!({"invitee_id": 2}))
        .await;
    assert_eq!(
        member.body["errors"][0]["message"],
        "Invitee is already a part of this organization"
    );
    let first = world
        .post("/orgs/acme/invitations", json!({"invitee_id": 3}))
        .await;
    assert_eq!(first.status, StatusCode::CREATED);
    let again = world
        .post("/orgs/acme/invitations", json!({"invitee_id": 3}))
        .await;
    assert_eq!(again.status, StatusCode::UNPROCESSABLE_ENTITY);
    let malformed = send(
        Method::POST,
        &world.url("/orgs/acme/invitations"),
        Some(&world.token),
        None,
    )
    .await;
    assert_eq!(malformed.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(world.events().iter().all(|(_, a)| a == "member_invited"));
}

/// A local webhook receiver that answers 200.
#[derive(Clone, Default)]
struct Receiver {
    received: Arc<Mutex<Vec<(String, Value)>>>,
}

async fn receive(
    State(receiver): State<Receiver>,
    headers: axum::http::HeaderMap,
    body: Bytes,
) -> &'static str {
    let event = headers["x-github-event"].to_str().unwrap().to_owned();
    receiver
        .received
        .lock()
        .unwrap()
        .push((event, serde_json::from_slice(&body).unwrap()));
    "ok"
}

async fn receiver() -> (Receiver, String) {
    let receiver = Receiver::default();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/hooks", listener.local_addr().unwrap());
    let router = axum::Router::new()
        .route("/hooks", axum::routing::post(receive))
        .with_state(receiver.clone());
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    (receiver, url)
}

#[tokio::test]
async fn accepting_through_the_user_route_activates_membership_and_teams() {
    let (receiver, url) = receiver().await;
    let mut seed = seed();
    seed.apps[0].webhook = Some(Webhook::new(url, "secret"));
    let emulator = Emulator::builder(seed)
        .dispatch(Dispatch::Manual)
        .start()
        .await
        .unwrap();
    let world = World::with(emulator).await;
    let created = world
        .post(
            "/orgs/acme/invitations",
            json!({"invitee_id": 3, "team_ids": [11]}),
        )
        .await;
    assert_eq!(created.status, StatusCode::CREATED);
    // A maintainer role through a team PUT on the open invitation.
    let put = world
        .put(
            "/orgs/acme/teams/docs/memberships/invitee",
            json!({"role": "maintainer"}),
        )
        .await;
    assert_eq!(put.status, StatusCode::OK, "{}", put.body);
    assert_eq!(put.body["state"], "pending");
    assert_eq!(put.body["role"], "maintainer");

    // Before accepting: no access to acme/widgets through core-child.
    let permission = world
        .get("/repos/acme/widgets/collaborators/invitee/permission")
        .await;
    assert_eq!(permission.body["permission"], "none");

    let user = world.emulator.mint_user_token("invitee", 4).unwrap();
    let url = |path: &str| world.url(path);
    let pending = send(
        Method::GET,
        &url("/user/memberships/orgs/acme"),
        Some(&user),
        None,
    )
    .await;
    assert_eq!(pending.body["state"], "pending");
    let listed = send(
        Method::GET,
        &url("/user/memberships/orgs?state=pending"),
        Some(&user),
        None,
    )
    .await;
    assert_eq!(listed.body.as_array().unwrap().len(), 1);
    let bad = send(
        Method::PATCH,
        &url("/user/memberships/orgs/acme"),
        Some(&user),
        Some(json!({"state": "pending"})),
    )
    .await;
    assert_eq!(bad.status, StatusCode::UNPROCESSABLE_ENTITY);
    let accepted = send(
        Method::PATCH,
        &url("/user/memberships/orgs/acme"),
        Some(&user),
        Some(json!({"state": "active"})),
    )
    .await;
    assert_eq!(accepted.status, StatusCode::OK, "{}", accepted.body);
    assert_eq!(accepted.body["state"], "active");
    assert_eq!(accepted.body["role"], "member");
    assert_eq!(accepted.body["organization"]["login"], "acme");

    let membership = world.get("/orgs/acme/memberships/invitee").await;
    assert_eq!(membership.body["state"], "active");
    for (slug, role) in [("core-child", "member"), ("docs", "maintainer")] {
        let team = world
            .get(&format!("/orgs/acme/teams/{slug}/memberships/invitee"))
            .await;
        assert_eq!(team.status, StatusCode::OK, "{slug}");
        assert_eq!(
            (&team.body["state"], &team.body["role"]),
            (&json!("active"), &json!(role))
        );
    }
    assert!(
        world
            .get("/orgs/acme/invitations")
            .await
            .body
            .as_array()
            .unwrap()
            .is_empty()
    );
    // core-child inherits core's write on acme/widgets.
    let permission = world
        .get("/repos/acme/widgets/collaborators/invitee/permission")
        .await;
    assert_eq!(permission.body["permission"], "write");
    assert_eq!(permission.body["role_name"], "write");
    let members = world.get("/orgs/acme/members").await;
    let logins: Vec<&str> = members
        .body
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["login"].as_str().unwrap())
        .collect();
    assert_eq!(logins, ["boss", "dev", "invitee"]);

    // Delivered once dispatched.
    assert!(receiver.received.lock().unwrap().is_empty());
    world.emulator.dispatch_deliveries();
    assert!(world.emulator.wait_for_deliveries(WAIT).await);
    let received = receiver.received.lock().unwrap().clone();
    let kinds: Vec<(String, String)> = received
        .iter()
        .map(|(event, payload)| {
            (
                event.clone(),
                payload["action"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    assert_eq!(
        kinds,
        [
            ("organization".to_owned(), "member_invited".to_owned()),
            ("organization".to_owned(), "member_added".to_owned()),
            ("membership".to_owned(), "added".to_owned()),
            ("membership".to_owned(), "added".to_owned()),
        ]
    );
    let added = &received[1].1;
    assert_eq!(added["membership"]["user"]["id"], 3);
    assert_eq!(added["membership"]["state"], "active");
    assert_eq!(added["membership"]["role"], "member");
    assert!(
        added["membership"]["organization_url"]
            .as_str()
            .unwrap()
            .ends_with("/orgs/acme")
    );
    assert_eq!(added["organization"]["id"], 100);
    assert_eq!(added["sender"]["login"], "invitee");
    assert_eq!(added["installation"]["id"], 5);
    let team_added = &received[2].1;
    assert_eq!(team_added["scope"], "team");
    assert_eq!(team_added["member"]["id"], 3);
    assert_eq!(team_added["team"]["id"], 11);
    assert_eq!(team_added["team"]["parent"]["slug"], "core");
    assert_eq!(team_added["organization"]["login"], "acme");
    assert_eq!(received[3].1["team"]["slug"], "docs");
}

#[tokio::test]
async fn an_email_invitation_is_accepted_by_a_verified_address() {
    let world = World::start().await;
    let created = world
        .post(
            "/orgs/acme/invitations",
            json!({"email": "Invitee@Example.test", "team_ids": [10]}),
        )
        .await;
    assert_eq!(created.status, StatusCode::CREATED);
    // An unverified address does not match.
    let unverified = world
        .post(
            "/orgs/acme/invitations",
            json!({"email": "unverified@example.test"}),
        )
        .await;
    assert_eq!(unverified.status, StatusCode::CREATED);
    assert_eq!(
        world.get("/orgs/acme/memberships/invitee").await.status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        world.act("invitee", "acme", "accept").await,
        StatusCode::NO_CONTENT
    );
    let membership = world.get("/orgs/acme/memberships/invitee").await;
    assert_eq!(membership.body["state"], "active");
    let team = world.get("/orgs/acme/teams/core/memberships/invitee").await;
    assert_eq!(team.body["state"], "active");
    let left: Vec<Value> = world
        .get("/orgs/acme/invitations")
        .await
        .body
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(left.len(), 1);
    assert_eq!(left[0]["email"], "unverified@example.test");
    assert_eq!(
        world.act("outsider", "acme", "accept").await,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn expired_invitations_move_to_the_failed_list() {
    let mut world = World::start().await;
    let created = world
        .post("/orgs/acme/invitations", json!({"invitee_id": 3}))
        .await;
    let id = created.body["id"].as_u64().unwrap();
    world.advance(7 * DAY - 60).await;
    assert_eq!(
        world
            .get("/orgs/acme/invitations")
            .await
            .body
            .as_array()
            .unwrap()
            .len(),
        1
    );
    world.advance(60).await;
    assert!(
        world
            .get("/orgs/acme/invitations")
            .await
            .body
            .as_array()
            .unwrap()
            .is_empty()
    );
    let failed = world.get("/orgs/acme/failed_invitations").await;
    assert_eq!(failed.status, StatusCode::OK);
    let entry = &failed.body[0];
    assert_eq!(entry["id"], id);
    assert_eq!(entry["failed_at"], "2030-01-08T00:00:00Z");
    assert_eq!(entry["failed_reason"], "Invitation expired");
    assert_eq!(entry["login"], "invitee");
    // No longer pending; a new invitation is allowed; the old one cannot be
    // accepted or cancelled.
    assert_eq!(
        world.get("/orgs/acme/memberships/invitee").await.status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        world.act("invitee", "acme", "accept").await,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        world
            .delete(&format!("/orgs/acme/invitations/{id}"))
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    let again = world
        .post("/orgs/acme/invitations", json!({"invitee_id": 3}))
        .await;
    assert_eq!(again.status, StatusCode::CREATED);
}

#[tokio::test]
async fn seeded_failed_invitations_are_listed_and_paged() {
    let mut seed = seed();
    for n in 1..=35 {
        seed = seed.org_invitation(
            OrgInvitation::new(500 + n, "acme", format!("u{n:03}"))
                .inviter("boss")
                .failed(start_2030(), "Invitation declined"),
        );
    }
    seed = seed.org_invitation(
        OrgInvitation::by_email(600, "acme", "x@example.test")
            .role(InvitationRole::BillingManager)
            .team(12),
    );
    let world = World::with(Emulator::start(seed).await.unwrap()).await;
    let first = world.get("/orgs/acme/failed_invitations?per_page=20").await;
    assert_eq!(first.body.as_array().unwrap().len(), 20);
    assert!(
        first.link().unwrap().contains("page=2>; rel=\"next\""),
        "{:?}",
        first.link()
    );
    let second = world
        .get("/orgs/acme/failed_invitations?per_page=20&page=2")
        .await;
    assert_eq!(second.body.as_array().unwrap().len(), 15);
    assert_eq!(second.body[0]["failed_reason"], "Invitation declined");
    assert_eq!(second.body[0]["inviter"]["login"], "boss");
    let pending = world.get("/orgs/acme/invitations").await;
    assert_eq!(pending.body.as_array().unwrap().len(), 1);
    assert_eq!(pending.body[0]["role"], "billing_manager");
    assert_eq!(pending.body[0]["team_count"], 1);
    // The failed invitees may be invited again.
    let again = world
        .post("/orgs/acme/invitations", json!({"invitee_id": 1001}))
        .await;
    assert_eq!(again.status, StatusCode::CREATED);
    assert!(again.body["id"].as_u64().unwrap() > 600);
}

#[tokio::test]
async fn the_budget_is_50_for_a_young_free_org_and_500_otherwise() {
    let young = Seed::new()
        .start_time(start_2030())
        .org(Org::new(1, "young").created_at(start_2030()))
        .org(Org::new(2, "old").created_at("2029-11-01T00:00:00Z".parse().unwrap()))
        .org(
            Org::new(3, "paid")
                .created_at(start_2030())
                .plan(Plan::new("team", 1000)),
        )
        .app(
            App::new(4, "bot", "Iv1.b", "s")
                .public_key(PUBLIC_KEY)
                .permission("members", Access::Write),
        )
        .installation(Installation::new(10, 4, "young"))
        .installation(Installation::new(11, 4, "old"))
        .installation(Installation::new(12, 4, "paid"));
    let emulator = Emulator::start(young).await.unwrap();
    let invite = async |installation: u64, org: &str, n: usize| {
        let token = mint(&emulator, json!({"installation_id": installation})).await;
        send(
            Method::POST,
            &format!("{}/orgs/{org}/invitations", emulator.api_base()),
            Some(&token),
            Some(json!({"email": format!("p{n}@example.test")})),
        )
        .await
    };
    for n in 0..50 {
        assert_eq!(
            invite(10, "young", n).await.status,
            StatusCode::CREATED,
            "{n}"
        );
    }
    let over = invite(10, "young", 50).await;
    assert_eq!(over.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        over.body["errors"][0]["message"],
        "Over invitation rate limit"
    );
    for (installation, org) in [(11, "old"), (12, "paid")] {
        for n in 0..51 {
            assert_eq!(
                invite(installation, org, n).await.status,
                StatusCode::CREATED,
                "{org} {n}"
            );
        }
    }
    // The young org's budget comes back after 24 hours.
    emulator.advance_clock(Duration::from_secs(DAY));
    assert_eq!(invite(10, "young", 51).await.status, StatusCode::CREATED);
    // ... and the organization is now old enough? Not yet: 50 again.
    for n in 52..101 {
        assert_eq!(
            invite(10, "young", n).await.status,
            StatusCode::CREATED,
            "{n}"
        );
    }
    assert_eq!(
        invite(10, "young", 101).await.status,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    // A month later it is established.
    emulator.advance_clock(Duration::from_secs(31 * DAY));
    for n in 102..153 {
        assert_eq!(
            invite(10, "young", n).await.status,
            StatusCode::CREATED,
            "{n}"
        );
    }
}

#[tokio::test]
async fn a_paid_plan_without_a_free_seat_refuses_member_invitations() {
    let mut seed = seed();
    seed.orgs[0].plan = Plan::new("team", 3);
    let world = World::with(Emulator::start(seed).await.unwrap()).await;
    // boss, dev + one invitation fill the three seats.
    let first = world
        .post("/orgs/acme/invitations", json!({"invitee_id": 3}))
        .await;
    assert_eq!(first.status, StatusCode::CREATED);
    let full = world
        .post("/orgs/acme/invitations", json!({"invitee_id": 4}))
        .await;
    assert_eq!(full.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        full.body["errors"][0]["message"]
            .as_str()
            .unwrap()
            .contains("seat")
    );
    assert_eq!(
        world.get("/orgs/acme").await.body["plan"]["filled_seats"],
        3
    );
    // A billing manager takes no seat.
    let billing = world
        .post(
            "/orgs/acme/invitations",
            json!({"invitee_id": 4, "role": "billing_manager"}),
        )
        .await;
    assert_eq!(billing.status, StatusCode::CREATED);
    // Cancelling frees the seat.
    let id = first.body["id"].as_u64().unwrap();
    world.delete(&format!("/orgs/acme/invitations/{id}")).await;
    let freed = world.put("/orgs/acme/memberships/u001", json!({})).await;
    assert_eq!(freed.status, StatusCode::OK);
}

#[tokio::test]
async fn memberships_put_and_delete() {
    let world = World::start().await;
    // A non-member is invited: pending, with a visible invitation.
    let put = world
        .put("/orgs/acme/memberships/outsider", json!({"role": "admin"}))
        .await;
    assert_eq!(put.status, StatusCode::OK, "{}", put.body);
    assert_eq!(
        (&put.body["state"], &put.body["role"]),
        (&json!("pending"), &json!("admin"))
    );
    let invitations = world.get("/orgs/acme/invitations").await;
    assert_eq!(invitations.body[0]["login"], "outsider");
    assert_eq!(invitations.body[0]["role"], "admin");
    // Re-PUT changes the pending role.
    let reput = world
        .put("/orgs/acme/memberships/outsider", json!({}))
        .await;
    assert_eq!(reput.body["role"], "member");
    assert_eq!(
        world
            .get("/orgs/acme/invitations")
            .await
            .body
            .as_array()
            .unwrap()
            .len(),
        1
    );
    // A member's role changes, still active.
    let promoted = world
        .put("/orgs/acme/memberships/dev", json!({"role": "admin"}))
        .await;
    assert_eq!(
        (&promoted.body["state"], &promoted.body["role"]),
        (&json!("active"), &json!("admin"))
    );
    let bad = world
        .put("/orgs/acme/memberships/dev", json!({"role": "owner"}))
        .await;
    assert_eq!(bad.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        world
            .put("/orgs/acme/memberships/ghost", json!({}))
            .await
            .status,
        StatusCode::NOT_FOUND
    );

    // DELETE of an invitee cancels the invitation; of a member removes them
    // and their team memberships.
    assert_eq!(
        world.delete("/orgs/acme/memberships/outsider").await.status,
        StatusCode::NO_CONTENT
    );
    assert!(
        world
            .get("/orgs/acme/invitations")
            .await
            .body
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        world.delete("/orgs/acme/memberships/dev").await.status,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        world.get("/orgs/acme/memberships/dev").await.status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        world
            .get("/orgs/acme/teams/core/memberships/dev")
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        world.delete("/orgs/acme/memberships/dev").await.status,
        StatusCode::NOT_FOUND
    );
    let removed = world.emulator.events().pop().unwrap();
    assert_eq!(removed.action.as_deref(), Some("member_removed"));
    assert_eq!(removed.payload["membership"]["user"]["login"], "dev");
    assert_eq!(removed.payload["membership"]["role"], "admin");
    assert_eq!(removed.payload["sender"]["login"], "org-bot[bot]");
    // A removed member can be reinstated with their old role.
    let reinstated = world
        .post(
            "/orgs/acme/invitations",
            json!({"invitee_id": 2, "role": "reinstate"}),
        )
        .await;
    assert_eq!(reinstated.status, StatusCode::CREATED);
    assert_eq!(reinstated.body["role"], "admin");
}

#[tokio::test]
async fn teams_and_team_memberships() {
    let world = World::start().await;
    let teams = world.get("/orgs/acme/teams?per_page=2").await;
    assert_eq!(teams.status, StatusCode::OK);
    let ids: Vec<u64> = teams
        .body
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["id"].as_u64().unwrap())
        .collect();
    assert_eq!(ids, [10, 11]);
    assert!(teams.link().unwrap().contains("rel=\"next\""));
    assert_eq!(teams.body[1]["parent"]["id"], 10);
    let core = world.get("/orgs/acme/teams/core").await;
    assert_eq!(core.status, StatusCode::OK);
    assert_eq!(
        (&core.body["id"], &core.body["slug"], &core.body["name"]),
        (&json!(10), &json!("core"), &json!("Core"))
    );
    assert_eq!(core.body["organization"]["id"], 100);
    assert_eq!(core.body["members_count"], 1);
    assert_eq!(
        world.get("/orgs/acme/teams/nope").await.status,
        StatusCode::NOT_FOUND
    );

    // dev is a maintainer; boss (an owner) is not in the team.
    let dev = world.get("/orgs/acme/teams/core/memberships/dev").await;
    assert_eq!(
        (&dev.body["role"], &dev.body["state"]),
        (&json!("maintainer"), &json!("active"))
    );
    assert!(
        dev.body["url"]
            .as_str()
            .unwrap()
            .contains("/team/10/memberships/dev")
    );
    assert_eq!(
        world
            .get("/orgs/acme/teams/core/memberships/boss")
            .await
            .status,
        StatusCode::NOT_FOUND
    );

    // An active member joins: active, membership.added; grants follow.
    let boss = world
        .put("/orgs/acme/teams/docs/memberships/boss", json!({}))
        .await;
    assert_eq!(boss.status, StatusCode::OK);
    assert_eq!(
        (&boss.body["role"], &boss.body["state"]),
        (&json!("member"), &json!("active"))
    );
    let bad = world
        .put(
            "/orgs/acme/teams/docs/memberships/boss",
            json!({"role": "owner"}),
        )
        .await;
    assert_eq!(bad.status, StatusCode::UNPROCESSABLE_ENTITY);

    // A non-member: pending, with a visible invitation that spends budget.
    let outsider = world
        .put(
            "/orgs/acme/teams/core/memberships/outsider",
            json!({"role": "member"}),
        )
        .await;
    assert_eq!(outsider.status, StatusCode::OK, "{}", outsider.body);
    assert_eq!(outsider.body["state"], "pending");
    let invitations = world.get("/orgs/acme/invitations").await;
    assert_eq!(invitations.body[0]["login"], "outsider");
    assert_eq!(invitations.body[0]["team_count"], 1);
    assert_eq!(
        world.get("/orgs/acme/memberships/outsider").await.body["state"],
        "pending"
    );
    // Removing the team from the invitation keeps the invitation.
    assert_eq!(
        world
            .delete("/orgs/acme/teams/core/memberships/outsider")
            .await
            .status,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        world.get("/orgs/acme/invitations").await.body[0]["team_count"],
        0
    );

    // DELETE an active team membership: membership.removed; access goes.
    assert_eq!(
        world
            .get("/repos/acme/widgets/collaborators/dev/permission")
            .await
            .body["permission"],
        "write"
    );
    let collaborators = world
        .get("/repos/acme/widgets/collaborators?affiliation=all")
        .await;
    assert!(
        collaborators
            .body
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["login"] == "dev")
    );
    assert_eq!(
        world
            .delete("/orgs/acme/teams/core/memberships/dev")
            .await
            .status,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        world
            .delete("/orgs/acme/teams/core/memberships/dev")
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        world
            .get("/repos/acme/widgets/collaborators/dev/permission")
            .await
            .body["permission"],
        "none"
    );
    assert_eq!(
        world.events(),
        [
            ("membership".to_owned(), "added".to_owned()),
            ("organization".to_owned(), "member_invited".to_owned()),
            ("membership".to_owned(), "removed".to_owned()),
        ]
    );
    let removed = world.emulator.events().pop().unwrap();
    assert_eq!(removed.payload["scope"], "team");
    assert_eq!(removed.payload["member"]["login"], "dev");
    assert_eq!(removed.payload["team"]["slug"], "core");
}

#[tokio::test]
async fn a_team_grant_makes_put_collaborator_a_204() {
    let mut seed = seed();
    seed.apps[0]
        .permissions
        .insert("administration".into(), Access::Write);
    let world = World::with(Emulator::start(seed).await.unwrap()).await;
    let dev = world
        .put(
            "/repos/acme/widgets/collaborators/dev",
            json!({"permission": "pull"}),
        )
        .await;
    assert_eq!(dev.status, StatusCode::NO_CONTENT);
    let outsider = world
        .put("/repos/acme/widgets/collaborators/outsider", json!({}))
        .await;
    assert_eq!(outsider.status, StatusCode::CREATED);
}

#[tokio::test]
async fn pending_seeded_memberships_and_the_act_routes() {
    let seed = seed()
        .membership(Membership::new("acme", "outsider", OrgRole::Member).pending())
        .team_membership(TeamMembership::new("acme/core", "outsider", TeamRole::Member).pending());
    let world = World::with(Emulator::start(seed).await.unwrap()).await;
    assert_eq!(
        world.get("/orgs/acme/memberships/outsider").await.body["state"],
        "pending"
    );
    assert_eq!(
        world
            .get("/orgs/acme/teams/core/memberships/outsider")
            .await
            .body["state"],
        "pending"
    );
    assert_eq!(
        world.act("outsider", "acme", "accept").await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        world
            .get("/orgs/acme/teams/core/memberships/outsider")
            .await
            .body["state"],
        "active"
    );
    assert_eq!(
        world.act("outsider", "acme", "leave").await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        world.get("/orgs/acme/memberships/outsider").await.status,
        StatusCode::NOT_FOUND
    );
    // Decline an invitation.
    world
        .post("/orgs/acme/invitations", json!({"invitee_id": 3}))
        .await;
    assert_eq!(
        world.act("invitee", "acme", "decline").await,
        StatusCode::NO_CONTENT
    );
    assert!(
        world
            .get("/orgs/acme/invitations")
            .await
            .body
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        world.act("invitee", "acme", "dance").await,
        StatusCode::NOT_FOUND
    );
    let left = world.emulator.events();
    let sent: Vec<(&str, &str)> = left
        .iter()
        .map(|e| {
            (
                e.action.as_deref().unwrap(),
                e.payload["sender"]["login"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        sent,
        [
            ("member_added", "outsider"),
            ("added", "outsider"),
            ("member_removed", "outsider"),
            ("member_invited", "org-bot[bot]"),
        ]
    );
}

#[tokio::test]
async fn permissions_and_scope_are_enforced() {
    let world = World::start().await;
    let read = mint(
        &world.emulator,
        json!({"installation_id": 5, "permissions": {"members": "read"}}),
    )
    .await;
    let url = world.url("/orgs/acme/invitations");
    let reply = send(
        Method::POST,
        &url,
        Some(&read),
        Some(json!({"invitee_id": 3})),
    )
    .await;
    assert_eq!(reply.status, StatusCode::FORBIDDEN);
    assert_eq!(
        reply.headers["x-accepted-github-permissions"],
        "members=write"
    );
    for (method, path) in [
        (Method::PUT, "/orgs/acme/memberships/outsider"),
        (Method::DELETE, "/orgs/acme/memberships/dev"),
        (Method::DELETE, "/orgs/acme/invitations/1"),
        (Method::PUT, "/orgs/acme/teams/core/memberships/boss"),
        (Method::DELETE, "/orgs/acme/teams/core/memberships/dev"),
    ] {
        let reply = send(
            method.clone(),
            &world.url(path),
            Some(&read),
            Some(json!({})),
        )
        .await;
        assert_eq!(reply.status, StatusCode::FORBIDDEN, "{method} {path}");
    }
    let none = mint(
        &world.emulator,
        json!({"installation_id": 5, "permissions": {"metadata": "read"}}),
    )
    .await;
    for path in [
        "/orgs/acme/members",
        "/orgs/acme/invitations",
        "/orgs/acme/failed_invitations",
        "/orgs/acme/teams",
        "/orgs/acme/teams/core",
        "/orgs/acme/memberships/dev",
    ] {
        let reply = send(Method::GET, &world.url(path), Some(&none), None).await;
        assert_eq!(reply.status, StatusCode::FORBIDDEN, "{path}");
        assert_eq!(
            reply.headers["x-accepted-github-permissions"],
            "members=read"
        );
    }
    // Another organization's installation.
    let other = mint(&world.emulator, json!({"installation_id": 6})).await;
    let reply = send(
        Method::GET,
        &world.url("/orgs/acme/members"),
        Some(&other),
        None,
    )
    .await;
    assert_eq!(reply.status, StatusCode::FORBIDDEN);
    // No credential; a user token on an installation route.
    assert_eq!(
        send(Method::GET, &world.url("/orgs/acme/members"), None, None)
            .await
            .status,
        StatusCode::UNAUTHORIZED
    );
    // The user routes need a user token whose App holds members.
    let user = world.emulator.mint_user_token("dev", 4).unwrap();
    let listed = send(
        Method::GET,
        &world.url("/user/memberships/orgs"),
        Some(&user),
        None,
    )
    .await;
    assert_eq!(listed.status, StatusCode::OK);
    assert_eq!(listed.body[0]["organization"]["login"], "acme");
    assert_eq!(listed.body[0]["state"], "active");
    let narrow = Seed::new().app(App::new(9, "plain", "Iv1.plain", "s").public_key(PUBLIC_KEY));
    world.emulator.seed(narrow).unwrap();
    let plain = world.emulator.mint_user_token("dev", 9).unwrap();
    let reply = send(
        Method::PATCH,
        &world.url("/user/memberships/orgs/acme"),
        Some(&plain),
        Some(json!({"state": "active"})),
    )
    .await;
    assert_eq!(reply.status, StatusCode::FORBIDDEN);
    assert_eq!(
        reply.headers["x-accepted-github-permissions"],
        "members=write"
    );
    let pat = world
        .emulator
        .mint_personal_access_token("outsider", &["read:org"])
        .unwrap();
    let none = send(
        Method::GET,
        &world.url("/user/memberships/orgs/acme"),
        Some(&pat),
        None,
    )
    .await;
    assert_eq!(none.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn spammed_invitation_writes_answer_422_spammed() {
    let world = World::start().await;
    let created = world
        .post("/orgs/acme/invitations", json!({"invitee_id": 3}))
        .await;
    let id = created.body["id"].as_u64().unwrap();
    let on = send(
        Method::POST,
        &format!("{}/orgs/acme/spammed", world.emulator.admin_base()),
        None,
        Some(json!({"spammed": true})),
    )
    .await;
    assert_eq!(on.status, StatusCode::NO_CONTENT);
    let post = world
        .post("/orgs/acme/invitations", json!({"invitee_id": 4}))
        .await;
    assert_eq!(post.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(post.body["errors"][0]["code"], "spammed");
    let delete = world.delete(&format!("/orgs/acme/invitations/{id}")).await;
    assert_eq!(delete.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(delete.body["errors"][0]["code"], "spammed");
    let unknown = send(
        Method::POST,
        &format!("{}/orgs/nobody/spammed", world.emulator.admin_base()),
        None,
        Some(json!({"spammed": true})),
    )
    .await;
    assert_eq!(unknown.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn refuse_assumed_mode_refuses_the_guesses() {
    let mut seed = seed().refuse_assumed();
    seed.orgs[0].plan = Plan::new("team", 2);
    let mut world = World::with(Emulator::start(seed).await.unwrap()).await;
    // Documented: the invitation itself, the pending membership.
    let created = world
        .post(
            "/orgs/acme/invitations",
            json!({"email": "a@example.test", "role": "billing_manager"}),
        )
        .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
    // The seat check is a guess.
    let seat = world
        .post("/orgs/acme/invitations", json!({"invitee_id": 3}))
        .await;
    assert_eq!(seat.status, StatusCode::NOT_IMPLEMENTED);
    assert_eq!(seat.body["behaviour"], "org-no-seat");
    // An existing member: documented 422, guessed text withheld.
    let member = world
        .post(
            "/orgs/acme/invitations",
            json!({"invitee_id": 2, "role": "billing_manager"}),
        )
        .await;
    assert_eq!(member.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(member.body["message"], "octoemu: assumed message withheld");
    // Team PUT for a non-member, admin fields, the failed list after expiry.
    let team = world
        .put("/orgs/acme/teams/core/memberships/outsider", json!({}))
        .await;
    assert_eq!(team.body["behaviour"], "team-put-non-member");
    let org = world.get("/orgs/acme").await;
    assert!(org.body.get("two_factor_requirement_enabled").is_none());
    assert_eq!(org.body["plan"]["seats"], 2);
    world.advance(8 * DAY).await;
    let failed = world.get("/orgs/acme/failed_invitations").await;
    assert_eq!(failed.body["behaviour"], "expired-org-invitations-failed");
    let user = world.emulator.mint_user_token("dev", 4).unwrap();
    let listed = send(
        Method::GET,
        &world.url("/user/memberships/orgs"),
        Some(&user),
        None,
    )
    .await;
    assert_eq!(listed.body["behaviour"], "user-token-org-membership-list");
}

#[tokio::test]
async fn the_admin_api_renames_an_organization() {
    let world = World::start().await;
    let admin = |path: &str| format!("{}{path}", world.emulator.admin_base());
    let widgetco = mint(&world.emulator, json!({"installation_id": 6})).await;
    let invited = send(
        Method::POST,
        &world.url("/orgs/widgetco/invitations"),
        Some(&widgetco),
        Some(json!({"invitee_id": 1001})),
    )
    .await;
    assert_eq!(invited.status, StatusCode::CREATED, "{}", invited.body);
    let renamed = send(
        Method::POST,
        &admin("/orgs/widgetco/rename"),
        None,
        Some(json!({"login": "widgets-inc"})),
    )
    .await;
    assert_eq!(renamed.status, StatusCode::NO_CONTENT, "{}", renamed.body);
    // Same ID, installation and invitation; the old login names nothing.
    let org = send(
        Method::GET,
        &world.url("/orgs/widgets-inc"),
        Some(&widgetco),
        None,
    )
    .await;
    assert_eq!(
        (org.status, org.body["id"].as_u64()),
        (StatusCode::OK, Some(200))
    );
    assert_eq!(org.body["login"], "widgets-inc");
    assert_eq!(
        send(
            Method::GET,
            &world.url("/orgs/widgetco"),
            Some(&widgetco),
            None
        )
        .await
        .status,
        StatusCode::NOT_FOUND
    );
    let listed = send(
        Method::GET,
        &world.url("/orgs/widgets-inc/invitations"),
        Some(&widgetco),
        None,
    )
    .await;
    assert_eq!(listed.body[0]["id"], invited.body["id"]);
    assert_eq!(
        world.act("u001", "widgets-inc", "accept").await,
        StatusCode::NO_CONTENT
    );
    let state = world.emulator.state();
    let installation = state["world"]["installations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["id"] == 6)
        .unwrap();
    assert_eq!(installation["account"], "widgets-inc");
    assert!(
        state["world"]["memberships"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["org"] == "widgets-inc" && m["user"] == "u001")
    );
    for (org, body, status) in [
        ("nobody", json!({"login": "x"}), StatusCode::NOT_FOUND),
        // Taken by a user, and by an organization.
        (
            "widgets-inc",
            json!({"login": "boss"}),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (
            "widgets-inc",
            json!({"login": "acme"}),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (
            "widgets-inc",
            json!({"login": "bad name"}),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        // An emulator limitation: an owner of repositories.
        (
            "acme",
            json!({"login": "acme-2"}),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
    ] {
        let reply = send(
            Method::POST,
            &admin(&format!("/orgs/{org}/rename")),
            None,
            Some(body),
        )
        .await;
        assert_eq!(reply.status, status, "{org}: {}", reply.body);
    }
}
