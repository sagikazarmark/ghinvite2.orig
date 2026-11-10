//! Native form/ingress contract. The workflow suite separately proves durable
//! receipts; this fixture records the actual web command and lost-ack response.
#[path = "console_delegation/bulk_audit.rs"]
mod bulk_audit;
mod common;
#[path = "console_delegation/org_cap.rs"]
mod org_cap;
#[path = "console_delegation/reissue.rs"]
mod reissue;
use axum::{
    body::Body,
    extract::{Path, State},
    http::{Request, StatusCode},
    response::{IntoResponse, Response},
};
use common::{
    authority_http_fixture::FakeLinkAuthority,
    sign_in::{self, ConsoleInstallationFixture, ConsoleTransport, GithubUser, OCTOCAT},
};
use ghinvite_core::{
    InvitationLink, InvitationLinkId, RequestId, RequestState,
    account::{AccountSettings, DelegationRefusal, SetDelegationInput},
    request_lifecycle::{DecideRequest, DecisionOutcome, DecisionReceipt, RequestSnapshot},
    storage::{AppInstallationReadStorage, AppInstallationStorage},
};
use ghinvite_github::{Method, mocks::Expectation};
use ghinvite_storage_sqlx::SqlxStorage;
use ghinvite_web::RestateClient;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, VecDeque},
    sync::{Arc, Mutex},
};
use tower::ServiceExt;

const SETTINGS: &str = "/console/accounts/acme/settings";
const DELEGATION: &str = "/console/accounts/acme/settings/delegation";
#[derive(Clone, Copy)]
enum Reply {
    Ok,
    Refused,
    LostAck,
}
#[derive(Default)]
struct RpcState {
    replies: VecDeque<Reply>,
    calls: Vec<Value>,
    retained: BTreeMap<String, (SetDelegationInput, Result<(), DelegationRefusal>)>,
    applied: Vec<bool>,
}
struct SettingsRpc {
    state: Arc<Mutex<RpcState>>,
    client: Arc<RestateClient>,
}
impl SettingsRpc {
    async fn start(replies: Vec<Reply>) -> Self {
        let state = Arc::new(Mutex::new(RpcState {
            replies: replies.into(),
            ..Default::default()
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let client = Arc::new(
            RestateClient::new(format!("http://{}", listener.local_addr().unwrap())).unwrap(),
        );
        let router = axum::Router::new()
            .route(
                "/{service}/{key}/{method}",
                axum::routing::post(settings_rpc),
            )
            .with_state(state.clone());
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        Self { state, client }
    }
    fn calls(&self) -> Vec<Value> {
        self.state.lock().unwrap().calls.clone()
    }
    fn applied(&self) -> Vec<bool> {
        self.state.lock().unwrap().applied.clone()
    }
}
async fn settings_rpc(
    State(state): State<Arc<Mutex<RpcState>>>,
    Path((service, key, method)): Path<(String, String, String)>,
    bytes: axum::body::Bytes,
) -> Response {
    if method == "org_cap_policy" {
        assert_eq!(
            (service.as_str(), key.as_str()),
            ("AccountInstallation", "repo:9001")
        );
        assert!(bytes.is_empty(), "unit read has no request body");
        return axum::Json(ghinvite_core::budget::OrgCapView {
            enabled: false,
            policy: None,
        })
        .into_response();
    }
    assert_eq!(
        (service.as_str(), key.as_str(), method.as_str()),
        ("AccountInstallation", "repo:9001", "set_delegation")
    );
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    let command: SetDelegationInput = serde_json::from_value(body.clone()).unwrap();
    let operation = String::from(command.operation_id.clone());
    let mut state = state.lock().unwrap();
    state.calls.push(body);
    if let Some((old, outcome)) = state.retained.get(&operation) {
        return if old == &command {
            axum::Json(outcome).into_response()
        } else {
            StatusCode::CONFLICT.into_response()
        };
    }
    let reply = state
        .replies
        .pop_front()
        .expect("explicit ingress response");
    let outcome = if matches!(reply, Reply::Refused) {
        Err(DelegationRefusal::NotOwner)
    } else {
        Ok(())
    };
    if outcome.is_ok() {
        state.applied.push(command.on)
    }
    state.retained.insert(operation, (command, outcome));
    if matches!(reply, Reply::LostAck) {
        StatusCode::SERVICE_UNAVAILABLE.into_response()
    } else {
        axum::Json(outcome).into_response()
    }
}
fn repositories(owner: &str, account: u64) -> Expectation {
    Expectation::ok_json(
        Method::Get,
        "https://api.github.com/installation/repositories?per_page=100",
        json!({"total_count":1,"repositories":[{"id":10,"full_name":format!("{owner}/api"),"owner":{"id":account,"login":owner}}]}),
    )
}
async fn fixture(
    client: Arc<RestateClient>,
    user: GithubUser<'_>,
    settings_read: bool,
) -> (axum::Router, String, Arc<SqlxStorage>) {
    fixture_with_reads(client, user, settings_read, vec![]).await
}
async fn fixture_with_reads(
    client: Arc<RestateClient>,
    user: GithubUser<'_>,
    settings_read: bool,
    extra: Vec<Expectation>,
) -> (axum::Router, String, Arc<SqlxStorage>) {
    fixture_with_reissues(client, user, settings_read, extra, false).await
}
async fn fixture_with_reissues(
    client: Arc<RestateClient>,
    user: GithubUser<'_>,
    settings_read: bool,
    extra: Vec<Expectation>,
    reissue_enabled: bool,
) -> (axum::Router, String, Arc<SqlxStorage>) {
    let storage = Arc::new(SqlxStorage::in_memory().await.unwrap());
    storage
        .seed_console_installation(&sign_in::acme_installation())
        .await
        .unwrap();
    let mut github = sign_in::oauth_expectations(user);
    if settings_read {
        github.push(repositories("acme", 9001));
    }
    github.extend(extra);
    let mut config = sign_in::console_config();
    config.reissue_enabled = reissue_enabled;
    let state = crate::common::sign_in::app_state(
        storage.clone(),
        Arc::new(ConsoleTransport::scripted(github)),
        client,
        config,
    );
    let app = common::build_app(state, tower_sessions::MemoryStore::default());
    let cookie = sign_in::sign_in(&app).await;
    (app, cookie, storage)
}
async fn request(
    app: &axum::Router,
    cookie: &str,
    method: &str,
    path: &str,
    form: &str,
) -> Response {
    app.clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header("cookie", cookie)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from(form.to_owned()))
                .unwrap(),
        )
        .await
        .unwrap()
}
async fn html(response: Response) -> String {
    String::from_utf8(
        response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec(),
    )
    .unwrap()
}
fn input<'a>(html: &'a str, name: &str) -> &'a str {
    let start = html.find(&format!("name=\"{name}\"")).unwrap();
    let before = &html[..start];
    let start = before.rfind("<input").unwrap();
    let end = html[start..].find('>').unwrap();
    &html[start..start + end + 1]
}
fn value(html: &str, name: &str) -> String {
    input(html, name)
        .split("value=\"")
        .nth(1)
        .unwrap()
        .split('"')
        .next()
        .unwrap()
        .to_owned()
}

#[tokio::test]
async fn owner_settings_shows_explicit_warning_default_off_and_valid_operation() {
    let rpc = SettingsRpc::start(vec![]).await;
    let (app, cookie, storage) = fixture(rpc.client.clone(), OCTOCAT, true).await;
    assert!(storage.account_settings(9001).await.unwrap().is_none());
    let response = request(&app, &cookie, "GET", SETTINGS, "").await;
    assert_eq!(response.status(), StatusCode::OK);
    let page = html(response).await;
    assert!(page.contains("Repository admin delegation"));
    assert!(page.contains("Repository admins will be able to create links and approve requests that add outside collaborators"));
    assert!(page.contains("setting may not apply to P1"));
    assert!(page.contains(DELEGATION));
    assert!(!input(&page, "on").contains("checked"));
    let operation = value(&page, "operation_id");
    assert!(ghinvite_core::admission::AdmissionOperationId::try_from(operation).is_ok());
    assert!(!value(&page, "csrf_token").is_empty());
    assert!(rpc.calls().is_empty());
    storage
        .put_account_settings(&AccountSettings {
            account_id: 9001,
            revision: 1,
            repo_admin_delegation: true,
            changed_by: 42,
            changed_at: chrono::Utc::now(),
        })
        .await
        .unwrap();
    let page = html(request(&app, &cookie, "GET", SETTINGS, "").await).await;
    assert!(input(&page, "on").contains("checked"));
}
#[tokio::test]
async fn on_off_commands_use_authenticated_actor_and_same_operation_replays_identically() {
    let rpc = SettingsRpc::start(vec![Reply::Ok, Reply::Ok]).await;
    let (app, cookie, _) = fixture(rpc.client.clone(), OCTOCAT, false).await;
    let csrf = common::csrf_token(&app, &cookie).await;
    let on = RequestId::new().to_string();
    let form = format!("csrf_token={csrf}&operation_id={on}&on=true");
    for _ in 0..2 {
        let response = request(&app, &cookie, "POST", DELEGATION, &form).await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert_eq!(response.headers()["location"], SETTINGS);
    }
    let off = RequestId::new().to_string();
    let response = request(
        &app,
        &cookie,
        "POST",
        DELEGATION,
        &format!("csrf_token={csrf}&operation_id={off}"),
    )
    .await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    let calls = rpc.calls();
    assert_eq!(calls.len(), 3);
    assert_eq!(calls[0], calls[1]);
    assert_eq!(
        calls[0],
        json!({"operation_id":on,"actor_user_id":42,"on":true})
    );
    assert_eq!(
        calls[2],
        json!({"operation_id":off,"actor_user_id":42,"on":false})
    );
    assert_eq!(rpc.applied(), vec![true, false]);
}
#[tokio::test]
async fn nonowner_is_concealed_before_settings_read_or_mutation() {
    let rpc = SettingsRpc::start(vec![]).await;
    let (app, cookie, _) =
        fixture(rpc.client.clone(), GithubUser::new("outsider", 99), false).await;
    let csrf = common::csrf_token(&app, &cookie).await;
    for (method, path, form) in [
        ("GET", SETTINGS, String::new()),
        (
            "POST",
            DELEGATION,
            format!(
                "csrf_token={csrf}&operation_id={}&on=true",
                RequestId::new()
            ),
        ),
    ] {
        let response = request(&app, &cookie, method, path, &form).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let body = html(response).await;
        assert!(!body.contains("Repository admin delegation"));
        assert!(!body.contains("9001"));
    }
    assert!(rpc.calls().is_empty());
}
#[tokio::test]
async fn missing_wrong_duplicate_csrf_and_spoofed_fields_never_dispatch() {
    let rpc = SettingsRpc::start(vec![]).await;
    let (app, cookie, _) = fixture(rpc.client.clone(), OCTOCAT, false).await;
    let csrf = common::csrf_token(&app, &cookie).await;
    let command = format!("operation_id={}&on=true", RequestId::new());
    for form in [
        command.clone(),
        format!("{command}&csrf_token=wrong"),
        format!("{command}&csrf_token={csrf}&csrf_token={csrf}"),
    ] {
        assert_eq!(
            request(&app, &cookie, "POST", DELEGATION, &form)
                .await
                .status(),
            StatusCode::FORBIDDEN
        );
    }
    for form in [
        format!("{command}&csrf_token={csrf}&actor_user_id=99"),
        format!("csrf_token={csrf}&operation_id=not-an-operation&on=true"),
    ] {
        assert_eq!(
            request(&app, &cookie, "POST", DELEGATION, &form)
                .await
                .status(),
            StatusCode::BAD_REQUEST
        );
    }
    assert!(rpc.calls().is_empty());
}
#[tokio::test]
async fn retained_owner_refusal_is_404_and_lost_ack_never_claims_success() {
    let rpc = SettingsRpc::start(vec![Reply::Refused, Reply::LostAck]).await;
    let (app, cookie, _) = fixture(rpc.client.clone(), OCTOCAT, false).await;
    let csrf = common::csrf_token(&app, &cookie).await;
    let response = request(
        &app,
        &cookie,
        "POST",
        DELEGATION,
        &format!(
            "csrf_token={csrf}&operation_id={}&on=true",
            RequestId::new()
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert!(!html(response).await.contains("delegation updated"));
    let operation = RequestId::new().to_string();
    let form = format!("csrf_token={csrf}&operation_id={operation}&on=true");
    let response = request(&app, &cookie, "POST", DELEGATION, &form).await;
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    let body = html(response).await;
    assert!(body.contains("could not confirm whether the request took effect"));
    assert!(body.contains("check before trying again"));
    assert!(body.contains("Back to settings"));
    assert!(!body.contains("delegation updated"));
    assert_eq!(rpc.applied(), vec![true]);
    assert_eq!(
        request(&app, &cookie, "POST", DELEGATION, &form)
            .await
            .status(),
        StatusCode::SEE_OTHER
    );
    assert_eq!(rpc.calls()[1], rpc.calls()[2]);
    assert_eq!(rpc.applied(), vec![true]);
}

fn personal_link(id: InvitationLinkId) -> InvitationLink {
    InvitationLink {
        batch_source: None,
        access_lifetime_seconds: None,
        email_domains: vec![],
        id,
        organization_installation_id: None,
        installation_id: 77,
        account_id: 42,
        created_by: 42,
        created_at: chrono::Utc::now(),
        expires_at: None,
        max_uses: None,
        invitation_lifetime_seconds: 604800,
        abuse_threshold_per_hour: None,
        budget_share_pct: 50,
        uses_count: 0,
        secret: ghinvite_core::invitation_link::LinkSecretState {
            secret_generation: 1,
            sha256: ghinvite_core::SecretHash::from_bytes([7; 32]),
        },
        approval_required: true,
        description: "private admin description".into(),
        internal_note: None,
        revoked_at: None,
        revoked_by: None,
        pause: None,
        targets: ghinvite_core::targets::Targets {
            repos: vec![ghinvite_core::targets::RepoTarget {
                repo_id: 10,
                repo_full_name: "octocat/api".into(),
                grant: ghinvite_core::targets::RepoRole::Write,
            }],
            ..Default::default()
        },
    }
}
#[tokio::test]
async fn retained_not_authorized_decision_recovery_is_concealed_and_never_successful() {
    for action in ["approve", "decline"] {
        let authority = FakeLinkAuthority::start().await;
        let link_id = InvitationLinkId::new();
        let request_id = RequestId::new();
        let operation = RequestId::new();
        let link = personal_link(link_id);
        authority.seed_link(&link);
        let pending = RequestSnapshot {
            batch: None,
            domain_evidence: None,
            request_id,
            link_id: Some(link_id),
            account_id: 42,
            requester_id: 99,
            justification: Some("PRIVATE_REQUEST_MARKER".into()),
            state: RequestState::Pending,
            admitted_at: chrono::Utc::now(),
            decision_deadline: Some(chrono::Utc::now() + chrono::Duration::days(7)),
            revision: 1,
            decision: None,
        };
        authority.seed_request(pending.clone());
        authority.fail("decide", 503);
        let storage = Arc::new(SqlxStorage::in_memory().await.unwrap());
        let mut account = sign_in::acme_installation();
        account.account_id = 42;
        account.account_login = "octocat".into();
        account.account_type = ghinvite_core::AccountType::User;
        storage.seed_console_installation(&account).await.unwrap();
        let app = common::build_app(
            crate::common::sign_in::app_state(
                storage,
                Arc::new(ConsoleTransport::scripted(sign_in::oauth_expectations(
                    OCTOCAT,
                ))),
                authority.client(),
                sign_in::console_config(),
            ),
            tower_sessions::MemoryStore::default(),
        );
        let cookie = sign_in::sign_in(&app).await;
        let csrf = common::csrf_token(&app, &cookie).await;
        let response = request(
            &app,
            &cookie,
            "POST",
            &format!("/console/accounts/octocat/requests/{request_id}/{action}"),
            &format!("csrf_token={csrf}&operation_id={operation}&link_id={link_id}"),
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        let original = authority.received::<DecideRequest>("decide")[0].clone();
        authority.seed_decision(
            original.clone(),
            DecisionReceipt {
                outcome: DecisionOutcome::NotAuthorized,
                request: pending.clone(),
            },
        );
        authority.recover("decide");
        let path = format!("/console/accounts/octocat/attempts/decision-{request_id}-{operation}");
        let before = authority.received::<DecideRequest>("decide").len();
        let response = request(&app, &cookie, "GET", &path, "").await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let body = html(response).await;
        for secret in [
            "PRIVATE_REQUEST_MARKER",
            "private admin description",
            "Request approved",
            "Request declined",
            "Retry original attempt",
            "Outcome unknown",
        ] {
            assert!(
                !body.contains(secret),
                "generic refusal cannot contain {secret}"
            );
        }
        assert_eq!(authority.received::<DecideRequest>("decide").len(), before);
        assert_eq!(
            authority
                .received::<DecideRequest>("decision_status")
                .last(),
            Some(&original)
        );
        let response = request(&app, &cookie, "POST", &path, &format!("csrf_token={csrf}")).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let body = html(response).await;
        assert!(!body.contains("Request approved"));
        assert!(!body.contains("Request declined"));
        assert_eq!(
            authority.received::<DecideRequest>("decide").last(),
            Some(&original)
        );
        assert_eq!(authority.request(request_id), Some(pending));
        assert!(authority.applied().is_empty());
    }
}

#[tokio::test]
async fn delegated_repository_admin_can_see_own_links_but_cannot_manage_delegation() {
    use ghinvite_core::storage::{RecordStorage, projection::fixture::Seed};
    let rpc = SettingsRpc::start(vec![]).await;
    let (app, cookie, storage) = fixture_with_reads(
        rpc.client.clone(),
        GithubUser::new("outsider", 99),
        false,
        vec![
            repositories("acme", 9001),
            sign_in::repo_identity(10, "acme/api", 9001),
            sign_in::repo_admin("acme/api", 99, "outsider"),
        ],
    )
    .await;
    storage
        .put_account_settings(&AccountSettings {
            account_id: 9001,
            revision: 1,
            repo_admin_delegation: true,
            changed_by: 42,
            changed_at: chrono::Utc::now(),
        })
        .await
        .unwrap();
    storage
        .upsert_user(&ghinvite_core::User {
            user_id: 42,
            login: "octocat".into(),
            avatar_url: None,
            github_created_at: None,
            last_seen_at: chrono::Utc::now(),
        })
        .await
        .unwrap();
    let mut link = personal_link(InvitationLinkId::new());
    link.account_id = 9001;
    link.targets.repos[0].repo_full_name = "acme/api".into();
    storage.seed_link(&link).await.unwrap();
    assert_eq!(
        request(&app, &cookie, "GET", "/console/accounts/acme/links", "")
            .await
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        request(&app, &cookie, "GET", SETTINGS, "").await.status(),
        StatusCode::NOT_FOUND
    );
    let csrf = common::csrf_token(&app, &cookie).await;
    assert_eq!(
        request(
            &app,
            &cookie,
            "POST",
            DELEGATION,
            &format!(
                "csrf_token={csrf}&operation_id={}&on=false",
                RequestId::new()
            )
        )
        .await
        .status(),
        StatusCode::NOT_FOUND
    );
    assert!(rpc.calls().is_empty());
}

#[tokio::test]
async fn active_org_owner_manages_settings_without_repo_installation() {
    let rpc = SettingsRpc::start(vec![Reply::Ok]).await;
    let storage = Arc::new(SqlxStorage::in_memory().await.unwrap());
    let mut installation = ghinvite_core::account::Installation::repo(sign_in::acme_installation());
    installation.app = ghinvite_core::account::App::Org;
    installation.account.installation_id = 2;
    installation.permissions = [(
        "members".into(),
        ghinvite_core::account::InstallationPermission::Write,
    )]
    .into();
    storage.put_app_installation(&installation).await.unwrap();
    let user = GithubUser::new("org-owner", 99);
    let mut responses = sign_in::oauth_expectations(user);
    responses.push(Expectation::ok_json(Method::Get,"https://api.github.com/orgs/acme/memberships/org-owner",json!({"role":"admin","state":"active","user":{"id":99,"login":"org-owner"},"organization":{"id":9001,"login":"acme"}})));
    let state = crate::common::sign_in::app_state(
        storage,
        Arc::new(ConsoleTransport::scripted(responses)),
        rpc.client.clone(),
        sign_in::console_config(),
    );
    let app = common::build_app(state, tower_sessions::MemoryStore::default());
    let cookie = sign_in::sign_in(&app).await;
    let page = request(&app, &cookie, "GET", SETTINGS, "").await;
    assert_eq!(page.status(), StatusCode::OK);
    let page = html(page).await;
    assert!(page.contains(DELEGATION));
    let form = format!(
        "csrf_token={}&operation_id={}&on=true",
        value(&page, "csrf_token"),
        value(&page, "operation_id")
    );
    assert_eq!(
        request(&app, &cookie, "POST", DELEGATION, &form)
            .await
            .status(),
        StatusCode::SEE_OTHER
    );
    assert_eq!(rpc.calls()[0]["actor_user_id"], 99);
    assert_eq!(rpc.applied(), vec![true]);
}

#[tokio::test]
async fn installed_org_pending_owner_and_member_cannot_fall_back_to_installer() {
    for (role, membership) in [("admin", "pending"), ("member", "active")] {
        let rpc = SettingsRpc::start(vec![]).await;
        let (app,cookie,storage) = fixture_with_reads(rpc.client.clone(),OCTOCAT,false,vec![Expectation::ok_json(Method::Get,"https://api.github.com/orgs/acme/memberships/octocat",json!({"role":role,"state":membership,"user":{"id":42,"login":"octocat"},"organization":{"id":9001,"login":"acme"}}))]).await;
        let mut org = ghinvite_core::account::Installation::repo(sign_in::acme_installation());
        org.app = ghinvite_core::account::App::Org;
        org.account.installation_id = 2;
        org.permissions = [(
            "members".into(),
            ghinvite_core::account::InstallationPermission::Write,
        )]
        .into();
        storage.put_app_installation(&org).await.unwrap();
        let csrf = common::csrf_token(&app, &cookie).await;
        assert_eq!(
            request(&app, &cookie, "GET", SETTINGS, "").await.status(),
            StatusCode::NOT_FOUND
        );
        let form = format!(
            "csrf_token={csrf}&operation_id={}&on=true",
            RequestId::new()
        );
        assert_eq!(
            request(&app, &cookie, "POST", DELEGATION, &form)
                .await
                .status(),
            StatusCode::NOT_FOUND
        );
        assert!(rpc.calls().is_empty());
    }
}

#[tokio::test]
async fn conflicting_app_numeric_accounts_are_unavailable_without_mutation() {
    let rpc = SettingsRpc::start(vec![]).await;
    let (app, cookie, storage) = fixture(rpc.client.clone(), OCTOCAT, false).await;
    let mut org = ghinvite_core::account::Installation::repo(sign_in::acme_installation());
    org.app = ghinvite_core::account::App::Org;
    org.account.installation_id = 2;
    org.account.account_id = 9002;
    storage.put_app_installation(&org).await.unwrap();
    let csrf = common::csrf_token(&app, &cookie).await;
    assert_eq!(
        request(&app, &cookie, "GET", SETTINGS, "").await.status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert_eq!(
        request(
            &app,
            &cookie,
            "POST",
            DELEGATION,
            &format!(
                "csrf_token={csrf}&operation_id={}&on=true",
                RequestId::new()
            )
        )
        .await
        .status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert!(rpc.calls().is_empty());
}
