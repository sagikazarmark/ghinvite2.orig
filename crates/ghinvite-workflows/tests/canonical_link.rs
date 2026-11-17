//! Authenticated native web → real Restate owners → the GitHub emulator, with
//! projection deliberately unavailable until after creation and admission replay.
#![cfg(feature = "integration")]

#[path = "common/web_apps.rs"]
mod web_apps;

use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
    response::Response,
};
use ghinvite_core::storage::projection::{ProjectionEnvelope, ProjectionStorage};
use ghinvite_core::storage::{AppInstallationReadStorage, ConsoleStorage, RecordStorage};
use serde_json::json;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;
use tower::ServiceExt;

struct DelayedProjection {
    storage: Arc<ghinvite_storage_sqlx::SqlxStorage>,
    delayed: AtomicBool,
}
#[async_trait::async_trait]
impl ProjectionStorage for DelayedProjection {
    async fn apply_reissue(
        &self,
        envelope: &ghinvite_core::reissue_projection::RequestReissueProjection,
    ) -> ghinvite_core::storage::Result<()> {
        if self.delayed.load(Ordering::SeqCst) {
            return Err(ghinvite_core::storage::Error::ProjectionDependency);
        }
        self.storage.apply_reissue(envelope).await
    }

    async fn apply_revocation(
        &self,
        envelope: &ghinvite_core::revocation::RevocationProjection,
    ) -> ghinvite_core::storage::Result<()> {
        if self.delayed.load(Ordering::SeqCst) {
            return Err(ghinvite_core::storage::Error::ProjectionDependency);
        }
        self.storage.apply_revocation(envelope).await
    }

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

fn revealed_secret(page: &str) -> String {
    page.split("/i#")
        .nth(1)
        .expect("show-once invitation URL")
        .chars()
        .take(43)
        .collect()
}
async fn select_secret(app: &axum::Router, cookie: &str, secret: &str) {
    let bootstrap = request(app, cookie, "/i", None).await;
    assert_eq!(bootstrap.status(), StatusCode::OK);
    let csrf = field(&html(bootstrap).await, "csrf_token");
    let response = request(
        app,
        cookie,
        "/i",
        Some(&format!("csrf_token={csrf}&secret={secret}")),
    )
    .await;
    assert_eq!(
        response.status(),
        StatusCode::SEE_OTHER,
        "{}",
        html(response).await
    );
    assert_eq!(response.headers()["location"], "/join");
}

#[tokio::test]
async fn authenticated_journey_settles_and_recovers_across_projection_and_response_loss() {
    let requester_login = Arc::new(AtomicBool::new(false));
    let login_identity = requester_login.clone();
    // GitHub: the suites' emulator world. The sign-in side stays canned
    // (labelled fault rules): the step switches the signed-in user under one
    // session, and `/user/installations/{id}/repositories` is outside the
    // emulator's routes. `crates/ghinvite-web/tests/emulator_signin.rs` and
    // the browser suite cover the real OAuth flow on the emulator.
    let harness = ghinvite_e2e::Harness::start().await;
    {
        use octoemu::faults::{Fault, FaultAction};
        let emulator = harness.emulator();
        let canned = [
            Fault::on(
                "POST",
                "/login/oauth/access_token",
                FaultAction::respond_json(
                    200,
                    json!({"access_token":"fixture", "token_type":"bearer", "scope":""}),
                ),
            ),
            Fault::on(
                "DELETE",
                format!("/applications/{}/token", web_apps::SIGNIN_CLIENT_ID),
                FaultAction::respond(204),
            ),
            Fault::on(
                "GET",
                "/user",
                FaultAction::custom(move |_| {
                    octoemu::faults::Reply::new(200).json(
                        if login_identity.load(Ordering::SeqCst) {
                            json!({"id":8,"login":"alice"})
                        } else {
                            json!({"id":7,"login":"creator"})
                        },
                    )
                }),
            ),
            Fault::on(
                "GET",
                "/user/memberships/orgs/acme",
                FaultAction::respond_json(
                    200,
                    json!({"role":"admin","state":"active","organization":{"id":100}}),
                ),
            ),
            Fault::on(
                "GET",
                "/user/installations/1/repositories",
                FaultAction::respond_json(
                    200,
                    json!({"total_count":2,"repositories":[{"id":10,"full_name":"acme/api","private":true},{"id":11,"full_name":"acme/web","private":true}]}),
                ),
            ),
        ];
        for fault in canned {
            emulator.add_fault(fault.label("sign-in fixture")).unwrap();
        }
    }
    let ingress = harness.restate.ingress_url().to_owned();
    let admin = harness.restate.admin_url().to_owned();
    let client = harness.client.clone();
    let base = harness.github_url.clone();
    let storage = Arc::new(
        ghinvite_storage_sqlx::SqlxStorage::in_memory()
            .await
            .unwrap(),
    );
    let projection = Arc::new(DelayedProjection {
        storage: storage.clone(),
        delayed: AtomicBool::new(true),
    });
    let endpoint = ghinvite_workflows::build_endpoint(
        ghinvite_workflows::AppState::new(storage.clone()).with_github_apps(Arc::new(
            ghinvite_workflows::RepositoryGithub::new(
                ghinvite_e2e::github_transport(client.clone()),
                &base,
                ghinvite_e2e::repository_apps(&base),
            )
            .unwrap(),
        )),
        projection.clone(),
        None,
    )
    .unwrap();
    harness.restate.deploy(endpoint).await;
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
    let setup = ghinvite_e2e::setup_command(
        json!({"app":"repo","installation_id":1,"actor_user_id":7,"account_id":100,"account_login":"acme", "account_type":"Organization","selected_repos":"all","installed_at":chrono::Utc::now()}),
    );
    let onboard = client
        .post(format!("{ingress}/Installation/repo:1/onboard"))
        .json(&setup)
        .send()
        .await
        .unwrap();
    assert!(onboard.status().is_success());
    client
        .post(format!("{ingress}/Installation/repo:1/onboard_webhook"))
        .json(&ghinvite_e2e::signed_owner_command(&setup, 7))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(20), async {
        while storage
            .app_installation(ghinvite_core::account::App::Repo, 1)
            .await
            .unwrap()
            .is_none_or(|installation| installation.installed_by_user_id != Some(7))
        {
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
    // Installation reads create tokens at and call the emulator.
    let github_transport = ghinvite_e2e::github_transport(client.clone());
    let web_github = ghinvite_web::WebGithub {
        apps: ghinvite_github::GithubApps::new(
            ghinvite_github::rest::RestEnv::new(github_transport.clone(), &base, "p1").unwrap(),
            ghinvite_e2e::installed_apps(&base, true),
        )
        .unwrap(),
        transport: github_transport,
    };
    let state = ghinvite_web::AppState::new(
        storage.clone(),
        web_github,
        Arc::new(ghinvite_web::RestateClient::new(proxy_url).unwrap()),
        ghinvite_web::WebConfig {
            webhooks: Some(web_apps::webhooks("journey-secret")),
            // Sign-in calls the emulator, too (canned rules above).
            signin_apps: Some(web_apps::signin_app(&base)),
            ..ghinvite_web::WebConfig::for_local_dev_with_secret([7; 32])
        },
    );
    let state = state
        .with_authority_cache(Arc::new(
            ghinvite_web::authority_cache::MemoryAuthorityCache::default(),
        ))
        .with_edge_limits(Arc::new(
            ghinvite_web::rate_limit::MemoryRateLimiter::default(),
        ));
    let app = ghinvite_web::build_app(state, tower_sessions::MemoryStore::default()).layer(
        axum::Extension(ghinvite_web::rate_limit::TrustedClientIp::new(
            std::net::Ipv4Addr::LOCALHOST.into(),
        )),
    );
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
        "csrf_token={csrf}&description=Workshop&permission=pull&repo_ids=10&repo_grants=10:pull&approval_required=on&expires_in_days=2"
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
    assert!(!page.contains(&format!("/i/{id}")));
    let rotated = request(
        &app,
        &cookie,
        &format!("{detail}/rotate"),
        Some(&format!(
            "csrf_token={csrf}&operation_id={}",
            ghinvite_core::RequestId::new()
        )),
    )
    .await;
    assert_eq!(rotated.status(), StatusCode::OK, "{}", html(rotated).await);
    let secret = revealed_secret(&html(rotated).await);
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
    let public = "/join".to_owned();
    let capability_hash = snapshot["secret"]["sha256"].clone();
    requester_login.store(true, Ordering::SeqCst);
    let requester_cookie = sign_in(&app).await;
    assert_eq!(
        request(&app, &requester_cookie, &format!("/i/{id}"), None)
            .await
            .status(),
        StatusCode::NOT_FOUND
    );
    select_secret(&app, &requester_cookie, &secret).await;
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
        .json(&json!({"secret_sha256":capability_hash,"secret_generation":2,"link_id":id,"requester_id":8,"operation_id":operation}))
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
            if harness.github().writes() == 1 {
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
    let delivery_path = format!("RepositoryDelivery/{request_id}:10:1/status");
    let delivered = owner(&client, &ingress, &delivery_path, json!(null)).await;
    assert_eq!(delivered["create"]["outcome"]["kind"], "created");
    // Read-only settlement evidence: GitHub reports current numeric identity/access,
    // and the complete pending list no longer contains the known invitation.
    let upstream = delivered["create"]["outcome"]["upstream_id"]
        .as_u64()
        .unwrap();
    // The invitee declines, and reads the repositories anyway (a direct
    // read collaborator).
    harness.github().decline("alice", upstream).await;
    harness
        .github()
        .collaborate("alice", octoemu::seed::Role::Pull)
        .await;
    tokio::time::timeout(Duration::from_secs(25), async {
        loop {
            if storage
                .get_delivery(&format!(
                    "{}:{}:{}",
                    command["request_id"].as_str().unwrap(),
                    command["repo_id"],
                    command["delivery_generation"]
                ))
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
                    .uri("/webhooks/repo")
                    .header("content-type", "application/json")
                    .header("x-github-event", "member")
                    .header("x-github-delivery", "journey-member")
                    .header("x-hub-signature-256", format!("sha256={signature}"))
                    .body(Body::from(payload.clone()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::ACCEPTED);
    }
    // The signed member event is ignored; the observer verifies current access.
    assert!(
        client
            .post(format!("{ingress}/DeliveryObservation/observe"))
            .json(&delivered)
            .send()
            .await
            .unwrap()
            .status()
            .is_success()
    );
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
    .expect("requester status and admin audit converge after verified read evidence");
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
            let rows = owner(&client, &admin, "query", json!({"query":format!("SELECT id FROM sys_invocation WHERE target_service_name = 'DeliveryProjection' AND target_service_key = '{request_id}:10:1' AND status != 'completed'")})).await;
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
    assert_eq!(harness.github().writes(), 1);
    harness.github().revoke("alice").await;
    // The same authenticated routes cover fresh admission after decline and
    // admission-time auto approval. Keep the deadline/race matrix in request_owner.
    for auto in [false, true] {
        // Independent delivery scenarios need independent upstream invitations;
        // the typed protocol correctly tracks a previous scenario's pending invite.
        harness.github().reset().await;
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
            "csrf_token={csrf}&description=Two+repositories&permission=admin&repo_ids=10&repo_ids=11&repo_grants=10:pull&repo_grants=11:push{}",
            if auto { "" } else { "&approval_required=on" }
        );
        let created = request(&app, &cookie, &action, Some(&body)).await;
        assert_eq!(created.status(), StatusCode::OK, "{}", html(created).await);
        let secret = revealed_secret(&html(created).await);
        select_secret(&app, &requester_cookie, &secret).await;
        let capability = owner(
            &client,
            &ingress,
            &format!("InvitationLink/{link_id}/link_status"),
            json!({"link_id":link_id,"admin":{"account_id":100,"user_id":7}}),
        )
        .await;
        let capability_hash = capability["secret"]["sha256"].clone();
        assert_eq!(
            capability["creation"]["targets"]["repos"][0]["grant"],
            "read"
        );
        assert_eq!(
            capability["creation"]["targets"]["repos"][1]["grant"],
            "write"
        );
        let public = "/join".to_owned();
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
            json!({"secret_sha256":capability_hash,"secret_generation":1,"link_id":link_id,"requester_id":8,"operation_id":operation}),
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
                json!({"secret_sha256":capability_hash,"secret_generation":1,"link_id":link_id,"requester_id":8,"operation_id":operation}),
            )
            .await;
            let fresh = page["request"]["request_id"].as_str().unwrap();
            assert_ne!(fresh, request_id);
            request_id = fresh.to_owned();
            harness
                .github()
                .outcome(
                    "acme/web",
                    "alice",
                    ghinvite_e2e::github::Outcome::AccessLostOnce,
                )
                .await;
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
        for command in plan["commands"].as_array().unwrap() {
            assert_eq!(
                command["permission"],
                if command["repo_id"] == 10 {
                    "pull"
                } else {
                    "push"
                }
            );
        }
        tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                let api = owner(
                    &client,
                    &ingress,
                    &format!("RepositoryDelivery/{request_id}:10:1/status"),
                    json!(null),
                )
                .await;
                let web = owner(
                    &client,
                    &ingress,
                    &format!("RepositoryDelivery/{request_id}:11:1/status"),
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
                        &format!("RepositoryDelivery/{request_id}:11:1/status"),
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
        for (path, permission) in [
            ("/repos/acme/api/collaborators/alice", "pull"),
            ("/repos/acme/web/collaborators/alice", "push"),
        ] {
            let writes = harness.github().write_permissions(path);
            assert!(!writes.is_empty());
            assert!(
                writes
                    .iter()
                    .all(|asked| asked.as_deref() == Some(permission)),
                "each repository keeps its own grant through approval and recovery"
            );
        }
    }
    proxy_task.abort();
    harness.finish().await;
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
