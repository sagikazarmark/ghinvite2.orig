//! Browser ingress contracts against a strict HTTP response fixture. Durable
//! coordinator, delivery and D1 behavior have separate real runtime proofs.
mod common;
use axum::{
    body::Body,
    extract::{Path, State},
    http::{Request, StatusCode},
    response::{IntoResponse, Response},
};
use chrono::Utc;
use common::sign_in::{self, ConsoleInstallationFixture, ConsoleTransport, OCTOCAT};
use ghinvite_core::{
    Account, AccountType, SelectedRepos,
    bulk::{BulkBatchId, Review},
    bulk_commands::{
        BatchProgress, CreateBatch, EditBatch, ProgressCursor, ProgressPageQuery, SubmitBatch,
    },
};
use ghinvite_github::{Method, mocks::Expectation};
use ghinvite_web::{AppState, RestateClient};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use tower::ServiceExt;

struct ObservedTransport {
    inner: ConsoleTransport,
    calls: Mutex<Vec<String>>,
}
impl ghinvite_github::HttpTransport for ObservedTransport {
    async fn send(
        &self,
        request: ghinvite_github::Request,
    ) -> std::result::Result<ghinvite_github::Response, ghinvite_github::TransportError> {
        self.calls.lock().unwrap().push(request.url.clone());
        let path = request.url.clone();
        let result = self.inner.send(request).await;
        if let Err(error) = &result {
            eprintln!("fixture resource {path}: {error}");
        }
        result
    }
}

#[derive(Default)]
struct Ingress {
    review: Option<Review>,
    edit_receipts: std::collections::BTreeMap<String, (EditBatch, Review)>,
    calls: Vec<(String, Value)>,
    lose_create: bool,
    lose_submit: bool,
    fail_review_after_ack: bool,
    fail_next_review: bool,
    account_id: Option<u64>,
    progress_next: Option<ProgressCursor>,
}
async fn ingress(
    State(state): State<Arc<Mutex<Ingress>>>,
    Path((service, key, method)): Path<(String, String, String)>,
    bytes: axum::body::Bytes,
) -> Response {
    let body: Value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    let mut state = state.lock().unwrap();
    state.calls.push((method.clone(), body.clone()));
    if service == "InviteBudget" {
        return axum::Json(json!({"app":if key.starts_with("org:"){ "org" }else{"repo"},"account_id":state.account_id.unwrap_or(42),"revision":1,"observed_at":Utc::now(),"confirmed_established":false,"buckets":[],"content_used_minute":0,"content_used_hour":0,"next_content_write_at":null})).into_response();
    }
    if service == "InvitationLink" {
        assert_eq!(method, "create");
        let command: ghinvite_core::invitation_link::CreateLink =
            serde_json::from_value(body).unwrap();
        return axum::Json(Ok::<_, ghinvite_core::invitation_link::CreateLinkError>(
            ghinvite_core::invitation_link::LinkCreated {
                link_id: command.link_id,
                secret_generation: 1,
            },
        ))
        .into_response();
    }
    assert_eq!(service, "BulkInviteBatch", "unexpected ingress service");
    match method.as_str() {
        "create"=>{
            let command:CreateBatch=serde_json::from_value(body).unwrap();
            if state.review.is_none(){state.review=Some(command.review.clone());}
            if std::mem::take(&mut state.lose_create){return StatusCode::SERVICE_UNAVAILABLE.into_response();}
            state.fail_next_review = std::mem::take(&mut state.fail_review_after_ack);
            axum::Json(command.review).into_response()
        },
        "review"=>{
            if std::mem::take(&mut state.fail_next_review){return StatusCode::SERVICE_UNAVAILABLE.into_response();}
            match &state.review {Some(review)=>axum::Json(review.clone()).into_response(),None=>StatusCode::NOT_FOUND.into_response()}
        },
        "remove_rows"=>{
            let command:EditBatch=serde_json::from_value(body).unwrap();
            let operation = String::from(command.operation_id.clone());
            state.fail_next_review = std::mem::take(&mut state.fail_review_after_ack);
            if let Some((original, receipt)) = state.edit_receipts.get(&operation) {
                return if original == &command {axum::Json(receipt.clone()).into_response()} else {StatusCode::CONFLICT.into_response()};
            }
            let review=state.review.as_mut().unwrap();
            match review.edit(command.expected_revision,command.remove.clone(),command.personal_link_rows.clone()){
                Ok(_)=>{
                    let receipt = review.clone();
                    state.edit_receipts.insert(operation, (command, receipt.clone()));
                    axum::Json(receipt).into_response()
                },
                Err(_)=>StatusCode::CONFLICT.into_response()
            }
        },
        "submit"=>{
            let command:SubmitBatch=serde_json::from_value(body).unwrap();
            let review=state.review.as_mut().unwrap();
            if review.submitted.is_none(){review.submit(command.expected_revision,command.submission).unwrap();}
            if std::mem::take(&mut state.lose_submit){return StatusCode::SERVICE_UNAVAILABLE.into_response();}
            axum::Json(state.review.clone().unwrap()).into_response()
        },
        "progress"|"progress_page"=>{
            if method == "progress_page" {
                let input: ProgressPageQuery = serde_json::from_value(body).unwrap();
                if input.cursor.is_some_and(|cursor| cursor.review_revision != state.review.as_ref().unwrap().revision) {
                    return StatusCode::CONFLICT.into_response();
                }
            }
            axum::Json(BatchProgress{next_cursor:state.progress_next,review:state.review.clone().unwrap(),handoff_pending:true,requests:vec![],emails:vec![],cancellation_pending:false,cancellation:None}).into_response()
        },
        "cancel_remaining"=>axum::Json(json!({"batch_id":state.review.as_ref().unwrap().manifest.batch_id,"stopped":true,"aborted":1,"already_written":0,"already_completed":0,"uncertain":0})).into_response(),
        _=>panic!("unexpected handler {method}"),
    }
}

struct EmailFixture {
    app: axum::Router,
    cookie: String,
    ingress: Arc<Mutex<Ingress>>,
    sql: sqlx::SqlitePool,
    storage: Arc<ghinvite_storage_sqlx::SqlxStorage>,
    state: AppState,
    ring: Arc<kekring::Keyring>,
    database: std::path::PathBuf,
}
impl EmailFixture {
    async fn finish(self) {
        drop(self.app);
        self.sql.close().await;
        std::fs::remove_file(self.database).unwrap();
    }
}
async fn email_fixture() -> EmailFixture {
    use ghinvite_core::{
        account::{App, Installation},
        storage::AppInstallationStorage,
    };
    let database = std::env::temp_dir().join(format!(
        "bulk-http-{}.sqlite",
        ghinvite_core::RequestId::new()
    ));
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
    storage
        .seed_console_installation(&sign_in::acme_installation())
        .await
        .unwrap();
    let mut org = Installation::repo(sign_in::acme_installation());
    org.app = App::Org;
    org.account.installation_id = 88;
    storage.put_app_installation(&org).await.unwrap();
    let ingress_state = Arc::new(Mutex::new(Ingress {
        account_id: Some(9001),
        ..Default::default()
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let router = axum::Router::new()
        .route("/{service}/{key}/{method}", axum::routing::post(ingress))
        .with_state(ingress_state.clone());
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let mut expected = sign_in::oauth_expectations(OCTOCAT);
    expected.extend([
        Expectation::ok_json(Method::Get,"https://api.github.com/orgs/acme/memberships/octocat",json!({"user":{"id":42,"login":"octocat"},"organization":{"id":9001,"login":"acme"},"role":"admin","state":"active"})),
        Expectation::ok_json(Method::Get,"https://api.github.com/orgs/acme",json!({"id":9001,"login":"acme","created_at":"2020-01-01T00:00:00Z"})),
        Expectation::ok_json(Method::Get,"https://api.github.com/orgs/acme/teams?per_page=100",json!([])),
        Expectation::ok_json(Method::Get,"https://api.github.com/installation/repositories?per_page=100",json!({"total_count":1,"repositories":[{"id":10,"full_name":"acme/api","owner":{"id":9001,"login":"acme"}}]})),
        sign_in::repo_identity(10,"acme/api",9001),
    ]);
    let ring = Arc::new(
        kekring::Keyring::builder("p1-email")
            .generation(1, [6; 32])
            .unwrap()
            .current(1)
            .unwrap()
            .build()
            .unwrap(),
    );
    let vault = Arc::new(
        ghinvite_email_vault::EmailVault::new(storage.clone(), ring)
            .with_review_storage(storage.clone()),
    );
    let mut config = sign_in::console_config();
    config.bulk_enabled = true;
    config.organization_delivery_enabled = true;
    let ring = config.keyring.clone();
    let state = crate::common::sign_in::app_state(
        storage.clone(),
        Arc::new(ConsoleTransport::scripted(expected)),
        Arc::new(RestateClient::new(format!("http://{address}")).unwrap()),
        config,
    )
    .with_bulk_email_vault(vault);
    let app = common::build_app(state.clone(), tower_sessions::MemoryStore::default());
    let cookie = sign_in::sign_in(&app).await;
    EmailFixture {
        app,
        cookie,
        ingress: ingress_state,
        sql,
        storage,
        state,
        ring,
        database,
    }
}

#[tokio::test]
async fn email_personal_capability_is_shown_once_across_concurrent_recovery_posts() {
    let fixture = email_fixture().await;
    let (app, cookie, ingress) = (&fixture.app, &fixture.cookie, &fixture.ingress);
    let csrf = common::csrf_token(app, cookie).await;
    let batch = BulkBatchId::new();
    let base = format!("/console/accounts/acme/bulk/{batch}");
    let mailbox = "private-bulk-marker@example.com";
    let response = request(
        app,
        cookie,
        "POST",
        &format!("{base}/resolve"),
        body(&[
            ("csrf_token", &csrf),
            ("entries", mailbox),
            ("repo_ids", "10"),
            ("repo_grants", "10:push"),
            ("include_organization", "true"),
        ]),
    )
    .await;
    let status = response.status();
    let reviewed = html(response).await;
    assert_eq!(status, StatusCode::OK, "{reviewed}");
    assert!(reviewed.contains(mailbox));
    assert!(reviewed.contains("Needs a personal link"));
    let edit = ghinvite_core::RequestId::new().to_string();
    let response = request(
        app,
        cookie,
        "POST",
        &format!("{base}/rows/remove"),
        body(&[
            ("csrf_token", &csrf),
            ("operation_id", &edit),
            ("expected_revision", "1"),
            ("personal_link_rows", "1"),
        ]),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    ingress.lock().unwrap().lose_submit = true;
    let operation = ghinvite_core::RequestId::new().to_string();
    let response = request(
        app,
        cookie,
        "POST",
        &format!("{base}/submit"),
        body(&[
            ("csrf_token", &csrf),
            ("operation_id", &operation),
            ("expected_revision", "2"),
        ]),
    )
    .await;
    let status = response.status();
    let pending = html(response).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{pending}");
    assert!(!pending.contains("/i#"));
    let path = format!("{base}/retry");
    let form = body(&[("csrf_token", &csrf), ("record_id", "submit")]);
    let (first, second) = tokio::join!(
        request(app, cookie, "POST", &path, form.clone()),
        request(app, cookie, "POST", &path, form.clone())
    );
    let first_status = first.status();
    let second_status = second.status();
    let first = html(first).await;
    let second = html(second).await;
    assert_eq!(first_status, StatusCode::OK, "{first}");
    assert_eq!(second_status, StatusCode::OK, "{second}");
    assert_eq!(
        usize::from(first.contains("Save these personal links"))
            + usize::from(second.contains("Save these personal links")),
        1
    );
    let shown = if first.contains("Save these personal links") {
        first
    } else {
        second
    };
    let secret = shown
        .split("href=\"/i#")
        .nth(1)
        .unwrap()
        .split('"')
        .next()
        .unwrap();
    assert_eq!(secret.len(), 43);
    expire_personal_plan(&fixture, batch).await;
    let response = request(app, cookie, "POST", &path, form).await;
    assert_eq!(response.status(), StatusCode::OK);
    let again = html(response).await;
    assert!(!again.contains(secret));
    assert!(!again.contains("Save these personal links"));
    assert_eq!(again.matches("<body").count(), 1);
    assert_eq!(again.matches("</body>").count(), 1);
    assert!(again.find("Recover original submission").unwrap() < again.find("</body>").unwrap());
    let calls = serde_json::to_string(&ingress.lock().unwrap().calls).unwrap();
    assert!(!calls.contains(mailbox));
    assert!(!calls.contains(secret));
    fixture.finish().await;
}

#[tokio::test]
#[ignore = "long-running Playwright fixture"]
async fn bulk_browser_server() {
    let (app, cookie, ingress, _transport, _state, _) = fixture_with_access(true, true, true).await;
    let reset = ingress.clone();
    let server = axum::Router::new()
        .route("/health", axum::routing::get(|| async { StatusCode::OK }))
        .route(
            "/fixture-login",
            axum::routing::get(move || {
                let state = reset.clone();
                let cookie = cookie.clone();
                async move {
                    *state.lock().unwrap() = Ingress {
                        lose_submit: true,
                        ..Default::default()
                    };
                    (
                        StatusCode::SEE_OTHER,
                        [
                            (
                                axum::http::header::SET_COOKIE,
                                format!("{cookie}; Path=/; HttpOnly; SameSite=Lax"),
                            ),
                            (
                                axum::http::header::LOCATION,
                                "/console/accounts/octocat/bulk/new".to_owned(),
                            ),
                        ],
                    )
                }
            }),
        )
        .route(
            "/fixture-effects",
            axum::routing::get(move || {
                let state = ingress.clone();
                async move { axum::Json(json!({"calls":state.lock().unwrap().calls})) }
            }),
        )
        .fallback(move |request: Request<Body>| {
            let app = app.clone();
            async move { app.oneshot(request).await.unwrap() }
        });
    axum::serve(
        tokio::net::TcpListener::bind("127.0.0.1:4198")
            .await
            .unwrap(),
        server,
    )
    .await
    .unwrap();
}
async fn fixture(
    enabled: bool,
    lookup: bool,
) -> (
    axum::Router,
    String,
    Arc<Mutex<Ingress>>,
    Arc<ObservedTransport>,
    AppState,
    Arc<ghinvite_storage_sqlx::SqlxStorage>,
) {
    fixture_with_access(enabled, lookup, false).await
}
async fn fixture_with_access(
    enabled: bool,
    lookup: bool,
    access: bool,
) -> (
    axum::Router,
    String,
    Arc<Mutex<Ingress>>,
    Arc<ObservedTransport>,
    AppState,
    Arc<ghinvite_storage_sqlx::SqlxStorage>,
) {
    let storage = Arc::new(
        ghinvite_storage_sqlx::SqlxStorage::in_memory()
            .await
            .unwrap(),
    );
    storage
        .seed_console_installation(&Account {
            installation_id: 77,
            account_id: 42,
            account_login: "octocat".into(),
            account_type: AccountType::User,
            installed_at: Utc::now(),
            uninstalled_at: None,
            selected_repos: SelectedRepos::All,
        })
        .await
        .unwrap();
    let ingress_state = Arc::new(Mutex::new(Ingress::default()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let router = axum::Router::new()
        .route("/{service}/{key}/{method}", axum::routing::post(ingress))
        .with_state(ingress_state.clone());
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let mut expected = sign_in::oauth_expectations(OCTOCAT);
    if lookup {
        expected.extend([
            Expectation::ok_json(Method::Get,"https://api.github.com/installation/repositories?per_page=100",json!({"total_count":1,"repositories":[{"id":10,"full_name":"octocat/api","owner":{"id":42,"login":"octocat"},"private":true}]})),
            Expectation::ok_json(Method::Get,"https://api.github.com/repos/octocat/api",json!({"id":10,"full_name":"octocat/api","owner":{"id":42,"login":"octocat"},"private":true})),
            Expectation::ok_json(Method::Get,"https://api.github.com/users/alice",json!({"id":8,"login":"alice"})),
            Expectation::ok_json(Method::Get,"https://api.github.com/repos/octocat/api/collaborators/alice/permission",json!({"user":{"id":8,"login":"alice"},"role_name":"none"})),
        ]);
        let mut missing = Expectation::ok_json(
            Method::Get,
            "https://api.github.com/users/missing",
            json!({}),
        );
        missing.response.status = 404;
        expected.push(missing);
    }
    let transport = Arc::new(ObservedTransport {
        inner: ConsoleTransport::scripted(expected),
        calls: Mutex::new(vec![]),
    });
    let mut config = sign_in::console_config();
    config.bulk_enabled = enabled;
    config.access_removal_enabled = access;
    let state = crate::common::sign_in::app_state(
        storage.clone(),
        transport.clone(),
        Arc::new(RestateClient::new(format!("http://{address}")).unwrap()),
        config,
    );
    let app = common::build_app(state.clone(), tower_sessions::MemoryStore::default());
    let cookie = sign_in::sign_in(&app).await;
    (app, cookie, ingress_state, transport, state, storage)
}
async fn request(
    app: &axum::Router,
    cookie: &str,
    method: &str,
    path: &str,
    body: String,
) -> Response {
    app.clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header("cookie", cookie)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from(body))
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
fn body(pairs: &[(&str, &str)]) -> String {
    url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(pairs.iter().copied())
        .finish()
}

#[tokio::test]
async fn bulk_gate_csrf_and_row_cap_precede_lookup_or_ingress() {
    for enabled in [false, true] {
        let (app, cookie, ingress, _, _, _) = fixture(enabled, false).await;
        let csrf = common::csrf_token(&app, &cookie).await;
        let batch = BulkBatchId::new();
        let path = format!("/console/accounts/octocat/bulk/{batch}/resolve");
        let denied = request(&app, &cookie, "POST", &path, "entries=alice".into()).await;
        assert_eq!(denied.status(), StatusCode::FORBIDDEN);
        let entries = vec!["alice"; 201].join("\n");
        let response = request(
            &app,
            &cookie,
            "POST",
            &path,
            body(&[("csrf_token", &csrf), ("entries", &entries)]),
        )
        .await;
        assert_eq!(
            response.status(),
            if enabled {
                StatusCode::BAD_REQUEST
            } else {
                StatusCode::NOT_FOUND
            }
        );
        assert_eq!(response.headers()["cache-control"], "private, no-store");
        assert!(ingress.lock().unwrap().calls.is_empty());
    }
}

#[tokio::test]
async fn retained_review_removal_and_lost_submit_ack_recover_identical_allocation() {
    let (app, cookie, ingress, transport, _, _) = fixture(true, true).await;
    let csrf = common::csrf_token(&app, &cookie).await;
    let batch = BulkBatchId::new();
    let base = format!("/console/accounts/octocat/bulk/{batch}");
    ingress.lock().unwrap().lose_create = true;
    let entry = body(&[
        ("csrf_token", &csrf),
        ("entries", "@alice\nalice\nmissing"),
        ("repo_ids", "10"),
        ("repo_grants", "10:push"),
    ]);
    let response = request(
        &app,
        &cookie,
        "POST",
        &format!("{base}/resolve"),
        entry.clone(),
    )
    .await;
    let status = response.status();
    let initial = html(response).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{initial}");
    assert!(
        initial.contains("Retry original operation"),
        "{initial}; ingress={:?}; github={:?}",
        ingress.lock().unwrap().calls,
        transport.calls.lock().unwrap()
    );
    let response = request(
        &app,
        &cookie,
        "POST",
        &format!("{base}/retry"),
        body(&[("csrf_token", &csrf), ("record_id", "create")]),
    )
    .await;
    let status = response.status();
    let review = html(response).await;
    assert_eq!(status, StatusCode::OK, "{review}");
    assert!(review.contains("Duplicate of row 1"));
    assert!(review.contains("Not found"));
    assert!(!review.contains("link-form-island"));
    let creates: Vec<_> = ingress
        .lock()
        .unwrap()
        .calls
        .iter()
        .filter(|(name, _)| name == "create")
        .map(|(_, value)| value.clone())
        .collect();
    assert_eq!(creates.len(), 2);
    assert_eq!(creates[0], creates[1]);
    assert!(
        !ingress
            .lock()
            .unwrap()
            .calls
            .iter()
            .any(|(name, _)| name == "submit")
    );
    let response = request(
        &app,
        &cookie,
        "POST",
        &format!("{base}/resolve"),
        entry.replace("missing", "changed"),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let edit = ghinvite_core::RequestId::new().to_string();
    let response = request(
        &app,
        &cookie,
        "POST",
        &format!("{base}/rows/remove"),
        body(&[
            ("csrf_token", &csrf),
            ("operation_id", &edit),
            ("expected_revision", "1"),
            ("remove_rows", "3"),
        ]),
    )
    .await;
    let status = response.status();
    let updated = html(response).await;
    assert_eq!(status, StatusCode::OK, "{updated}");
    assert!(updated.contains("Review revision 2"));
    ingress.lock().unwrap().lose_submit = true;
    let operation = ghinvite_core::RequestId::new().to_string();
    let response = request(
        &app,
        &cookie,
        "POST",
        &format!("{base}/submit"),
        body(&[
            ("csrf_token", &csrf),
            ("operation_id", &operation),
            ("expected_revision", "2"),
        ]),
    )
    .await;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let response = request(
        &app,
        &cookie,
        "POST",
        &format!("{base}/retry"),
        body(&[("csrf_token", &csrf), ("record_id", "submit")]),
    )
    .await;
    let status = response.status();
    let submitted = html(response).await;
    assert_eq!(status, StatusCode::OK, "{submitted}");
    assert!(submitted.contains("The batch was submitted"));
    assert!(submitted.contains("Cancel unsent work"));
    let submissions: Vec<SubmitBatch> = ingress
        .lock()
        .unwrap()
        .calls
        .iter()
        .filter(|(name, _)| name == "submit")
        .map(|(_, value)| serde_json::from_value(value.clone()).unwrap())
        .collect();
    assert_eq!(submissions.len(), 2);
    assert_eq!(submissions[0], submissions[1]);
    assert_eq!(submissions[0].submission.requests.len(), 1);
    assert_eq!(submissions[0].submission.submitted_by, 42);
    assert!(submissions[0].submission.emails.is_empty());
    let cancel = ghinvite_core::RequestId::new().to_string();
    let response = request(
        &app,
        &cookie,
        "POST",
        &format!("{base}/cancel"),
        body(&[("csrf_token", &csrf), ("operation_id", &cancel)]),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
}

/// Test-side mutation of the real encrypted storage format. The session and
/// outer continuation stay valid; only a nested envelope's lifetime elapses.
async fn continuation(f: &EmailFixture, id: &str) -> (String, String, Value) {
    use kekring::ContextEnvelopeSync;
    use sqlx::Row;
    let row = sqlx::query(
        "SELECT scope,binding,payload,expires_at FROM attempt_continuations WHERE id=?",
    )
    .bind(id)
    .fetch_one(&f.sql)
    .await
    .unwrap();
    assert!(row.get::<i64, _>("expires_at") > Utc::now().timestamp());
    let scope: String = row.get("scope");
    let binding: String = row.get("binding");
    let context = kekring::EncryptionContext::try_from(&kekring::purposes::AttemptContinuation {
        scope: &scope,
        binding: &binding,
    })
    .unwrap();
    let encrypted = anykms_core::Ciphertext::from_base64(&row.get::<String, _>("payload")).unwrap();
    let plain = f
        .ring
        .context_envelope(anykms_recipes::envelope::Padding::NONE)
        .unwrap()
        .decrypt_sync(&encrypted, &context)
        .unwrap();
    (scope, binding, serde_json::from_slice(&plain).unwrap())
}
fn expire_nested(f: &EmailFixture, ciphertext: &str, context_scope: &str) -> String {
    use kekring::ContextEnvelopeSync;
    let session = f.cookie.split_once('=').unwrap().1;
    let context = kekring::EncryptionContext::try_from(&kekring::purposes::AttemptContinuation {
        scope: context_scope,
        binding: session,
    })
    .unwrap();
    let envelope = f
        .ring
        .context_envelope(anykms_recipes::envelope::Padding::NONE)
        .unwrap();
    let encrypted = anykms_core::Ciphertext::from_base64(ciphertext).unwrap();
    let plain = envelope.decrypt_sync(&encrypted, &context).unwrap();
    let mut value: Value = serde_json::from_slice(&plain).unwrap();
    let now = Utc::now().timestamp();
    value["created_at"] = json!(now - 7200);
    value["expires_at"] = json!(now - 3600);
    envelope
        .encrypt_sync(&serde_json::to_vec(&value).unwrap(), &context)
        .unwrap()
        .to_base64()
}
async fn store_continuation(f: &EmailFixture, id: &str, scope: &str, binding: &str, value: &Value) {
    use kekring::ContextEnvelopeSync;
    let context = kekring::EncryptionContext::try_from(&kekring::purposes::AttemptContinuation {
        scope,
        binding,
    })
    .unwrap();
    let encrypted = f
        .ring
        .context_envelope(anykms_recipes::envelope::Padding::NONE)
        .unwrap()
        .encrypt_sync(&serde_json::to_vec(value).unwrap(), &context)
        .unwrap();
    let result = sqlx::query("UPDATE attempt_continuations SET payload=? WHERE scope=? AND id=?")
        .bind(encrypted.to_base64())
        .bind(scope)
        .bind(id)
        .execute(&f.sql)
        .await
        .unwrap();
    assert_eq!(result.rows_affected(), 1);
}
async fn expire_email_plan(f: &EmailFixture, batch: BulkBatchId) {
    let (scope, binding, mut record) = continuation(f, "draft").await;
    let context = format!("bulk-email-plan/v1/{batch}/9001/88");
    record["Draft"]["email_plan"] = json!(expire_nested(
        f,
        record["Draft"]["email_plan"].as_str().unwrap(),
        &context
    ));
    store_continuation(f, "draft", &scope, &binding, &record).await;
}
async fn expire_personal_plan(f: &EmailFixture, batch: BulkBatchId) {
    let (scope, binding, mut record) = continuation(f, "submit").await;
    let links = record["Submit"]["personal"].as_array_mut().unwrap();
    assert_eq!(links.len(), 1);
    let link = &mut links[0];
    let context = format!(
        "bulk-personal-link/v1/{batch}/9001/88/{}/{}",
        link["plan"]["source"]["row"].as_u64().unwrap(),
        link["plan"]["link_id"].as_str().unwrap()
    );
    link["ciphertext"] = json!(expire_nested(
        f,
        link["ciphertext"].as_str().unwrap(),
        &context
    ));
    store_continuation(f, "submit", &scope, &binding, &record).await;
}
async fn resolve_email(
    f: &EmailFixture,
    entries: &str,
    personal: bool,
) -> (BulkBatchId, String, String) {
    let csrf = common::csrf_token(&f.app, &f.cookie).await;
    let batch = BulkBatchId::new();
    let base = format!("/console/accounts/acme/bulk/{batch}");
    let mut pairs = vec![
        ("csrf_token", csrf.as_str()),
        ("entries", entries),
        ("include_organization", "true"),
    ];
    if personal {
        pairs.extend([("repo_ids", "10"), ("repo_grants", "10:push")]);
    }
    let response = request(
        &f.app,
        &f.cookie,
        "POST",
        &format!("{base}/resolve"),
        body(&pairs),
    )
    .await;
    let status = response.status();
    let text = html(response).await;
    assert_eq!(status, StatusCode::OK, "{text}");
    (batch, base, csrf)
}
#[tokio::test]
async fn accepted_submission_recovers_after_nested_email_expiry_and_draft_removal() {
    for accepted in [false, true] {
        let f = email_fixture().await;
        let (batch, base, csrf) = resolve_email(&f, "recovery-private@example.com", false).await;
        let form = body(&[
            ("csrf_token", &csrf),
            ("operation_id", &ghinvite_core::RequestId::new().to_string()),
            ("expected_revision", "1"),
        ]);
        if accepted {
            f.ingress.lock().unwrap().lose_submit = true;
            assert_eq!(
                request(
                    &f.app,
                    &f.cookie,
                    "POST",
                    &format!("{base}/submit"),
                    form.clone()
                )
                .await
                .status(),
                StatusCode::SERVICE_UNAVAILABLE
            );
        }
        if accepted {
            expire_email_plan(&f, batch).await;
        } else {
            use ghinvite_core::storage::email_review::EmailReviewStorage;
            let expiry: i64 = sqlx::query_scalar("SELECT max(expires_at) FROM email_review")
                .fetch_one(&f.sql)
                .await
                .unwrap();
            assert_eq!(f.storage.prune_review_emails(expiry).await.unwrap(), 1);
        }
        if !accepted {
            // Negative control: normal bounded cleanup at escrow expiry makes
            // staging unavailable. The creator's outer/session plan stays valid.
            let response =
                request(&f.app, &f.cookie, "POST", &format!("{base}/submit"), form).await;
            assert_eq!(response.status(), StatusCode::CONFLICT);
            let text = html(response).await;
            assert!(text.contains("The address review has expired or could not be restored"));
            assert!(text.contains("start a new batch"));
            assert!(!text.contains("recovery-private@example.com"));
            assert!(
                !f.ingress
                    .lock()
                    .unwrap()
                    .calls
                    .iter()
                    .any(|(name, _)| name == "submit")
            );
            let count: i64 = sqlx::query_scalar("SELECT count(*) FROM email_vault")
                .fetch_one(&f.sql)
                .await
                .unwrap();
            assert_eq!(count, 0);
        } else {
            let original: Vec<_> = f
                .ingress
                .lock()
                .unwrap()
                .calls
                .iter()
                .filter(|(name, _)| name == "submit")
                .map(|(_, v)| v.clone())
                .collect();
            assert_eq!(original.len(), 1);
            let before: Vec<(String, String)> =
                sqlx::query_as("SELECT email_ref,ciphertext FROM email_vault ORDER BY email_ref")
                    .fetch_all(&f.sql)
                    .await
                    .unwrap();
            for remove in [false, true] {
                if remove {
                    assert_eq!(
                        sqlx::query("DELETE FROM attempt_continuations WHERE id='draft'")
                            .execute(&f.sql)
                            .await
                            .unwrap()
                            .rows_affected(),
                        1
                    );
                }
                let response = request(
                    &f.app,
                    &f.cookie,
                    "POST",
                    &format!("{base}/retry"),
                    body(&[("csrf_token", &csrf), ("record_id", "submit")]),
                )
                .await;
                let status = response.status();
                let text = html(response).await;
                assert_eq!(status, StatusCode::OK, "{text}");
                assert!(text.contains("The batch was submitted"));
                assert!(!text.contains("recovery-private@example.com"));
            }
            let submissions: Vec<_> = f
                .ingress
                .lock()
                .unwrap()
                .calls
                .iter()
                .filter(|(name, _)| name == "submit")
                .map(|(_, v)| v.clone())
                .collect();
            assert_eq!(submissions, vec![original[0].clone(); 3]);
            let after: Vec<(String, String)> =
                sqlx::query_as("SELECT email_ref,ciphertext FROM email_vault ORDER BY email_ref")
                    .fetch_all(&f.sql)
                    .await
                    .unwrap();
            assert_eq!(
                before, after,
                "recovery must not restage or replace ciphertext"
            );
        }
        f.finish().await;
    }
}
#[tokio::test]
async fn removed_email_rows_are_never_staged_in_the_delivery_vault() {
    let f = email_fixture().await;
    let (batch, base, csrf) = resolve_email(
        &f,
        "selected-private@example.com\nremoved-private@example.com",
        true,
    )
    .await;
    let response = request(
        &f.app,
        &f.cookie,
        "POST",
        &format!("{base}/rows/remove"),
        body(&[
            ("csrf_token", &csrf),
            ("operation_id", &ghinvite_core::RequestId::new().to_string()),
            ("expected_revision", "1"),
            ("remove_rows", "2"),
            ("personal_link_rows", "1"),
        ]),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let response = request(
        &f.app,
        &f.cookie,
        "POST",
        &format!("{base}/submit"),
        body(&[
            ("csrf_token", &csrf),
            ("operation_id", &ghinvite_core::RequestId::new().to_string()),
            ("expected_revision", "2"),
        ]),
    )
    .await;
    let status = response.status();
    let text = html(response).await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let rows: Vec<(String, i64, String, String)> =
        sqlx::query_as("SELECT batch_id,row_number,email_ref,ciphertext FROM email_vault")
            .fetch_all(&f.sql)
            .await
            .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].0, batch.to_string());
    assert_eq!(rows[0].1, 1);
    assert!(!rows[0].3.contains("selected-private"));
    assert!(!rows[0].3.contains("removed-private"));
    let calls = f.ingress.lock().unwrap().calls.clone();
    let submitted: SubmitBatch = serde_json::from_value(
        calls
            .iter()
            .find(|(name, _)| name == "submit")
            .unwrap()
            .1
            .clone(),
    )
    .unwrap();
    assert_eq!(submitted.submission.emails.len(), 1);
    assert_eq!(submitted.submission.emails[0].email_ref.as_hex(), rows[0].2);
    let wire = serde_json::to_string(&calls).unwrap();
    assert!(!wire.contains("selected-private@example.com"));
    assert!(!wire.contains("removed-private@example.com"));
    f.finish().await;
}

#[tokio::test]
async fn progress_cursor_navigation_and_invalid_paths_preserve_the_retained_review() {
    let f = email_fixture().await;
    let (_, base, csrf) = resolve_email(&f, "cursor-private@example.com", false).await;
    let response = request(
        &f.app,
        &f.cookie,
        "POST",
        &format!("{base}/submit"),
        body(&[
            ("csrf_token", &csrf),
            ("operation_id", &ghinvite_core::RequestId::new().to_string()),
            ("expected_revision", "1"),
        ]),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let before = f.ingress.lock().unwrap().calls.len();
    for (path, expected) in [
        (
            "/console/accounts/acme/bulk/not-a-batch".to_owned(),
            StatusCode::NOT_FOUND,
        ),
        (format!("{base}?after=1:one:0"), StatusCode::BAD_REQUEST),
        (format!("{base}?after=2:1:0:extra"), StatusCode::BAD_REQUEST),
        (format!("{base}?after=1:65536:0"), StatusCode::BAD_REQUEST),
    ] {
        let response = request(&f.app, &f.cookie, "GET", &path, String::new()).await;
        assert_eq!(response.status(), expected);
        assert_eq!(response.headers()["cache-control"], "private, no-store");
        assert!(!html(response).await.contains("cursor-private@example.com"));
    }
    assert_eq!(
        f.ingress.lock().unwrap().calls.len(),
        before,
        "invalid syntax must not reach the coordinator"
    );
    let response = request(
        &f.app,
        &f.cookie,
        "GET",
        &format!("{base}?after=99:1:0"),
        String::new(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let cursor = ProgressCursor {
        review_revision: 2,
        row: 1,
        target_offset: 0,
    };
    f.ingress.lock().unwrap().progress_next = Some(cursor);
    let response = request(&f.app, &f.cookie, "GET", &base, String::new()).await;
    assert_eq!(response.status(), StatusCode::OK);
    let text = html(response).await;
    assert!(text.contains("Next progress page"));
    assert!(text.contains(&format!("{base}?after=2:1:0")));
    f.ingress.lock().unwrap().progress_next = None;
    let response = request(
        &f.app,
        &f.cookie,
        "GET",
        &format!("{base}?after=2:1:0"),
        String::new(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let text = html(response).await;
    assert!(!text.contains("Next progress page"));
    let calls = f.ingress.lock().unwrap().calls.clone();
    let (_, last) = calls
        .iter()
        .rev()
        .find(|(name, _)| name == "progress_page")
        .unwrap();
    let input: ProgressPageQuery = serde_json::from_value(last.clone()).unwrap();
    assert_eq!(input.cursor, Some(cursor));
    assert_eq!(
        f.ingress.lock().unwrap().review.as_ref().unwrap().revision,
        2
    );
    f.finish().await;
}

#[tokio::test]
async fn retained_batch_requires_current_authority_for_reads_and_submission() {
    use ghinvite_core::{
        account::App,
        storage::{AppInstallationReadStorage, AppInstallationStorage},
    };
    let f = email_fixture().await;
    let (_, base, csrf) = resolve_email(&f, "authority-private@example.com", false).await;
    // Withdraw both canonical installations through their actual projection
    // adapter. The existing session and cached membership are deliberately kept.
    for app in [App::Repo, App::Org] {
        let mut installation = f
            .storage
            .active_app_installation(app, 9001)
            .await
            .unwrap()
            .unwrap();
        installation.revision += 1;
        installation.account.uninstalled_at = Some(Utc::now());
        f.storage.put_app_installation(&installation).await.unwrap();
    }
    let before = f.ingress.lock().unwrap().calls.len();
    let response = request(&f.app, &f.cookie, "GET", &base, String::new()).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let response = request(
        &f.app,
        &f.cookie,
        "POST",
        &format!("{base}/submit"),
        body(&[
            ("csrf_token", &csrf),
            ("operation_id", &ghinvite_core::RequestId::new().to_string()),
            ("expected_revision", "1"),
        ]),
    )
    .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(f.ingress.lock().unwrap().calls.len(), before);
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM email_vault")
        .fetch_one(&f.sql)
        .await
        .unwrap();
    assert_eq!(count, 0);
    f.finish().await;
}

#[tokio::test]
async fn accepted_batch_remains_available_when_an_undisplayed_personal_url_expires() {
    let f = email_fixture().await;
    let (batch, base, csrf) = resolve_email(&f, "undisplayed-private@example.com", true).await;
    let response = request(
        &f.app,
        &f.cookie,
        "POST",
        &format!("{base}/rows/remove"),
        body(&[
            ("csrf_token", &csrf),
            ("operation_id", &ghinvite_core::RequestId::new().to_string()),
            ("expected_revision", "1"),
            ("personal_link_rows", "1"),
        ]),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    f.ingress.lock().unwrap().lose_submit = true;
    let response = request(
        &f.app,
        &f.cookie,
        "POST",
        &format!("{base}/submit"),
        body(&[
            ("csrf_token", &csrf),
            ("operation_id", &ghinvite_core::RequestId::new().to_string()),
            ("expected_revision", "2"),
        ]),
    )
    .await;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    expire_personal_plan(&f, batch).await;
    let response = request(
        &f.app,
        &f.cookie,
        "POST",
        &format!("{base}/retry"),
        body(&[("csrf_token", &csrf), ("record_id", "submit")]),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let text = html(response).await;
    assert!(text.contains("The batch was submitted"));
    assert!(text.contains("Personal URLs could not be restored"));
    assert!(text.contains("rotate any URL you did not save"));
    assert!(!text.contains("/i#"));
    let claims: i64 =
        sqlx::query_scalar("SELECT count(*) FROM attempt_continuations WHERE id='display'")
            .fetch_one(&f.sql)
            .await
            .unwrap();
    assert_eq!(
        claims, 0,
        "failed decryption must not consume the one-time display claim"
    );
    let submissions: Vec<_> = f
        .ingress
        .lock()
        .unwrap()
        .calls
        .iter()
        .filter(|(name, _)| name == "submit")
        .map(|(_, value)| value.clone())
        .collect();
    assert_eq!(submissions.len(), 2);
    assert_eq!(submissions[0], submissions[1]);
    assert!(
        !serde_json::to_string(&submissions)
            .unwrap()
            .contains("undisplayed-private@example.com")
    );
    let response = request(&f.app, &f.cookie, "GET", &base, String::new()).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(!html(response).await.contains("/i#"));
    f.finish().await;
}

async fn other_admin_browser(f: &EmailFixture) -> (axum::Router, String) {
    let state = f.state.clone();
    let mut expected = sign_in::oauth_expectations(sign_in::GithubUser::new("second-admin", 43));
    expected.extend([
        Expectation::ok_json(Method::Get,"https://api.github.com/orgs/acme/memberships/second-admin",json!({"user":{"id":43,"login":"second-admin"},"organization":{"id":9001,"login":"acme"},"role":"admin","state":"active"})),
        Expectation::ok_json(Method::Get,"https://api.github.com/orgs/acme",json!({"id":9001,"login":"acme","created_at":"2020-01-01T00:00:00Z"})),
        Expectation::ok_json(Method::Get,"https://api.github.com/orgs/acme/teams?per_page=100",json!([])),
        sign_in::repo_identity(10,"acme/api",9001),
    ]);
    let state = sign_in::with_github(state, Arc::new(ConsoleTransport::scripted(expected)));
    let app = common::build_app(state, tower_sessions::MemoryStore::default());
    let cookie = sign_in::sign_in(&app).await;
    assert_ne!(cookie, f.cookie);
    (app, cookie)
}

#[tokio::test]
async fn another_current_admin_submits_selected_email_rows_without_creator_browser_secrets() {
    let f = email_fixture().await;
    let (_, base, csrf) = resolve_email(
        &f,
        "creator-private@example.com\nremoved-private@example.com",
        true,
    )
    .await;
    let response = request(
        &f.app,
        &f.cookie,
        "POST",
        &format!("{base}/rows/remove"),
        body(&[
            ("csrf_token", &csrf),
            ("operation_id", &ghinvite_core::RequestId::new().to_string()),
            ("expected_revision", "1"),
            ("remove_rows", "2"),
            ("personal_link_rows", "1"),
        ]),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let escrow: Vec<(i64, Option<String>, i64, i64)> = sqlx::query_as(
        "SELECT row_number,ciphertext,created_at,expires_at FROM email_review ORDER BY row_number",
    )
    .fetch_all(&f.sql)
    .await
    .unwrap();
    assert_eq!(escrow.len(), 2);
    assert_eq!(escrow[0].0, 1);
    assert!(escrow[0].1.is_some());
    assert!(
        !escrow[0]
            .1
            .as_ref()
            .unwrap()
            .contains("creator-private@example.com")
    );
    assert_eq!(escrow[1].0, 2);
    assert!(
        escrow[1].1.is_none(),
        "acknowledged removal retires only the removed review ciphertext"
    );
    assert!(
        escrow
            .iter()
            .all(|(_, _, created, expires)| expires - created == 3600)
    );
    let (app, cookie) = other_admin_browser(&f).await;
    let csrf2 = common::csrf_token(&app, &cookie).await;
    let response = request(&app, &cookie, "GET", &base, String::new()).await;
    assert_eq!(response.status(), StatusCode::OK);
    let text = html(response).await;
    assert!(!text.contains("creator-private@example.com"));
    assert!(!text.contains("removed-private@example.com"));
    assert!(!text.contains("/i#"));
    f.ingress.lock().unwrap().lose_submit = true;
    let response = request(
        &app,
        &cookie,
        "POST",
        &format!("{base}/submit"),
        body(&[
            ("csrf_token", &csrf2),
            ("operation_id", &ghinvite_core::RequestId::new().to_string()),
            ("expected_revision", "2"),
        ]),
    )
    .await;
    let status = response.status();
    let text = html(response).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{text}");
    let response = request(
        &app,
        &cookie,
        "POST",
        &format!("{base}/retry"),
        body(&[("csrf_token", &csrf2), ("record_id", "submit")]),
    )
    .await;
    let status = response.status();
    let text = html(response).await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert!(text.contains("The batch was submitted"));
    assert!(!text.contains("creator-private@example.com"));
    assert!(!text.contains("removed-private@example.com"));
    let secret = text
        .split("href=\"/i#")
        .nth(1)
        .unwrap()
        .split('"')
        .next()
        .unwrap();
    assert_eq!(secret.len(), 43);
    let response = request(&f.app, &f.cookie, "GET", &base, String::new()).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(!html(response).await.contains(secret));
    let response = request(
        &f.app,
        &f.cookie,
        "POST",
        &format!("{base}/retry"),
        body(&[("csrf_token", &csrf), ("record_id", "submit")]),
    )
    .await;
    assert_eq!(
        response.status(),
        StatusCode::NOT_FOUND,
        "creator has no submitting browser continuation"
    );
    {
        let state = f.ingress.lock().unwrap();
        let review = state.review.as_ref().unwrap();
        assert_eq!(review.manifest.admin.user_id, 42);
        assert_eq!(review.submitted.as_ref().unwrap().submitted_by, 43);
        assert_eq!(review.submitted.as_ref().unwrap().emails.len(), 1);
        assert_eq!(review.submitted.as_ref().unwrap().emails[0].approved_by, 43);
        let submissions: Vec<_> = state
            .calls
            .iter()
            .filter(|(name, _)| name == "submit")
            .map(|(_, value)| value.clone())
            .collect();
        assert_eq!(submissions.len(), 2);
        assert_eq!(submissions[0], submissions[1]);
        let wire = serde_json::to_string(&state.calls).unwrap();
        assert!(!wire.contains("creator-private@example.com"));
        assert!(!wire.contains("removed-private@example.com"));
        assert!(!wire.contains(secret));
    }
    let rows: Vec<i64> =
        sqlx::query_scalar("SELECT row_number FROM email_vault ORDER BY row_number")
            .fetch_all(&f.sql)
            .await
            .unwrap();
    assert_eq!(rows, vec![1]);
    drop(app);
    f.finish().await;
}

#[tokio::test]
async fn fresh_submission_uses_review_escrow_after_creator_label_plan_expires() {
    let f = email_fixture().await;
    let (batch, base, csrf) = resolve_email(&f, "expired-label-private@example.com", false).await;
    expire_email_plan(&f, batch).await;
    let response = request(
        &f.app,
        &f.cookie,
        "POST",
        &format!("{base}/submit"),
        body(&[
            ("csrf_token", &csrf),
            ("operation_id", &ghinvite_core::RequestId::new().to_string()),
            ("expected_revision", "1"),
        ]),
    )
    .await;
    let status = response.status();
    let text = html(response).await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert!(text.contains("The batch was submitted"));
    assert!(!text.contains("expired-label-private@example.com"));
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM email_vault")
        .fetch_one(&f.sql)
        .await
        .unwrap();
    assert_eq!(count, 1);
    f.finish().await;
}

#[tokio::test]
async fn old_create_and_edit_receipts_render_current_submitted_state_or_recovery() {
    for unavailable in [false, true] {
        let (app, cookie, ingress, transport, _, _) = fixture(true, true).await;
        let csrf = common::csrf_token(&app, &cookie).await;
        let batch = BulkBatchId::new();
        let base = format!("/console/accounts/octocat/bulk/{batch}");
        ingress.lock().unwrap().lose_create = true;
        let response = request(
            &app,
            &cookie,
            "POST",
            &format!("{base}/resolve"),
            body(&[
                ("csrf_token", &csrf),
                ("entries", "alice\nmissing"),
                ("repo_ids", "10"),
                ("repo_grants", "10:push"),
            ]),
        )
        .await;
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        // The owner accepted Create despite the lost acknowledgement. Its
        // state then advances through an edit and submission before recovery.
        let edit = ghinvite_core::RequestId::new().to_string();
        let response = request(
            &app,
            &cookie,
            "POST",
            &format!("{base}/rows/remove"),
            body(&[
                ("csrf_token", &csrf),
                ("operation_id", &edit),
                ("expected_revision", "1"),
                ("remove_rows", "2"),
            ]),
        )
        .await;
        let status = response.status();
        assert_eq!(status, StatusCode::OK, "{}", html(response).await);
        let submit = ghinvite_core::RequestId::new().to_string();
        let response = request(
            &app,
            &cookie,
            "POST",
            &format!("{base}/submit"),
            body(&[
                ("csrf_token", &csrf),
                ("operation_id", &submit),
                ("expected_revision", "2"),
            ]),
        )
        .await;
        let status = response.status();
        assert_eq!(status, StatusCode::OK, "{}", html(response).await);
        let current = ingress.lock().unwrap().review.clone().unwrap();
        assert_eq!(current.revision, 3);
        let github_before = transport.calls.lock().unwrap().clone();
        for record in ["create".to_owned(), format!("edit-{edit}")] {
            ingress.lock().unwrap().fail_review_after_ack = unavailable;
            let retry_body = body(&[("csrf_token", &csrf), ("record_id", &record)]);
            let response = request(
                &app,
                &cookie,
                "POST",
                &format!("{base}/retry"),
                retry_body.clone(),
            )
            .await;
            let status = response.status();
            let page = html(response).await;
            assert!(
                !page.contains("No GitHub invitation has been sent"),
                "{page}"
            );
            assert!(!page.contains("Submit batch"), "{page}");
            if unavailable {
                assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{page}");
                assert!(page.contains("The operation was recorded"), "{page}");
                assert!(page.contains(&format!("value=\"{record}\"")), "{page}");
                let response =
                    request(&app, &cookie, "POST", &format!("{base}/retry"), retry_body).await;
                let status = response.status();
                let page = html(response).await;
                assert_eq!(status, StatusCode::OK, "{page}");
                assert!(page.contains("The batch was submitted"), "{page}");
                assert!(!page.contains("Submit batch"), "{page}");
            } else {
                assert_eq!(status, StatusCode::OK, "{page}");
                assert!(page.contains("The batch was submitted"), "{page}");
                assert!(page.contains("Review revision 3"), "{page}");
            }
        }
        assert_eq!(*transport.calls.lock().unwrap(), github_before);
        let owner = ingress.lock().unwrap();
        assert_eq!(owner.review.as_ref(), Some(&current));
        for method in ["create", "remove_rows"] {
            let commands: Vec<_> = owner
                .calls
                .iter()
                .filter(|(name, _)| name == method)
                .collect();
            assert!(commands.len() >= 2);
            assert!(
                commands
                    .iter()
                    .all(|(_, command)| command == &commands[0].1)
            );
        }
        assert_eq!(
            owner
                .calls
                .iter()
                .filter(|(name, _)| name == "submit")
                .count(),
            1
        );
    }
}

#[tokio::test]
async fn accepted_create_recovers_after_uninstall_without_new_lookup_or_submission() {
    use ghinvite_core::{
        account::App,
        storage::{AppInstallationReadStorage, AppInstallationStorage},
    };
    let (app, cookie, ingress, transport, _, storage) = fixture(true, true).await;
    let csrf = common::csrf_token(&app, &cookie).await;
    let batch = BulkBatchId::new();
    let base = format!("/console/accounts/octocat/bulk/{batch}");
    ingress.lock().unwrap().lose_create = true;
    let response = request(
        &app,
        &cookie,
        "POST",
        &format!("{base}/resolve"),
        body(&[
            ("csrf_token", &csrf),
            ("entries", "alice"),
            ("repo_ids", "10"),
            ("repo_grants", "10:push"),
        ]),
    )
    .await;
    let status = response.status();
    let page = html(response).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{page}");
    let original = ingress.lock().unwrap().review.clone().unwrap();
    let before = transport.calls.lock().unwrap().clone();
    let mut installation = storage
        .active_app_installation(App::Repo, 42)
        .await
        .unwrap()
        .unwrap();
    installation.revision += 1;
    installation.account.uninstalled_at = Some(Utc::now());
    storage.put_app_installation(&installation).await.unwrap();
    let response = request(
        &app,
        &cookie,
        "POST",
        &format!("{base}/retry"),
        body(&[("csrf_token", &csrf), ("record_id", "create")]),
    )
    .await;
    let status = response.status();
    let page = html(response).await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert_eq!(
        *transport.calls.lock().unwrap(),
        before,
        "historical replay must not repeat GitHub lookups"
    );
    let retained = ingress.lock().unwrap();
    assert_eq!(retained.review.as_ref(), Some(&original));
    let creates: Vec<_> = retained
        .calls
        .iter()
        .filter(|(name, _)| name == "create")
        .map(|(_, value)| value)
        .collect();
    assert_eq!(creates.len(), 2);
    assert_eq!(creates[0], creates[1]);
    assert!(!retained.calls.iter().any(|(name, _)| name == "submit"));
}
