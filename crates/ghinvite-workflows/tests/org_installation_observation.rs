//! Actual Restate + HTTP proof for app-specific organization scope observation.
#![cfg(feature = "integration")]
use axum::{
    Json,
    extract::State,
    http::{Method, StatusCode},
    response::IntoResponse,
};
use ghinvite_e2e::{Call, Harness};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
#[derive(Default)]
struct Github {
    mode: String,
    calls: Vec<(String, String)>,
    mints: Vec<(u64, Value)>,
}
async fn github(
    State(state): State<Arc<Mutex<Github>>>,
    request: axum::extract::Request,
) -> axum::response::Response {
    let method = request.method().clone();
    let path = request.uri().path().to_owned();
    let authorization = request
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    let body = axum::body::to_bytes(request.into_body(), 8192)
        .await
        .unwrap();
    let mut state = state.lock().unwrap();
    state.calls.push((method.to_string(), path.clone()));
    if method == Method::GET && (path == "/app/installations/1" || path == "/app/installations/2") {
        assert!(authorization.starts_with("Bearer ey"));
        let org = path.ends_with('2');
        let id = if org { 2 } else { 1 };
        if org && state.mode == "missing" {
            return StatusCode::NOT_FOUND.into_response();
        }
        let suspended = if org && state.mode == "suspended" {
            json!("2026-01-01T00:00:00Z")
        } else {
            Value::Null
        };
        return Json(json!({"id":id,"app_id":if org {456}else{123},"account":{"id":if org && state.mode=="account_mismatch" {999}else{100},"login":"acme","type":"Organization"},"created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T00:00:00Z","suspended_at":suspended,"repository_selection":"all","permissions":if org {json!({"members":"write","metadata":"read"})}else{json!({"administration":"write","metadata":"read"})}})).into_response();
    }
    if method == Method::POST && path.ends_with("/access_tokens") {
        let id = if path == "/app/installations/2/access_tokens" {
            2
        } else {
            assert_eq!(path, "/app/installations/1/access_tokens");
            1
        };
        let body: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            body,
            if id == 2 {
                json!({"permissions":{"members":"read","metadata":"read"}})
            } else {
                json!({"permissions":{"administration":"read","metadata":"read"}})
            }
        );
        state.mints.push((id, body));
        return (StatusCode::CREATED,Json(json!({"token":if id==2 {"ghs_org_scoped"}else{"ghs_repo_scoped"},"expires_at":"2099-01-01T00:00:00Z","permissions":if id==2 {json!({"members":"read","metadata":"read"})}else{json!({"administration":"read","metadata":"read"})}}))).into_response();
    }
    if method == Method::GET && path == "/orgs/acme" {
        assert_eq!(authorization, "Bearer ghs_org_scoped");
        if state.mode == "upstream_failure" {
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
        return Json(json!({"id":if state.mode=="org_mismatch" {999}else{100},"login":"acme","created_at":"2020-01-01T00:00:00Z"})).into_response();
    }
    if method == Method::GET && path == "/installation/repositories" {
        assert_eq!(authorization, "Bearer ghs_repo_scoped");
        return Json(json!({"total_count":1,"repositories":[{"id":10,"full_name":"acme/api","name":"api","owner":{"id":100,"login":"acme"},"private":true}]})).into_response();
    }
    panic!("unexpected {method} {path}");
}
async fn invoke(
    h: &Harness,
    service: &str,
    key: &str,
    handler: &str,
    input: Option<&Value>,
) -> Value {
    let result = h
        .restate
        .invoke(&Call::object(service, key, handler), input, None)
        .await;
    assert_eq!(result.status, 200, "{handler}: {}", result.body);
    result.body
}
#[tokio::test]
async fn organization_scope_has_its_own_read_profile_and_fail_closed_observations() {
    let github_state = Arc::new(Mutex::new(Github::default()));
    let h = Harness::start_with_github_router(
        axum::Router::new()
            .fallback(github)
            .with_state(github_state.clone()),
    )
    .await;
    let storage = Arc::new(
        ghinvite_storage_sqlx::SqlxStorage::in_memory()
            .await
            .unwrap(),
    );
    let github_transport = ghinvite_e2e::github_transport(h.client.clone());
    let state = ghinvite_workflows::AppState::new(storage).with_github_apps(Arc::new(
        ghinvite_workflows::RepositoryGithub::new(
            github_transport,
            &h.github_url,
            ghinvite_e2e::installed_apps(&h.github_url, true),
        )
        .unwrap(),
    ));
    h.restate
        .deploy(
            restate_sdk::endpoint::Endpoint::builder()
                .bind(ghinvite_workflows::sweep_schedule::SweepSchedule::default())
                .bind(ghinvite_workflows::installation::Installation {
                    state: state.clone(),
                })
                .bind(ghinvite_workflows::availability::AccountInstallation {
                    state: state.clone(),
                })
                .bind(ghinvite_workflows::availability::InstallationProjection { state })
                .build(),
        )
        .await;
    for (app, id) in [("repo", 1), ("org", 2)] {
        let input = json!({"operation_id":ghinvite_core::RequestId::new(),"verified":{"app":app,"account":{"installation_id":id,"account_id":100,"account_login":"acme","account_type":"Organization","selected_repos":[],"installed_at":"2026-01-01T00:00:00Z","uninstalled_at":null},"suspended_at":null,"permissions":{}}});
        invoke(
            &h,
            "Installation",
            &format!("{app}:{id}"),
            "onboard",
            Some(&input),
        )
        .await;
        invoke(
            &h,
            "AccountInstallation",
            &format!("{app}:100"),
            "refresh",
            Some(&json!(id)),
        )
        .await;
    }
    let org = invoke(&h, "AccountInstallation", "org:100", "status", None).await;
    let repo = invoke(&h, "AccountInstallation", "repo:100", "status", None).await;
    assert_eq!(
        org["observation"],
        json!({"kind":"available","repo_ids":[]})
    );
    assert_eq!(
        repo["observation"],
        json!({"kind":"available","repo_ids":[10]})
    );
    assert_eq!(org["installation"]["app"], "org");
    assert_eq!(repo["installation"]["app"], "repo");
    let initial_mints = github_state.lock().unwrap().mints.clone();
    assert!(initial_mints.iter().any(|(id, _)| *id == 1));
    assert!(initial_mints.iter().any(|(id, _)| *id == 2));
    for (mode, expected) in [
        ("suspended", "unavailable"),
        ("missing", "unavailable"),
        ("account_mismatch", "unavailable"),
        ("org_mismatch", "unavailable"),
        ("upstream_failure", "unknown"),
    ] {
        github_state.lock().unwrap().mode = mode.into();
        invoke(
            &h,
            "AccountInstallation",
            "org:100",
            "refresh",
            Some(&json!(2)),
        )
        .await;
        let status = invoke(&h, "AccountInstallation", "org:100", "status", None).await;
        assert_eq!(status["observation"]["kind"], expected, "{mode}");
        assert_eq!(
            invoke(&h, "AccountInstallation", "repo:100", "status", None).await["observation"],
            repo["observation"]
        );
    }
    github_state.lock().unwrap().mode.clear();
    invoke(
        &h,
        "AccountInstallation",
        "org:100",
        "refresh",
        Some(&json!(2)),
    )
    .await;
    assert_eq!(
        invoke(&h, "AccountInstallation", "org:100", "status", None).await["observation"]["kind"],
        "available"
    );
    invoke(
        &h,
        "AccountInstallation",
        "org:100",
        "uninstall",
        Some(&json!({"app":"org","installation_id":2,"uninstalled_at":chrono::Utc::now()})),
    )
    .await;
    let before = github_state.lock().unwrap().calls.len();
    invoke(&h, "AccountInstallation", "org:100", "recheck", None).await;
    assert_eq!(github_state.lock().unwrap().calls.len(), before);
    assert_eq!(
        invoke(&h, "AccountInstallation", "org:100", "status", None).await["observation"]["kind"],
        "unavailable"
    );
}
