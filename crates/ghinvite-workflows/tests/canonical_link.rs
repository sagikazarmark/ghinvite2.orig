//! Authenticated native web → real Restate owners → GitHub HTTP stub, with
//! projection deliberately unavailable until after creation and admission replay.
#![cfg(feature = "integration")]

use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
    response::Response,
};
use ghinvite_core::storage::projection::{ProjectionEnvelope, ProjectionStorage};
use ghinvite_core::storage::{ConsoleStorage, RecordStorage};
use ghinvite_github::transport::{HttpTransport, ReqwestTransport};
use serde_json::json;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;
use tower::ServiceExt;

// Only the network destination changes: OAuth and UserApiClient still use the
// production request construction, transport, and response decoding.
struct GithubHttpStub {
    base: String,
    transport: ReqwestTransport,
}
#[async_trait::async_trait]
impl HttpTransport for GithubHttpStub {
    async fn send(
        &self,
        mut request: ghinvite_github::transport::Request,
    ) -> ghinvite_github::Result<ghinvite_github::transport::Response> {
        request.url = request
            .url
            .replace("https://api.github.com", &self.base)
            .replace("https://github.com", &self.base);
        self.transport.send(request).await
    }
}

struct DelayedProjection {
    storage: Arc<ghinvite_storage_sqlx::SqlxStorage>,
    delayed: AtomicBool,
}
#[async_trait::async_trait]
impl ProjectionStorage for DelayedProjection {
    async fn apply_request(
        &self,
        envelope: &ghinvite_core::storage::projection::RequestProjectionEnvelope,
    ) -> ghinvite_core::storage::Result<()> {
        if self.delayed.load(Ordering::SeqCst) {
            return Err(ghinvite_core::storage::Error::ProjectionDependency);
        }
        self.storage.apply_request(envelope).await
    }
    async fn apply_transition(
        &self,
        envelope: &ProjectionEnvelope,
    ) -> ghinvite_core::storage::Result<()> {
        if self.delayed.load(Ordering::SeqCst) {
            return Err(ghinvite_core::storage::Error::ProjectionDependency);
        }
        self.storage.apply_transition(envelope).await
    }
}

async fn request(app: &axum::Router, cookie: &str, uri: &str, body: Option<&str>) -> Response {
    let mut builder = Request::builder().uri(uri).header("cookie", cookie);
    if body.is_some() {
        builder = builder
            .method("POST")
            .header("content-type", "application/x-www-form-urlencoded");
    }
    app.clone()
        .oneshot(
            builder
                .body(Body::from(body.unwrap_or_default().to_owned()))
                .unwrap(),
        )
        .await
        .unwrap()
}
async fn html(response: Response) -> String {
    String::from_utf8(
        to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap()
}
async fn sign_in(app: &axum::Router) -> String {
    let login = request(app, "", "/login", None).await;
    let state = login.headers()["location"]
        .to_str()
        .unwrap()
        .split("state=")
        .nth(1)
        .unwrap()
        .split('&')
        .next()
        .unwrap();
    let callback = request(
        app,
        &cookie(&login),
        &format!("/oauth/callback?code=fixture&state={state}"),
        None,
    )
    .await;
    assert_eq!(callback.status(), StatusCode::SEE_OTHER);
    cookie(&callback)
}

async fn owner(
    client: &reqwest::Client,
    ingress: &str,
    path: &str,
    body: serde_json::Value,
) -> serde_json::Value {
    let request = client
        .post(format!("{ingress}/{path}"))
        .header("accept", "application/json");
    let request = if body.is_null() {
        request
    } else {
        request.json(&body)
    };
    let response = request.send().await.unwrap();
    assert!(
        response.status().is_success(),
        "{path}: {}",
        response.text().await.unwrap()
    );
    response.json().await.unwrap()
}
fn cookie(response: &Response) -> String {
    response.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .into()
}
fn field(html: &str, name: &str) -> String {
    html.split(&format!("name=\"{name}\""))
        .nth(1)
        .unwrap()
        .split("value=\"")
        .nth(1)
        .unwrap()
        .split('"')
        .next()
        .unwrap()
        .into()
}

#[tokio::test]
async fn authenticated_journey_settles_and_recovers_across_projection_and_response_loss() {
    let ingress =
        std::env::var("RESTATE_INGRESS_URL").expect("bash scripts/test-restate.sh canonical_link");
    let admin = std::env::var("RESTATE_ADMIN_URL").unwrap();
    let host = std::env::var("RESTATE_ENDPOINT_HOST").unwrap();
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(25))
        .build()
        .unwrap();
    let requester_login = Arc::new(AtomicBool::new(false));
    let login_identity = requester_login.clone();
    let stub = ghinvite_github::stub::router()
        .route("/login/oauth/access_token", axum::routing::post(|| async { axum::Json(json!({"access_token":"fixture", "token_type":"bearer", "scope":""})) }))
        .route("/user", axum::routing::get(move || { let requester = login_identity.load(Ordering::SeqCst); async move { axum::Json(if requester { json!({"id":8,"login":"alice"}) } else { json!({"id":7,"login":"creator"}) }) } }))
        .route("/user/memberships/orgs/acme", axum::routing::get(|| async { axum::Json(json!({"role":"admin","state":"active","organization":{"id":100}})) }))
        .route("/user/installations/1/repositories", axum::routing::get(|| async { axum::Json(json!({"total_count":2,"repositories":[{"id":10,"full_name":"acme/api","private":true},{"id":11,"full_name":"acme/web","private":true}]})) }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let stub_task = tokio::spawn(async move {
        axum::serve(listener, stub).await.unwrap();
    });
    let storage = Arc::new(
        ghinvite_storage_sqlx::SqlxStorage::in_memory()
            .await
            .unwrap(),
    );
    let github = Arc::new(
        ghinvite_github::InstallationClient::new(
            Arc::new(ReqwestTransport::with_client(client.clone())),
            ghinvite_github::jwt::AppJwtSigner::from_pem(
                123,
                include_str!("../../ghinvite-github/src/jwt_test_key.pem"),
            )
            .unwrap(),
        )
        .with_base(&base),
    );
    let projection = Arc::new(DelayedProjection {
        storage: storage.clone(),
        delayed: AtomicBool::new(true),
    });
    let endpoint = ghinvite_workflows::build_endpoint(
        ghinvite_workflows::AppState::new(storage.clone(), github),
        projection.clone(),
        None,
    )
    .unwrap();
    let listener = tokio::net::TcpListener::bind("0.0.0.0:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let endpoint_task = tokio::spawn(async move {
        restate_sdk::http_server::HttpServer::new(endpoint)
            .serve(listener)
            .await;
    });
    let deployment = client
        .post(format!("{admin}/deployments"))
        .json(&json!({"uri":format!("http://{host}:{port}")}))
        .send()
        .await
        .unwrap();
    assert!(
        deployment.status().is_success(),
        "{}",
        deployment.text().await.unwrap()
    );
    // Only this disposable runtime: original executions must actually expire.
    for service in ["InvitationLink", "InvitationRequest", "RepositoryDelivery"] {
        client
            .patch(format!("{admin}/services/{service}"))
            .json(&json!({"journal_retention":"2s","idempotency_retention":"2s"}))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap();
    }
    let onboard = client.post(format!("{ingress}/Installation/1/onboard"))
        .json(&json!({"installation_id":1,"actor_user_id":7,"account_id":100,"account_login":"acme",
            "account_type":"Organization","selected_repos":"all","installed_at":chrono::Utc::now()}))
        .send().await.unwrap();
    assert!(onboard.status().is_success());
    tokio::time::timeout(Duration::from_secs(20), async {
        while storage.get_installation(1).await.unwrap().is_none() {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap();
    // Forward actual ingress HTTP, but lose the first successful creation
    // acknowledgement. Recovery must use the browser's retained exact input.
    let lost = Arc::new(std::sync::Mutex::new(std::collections::BTreeSet::new()));
    let receipts = Arc::new(std::sync::Mutex::new(Vec::<serde_json::Value>::new()));
    let executions = Arc::new(std::sync::Mutex::new(Vec::<(
        String,
        serde_json::Value,
        serde_json::Value,
        String,
    )>::new()));
    let proxy = {
        let client = client.clone();
        let ingress = ingress.clone();
        let lost = lost.clone();
        let receipts = receipts.clone();
        let executions = executions.clone();
        axum::Router::new().fallback(move |request: axum::extract::Request| {
            let client = client.clone();
            let ingress = ingress.clone();
            let lost = lost.clone();
            let receipts = receipts.clone();
            let executions = executions.clone();
            async move {
                let path = request.uri().path().to_owned();
                let body = to_bytes(request.into_body(), 1024 * 1024).await.unwrap();
                let mutation = ["/create", "/admit", "/decide"]
                    .iter()
                    .any(|method| path.ends_with(method));
                let input: serde_json::Value = if body.is_empty() {
                    serde_json::Value::Null
                } else {
                    serde_json::from_slice(&body).unwrap()
                };
                let upstream = client.post(format!(
                    "{ingress}{path}{}",
                    if mutation { "/send" } else { "" }
                ));
                let upstream = if body.is_empty() {
                    upstream
                } else {
                    upstream
                        .header("content-type", "application/json")
                        .body(body)
                };
                let upstream = upstream.send().await.unwrap();
                let (upstream, invocation) = if mutation && upstream.status().is_success() {
                    let ack: serde_json::Value = upstream.json().await.unwrap();
                    let invocation = ack["invocationId"].as_str().unwrap().to_owned();
                    (
                        client
                            .get(format!("{ingress}/restate/invocation/{invocation}/attach"))
                            .send()
                            .await
                            .unwrap(),
                        Some(invocation),
                    )
                } else {
                    (upstream, None)
                };
                let status = upstream.status();
                let body = upstream.bytes().await.unwrap();
                if ["/create", "/admit", "/decide"]
                    .iter()
                    .any(|method| path.ends_with(method))
                    && status.is_success()
                {
                    executions.lock().unwrap().push((
                        path.clone(),
                        input,
                        serde_json::from_slice(&body).unwrap(),
                        invocation.unwrap(),
                    ));
                    if path.ends_with("/create") {
                        receipts
                            .lock()
                            .unwrap()
                            .push(serde_json::from_slice(&body).unwrap());
                    }
                    if lost
                        .lock()
                        .unwrap()
                        .insert(path.rsplit('/').next().unwrap().to_owned())
                    {
                        return Response::builder().status(502).body(Body::empty()).unwrap();
                    }
                }
                Response::builder()
                    .status(status)
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap()
            }
        })
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_url = format!("http://{}", listener.local_addr().unwrap());
    let proxy_task = tokio::spawn(async move {
        axum::serve(listener, proxy).await.unwrap();
    });
    let state = ghinvite_web::AppState::new(
        storage.clone(),
        Arc::new(GithubHttpStub {
            base: base.clone(),
            transport: ReqwestTransport::with_client(client.clone()),
        }),
        Arc::new(ghinvite_web::RestateClient::new(proxy_url).unwrap()),
        ghinvite_web::WebConfig {
            webhook_secret: b"journey-secret".to_vec(),
            ..ghinvite_web::WebConfig::for_local_dev_with_secret([7; 32])
        },
    );
    let app = ghinvite_web::build_app(state, tower_sessions::MemoryStore::default());
    let cookie = sign_in(&app).await;
    let form = html(request(&app, &cookie, "/console/accounts/acme/links/new", None).await).await;
    let action = form
        .split("action=\"/console/accounts/acme/links?")
        .nth(1)
        .unwrap()
        .split('"')
        .next()
        .unwrap()
        .replace("&amp;", "&")
        .replace("&#38;", "&");
    let action = format!("/console/accounts/acme/links?{action}");
    let query = url_query(&action);
    let id = query["link_id"].clone();
    assert_eq!(id.len(), 26);
    let csrf = field(&form, "csrf_token");
    let body = format!(
        "csrf_token={csrf}&description=Workshop&permission=pull&repo_ids=10&approval_required=on&expires_in_days=2"
    );
    // Validation and repository reload both retain the allocated ID/anchor.
    for fields in [
        body.replace("description=Workshop", "description="),
        format!("{body}&reload_repos=true"),
    ] {
        let response = request(&app, &cookie, &action, Some(&fields)).await;
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "{}",
            html(response).await
        );
        let rendered = html(response).await;
        assert!(
            rendered
                .replace("&#38;", "&")
                .replace("&amp;", "&")
                .contains(&format!("link_id={id}&anchor={}", query["anchor"]))
        );
    }
    let created = request(&app, &cookie, &action, Some(&body)).await;
    assert_eq!(
        created.status(),
        StatusCode::BAD_GATEWAY,
        "{}",
        html(created).await
    );
    let unknown = html(created).await;
    assert!(unknown.contains("Outcome unknown"));
    assert!(unknown.contains(&format!("create-{id}")));
    let recovery = format!(
        "/console/accounts/acme/attempts/create-{}",
        id.to_lowercase()
    );
    let status = request(&app, &cookie, &recovery, None).await;
    assert_eq!(status.status(), StatusCode::OK);
    assert_eq!(
        request(
            &app,
            &cookie,
            &recovery,
            Some(&format!("csrf_token={csrf}"))
        )
        .await
        .status(),
        StatusCode::SEE_OTHER
    );
    {
        let receipts = receipts.lock().unwrap();
        assert_eq!(receipts.len(), 2);
        assert_eq!(
            receipts[0], receipts[1],
            "recovery preserves input and original creation time"
        );
    }
    let detail = format!("/console/accounts/acme/links/{id}");
    let history = request(&app, &cookie, &format!("{detail}/requests"), None).await;
    assert_eq!(history.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert!(html(history).await.contains("Request history unavailable"));
    let page = html(request(&app, &cookie, &detail, None).await).await;
    assert!(page.contains(&format!("/i/{id}")));
    assert!(
        storage
            .get_invitation_link_by_id(id.parse().unwrap())
            .await
            .unwrap()
            .is_none()
    );
    let snapshot: serde_json::Value = client
        .post(format!("{ingress}/InvitationLink/{id}/link_status"))
        .json(&json!({"link_id":id,"admin":{"account_id":100,"user_id":7}}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        snapshot["creation"]["expires_at"],
        json!(
            chrono::DateTime::from_timestamp(
                query["anchor"].parse::<i64>().unwrap() + 2 * 86400,
                0
            )
            .unwrap()
        )
    );
    let variant = action.replace(&id, &id.to_lowercase());
    assert_eq!(
        request(&app, &cookie, &variant, Some(&body)).await.status(),
        StatusCode::SEE_OTHER
    );
    assert_eq!(
        request(
            &app,
            &cookie,
            &variant,
            Some(&body.replace("Workshop", "Changed"))
        )
        .await
        .status(),
        StatusCode::CONFLICT
    );
    let public = format!("/i/{id}");
    requester_login.store(true, Ordering::SeqCst);
    let requester_cookie = sign_in(&app).await;
    let page = html(request(&app, &requester_cookie, &public.to_lowercase(), None).await).await;
    assert!(page.contains("acme/api"));
    let operation = field(&page, "operation_id");
    let requester_csrf = field(&page, "csrf_token");
    let submit =
        format!("csrf_token={requester_csrf}&operation_id={operation}&justification=++Original++");
    let unknown = request(&app, &requester_cookie, &public, Some(&submit)).await;
    assert_eq!(unknown.status(), StatusCode::BAD_GATEWAY);
    let unknown = html(unknown).await;
    assert!(
        unknown.contains("Outcome unknown")
            && unknown.contains(&operation)
            && unknown.contains("Original")
    );
    for path in [&public.to_lowercase(), &public] {
        let admitted = request(&app, &requester_cookie, path, Some(&submit)).await;
        assert_eq!(
            admitted.status(),
            StatusCode::SEE_OTHER,
            "{}",
            html(admitted).await
        );
    }
    let status = html(
        request(
            &app,
            &requester_cookie,
            &format!("{public}?operation_id={operation}"),
            None,
        )
        .await,
    )
    .await;
    assert!(status.contains("Request accepted at"));
    assert!(status.contains("Awaiting review"));
    let page: serde_json::Value = client
        .post(format!("{ingress}/InvitationLink/{id}/requester_page"))
        .json(&json!({"link_id":id,"requester_id":8,"operation_id":operation}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let request_id = page["request"]["request_id"].as_str().unwrap();
    let request_detail = request(
        &app,
        &cookie,
        &format!("/console/accounts/acme/requests/{request_id}"),
        None,
    )
    .await;
    assert_eq!(request_detail.status(), StatusCode::OK);
    assert!(html(request_detail).await.contains("Original"));
    for (path, input) in [
        (
            format!("InvitationRequest/{request_id}/admin_status"),
            json!({"request_id":request_id,"admin":{"account_id":999,"user_id":7}}),
        ),
        (
            format!("InvitationLink/{id}/link_status"),
            json!({"link_id":id,"admin":{"account_id":999,"user_id":7}}),
        ),
    ] {
        assert_eq!(
            client
                .post(format!("{ingress}/{path}"))
                .json(&input)
                .send()
                .await
                .unwrap()
                .status(),
            404
        );
    }
    let approve = format!("/console/accounts/acme/requests/{request_id}/approve");
    let decision_operation = ghinvite_core::RequestId::new();
    let decision_body = format!("csrf_token={csrf}&link_id={id}&operation_id={decision_operation}");
    let unknown = request(&app, &cookie, &approve, Some(&decision_body)).await;
    assert_eq!(unknown.status(), StatusCode::BAD_GATEWAY);
    assert!(html(unknown).await.contains("Outcome unknown"));
    let approved = request(&app, &cookie, &approve, Some(&decision_body)).await;
    assert_eq!(approved.status(), StatusCode::SEE_OTHER);
    let approved_page =
        html(request(&app, &cookie, "/console/accounts/acme/requests", None).await).await;
    assert!(approved_page.contains("Request approved"));
    let declined = request(
        &app,
        &cookie,
        &format!("/console/accounts/acme/requests/{request_id}/decline"),
        Some(&format!(
            "csrf_token={csrf}&link_id={id}&operation_id={}&reason=private",
            ghinvite_core::RequestId::new()
        )),
    )
    .await;
    assert_eq!(declined.status(), StatusCode::CONFLICT);
    assert!(html(declined).await.contains("not applied"));
    let status = html(
        request(
            &app,
            &requester_cookie,
            &format!("{public}?operation_id={operation}"),
            None,
        )
        .await,
    )
    .await;
    assert!(status.contains("Request accepted at"));
    assert!(!status.contains("Awaiting review"));
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let calls: serde_json::Value = client
                .get(format!("{base}/calls"))
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            if calls["requests"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|c| c["method"] == "PUT")
                .count()
                == 1
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("recorded approval delivers while SQL projection is withheld");
    assert!(
        storage
            .get_invitation_link_by_id(id.parse().unwrap())
            .await
            .unwrap()
            .is_none()
    );
    projection.delayed.store(false, Ordering::SeqCst);
    tokio::time::timeout(Duration::from_secs(25), async {
        loop {
            if storage
                .get_invitation_link_by_id(id.parse().unwrap())
                .await
                .unwrap()
                .is_some_and(|link| link.uses_count == 1)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("delayed projection converges to one admitted use");
    let audit = storage
        .list_audit_events(100, None, ghinvite_core::storage::AuditPosition::Latest)
        .await
        .unwrap();
    assert_eq!(
        audit
            .events
            .iter()
            .filter(|event| event.event_type.as_str() == "invitation_link.created")
            .count(),
        1
    );
    let query = json!({"link_id":id,"request_id":request_id,"requester_id":8});
    let plan = owner(
        &client,
        &ingress,
        &format!("InvitationRequest/{request_id}/approved_plan"),
        query.clone(),
    )
    .await;
    let command = &plan["commands"][0];
    let delivery_path = format!("RepositoryDelivery/{request_id}:10/status");
    let delivered = owner(&client, &ingress, &delivery_path, json!(null)).await;
    assert_eq!(delivered["create"]["outcome"]["kind"], "created");
    // Supported member evidence: GitHub reports current numeric identity/access,
    // and the complete pending list no longer contains the known invitation.
    let upstream = delivered["create"]["outcome"]["upstream_id"]
        .as_u64()
        .unwrap();
    client
        .delete(format!("{base}/repos/acme/api/invitations/{upstream}"))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    client
        .post(format!("{base}/identity"))
        .json(&json!({"login":"alice","addressed_id":8,"role_name":"read"}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(25), async {
        loop {
            if storage
                .get_github_invitation(command["invitation_id"].as_str().unwrap().parse().unwrap())
                .await
                .unwrap()
                .is_some()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .unwrap();
    let payload = json!({"action":"added","installation":{"id":1},"repository":{"id":10,"owner":{"id":100}},"member":{"id":8}}).to_string();
    use hmac::{Hmac, Mac};
    let mut mac = Hmac::<sha2::Sha256>::new_from_slice(b"journey-secret").unwrap();
    mac.update(payload.as_bytes());
    let signature: String = mac
        .finalize()
        .into_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    for _ in 0..2 {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/webhooks/github")
                    .header("content-type", "application/json")
                    .header("x-github-event", "member")
                    .header("x-github-delivery", "journey-member")
                    .header("x-hub-signature-256", format!("sha256={signature}"))
                    .body(Body::from(payload.clone()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
    }
    tokio::time::timeout(Duration::from_secs(25), async {
        loop {
            let page = html(
                request(
                    &app,
                    &requester_cookie,
                    &format!("{public}?operation_id={operation}"),
                    None,
                )
                .await,
            )
            .await;
            let audit =
                html(request(&app, &cookie, "/console/accounts/acme/audit", None).await).await;
            if page.contains("Repository access accepted")
                && audit.contains("GitHub invitation accepted")
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("requester status and admin audit converge after signed evidence");
    let settled = owner(&client, &ingress, &delivery_path, json!(null)).await;
    assert_eq!(settled["settlement"]["state"], "accepted");
    assert_eq!(settled["create"], delivered["create"]);

    let original_executions = executions.lock().unwrap().clone();
    assert!(
        original_executions
            .iter()
            .any(|(path, _, _, _)| path.ends_with("/admit"))
    );
    assert!(
        original_executions
            .iter()
            .any(|(path, _, _, _)| path.ends_with("/decide"))
    );
    let dispatch = owner(
        &client,
        &ingress,
        &format!("InvitationRequest/{request_id}/delivery_status"),
        query.clone(),
    )
    .await;
    let mut invocation_ids: Vec<_> = original_executions
        .iter()
        .map(|(_, _, _, id)| id.clone())
        .collect();
    invocation_ids.extend(
        dispatch["submitted"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["invocation_id"].as_str().unwrap().to_owned()),
    );
    let ids = invocation_ids
        .iter()
        .map(|id| format!("'{id}'"))
        .collect::<Vec<_>>()
        .join(",");
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let rows = owner(
                &client,
                &admin,
                "query",
                json!({"query":format!("SELECT id FROM sys_invocation WHERE id IN ({ids})")}),
            )
            .await;
            if rows["rows"].as_array().unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("completed execution is cleaned without restarting authority");
    let before = storage
        .list_audit_events(100, None, ghinvite_core::storage::AuditPosition::Latest)
        .await
        .unwrap()
        .events;
    for (path, input, receipt, _) in &original_executions {
        assert_eq!(
            owner(
                &client,
                &ingress,
                path.trim_start_matches('/'),
                input.clone()
            )
            .await,
            *receipt
        );
    }
    assert_eq!(
        request(&app, &requester_cookie, &public, Some(&submit))
            .await
            .status(),
        StatusCode::SEE_OTHER
    );
    assert_eq!(
        request(&app, &cookie, &approve, Some(&decision_body))
            .await
            .status(),
        StatusCode::SEE_OTHER
    );
    let recovery = owner(&client, &ingress, "DeliveryRecovery/recover", query.clone()).await;
    for submitted in recovery["submitted"].as_array().unwrap() {
        let invocation = submitted["invocation_id"].as_str().unwrap();
        client
            .get(format!("{ingress}/restate/invocation/{invocation}/attach"))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap();
    }
    // All recovery sends completed. Drain the independently queued projections
    // before asserting absence of duplicate history/effects.
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let rows = owner(&client, &admin, "query", json!({"query":format!("SELECT id FROM sys_invocation WHERE target_service_name = 'DeliveryProjection' AND target_service_key = '{request_id}:10' AND status != 'completed'")})).await;
            if rows["rows"].as_array().unwrap().is_empty() { break; }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }).await.unwrap();
    assert_eq!(
        owner(&client, &ingress, &delivery_path, json!(null)).await,
        settled
    );
    assert_eq!(
        owner(
            &client,
            &ingress,
            &format!("InvitationRequest/{request_id}/approved_plan"),
            query
        )
        .await,
        plan
    );
    assert_eq!(
        storage
            .list_audit_events(100, None, ghinvite_core::storage::AuditPosition::Latest)
            .await
            .unwrap()
            .events,
        before
    );
    let calls: serde_json::Value = client
        .get(format!("{base}/calls"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        calls["requests"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|call| call["method"] == "PUT")
            .count(),
        1
    );
    client
        .post(format!("{base}/identity"))
        .json(&json!({"login":"alice","addressed_id":8}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    // The same authenticated routes cover fresh admission after decline and
    // admission-time auto approval. Keep the deadline/race matrix in request_owner.
    for auto in [false, true] {
        let form =
            html(request(&app, &cookie, "/console/accounts/acme/links/new", None).await).await;
        let action = form
            .split("action=\"/console/accounts/acme/links?")
            .nth(1)
            .unwrap()
            .split('"')
            .next()
            .unwrap()
            .replace("&amp;", "&")
            .replace("&#38;", "&");
        let action = format!("/console/accounts/acme/links?{action}");
        let link_id = url_query(&action)["link_id"].clone();
        let body = format!(
            "csrf_token={csrf}&description=Two+repositories&permission=pull&repo_ids=10&repo_ids=11{}",
            if auto { "" } else { "&approval_required=on" }
        );
        assert_eq!(
            request(&app, &cookie, &action, Some(&body)).await.status(),
            StatusCode::SEE_OTHER
        );
        let public = format!("/i/{link_id}");
        let page = html(request(&app, &requester_cookie, &public, None).await).await;
        let mut operation = field(&page, "operation_id");
        let submission = |op: &str| {
            format!("csrf_token={requester_csrf}&operation_id={op}&justification=Fresh+request")
        };
        assert_eq!(
            request(
                &app,
                &requester_cookie,
                &public,
                Some(&submission(&operation))
            )
            .await
            .status(),
            StatusCode::SEE_OTHER
        );
        let page = owner(
            &client,
            &ingress,
            &format!("InvitationLink/{link_id}/requester_page"),
            json!({"link_id":link_id,"requester_id":8,"operation_id":operation}),
        )
        .await;
        let mut request_id = page["request"]["request_id"].as_str().unwrap().to_owned();
        if !auto {
            let decline = format!("/console/accounts/acme/requests/{request_id}/decline");
            let body = format!(
                "csrf_token={csrf}&link_id={link_id}&operation_id={}&reason=Try+again",
                ghinvite_core::RequestId::new()
            );
            assert_eq!(
                request(&app, &cookie, &decline, Some(&body)).await.status(),
                StatusCode::SEE_OTHER
            );
            let page = html(
                request(
                    &app,
                    &requester_cookie,
                    &format!("{public}?fresh=true"),
                    None,
                )
                .await,
            )
            .await;
            operation = field(&page, "operation_id");
            assert_eq!(
                request(
                    &app,
                    &requester_cookie,
                    &public,
                    Some(&submission(&operation))
                )
                .await
                .status(),
                StatusCode::SEE_OTHER
            );
            let page = owner(
                &client,
                &ingress,
                &format!("InvitationLink/{link_id}/requester_page"),
                json!({"link_id":link_id,"requester_id":8,"operation_id":operation}),
            )
            .await;
            let fresh = page["request"]["request_id"].as_str().unwrap();
            assert_ne!(fresh, request_id);
            request_id = fresh.to_owned();
            client.post(format!("{base}/outcomes")).json(&json!({"owner":"acme","repo":"web","user":"alice","outcome":"access_lost_once"})).send().await.unwrap().error_for_status().unwrap();
            let approve = format!("/console/accounts/acme/requests/{request_id}/approve");
            let body = format!(
                "csrf_token={csrf}&link_id={link_id}&operation_id={}",
                ghinvite_core::RequestId::new()
            );
            assert_eq!(
                request(&app, &cookie, &approve, Some(&body)).await.status(),
                StatusCode::SEE_OTHER
            );
        } else {
            assert_eq!(page["request"]["state"], "approved");
            assert!(page["request"]["decision_deadline"].is_null());
        }
        let query = json!({"link_id":link_id,"request_id":request_id,"requester_id":8});
        let plan = owner(
            &client,
            &ingress,
            &format!("InvitationRequest/{request_id}/approved_plan"),
            query.clone(),
        )
        .await;
        assert_eq!(plan["commands"].as_array().unwrap().len(), 2);
        tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                let api = owner(
                    &client,
                    &ingress,
                    &format!("RepositoryDelivery/{request_id}:10/status"),
                    json!(null),
                )
                .await;
                let web = owner(
                    &client,
                    &ingress,
                    &format!("RepositoryDelivery/{request_id}:11/status"),
                    json!(null),
                )
                .await;
                if api["create"]["outcome"]["kind"] == "created"
                    && web["create"]["outcome"]["kind"] == if auto { "created" } else { "blocked" }
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await
        .expect("repositories progress independently");
        let link = owner(
            &client,
            &ingress,
            &format!("InvitationLink/{link_id}/link_status"),
            json!({"link_id":link_id,"admin":{"account_id":100,"user_id":7}}),
        )
        .await;
        assert_eq!(link["uses"], if auto { 1 } else { 2 });
        if !auto {
            owner(&client, &ingress, "DeliveryRecovery/recover", query).await;
            tokio::time::timeout(Duration::from_secs(20), async {
                loop {
                    let web = owner(
                        &client,
                        &ingress,
                        &format!("RepositoryDelivery/{request_id}:11/status"),
                        json!(null),
                    )
                    .await;
                    if web["create"]["outcome"]["kind"] == "created" {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            })
            .await
            .expect("explicit recovery resumes refused repository");
        }
    }
    endpoint_task.abort();
    proxy_task.abort();
    stub_task.abort();
}

fn url_query(action: &str) -> std::collections::BTreeMap<String, String> {
    action
        .split('?')
        .nth(1)
        .unwrap()
        .split('&')
        .map(|part| {
            let (key, value) = part.split_once('=').unwrap();
            (key.into(), value.into())
        })
        .collect()
}
