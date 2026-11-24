//! P1 M0 role/fence acceptance through real Restate, SQLite and GitHub HTTP.
//! Covers AC-ROLE-1/2/4/5/6 (M0), AC-DLV-4, and the fence/rejection part of
//! AC-DLV-5, including the app-scoped permission recheck after release.
//! Three real lost-run-ack retries and three later create calls after
//! timeout each preserve one PUT. The M1 budget gate is separate.
//! This bounded create-journal marker scan is not the full AC-JRN-1 sys_* gate.
#![cfg(feature = "integration")]
use axum::{
    Json,
    extract::State,
    response::{IntoResponse, Response},
};
use chrono::Utc;
use ghinvite_core::{
    GithubInvitationId, InvitationLinkId, Permission, RequestId,
    delivery::{BlockedReason, CreateCommand, CreateOutcome, CreateReceipt},
    storage::{DeliveryStorage, RecordStorage},
    targets::{CustomRoleName, EffectiveRole, RepoRole},
};
use ghinvite_e2e::{Call, Harness, journal::JournalAudit};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex, atomic::Ordering},
    time::Duration,
};

const MARKER: &str = "ghs_PROTOCOL_MARKER";
const ROLE_PATH: &str = "/repos/acme/api/collaborators/alice/permission";
const PUT_PATH: &str = "/repos/acme/api/collaborators/alice";
const PATCH_PATH: &str = "/repos/acme/api/invitations/99";
const MINT_PATH: &str = "/app/installations/1/access_tokens";

#[derive(Clone, Copy)]
enum WriteReply {
    Status(u16),
    Policy(u16, &'static str),
    Timeout,
    RateLimited(u64),
    Spammed,
}
#[derive(Clone, Debug)]
struct Trace {
    method: String,
    path: String,
    body: Value,
    fence: Option<(i64, i64)>,
    // A non-secret scope/generation label, never the Authorization value.
    credential: Option<(String, usize)>,
}
struct Scenario {
    sequence_property: bool,
    sequence_replies: VecDeque<sequence_property::WireReply>,
    sequence_consumed: Vec<sequence_property::WireReply>,
    key: Option<String>,
    roles: Vec<&'static str>,
    role_reads: usize,
    pending: Option<&'static str>,
    // An invitation at this role appears after S1's listing: the PUT answers
    // 201 with it, at its own role, as GitHub does for a pending invitation.
    raced_invitation: Option<&'static str>,
    writes: VecDeque<WriteReply>,
    read_mints: usize,
    write_mints: usize,
    write_mint_replies: VecDeque<u16>,
    installation_read_replies: VecDeque<u16>,
    deletes: VecDeque<WriteReply>,
    created_at: Option<chrono::DateTime<Utc>>,
    invitation_clock_override: Option<Value>,
    accept_after_metadata: bool,
    // Invitation listings that showed the pending invitation. Generated cases
    // use it to tell a workflow read that observed presence from absence.
    presence_listings: usize,
    trace: Vec<Trace>,
}
impl Default for Scenario {
    fn default() -> Self {
        Self {
            sequence_property: false,
            sequence_replies: VecDeque::new(),
            sequence_consumed: vec![],
            key: None,
            roles: vec!["none"],
            role_reads: 0,
            pending: None,
            raced_invitation: None,
            writes: VecDeque::from([WriteReply::Status(201)]),
            read_mints: 0,
            write_mints: 0,
            write_mint_replies: VecDeque::new(),
            installation_read_replies: VecDeque::new(),
            deletes: VecDeque::from([WriteReply::Status(204)]),
            created_at: None,
            invitation_clock_override: None,
            accept_after_metadata: false,
            presence_listings: 0,
            trace: vec![],
        }
    }
}
struct Github {
    scenario: Mutex<Scenario>,
    sql: sqlx::SqlitePool,
}
fn invitation(permission: &str) -> Value {
    json!({"id":99,"invitee":{"id":8,"login":"alice"},"permissions":permission,"created_at":"2026-05-04T12:00:00Z","expired":false})
}
async fn github(State(state): State<Arc<Github>>, request: axum::extract::Request) -> Response {
    let method = request.method().to_string();
    let path = request.uri().path().to_owned();
    let credential = request
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer ghs_PROTOCOL_MARKER_"))
        .and_then(|v| v.rsplit_once('_'))
        .map(|(scope, generation)| (scope.to_owned(), generation.parse::<usize>().unwrap()));
    let bytes = axum::body::to_bytes(request.into_body(), 64 * 1024)
        .await
        .unwrap();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    let key = state.scenario.lock().unwrap().key.clone();
    let fence = if let Some(key) = key {
        sqlx::query_as::<_, (i64,i64)>("SELECT attempt_generation,retryable FROM delivery_attempts WHERE delivery_key=? AND step='repo'")
            .bind(key).fetch_optional(&state.sql).await.unwrap()
    } else {
        None
    };
    if let Some((reply, delay)) =
        sequence_property::scripted_write(&state, &method, &path, &body, fence, &credential)
    {
        if delay {
            tokio::time::sleep(Duration::from_secs(3)).await;
        }
        return reply;
    }
    let (reply, delay) = {
        let mut state = state.scenario.lock().unwrap();
        state.trace.push(Trace {
            method: method.clone(),
            path: path.clone(),
            body: body.clone(),
            fence,
            credential: credential.clone(),
        });
        let mut delay = false;
        let response = match (method.as_str(), path.as_str()) {
            ("POST", MINT_PATH) => {
                let scope = match body["permissions"]["administration"].as_str() {
                    Some("read") => { state.read_mints += 1; "read" },
                    Some("write") => { state.write_mints += 1; "write" },
                    _ => "legacy",
                };
                let generation = if scope == "write" { state.write_mints } else { state.read_mints };
                if scope != "legacy" { assert_eq!(body, json!({"permissions":{"administration":scope,"metadata":"read"}})); }
                if scope == "write" && let Some(status) = state.write_mint_replies.pop_front() {
                    assert!(matches!(status, 422 | 429));
                    return (axum::http::StatusCode::from_u16(status).unwrap(), Json(json!({"message":if status == 429 { "secondary rate limit" } else { "The permissions requested are not granted to this installation." }}))).into_response();
                }
                (axum::http::StatusCode::CREATED, Json(json!({"token":format!("{MARKER}_{scope}_{generation}"),"expires_at":Utc::now()+chrono::Duration::hours(1)}))).into_response()
            },
            ("GET", "/app/installations/1") if !state.installation_read_replies.is_empty() => {
                let status=state.installation_read_replies.pop_front().unwrap();
                (axum::http::StatusCode::from_u16(status).unwrap(),[("retry-after","90")],Json(json!({"message":"secondary rate limit"}))).into_response()
            },
            ("GET", "/app/installations/1") => Json(json!({"id":1,"account":{"id":100,"login":"acme","type":"Organization"},"app_id":123,"created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T00:00:00Z","repository_selection":"all","permissions":{"administration":"write","metadata":"read"},"suspended_at":null})).into_response(),
            ("GET", "/installation/repositories") => Json(json!({"total_count":1,"repositories":[{"id":10,"full_name":"acme/api","private":true}]})).into_response(),
            ("GET", "/repos/acme/api") => Json(json!({"id":10,"owner":{"id":100,"login":"acme"},"full_name":"acme/api","private":true})).into_response(),
            ("GET", "/user/8" | "/users/alice") => Json(json!({"id":8,"login":"alice"})).into_response(),
            ("GET", ROLE_PATH) => {
                let role = state.roles[state.role_reads.min(state.roles.len()-1)]; state.role_reads += 1;
                Json(json!({"user":{"id":8,"login":"alice"},"permission":"write","role_name":role})).into_response()
            },
            ("GET", "/repos/acme/api/invitations") => {
                let response=state.pending.map(|p| { let mut i=invitation(p); if let Some(at)=state.created_at { i["created_at"]=json!(at); } json!([i]) }).unwrap_or(json!([]));
                if state.pending.is_some() { state.presence_listings += 1; }
                if state.pending.is_some() && state.accept_after_metadata { state.pending=None; state.roles=vec!["write"]; }
                Json(response).into_response()
            },
            ("DELETE", PUT_PATH) => {
                assert_eq!(credential.as_ref().map(|c|c.0.as_str()),Some("write"));
                let reply=state.deletes.pop_front().expect("unexpected extra removal");
                let status=match reply { WriteReply::Status(n)|WriteReply::Policy(n,_)=>n,WriteReply::Timeout=>{delay=true;503},WriteReply::RateLimited(_)=>429,WriteReply::Spammed=>422 };
                if status==204 || matches!(reply,WriteReply::Timeout) {state.roles=vec!["none"];state.role_reads=0;}
                (axum::http::StatusCode::from_u16(status).unwrap(),Json(json!({"message":"bounded fixture"}))).into_response()
            },
            ("DELETE", PATCH_PATH) => {
                assert_eq!(credential.as_ref().map(|c|c.0.as_str()), Some("write"));
                let reply=state.deletes.pop_front().expect("unexpected extra DELETE");
                let status=match reply { WriteReply::Status(n)|WriteReply::Policy(n,_)=>n, WriteReply::Timeout=>{delay=true;503}, WriteReply::RateLimited(_)=>429, WriteReply::Spammed=>422 };
                if status==204 || matches!(reply,WriteReply::Timeout) {state.pending=None;}
                if let WriteReply::Policy(404, policy)=reply { state.pending=None; if policy=="accepted" {state.roles=vec!["write"];} }
                if let WriteReply::RateLimited(seconds)=reply { (axum::http::StatusCode::from_u16(status).unwrap(), [("retry-after",seconds.to_string())], Json(json!({"message":"secondary rate limit"}))).into_response() }
                else if matches!(reply, WriteReply::Spammed) { (axum::http::StatusCode::from_u16(status).unwrap(),Json(json!({"errors":[{"code":"spammed"}]}))).into_response() }
                else { (axum::http::StatusCode::from_u16(status).unwrap(),Json(json!({"message":"bounded fixture"}))).into_response() }
            },
            ("GET", PUT_PATH) => axum::http::StatusCode::NOT_FOUND.into_response(),
            ("PUT", PUT_PATH) | ("PATCH", PATCH_PATH) => {
                assert_eq!(credential.as_ref().map(|c|c.0.as_str()), Some("write"));
                assert_eq!(body, if method=="PUT" {json!({"permission":"push"})} else {json!({"permissions":"write"})});
                let reply = state.writes.pop_front().expect("unexpected extra GitHub mutation");
                let status = match reply { WriteReply::Status(status) | WriteReply::Policy(status, _) => status, WriteReply::Timeout => {delay=true;503}, WriteReply::RateLimited(_)=>429, WriteReply::Spammed=>422 };
                let permission = match method.as_str() { "PUT" => state.raced_invitation.unwrap_or("write"), _ => "write" };
                if status==201 || status==200 { state.pending=Some(permission); if state.created_at.is_none() { state.created_at=Some(Utc::now()); } }
                let status = axum::http::StatusCode::from_u16(status).unwrap();
                if let WriteReply::RateLimited(seconds) = reply { (status, [("retry-after", seconds.to_string())], Json(json!({"message":"secondary rate limit"}))).into_response() }
                else if matches!(reply, WriteReply::Spammed) { (status,Json(json!({"errors":[{"code":"spammed"}]}))).into_response() }
                else if status==axum::http::StatusCode::NO_CONTENT { status.into_response() }
                else if status.is_success() {
                    let mut response = invitation(permission);
                    if let Some(at) = state.created_at {
                        response["created_at"] = json!(at);
                    }
                    if let Some(at) = &state.invitation_clock_override {
                        response["created_at"] = at.clone();
                    }
                    (status, Json(response)).into_response()
                }
                else { (status,Json(json!({"message":match reply { WriteReply::Policy(_, message) => message, _ => "Bad credentials" }, "private":"POLICY_SECRET_MARKER"}))).into_response() }
            },
            _ => panic!("unexpected GitHub HTTP operation: {method} {path}"),
        };
        (response, delay)
    };
    if delay {
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
    reply
}

/// A token cache the fixture empties before each command, so every command,
/// and the app-scoped permission recheck it may trigger, creates its own
/// tokens through `GithubApps`.
struct FreshTokens {
    app: octotoken::octoauth::App,
    provider: std::sync::RwLock<octotoken::AppTokenProvider>,
}
impl FreshTokens {
    fn new(app: octotoken::octoauth::App) -> Self {
        Self {
            provider: std::sync::RwLock::new(octotoken::AppTokenProvider::new(app.clone())),
            app,
        }
    }
    fn reset(&self) {
        *self.provider.write().unwrap() = octotoken::AppTokenProvider::new(self.app.clone());
    }
    fn current(&self) -> octotoken::AppTokenProvider {
        self.provider.read().unwrap().clone()
    }
}
impl octotoken::InstallationTokenProvider for FreshTokens {
    async fn token(
        &self,
        installation: octotoken::InstallationId,
        scope: &octotoken::TokenScope,
    ) -> Result<octotoken::InstallationToken, octotoken::TokenError> {
        let provider = self.current();
        provider.token(installation, scope).await
    }
    async fn replace(
        &self,
        rejected: &octotoken::InstallationToken,
    ) -> Result<octotoken::InstallationToken, octotoken::TokenError> {
        let provider = self.current();
        provider.replace(rejected).await
    }
    fn invalidate(&self, rejected: &octotoken::InstallationToken) {
        self.current().invalidate(rejected);
    }
    fn invalidate_installation(&self, installation: octotoken::InstallationId) {
        self.current().invalidate_installation(installation);
    }
}

struct Runtime {
    harness: Harness,
    storage: Arc<ghinvite_storage_sqlx::SqlxStorage>,
    github: Arc<Github>,
    tokens: Vec<Arc<FreshTokens>>,
    faults: Arc<ghinvite_workflows::delivery::DeliveryFaults>,
    access_faults: Arc<ghinvite_workflows::access_grant::AccessGrantFaults>,
    database: std::path::PathBuf,
    last_create: Mutex<Option<String>>,
}
impl Runtime {
    async fn start() -> Self {
        Self::start_mode(false).await
    }
    async fn start_mode(cancellation: bool) -> Self {
        Self::start_options(cancellation, false).await
    }
    async fn start_options(cancellation: bool, budget: bool) -> Self {
        Self::start_all(cancellation, budget, false).await
    }
    async fn start_access() -> Self {
        Self::start_all(false, false, true).await
    }
    async fn start_all(cancellation: bool, budget: bool, access: bool) -> Self {
        let database =
            std::env::temp_dir().join(format!("repository-protocol-{}.sqlite", RequestId::new()));
        let storage = Arc::new(
            ghinvite_storage_sqlx::SqlxStorage::at_path(&database)
                .await
                .unwrap(),
        );
        let sql = sqlx::SqlitePool::connect_with(
            sqlx::sqlite::SqliteConnectOptions::new().filename(&database),
        )
        .await
        .unwrap();
        let github_state = Arc::new(Github {
            scenario: Mutex::new(Scenario::default()),
            sql,
        });
        let harness = Harness::start_with_github_router(
            axum::Router::new()
                .fallback(github)
                .with_state(github_state.clone()),
        )
        .await;
        for (user_id, login) in [(7, "creator"), (8, "alice")] {
            storage
                .upsert_user(&ghinvite_core::User {
                    user_id,
                    login: login.into(),
                    avatar_url: None,
                    github_created_at: None,
                    last_seen_at: Utc::now(),
                })
                .await
                .unwrap();
        }
        // Real HTTP transport with a short fixture read deadline. Request-level
        // total timeout still exists; this makes the intentional header stall bounded.
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .read_timeout(Duration::from_millis(500))
            .build()
            .unwrap();
        let github_transport = ghinvite_e2e::github_transport(client);
        let mut tokens = Vec::new();
        let installed = ghinvite_e2e::repository_apps(&harness.github_url)
            .into_iter()
            .map(|installed| {
                let fresh = Arc::new(FreshTokens::new(installed.app.clone()));
                tokens.push(fresh.clone());
                (installed, fresh as Arc<dyn ghinvite_github::TokenSource>)
            })
            .collect::<Vec<_>>();
        let github_apps = ghinvite_github::GithubApps::with_sources(
            ghinvite_github::rest::RestEnv::new(github_transport, &harness.github_url, "p1")
                .unwrap(),
            installed,
        )
        .unwrap();
        let apps = Arc::new(ghinvite_workflows::RepositoryGithub::with_apps(github_apps));
        let state = ghinvite_workflows::AppState::new(storage.clone()).with_github_apps(apps);
        let state = if cancellation {
            state.with_pre_activation_cancellation()
        } else {
            state
        };
        let state = if budget {
            state.with_invite_budget(storage.clone())
        } else {
            state
        };
        let state = if access {
            state.with_access_grants(storage.clone())
        } else {
            state
        };
        let builder = restate_sdk::endpoint::Endpoint::builder()
            .bind(ghinvite_workflows::reconcile::Reconcile {
                state: state.clone(),
            })
            .bind(final_notice::InvitationRequest)
            .bind(ghinvite_workflows::installation::Installation {
                state: state.clone(),
            })
            .bind(ghinvite_workflows::availability::AccountInstallation {
                state: state.clone(),
            })
            .bind(ghinvite_workflows::availability::InstallationProjection {
                state: state.clone(),
            });
        let builder = ghinvite_workflows::projection::bind(builder, storage.clone());
        let builder = if cancellation || budget || access {
            ghinvite_workflows::invite_budget::bind_pre_activation(
                builder,
                storage.clone(),
                Default::default(),
            )
        } else {
            builder
        };
        let access_faults =
            Arc::new(ghinvite_workflows::access_grant::AccessGrantFaults::default());
        let builder = if access {
            ghinvite_workflows::access_grant::bind_with_faults(
                builder,
                state.clone(),
                storage.clone(),
                access_faults.clone(),
            )
        } else {
            builder
        };
        let faults = Arc::new(ghinvite_workflows::delivery::DeliveryFaults::default());
        harness
            .restate
            .deploy(
                ghinvite_workflows::delivery::bind_with_faults(builder, state, faults.clone())
                    .build(),
            )
            .await;
        JournalAudit::retain(
            &harness,
            &["RepositoryDelivery", "Installation", "AccountInstallation"],
        )
        .await;
        let reply=harness.restate.invoke(&Call::object("Installation", "repo:1","onboard"),Some(&ghinvite_e2e::setup_command(json!({"app":"repo","installation_id":1,"actor_user_id":7,"account_id":100,"account_login":"acme","account_type":"Organization","selected_repos":"all","installed_at":Utc::now()}))),None).await;
        assert_eq!(reply.status, 200, "{reply:?}");
        Self {
            harness,
            storage,
            github: github_state,
            tokens,
            faults,
            access_faults,
            database,
            last_create: Mutex::new(None),
        }
    }
    async fn command(&self, scenario: Scenario, permission: Permission) -> CreateCommand {
        let fixture_time = scenario.created_at.unwrap_or_else(Utc::now);
        for tokens in &self.tokens {
            tokens.reset();
        }
        let link:ghinvite_core::InvitationLink=serde_json::from_value(json!({"secret":{"secret_generation":1,"sha256":"2a".repeat(32)},"id":InvitationLinkId::new(),"installation_id":1,"account_id":100,"created_by":7,"created_at":fixture_time,"expires_at":null,"max_uses":null,"uses_count":1,"approval_required":true,"description":"Protocol acceptance","internal_note":null,"revoked_at":null,"revoked_by":null,"targets":{"organization":null,"teams":[],"repos":[{"repo_id":10,"repo_full_name":"acme/api","grant":ghinvite_core::targets::RepoRole::from(permission)}]}})).unwrap();
        let request:ghinvite_core::InvitationRequest=serde_json::from_value(json!({"id":RequestId::new(),"invitation_link_id":link.id,"requester_id":8,"justification":"Protocol","state":"approved","decided_by":7,"decided_at":fixture_time,"decline_reason":null,"created_at":fixture_time,"decision_deadline":null})).unwrap();
        ghinvite_core::storage::projection::fixture::seed(
            self.storage.as_ref(),
            &link,
            std::slice::from_ref(&request),
            1,
        )
        .await
        .unwrap();
        let command = CreateCommand {
            batch: None,
            budget_attribution: Default::default(),
            access_lifetime_seconds: None,
            budget_share_pct: 50,
            invitation_id: Some(GithubInvitationId::new()),
            delivery_generation: 1,
            link_id: Some(link.id),
            request_id: request.id,
            approval_id: format!("fixture/{}", request.id),
            account_id: 100,
            installation_id: 1,
            requester_id: 8,
            repo_id: 10,
            repo_full_name: "acme/api".into(),
            permission,
            approved_at: request.decided_at.unwrap(),
        };
        *self.github.scenario.lock().unwrap() = Scenario {
            key: Some(command.delivery_key()),
            ..scenario
        };
        command
    }
    async fn create(&self, command: &CreateCommand) -> (CreateReceipt, Vec<String>) {
        let reply = self
            .harness
            .restate
            .invoke(
                &Call::object("RepositoryDelivery", &command.delivery_key(), "create"),
                Some(&json!(command)),
                None,
            )
            .await;
        assert_eq!(reply.status, 200, "{reply:?}");
        *self.last_create.lock().unwrap() = Some(reply.invocation_id().to_owned());
        let journal = self
            .harness
            .restate
            .admin()
            .journal(reply.invocation_id())
            .await;
        assert!(
            !journal.is_empty(),
            "must inspect retained bytes, not absent journals"
        );
        assert!(
            journal.iter().all(|entry| !entry.raw_contains(MARKER)),
            "installation token marker reached create journal"
        );
        assert!(!reply.body.to_string().contains(MARKER));
        let names = journal
            .iter()
            .filter(|e| e.is_run())
            .filter_map(|e| e.name.clone())
            .collect();
        (serde_json::from_value(reply.body).unwrap(), names)
    }
    async fn assert_permission_recheck(&self, released_generation: Option<i64>) {
        let rows = tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                let response: Value = self.harness.client.post(format!("{}/query", self.harness.restate.admin_url()))
                    .header("accept", "application/json")
                    .json(&json!({"query":"SELECT id, status, completion_failure FROM sys_invocation WHERE target_service_name = 'Installation' AND target_service_key = 'repo:1' AND target_handler_name = 'recheck_permissions'"}))
                    .send().await.unwrap().error_for_status().unwrap().json().await.unwrap();
                let rows = response["rows"].as_array().expect("invocation rows");
                if rows.len() == 1 && rows[0]["status"] == "completed" { break rows.clone(); }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        }).await.expect("app-scoped permission recheck must complete");
        assert!(rows[0]["completion_failure"].is_null(), "{rows:?}");
        let recheck_id = rows[0]["id"].as_str().unwrap();
        let recheck = self.harness.restate.admin().journal(recheck_id).await;
        assert!(
            !recheck.is_empty(),
            "inspect the actual retained handler journal"
        );
        assert!(recheck.iter().all(|entry| !entry.raw_contains(MARKER)));
        assert!(
            recheck
                .iter()
                .any(|entry| entry.entry_type == "Command: Call"
                    && entry.raw_contains("AccountInstallation")
                    && entry.raw_contains("repo:100")
                    && entry.raw_contains("recheck_permissions")),
            "wrapper must reach the matching account owner"
        );
        let create_id = self.last_create.lock().unwrap().clone().unwrap();
        let create = self.harness.restate.admin().journal(&create_id).await;
        let sends = create
            .iter()
            .filter(|entry| {
                entry.entry_type == "Command: OneWayCall"
                    && entry.raw_contains("Installation")
                    && entry.raw_contains("repo:1")
                    && entry.raw_contains("recheck_permissions")
            })
            .collect::<Vec<_>>();
        assert_eq!(
            sends.len(),
            1,
            "exactly one durable app-scoped recheck dispatch"
        );
        // The bounded 401 releases occur inside S4. Its completion precedes
        // dispatch, and the real verification HTTP call below observes the
        // released SQL generation rather than merely a scheduled release.
        let write = create
            .iter()
            .find(|entry| entry.is_run() && entry.name.as_deref() == Some("guarded_github_create"))
            .expect("durable write stage");
        let completion = create
            .iter()
            .find(|entry| {
                entry.entry_type == "Notification: Run"
                    && entry.run_completion_id == write.run_completion_id
            })
            .expect("write stage completed before dispatch");
        assert!(
            sends[0].index > completion.index,
            "recheck must follow completed write/release stage"
        );
        let trace = self.trace();
        let verification = operations(&trace, "GET", "/app/installations/1");
        assert_eq!(
            verification.len(),
            1,
            "recheck verifies actual installed permissions"
        );
        assert_eq!(
            verification[0].fence,
            released_generation.map(|generation| (generation, 1))
        );
    }

    async fn fence(&self, command: &CreateCommand) -> Option<(i64, i64)> {
        sqlx::query_as("SELECT attempt_generation,retryable FROM delivery_attempts WHERE delivery_key=? AND step='repo'")
            .bind(command.delivery_key()).fetch_optional(&self.github.sql).await.unwrap()
    }
    fn trace(&self) -> Vec<Trace> {
        self.github.scenario.lock().unwrap().trace.clone()
    }
    async fn finish(self) {
        self.harness.finish().await;
        self.github.sql.close().await;
        drop(self.storage);
        std::fs::remove_file(&self.database).unwrap();
    }
}
fn operations<'a>(trace: &'a [Trace], method: &str, path: &str) -> Vec<&'a Trace> {
    trace
        .iter()
        .filter(|call| call.method == method && call.path == path)
        .collect()
}
fn assert_stages(names: &[String], write: bool) {
    let pre = names
        .iter()
        .position(|n| n == "pre_read_repository")
        .expect("durable S1");
    if write {
        assert!(
            names
                .iter()
                .position(|n| n == "guarded_github_create")
                .expect("durable S4")
                > pre
        );
    } else {
        assert!(!names.iter().any(|n| n == "guarded_github_create"));
    }
}

#[tokio::test]
async fn roles_pending_and_race_never_lower_access_or_claim_unneeded_fences() {
    let runtime = Runtime::start().await;
    let cases = [
        (
            vec!["maintain"],
            None,
            Permission::Push,
            CreateOutcome::AlreadySatisfied {
                effective_role: EffectiveRole::Maintain,
            },
            None,
            false,
        ),
        (
            vec!["security-reviewer"],
            None,
            Permission::Push,
            CreateOutcome::SkippedCustomRole {
                role_name: CustomRoleName::new("security-reviewer").unwrap(),
            },
            None,
            false,
        ),
        (
            vec!["read"],
            None,
            Permission::Push,
            CreateOutcome::Upgraded {
                from: EffectiveRole::Read,
                to: RepoRole::Write,
            },
            Some(("PUT", 204)),
            true,
        ),
        (
            vec!["none"],
            Some("admin"),
            Permission::Pull,
            CreateOutcome::AlreadyPending {
                upstream_id: 99,
                permission: RepoRole::Admin,
            },
            None,
            false,
        ),
        (
            vec!["none"],
            Some("read"),
            Permission::Push,
            CreateOutcome::AlreadyPending {
                upstream_id: 99,
                permission: RepoRole::Write,
            },
            Some(("PATCH", 200)),
            true,
        ),
        (
            vec!["read", "maintain"],
            None,
            Permission::Push,
            CreateOutcome::AlreadySatisfied {
                effective_role: EffectiveRole::Maintain,
            },
            None,
            true,
        ),
    ];
    for (roles, pending, permission, expected, mutation, write_stage) in cases {
        let command = runtime
            .command(
                Scenario {
                    roles,
                    pending,
                    writes: mutation
                        .map(|(_, s)| VecDeque::from([WriteReply::Status(s)]))
                        .unwrap_or_default(),
                    ..Default::default()
                },
                permission,
            )
            .await;
        let (receipt, names) = runtime.create(&command).await;
        assert_eq!(receipt.outcome, expected, "trace={:?}", runtime.trace());
        assert_stages(&names, write_stage);
        let trace = runtime.trace();
        let puts = operations(&trace, "PUT", PUT_PATH);
        let patches = operations(&trace, "PATCH", PATCH_PATH);
        assert_eq!(
            puts.len(),
            usize::from(mutation.is_some_and(|(m, _)| m == "PUT"))
        );
        assert_eq!(
            patches.len(),
            usize::from(mutation.is_some_and(|(m, _)| m == "PATCH"))
        );
        assert_eq!(runtime.fence(&command).await, mutation.map(|_| (1, 0)));
        let reads = operations(&trace, "GET", ROLE_PATH);
        assert_eq!(reads.len(), if write_stage { 2 } else { 1 });
        assert!(
            reads.iter().all(|r| r.fence.is_none()),
            "re-read must precede claim"
        );
        if write_stage {
            let reread = trace.iter().rposition(|r| r.path == ROLE_PATH).unwrap();
            let ensure = trace
                .iter()
                .position(|r| {
                    r.path == MINT_PATH && r.body["permissions"]["administration"] == "write"
                })
                .unwrap();
            assert!(ensure < reread);
        }
        let count = trace.len();
        assert_eq!(
            runtime.create(&command).await.0,
            receipt,
            "confirmed result is immutable"
        );
        assert_eq!(
            runtime.trace().len(),
            count,
            "confirmed replay performs no GitHub calls"
        );
    }
    runtime.finish().await;
}

#[tokio::test]
async fn invitation_raced_in_before_the_put_is_patched_to_the_grant() {
    let runtime = Runtime::start().await;
    let command = runtime
        .command(
            Scenario {
                raced_invitation: Some("read"),
                writes: VecDeque::from([WriteReply::Status(201), WriteReply::Status(200)]),
                ..Default::default()
            },
            Permission::Push,
        )
        .await;
    let (receipt, names) = runtime.create(&command).await;
    assert_eq!(
        receipt.outcome,
        CreateOutcome::AlreadyPending {
            upstream_id: 99,
            permission: RepoRole::Write
        },
        "trace={:?}",
        runtime.trace()
    );
    assert_stages(&names, true);
    let trace = runtime.trace();
    // The PUT's 201 carried the invitation at read: it changed nothing, so
    // its generation is released and the next one PATCHes the invitation.
    let puts = operations(&trace, "PUT", PUT_PATH);
    let patches = operations(&trace, "PATCH", PATCH_PATH);
    assert_eq!(puts.len(), 1);
    assert_eq!(puts[0].fence, Some((1, 0)));
    assert_eq!(patches.len(), 1);
    assert_eq!(patches[0].fence, Some((2, 0)));
    assert_eq!(runtime.fence(&command).await, Some((2, 0)));
    let count = trace.len();
    assert_eq!(runtime.create(&command).await.0, receipt);
    assert_eq!(
        runtime.trace().len(),
        count,
        "confirmed replay performs no GitHub calls"
    );
    runtime.finish().await;
}

#[tokio::test]
async fn unauthorized_write_releases_its_generation_and_remints_once() {
    let runtime = Runtime::start().await;
    let command = runtime
        .command(
            Scenario {
                writes: VecDeque::from([WriteReply::Status(401), WriteReply::Status(201)]),
                ..Default::default()
            },
            Permission::Push,
        )
        .await;
    let (receipt, names) = runtime.create(&command).await;
    assert_eq!(receipt.outcome, CreateOutcome::Created { upstream_id: 99 });
    assert_stages(&names, true);
    let trace = runtime.trace();
    let puts = operations(&trace, "PUT", PUT_PATH);
    assert_eq!(puts.len(), 2);
    assert_eq!(puts[0].fence, Some((1, 0)));
    assert_eq!(puts[1].fence, Some((2, 0)));
    assert_eq!(puts[0].credential, Some(("write".into(), 1)));
    assert_eq!(puts[1].credential, Some(("write".into(), 2)));
    let mints = operations(&trace, "POST", MINT_PATH);
    let writes = mints
        .iter()
        .filter(|c| c.body["permissions"]["administration"] == "write")
        .collect::<Vec<_>>();
    assert_eq!(writes.len(), 2);
    assert_eq!(writes[0].fence, None);
    assert_eq!(writes[1].fence, Some((1, 1)));
    assert_eq!(
        mints
            .iter()
            .filter(|c| c.body["permissions"]["administration"] == "read")
            .count(),
        1
    );
    assert_eq!(runtime.fence(&command).await, Some((2, 0)));
    assert_eq!(runtime.create(&command).await.0, receipt);
    assert_eq!(operations(&runtime.trace(), "PUT", PUT_PATH).len(), 2);
    runtime.finish().await;
}

#[tokio::test]
async fn two_unauthorized_writes_release_both_generations_and_block() {
    let runtime = Runtime::start().await;
    let command = runtime
        .command(
            Scenario {
                writes: VecDeque::from([WriteReply::Status(401), WriteReply::Status(401)]),
                ..Default::default()
            },
            Permission::Push,
        )
        .await;
    let (receipt, names) = runtime.create(&command).await;
    assert_eq!(
        receipt.outcome,
        CreateOutcome::Blocked {
            reason: BlockedReason::InstallationUnavailable
        }
    );
    assert_stages(&names, true);
    runtime.assert_permission_recheck(Some(2)).await;
    let trace = runtime.trace();
    let puts = operations(&trace, "PUT", PUT_PATH);
    assert_eq!(puts.len(), 2);
    assert_eq!(puts[0].fence, Some((1, 0)));
    assert_eq!(puts[1].fence, Some((2, 0)));
    let recheck_start = trace
        .iter()
        .position(|call| call.method == "GET" && call.path == "/app/installations/1")
        .unwrap();
    let remint = operations(&trace[..recheck_start], "POST", MINT_PATH)
        .into_iter()
        .filter(|c| c.body["permissions"]["administration"] == "write")
        .collect::<Vec<_>>();
    assert_eq!(remint.len(), 2);
    assert_eq!(remint[1].fence, Some((1, 1)));
    assert_eq!(runtime.fence(&command).await, Some((2, 1)));
    assert!(
        !runtime
            .storage
            .delivery_attempt_exists(&command)
            .await
            .unwrap()
    );
    let refresh_mints = operations(&trace[recheck_start..], "POST", MINT_PATH);
    assert_eq!(
        refresh_mints.len(),
        1,
        "one additional mint belongs to permission recheck, not another PUT attempt"
    );
    assert_eq!(refresh_mints[0].fence, Some((2, 1)));
    assert_eq!(operations(&trace, "PUT", PUT_PATH).len(), 2);
    runtime.finish().await;
}

#[tokio::test]
async fn write_mint_422_blocks_without_a_fence_and_rechecks_app_permissions() {
    let runtime = Runtime::start().await;
    let command = runtime
        .command(
            Scenario {
                write_mint_replies: VecDeque::from([422]),
                writes: VecDeque::new(),
                ..Default::default()
            },
            Permission::Push,
        )
        .await;
    let (receipt, names) = runtime.create(&command).await;
    assert_eq!(
        receipt.outcome,
        CreateOutcome::Blocked {
            reason: BlockedReason::InstallationPermissions
        }
    );
    assert_stages(&names, true);
    runtime.assert_permission_recheck(None).await;
    let trace = runtime.trace();
    assert!(operations(&trace, "PUT", PUT_PATH).is_empty());
    assert!(operations(&trace, "PATCH", PATCH_PATH).is_empty());
    assert_eq!(runtime.fence(&command).await, None);
    assert!(
        trace.iter().all(|call| call.fence.is_none()),
        "mint refusal cannot claim a write fence"
    );
    let boundary = trace
        .iter()
        .position(|call| call.method == "GET" && call.path == "/app/installations/1")
        .unwrap();
    let write_mints = |calls: &[Trace]| {
        operations(calls, "POST", MINT_PATH)
            .into_iter()
            .filter(|call| call.body["permissions"]["administration"] == "write")
            .count()
    };
    assert_eq!(
        write_mints(&trace[..boundary]),
        1,
        "one refused write credential mint"
    );
    assert_eq!(
        write_mints(&trace[boundary..]),
        1,
        "one permission-refresh mint, with no delivery write"
    );
    runtime.finish().await;
}

async fn ambiguous_write(lost_ack: bool) {
    let runtime = Runtime::start().await;
    let command = runtime
        .command(
            Scenario {
                writes: VecDeque::from([if lost_ack {
                    WriteReply::Status(201)
                } else {
                    WriteReply::Timeout
                }]),
                ..Default::default()
            },
            Permission::Push,
        )
        .await;
    runtime
        .faults
        .lose_http_results
        .store(if lost_ack { 3 } else { 0 }, Ordering::SeqCst);
    let (receipt, names) = runtime.create(&command).await;
    assert_eq!(
        receipt.outcome,
        if lost_ack {
            CreateOutcome::Created { upstream_id: 99 }
        } else {
            CreateOutcome::OutcomeUnknown
        }
    );
    assert_stages(&names, true);
    assert_eq!(
        runtime.faults.http_result_losses.load(Ordering::SeqCst),
        if lost_ack { 3 } else { 0 }
    );
    assert_eq!(runtime.fence(&command).await, Some((1, 0)));
    for _ in 0..3 {
        let (replayed, _) = runtime.create(&command).await;
        assert_eq!(replayed.outcome, receipt.outcome);
    }
    assert_eq!(
        operations(&runtime.trace(), "PUT", PUT_PATH).len(),
        1,
        "retry/recovery must never repeat an ambiguous write"
    );
    assert_eq!(runtime.fence(&command).await, Some((1, 0)));
    runtime.finish().await;
}
#[tokio::test]
async fn three_lost_journal_acks_force_real_run_retries_without_second_put() {
    ambiguous_write(true).await;
}
#[tokio::test]
async fn http_timeout_and_repeated_recovery_never_repeat_put() {
    ambiguous_write(false).await;
}

#[tokio::test]
async fn policy_refusals_are_journaled_without_text_and_release_the_exact_fence() {
    let runtime = Runtime::start().await;
    for (status, message, reason) in [
        (
            403,
            "Inviting outside collaborators is restricted by organization policy.",
            BlockedReason::OutsideCollaboratorsRestricted,
        ),
        (
            422,
            "Inviting outside collaborators is restricted by enterprise policy.",
            BlockedReason::OutsideCollaboratorsRestricted,
        ),
        (
            403,
            "Although you appear to have the correct authorization credentials, the `policy-marker` organization has an IP allow list enabled, and your IP address is not permitted to access this resource.",
            BlockedReason::IpAllowList,
        ),
    ] {
        let command = runtime
            .command(
                Scenario {
                    writes: VecDeque::from([
                        WriteReply::Policy(status, message),
                        WriteReply::Policy(status, message),
                    ]),
                    ..Default::default()
                },
                Permission::Push,
            )
            .await;
        let (receipt, names) = runtime.create(&command).await;
        assert_eq!(receipt.outcome, CreateOutcome::Blocked { reason });
        assert_stages(&names, true);
        let trace = runtime.trace();
        let puts = operations(&trace, "PUT", PUT_PATH);
        assert_eq!(puts.len(), 1);
        assert_eq!(puts[0].fence, Some((1, 0)));
        assert_eq!(
            runtime.fence(&command).await,
            Some((1, 1)),
            "positive policy refusal releases its generation"
        );
        let invocation = runtime.last_create.lock().unwrap().clone().unwrap();
        let journal = runtime.harness.restate.admin().journal(&invocation).await;
        assert!(!journal.is_empty());
        for entry in journal {
            assert!(!entry.raw_contains("POLICY_SECRET_MARKER"));
            assert!(!entry.raw_contains(message));
        }
        // Blocked is deliberately nonterminal. A later create invocation can
        // recheck policy, but must claim a new generation for its one write.
        let retry = runtime.create(&command).await.0;
        assert_eq!(retry.command, receipt.command);
        assert_eq!(retry.outcome, receipt.outcome);
        assert_eq!(retry.revision, receipt.revision + 1);
        let trace = runtime.trace();
        let puts = operations(&trace, "PUT", PUT_PATH);
        assert_eq!(puts.len(), 2);
        assert_eq!(puts[1].fence, Some((2, 0)));
        assert_eq!(runtime.fence(&command).await, Some((2, 1)));
    }
    runtime.finish().await;
}

#[path = "repository_protocol/control.rs"]
mod control;

#[path = "repository_protocol/cancellation.rs"]
mod cancellation;

#[path = "repository_protocol/budget.rs"]
mod budget;

#[path = "repository_protocol/final_notice.rs"]
mod final_notice;

#[path = "repository_protocol/audit.rs"]
mod audit;

#[path = "repository_protocol/access_grants.rs"]
mod access_grants;

#[path = "repository_protocol/reissue.rs"]
mod reissue;

#[path = "repository_protocol/bulk.rs"]
mod bulk;

#[path = "repository_protocol/requester_refresh.rs"]
mod requester_refresh;

#[path = "repository_protocol/invitation_timing.rs"]
mod invitation_timing;

#[path = "repository_protocol/sequence_property.rs"]
mod sequence_property;
