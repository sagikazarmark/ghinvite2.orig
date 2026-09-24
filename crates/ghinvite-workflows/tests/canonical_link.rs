//! Authenticated native web → real Restate owners → GitHub HTTP stub, with
//! projection deliberately unavailable until after creation and admission replay.
#![cfg(feature = "integration")]

use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
    response::Response,
};
use ghinvite_core::storage::projection::{ProjectionEnvelope, ProjectionStorage};
use ghinvite_core::storage::{ConsoleStorage, InstallationStorage, RecordStorage};
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
async fn confirmed_link_opens_and_accepts_replay_before_projection() {
    let ingress =
        std::env::var("RESTATE_INGRESS_URL").expect("bash scripts/test-restate.sh canonical_link");
    let admin = std::env::var("RESTATE_ADMIN_URL").unwrap();
    let host = std::env::var("RESTATE_ENDPOINT_HOST").unwrap();
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(25))
        .build()
        .unwrap();
    let stub = ghinvite_github::stub::router()
        .route("/login/oauth/access_token", axum::routing::post(|| async { axum::Json(json!({"access_token":"fixture", "token_type":"bearer", "scope":""})) }))
        .route("/user", axum::routing::get(|| async { axum::Json(json!({"id":7,"login":"creator"})) }))
        .route("/user/memberships/orgs/acme", axum::routing::get(|| async { axum::Json(json!({"role":"admin","state":"active","organization":{"id":100}})) }))
        .route("/user/installations/1/repositories", axum::routing::get(|| async { axum::Json(json!({"total_count":1,"repositories":[{"id":10,"full_name":"acme/api","private":true}]})) }));
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
    storage
        .insert_installation(&ghinvite_core::Account {
            installation_id: 1,
            account_id: 100,
            account_login: "acme".into(),
            account_type: ghinvite_core::AccountType::Organization,
            installed_at: chrono::Utc::now(),
            uninstalled_at: None,
            selected_repos: ghinvite_core::SelectedRepos::All,
        })
        .await
        .unwrap();
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
    // Forward actual ingress HTTP, but lose the first successful creation
    // acknowledgement. Recovery must use the browser's retained exact input.
    let lost = Arc::new(AtomicBool::new(false));
    let receipts = Arc::new(std::sync::Mutex::new(Vec::<serde_json::Value>::new()));
    let proxy = {
        let client = client.clone();
        let ingress = ingress.clone();
        let lost = lost.clone();
        let receipts = receipts.clone();
        axum::Router::new().fallback(move |request: axum::extract::Request| {
            let client = client.clone();
            let ingress = ingress.clone();
            let lost = lost.clone();
            let receipts = receipts.clone();
            async move {
                let path = request.uri().path().to_owned();
                let body = to_bytes(request.into_body(), 1024 * 1024).await.unwrap();
                let upstream = client
                    .post(format!("{ingress}{path}"))
                    .header("content-type", "application/json")
                    .body(body)
                    .send()
                    .await
                    .unwrap();
                let status = upstream.status();
                let body = upstream.bytes().await.unwrap();
                if path.ends_with("/create") && status.is_success() {
                    receipts
                        .lock()
                        .unwrap()
                        .push(serde_json::from_slice(&body).unwrap());
                    if !lost.swap(true, Ordering::SeqCst) {
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
            base,
            transport: ReqwestTransport::with_client(client.clone()),
        }),
        Arc::new(ghinvite_web::RestateClient::new(proxy_url).unwrap()),
        ghinvite_web::WebConfig::for_local_dev_with_secret([7; 32]),
    );
    let app = ghinvite_web::build_app(state, tower_sessions::MemoryStore::default());
    let login = request(&app, "", "/login", None).await;
    let state = login.headers()["location"]
        .to_str()
        .unwrap()
        .split("state=")
        .nth(1)
        .unwrap()
        .split('&')
        .next()
        .unwrap();
    let signed_in = request(
        &app,
        &cookie(&login),
        &format!("/oauth/callback?code=fixture&state={state}"),
        None,
    )
    .await;
    assert_eq!(signed_in.status(), StatusCode::SEE_OTHER);
    let cookie = cookie(&signed_in);
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
    let page = html(request(&app, &cookie, &public.to_lowercase(), None).await).await;
    assert!(page.contains("acme/api"));
    let operation = field(&page, "operation_id");
    let submit = format!("csrf_token={csrf}&operation_id={operation}&justification=++Original++");
    for path in [&public.to_lowercase(), &public] {
        let admitted = request(&app, &cookie, path, Some(&submit)).await;
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
            &cookie,
            &format!("{public}?operation_id={operation}"),
            None,
        )
        .await,
    )
    .await;
    assert!(status.contains("Request accepted at"));
    assert!(status.contains("Awaiting review"));
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
