//! Fault rules (GE-07) over HTTP: matching (method, pattern, credential,
//! body, nth/times), every action (respond, delay, hold, apply-then-lose,
//! close, reset, custom), the admin API, the call log's marks, and that the
//! admin API is never faulted.
#![cfg(not(target_arch = "wasm32"))]

use octoemu::Emulator;
use octoemu::calls::CredentialKind;
use octoemu::faults::{DelayClock, Fault, FaultAction, Reply};
use octoemu::seed::{Access, App, Installation, Org, Repo, Seed, User};
use reqwest::{Client, StatusCode};
use serde_json::{Value, json};
use std::time::{Duration, Instant};

/// reqwest's `json` feature is off in this workspace.
trait JsonBody {
    fn json_body(self, body: &Value) -> Self;
}

impl JsonBody for reqwest::RequestBuilder {
    fn json_body(self, body: &Value) -> Self {
        self.header("content-type", "application/json")
            .body(body.to_string())
    }
}

trait JsonValue {
    async fn value(self) -> Result<Value, reqwest::Error>;
}

impl JsonValue for reqwest::Response {
    async fn value(self) -> Result<Value, reqwest::Error> {
        let text = self.text().await?;
        Ok(serde_json::from_str(&text).unwrap_or(Value::String(text)))
    }
}

const PUBLIC_KEY: &str = include_str!("fixtures/rsa2048.pub.pem");
const PUT: &str = "/repos/acme/widgets/collaborators/hubot";

fn seed() -> Seed {
    Seed::new()
        .user(User::new(1, "octocat"))
        .user(User::new(3, "hubot"))
        .org(Org::new(2, "acme"))
        .repo(Repo::new(101, "acme", "widgets").private())
        .app(
            App::new(4, "invite-bot", "Iv23.fixture", "fixture-client-secret")
                .public_key(PUBLIC_KEY)
                .permission("administration", Access::Write)
                .permission("metadata", Access::Read),
        )
        .installation(Installation::new(5, 4, "acme"))
}

struct World {
    emulator: Emulator,
    token: String,
    client: Client,
}

impl World {
    async fn start() -> Self {
        let emulator = Emulator::start(seed()).await.unwrap();
        let client = Client::builder().no_proxy().build().unwrap();
        let minted: Value = client
            .post(format!("{}/tokens/installation", emulator.admin_base()))
            .json_body(&json!({"installation_id": 5}))
            .send()
            .await
            .unwrap()
            .value()
            .await
            .unwrap();
        let token = minted["token"].as_str().unwrap().to_owned();
        Self {
            emulator,
            token,
            client,
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.emulator.api_base())
    }

    async fn put(&self, role: &str) -> Result<reqwest::Response, reqwest::Error> {
        self.client
            .put(self.url(PUT))
            .bearer_auth(&self.token)
            .json_body(&json!({"permission": role}))
            .send()
            .await
    }

    async fn invitations(&self) -> usize {
        let body: Value = self
            .client
            .get(self.url("/repos/acme/widgets/invitations"))
            .bearer_auth(&self.token)
            .send()
            .await
            .unwrap()
            .value()
            .await
            .unwrap();
        body.as_array().unwrap().len()
    }

    async fn admin(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let mut request = self
            .client
            .request(method, format!("{}{path}", self.emulator.admin_base()));
        if let Some(body) = body {
            request = request.json_body(&body);
        }
        let response = request.send().await.unwrap();
        let status = response.status();
        let text = response.text().await.unwrap();
        (status, serde_json::from_str(&text).unwrap_or(Value::Null))
    }
}

#[tokio::test]
async fn respond_answers_without_running_the_handler_and_counts_nth_and_times() {
    let world = World::start().await;
    let id = world
        .emulator
        .add_fault(
            Fault::on(
                "put",
                "/repos/{owner}/{repo}/collaborators/{username}",
                FaultAction::Respond(
                    Reply::new(403)
                        .header("retry-after", "1")
                        .json(json!({"message": "You have exceeded a secondary rate limit"})),
                ),
            )
            .nth(2)
            .once(),
        )
        .unwrap();
    // The first PUT is let through (nth 2): an invitation.
    assert_eq!(world.put("push").await.unwrap().status(), 201);
    let refused = world.put("push").await.unwrap();
    assert_eq!(refused.status(), 403);
    assert_eq!(refused.headers()["retry-after"], "1");
    assert!(refused.headers().contains_key("date"));
    let body: Value = refused.value().await.unwrap();
    assert_eq!(body["message"], "You have exceeded a secondary rate limit");
    // Spent (times 1): the re-PUT reaches GitHub's model again.
    assert_eq!(world.put("push").await.unwrap().status(), 201);
    assert_eq!(world.invitations().await, 1);
    let status = world.emulator.fault(id).unwrap();
    assert_eq!((status.matched, status.fired), (3, 1));

    let calls = world.emulator.calls();
    let marked: Vec<_> = calls.iter().filter(|c| c.fault.is_some()).collect();
    assert_eq!(marked.len(), 1);
    let mark = marked[0].fault.as_ref().unwrap();
    assert_eq!(
        (mark.rule, mark.action.as_str(), mark.handler_status),
        (id, "respond", None)
    );
    assert_eq!(marked[0].status, 403);
    assert_eq!(marked[0].route, None);
    // The handler did not run, but the credential's owner is still logged.
    assert!(matches!(
        marked[0].identity,
        Some(octoemu::calls::Identity::Installation {
            installation_id: 5,
            ..
        })
    ));
    // The non-secret body fields are recorded, faulted or not.
    assert!(
        calls
            .iter()
            .filter(|c| c.method == "PUT")
            .all(|c| c.body_fields.as_ref().unwrap()["permission"] == "push")
    );
}

#[tokio::test]
async fn rules_filter_by_credential_and_body_and_never_touch_the_admin_api() {
    let world = World::start().await;
    world
        .emulator
        .add_fault(
            Fault::new("/repos/**", FaultAction::respond(500)).credential(CredentialKind::Jwt),
        )
        .unwrap();
    world
        .emulator
        .add_fault(Fault::on("PUT", PUT, FaultAction::respond(422)).body_contains("\"admin\""))
        .unwrap();
    world
        .emulator
        .add_fault(Fault::new("/__octoemu/**", FaultAction::respond(500)))
        .unwrap();
    world
        .emulator
        .add_fault(Fault::new("/**", FaultAction::respond(503)).method("DELETE"))
        .unwrap();
    assert_eq!(world.put("admin").await.unwrap().status(), 422);
    assert_eq!(world.put("push").await.unwrap().status(), 201);
    let (status, _) = world.admin(reqwest::Method::GET, "/health", None).await;
    assert_eq!(status, 200);
    let deleted = world
        .client
        .delete(world.url("/installation/token"))
        .bearer_auth(&world.token)
        .send()
        .await
        .unwrap();
    assert_eq!(deleted.status(), 503);
    // Faults apply to unimplemented routes too (no route matched).
    let other = world.client.get(world.url("/meta")).send().await.unwrap();
    assert_eq!(other.status(), 501);
}

#[tokio::test]
async fn apply_then_lose_changes_state_and_loses_the_response() {
    let world = World::start().await;
    let id = world
        .emulator
        .add_fault(Fault::on("PUT", PUT, FaultAction::ApplyThenLose { status: Some(502) }).once())
        .unwrap();
    let lost = world.put("push").await.unwrap();
    assert_eq!(lost.status(), 502);
    assert!(lost.text().await.unwrap().contains("response lost"));
    // The invitation exists all the same.
    assert_eq!(world.invitations().await, 1);
    let call = world
        .emulator
        .calls()
        .into_iter()
        .find(|c| c.method == "PUT")
        .unwrap();
    let mark = call.fault.unwrap();
    assert_eq!((mark.rule, mark.handler_status), (id, Some(201)));
    assert_eq!(call.status, 502);
    assert_eq!(
        call.route.as_deref(),
        Some("PUT /repos/{owner}/{repo}/collaborators/{username}")
    );

    // Without a status the connection closes after the write.
    world
        .emulator
        .add_fault(Fault::on(
            "DELETE",
            "/repos/acme/widgets/invitations/{id}",
            FaultAction::ApplyThenLose { status: None },
        ))
        .unwrap();
    let invitation: Value = world
        .client
        .get(world.url("/repos/acme/widgets/invitations"))
        .bearer_auth(&world.token)
        .send()
        .await
        .unwrap()
        .value()
        .await
        .unwrap();
    let path = format!("/repos/acme/widgets/invitations/{}", invitation[0]["id"]);
    let dropped = world
        .client
        .delete(world.url(&path))
        .bearer_auth(&world.token)
        .send()
        .await;
    assert!(dropped.is_err(), "{dropped:?}");
    assert_eq!(world.invitations().await, 0);
    let call = world
        .emulator
        .calls()
        .into_iter()
        .find(|c| c.method == "DELETE")
        .unwrap();
    assert_eq!(call.status, 0);
    assert_eq!(call.fault.unwrap().handler_status, Some(204));
}

#[tokio::test]
async fn close_and_reset_drop_the_connection_without_running_the_handler() {
    let world = World::start().await;
    let close = world
        .emulator
        .add_fault(Fault::on("PUT", PUT, FaultAction::Close).once())
        .unwrap();
    let closed = world.put("push").await;
    assert!(closed.is_err(), "{closed:?}");
    world
        .emulator
        .add_fault(Fault::on("PUT", PUT, FaultAction::Reset).once())
        .unwrap();
    let reset = world.put("push").await;
    assert!(reset.is_err(), "{reset:?}");
    assert_eq!(world.invitations().await, 0);
    assert_eq!(world.emulator.fault(close).unwrap().fired, 1);
    // The emulator keeps serving.
    assert_eq!(world.put("push").await.unwrap().status(), 201);
    let statuses: Vec<(u16, Option<String>)> = world
        .emulator
        .calls()
        .into_iter()
        .filter(|c| c.method == "PUT")
        .map(|c| (c.status, c.fault.map(|f| f.action)))
        .collect();
    assert_eq!(
        statuses,
        [
            (0, Some("close".to_owned())),
            (0, Some("reset".to_owned())),
            (201, None)
        ]
    );
}

#[tokio::test]
async fn delays_wait_in_real_time_or_move_the_virtual_clock() {
    let world = World::start().await;
    world
        .emulator
        .add_fault(
            Fault::on(
                "GET",
                "/repos/acme/widgets",
                FaultAction::Delay {
                    ms: 300,
                    clock: DelayClock::Real,
                },
            )
            .once(),
        )
        .unwrap();
    let started = Instant::now();
    let response = world
        .client
        .get(world.url("/repos/acme/widgets"))
        .bearer_auth(&world.token)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert!(started.elapsed() >= Duration::from_millis(300));

    let before = world.emulator.now();
    world
        .emulator
        .add_fault(
            Fault::on(
                "GET",
                "/repos/acme/widgets",
                FaultAction::Delay {
                    ms: 3_600_000,
                    clock: DelayClock::Virtual,
                },
            )
            .once(),
        )
        .unwrap();
    let started = Instant::now();
    let response = world
        .client
        .get(world.url("/repos/acme/widgets"))
        .bearer_auth(&world.token)
        .send()
        .await
        .unwrap();
    assert!(started.elapsed() < Duration::from_secs(5));
    // The token is an hour old by then: GitHub's model refuses it.
    assert_eq!(response.status(), 401);
    assert!(world.emulator.now() - before >= chrono::TimeDelta::hours(1));
    let call = world.emulator.calls().pop().unwrap();
    assert_eq!(call.fault.unwrap().handler_status, Some(401));
}

#[tokio::test]
async fn hold_waits_for_release() {
    let world = World::start().await;
    let id = world
        .emulator
        .add_fault(Fault::on("PUT", PUT, FaultAction::Hold))
        .unwrap();
    let put = {
        let client = world.client.clone();
        let url = world.url(PUT);
        let token = world.token.clone();
        tokio::spawn(async move {
            client
                .put(url)
                .bearer_auth(token)
                .json_body(&json!({"permission": "push"}))
                .send()
                .await
                .unwrap()
                .status()
        })
    };
    assert!(
        world
            .emulator
            .wait_for_held(id, 1, Duration::from_secs(5))
            .await
    );
    assert_eq!(world.invitations().await, 0);
    let (status, body) = world
        .admin(reqwest::Method::GET, &format!("/faults/{id}"), None)
        .await;
    assert_eq!(status, 200);
    assert_eq!(
        (body["held"].as_u64(), body["released"].as_bool()),
        (Some(1), Some(false))
    );
    let (status, _) = world
        .admin(
            reqwest::Method::POST,
            &format!("/faults/{id}/release"),
            None,
        )
        .await;
    assert_eq!(status, 204);
    assert_eq!(put.await.unwrap(), 201);
    assert_eq!(world.invitations().await, 1);
    // Released: later requests pass.
    assert_eq!(world.put("pull").await.unwrap().status(), 201);
    let (status, _) = world
        .admin(
            reqwest::Method::POST,
            &format!("/faults/{id}/wait"),
            Some(json!({"held": 1, "timeout_ms": 50})),
        )
        .await;
    assert_eq!(status, 504);
}

#[tokio::test]
async fn a_held_request_does_not_block_shutdown() {
    let world = World::start().await;
    let id = world
        .emulator
        .add_fault(Fault::on("PUT", PUT, FaultAction::Hold))
        .unwrap();
    let put = {
        let client = world.client.clone();
        let url = world.url(PUT);
        let token = world.token.clone();
        tokio::spawn(async move {
            client
                .put(url)
                .bearer_auth(token)
                .json_body(&json!({"permission": "push"}))
                .send()
                .await
        })
    };
    assert!(
        world
            .emulator
            .wait_for_held(id, 1, Duration::from_secs(5))
            .await
    );
    tokio::time::timeout(Duration::from_secs(10), world.emulator.shutdown())
        .await
        .expect("shutdown releases held requests")
        .unwrap();
    let _ = put.await.unwrap();
}

#[tokio::test]
async fn custom_responders_see_the_pattern_parameters() {
    let world = World::start().await;
    world
        .emulator
        .add_fault(Fault::on(
            "GET",
            "/users/{username}",
            FaultAction::custom(|request| {
                Reply::new(200).json(json!({"id": 99, "login": request.params["username"]}))
            }),
        ))
        .unwrap();
    let body: Value = world
        .client
        .get(world.url("/users/hubot"))
        .send()
        .await
        .unwrap()
        .value()
        .await
        .unwrap();
    assert_eq!(body, json!({"id": 99, "login": "hubot"}));
    assert_eq!(world.emulator.faults()[0].action, json!({"kind": "custom"}));
}

#[tokio::test]
async fn the_admin_api_adds_lists_and_removes_rules() {
    let world = World::start().await;
    let (status, created) = world
        .admin(
            reqwest::Method::POST,
            "/faults",
            Some(json!({
                "method": "GET", "path": "/repos/{owner}/{repo}", "credential": "installation_token",
                "times": 2, "label": "flaky read",
                "action": {"kind": "respond", "status": 503, "body": {"message": "Service unavailable"}}
            })),
        )
        .await;
    assert_eq!(status, 201, "{created}");
    let id = created["id"].as_u64().unwrap();
    assert_eq!(created["label"], "flaky read");
    assert_eq!(created["action"]["status"], 503);
    let read = world
        .client
        .get(world.url("/repos/acme/widgets"))
        .bearer_auth(&world.token)
        .send()
        .await
        .unwrap();
    assert_eq!(read.status(), 503);
    let (_, listed) = world.admin(reqwest::Method::GET, "/faults", None).await;
    assert_eq!(listed["faults"][0]["fired"], 1);
    for (body, why) in [
        (
            json!({"path": "repos", "action": {"kind": "close"}}),
            "relative path",
        ),
        (
            json!({"path": "/x", "action": {"kind": "respond", "status": 42}}),
            "status",
        ),
        (
            json!({"path": "/x", "action": {"kind": "custom"}}),
            "custom over HTTP",
        ),
        (
            json!({"path": "/x", "action": {"kind": "explode"}}),
            "unknown action",
        ),
    ] {
        let (status, _) = world
            .admin(reqwest::Method::POST, "/faults", Some(body))
            .await;
        assert!(status == 400 || status == 422, "{why}: {status}");
    }
    let (status, _) = world
        .admin(reqwest::Method::DELETE, &format!("/faults/{id}"), None)
        .await;
    assert_eq!(status, 204);
    let (status, _) = world
        .admin(reqwest::Method::DELETE, &format!("/faults/{id}"), None)
        .await;
    assert_eq!(status, 404);
    world
        .emulator
        .add_fault(Fault::new("/**", FaultAction::Close))
        .unwrap();
    let (status, _) = world.admin(reqwest::Method::DELETE, "/faults", None).await;
    assert_eq!(status, 204);
    assert!(world.emulator.faults().is_empty());
    let read = world
        .client
        .get(world.url("/repos/acme/widgets"))
        .bearer_auth(&world.token)
        .send()
        .await
        .unwrap();
    assert_eq!(read.status(), 200);
}
