//! Test-only access to production HTTP client boundaries inside workerd.
use ghinvite_github::{
    HttpTransport,
    transport::{Method, Request},
    worker::WorkerFetchTransport,
};
use worker::{Env, HttpRequest};
#[path = "pending_signin_fixture.rs"]
mod pending_signin;
#[path = "session_cutover_fixture.rs"]
mod session_cutover;

pub async fn fetch(req: HttpRequest, env: &Env) -> worker::Result<axum::response::Response> {
    if req.uri().path() == "/__fixture/session-cutover-inspect" {
        return session_cutover::fetch(req, env).await;
    }
    let method = req.uri().path().trim_start_matches("/__fixture/");
    if method == "pending-signin-replace"
        || method == "pending-signin-take"
        || method == "pending-signin-callback"
        || method == "pending-signin-purpose"
    {
        return pending_signin::fetch(req, env).await;
    }
    let result = if method == "telemetry-admit" {
        // Actual production authority/timing boundary; fixed private test inputs.
        let client =
            ghinvite_web::RestateClient::new(env.var("GHINVITE_RESTATE_INGRESS")?.to_string())
                .map_err(super::worker_err)?;
        let authority =
            ghinvite_web::link_authority::LinkAuthority::new(std::sync::Arc::new(client));
        authority
            .admit(ghinvite_core::admission::Admit {
                link_id: "01ARZ3NDEKTSV4RRFFQ69G5FAV".parse().unwrap(),
                operation_id: "01ARZ3NDEKTSV4RRFFQ69G5FAW".to_owned().try_into().unwrap(),
                requester_id: 91919191,
                justification: Some("PRIVATE_JUSTIFICATION".into()),
                secret_sha256: [42; 32].into(),
                secret_generation: 7,
                domain_evidence: None,
                scope_evidence: vec![],
            })
            .await
            .map(|_| serde_json::json!({"ok":true}))
            .map_err(|_| "bounded admission refusal".to_owned())
    } else if method == "authority-cache" {
        use ghinvite_web::authority_cache::AuthorityCache;
        match super::authority_cache::KvAuthorityCache::from_env(env) {
            Ok(cache) => cache
                .get(req.uri().query().unwrap_or("authz:repo:1:2:3"))
                .await
                .map(|value| serde_json::json!(value))
                .map_err(|_| "authority cache unavailable".to_owned()),
            Err(_) => Err("authority cache unavailable".to_owned()),
        }
    } else if method == "authority-cache-write" {
        use ghinvite_web::authority_cache::{AuthorityCache, CacheEntry};
        match super::authority_cache::KvAuthorityCache::from_env(env) {
            Ok(cache) => cache
                .put(
                    "authz:repo:1:2:3",
                    CacheEntry {
                        observed_at: chrono::Utc::now().timestamp(),
                        value: serde_json::json!({"allowed":true}),
                    },
                )
                .await
                .map(|()| serde_json::json!({"ok":true}))
                .map_err(|_| "authority cache unavailable".to_owned()),
            Err(_) => Err("authority cache unavailable".to_owned()),
        }
    } else if method == "continuation-rotation" {
        let storage = ghinvite_storage_d1::D1Storage::new(env.d1("DB")?);
        ghinvite_web::continuation_rotation::verify_runtime(&storage)
            .await
            .map(|()| serde_json::json!({"ok":true}))
            .map_err(|_| "continuation rotation verification failed".to_owned())
    } else if method == "continuation-envelope" {
        let storage = ghinvite_storage_d1::D1Storage::new(env.d1("DB")?);
        let config = super::config_from_env(env)?;
        ghinvite_web::verify_continuation_envelope(&storage, config.keyring)
            .await
            .map(|()| serde_json::json!({"ok": true}))
            .map_err(|_| "continuation envelope verification failed".to_owned())
    } else if let Some(case) = method.strip_prefix("github-") {
        let path = if case == "waker" { "headers" } else { case };
        let request = Request::new(Method::Put, format!("https://transport.test/{path}"))
            .header("authorization", format!("Bearer ghs_{}", "x".repeat(640)))
            .timeout(std::time::Duration::from_millis(200));
        let transport = WorkerFetchTransport::new();
        let response = if case == "waker" {
            use std::future::Future;
            let mut future = Box::pin(transport.send(request));
            let mut first = true;
            std::future::poll_fn(|cx| {
                if first {
                    first = false;
                    // A runtime may replace the task polling a pending future.
                    let mut previous = std::task::Context::from_waker(std::task::Waker::noop());
                    assert!(future.as_mut().poll(&mut previous).is_pending());
                }
                future.as_mut().poll(cx)
            })
            .await
        } else {
            transport.send(request).await
        };
        response
            .map(|response| serde_json::json!({"status": response.status, "bytes": response.body.len()}))
            .map_err(|error| format!("{error:?}"))
    } else if method == "github" {
        WorkerFetchTransport::new()
            .send(Request::new(Method::Put, "https://transport.test/github"))
            .await
            .map(|response| serde_json::json!({"status": response.status}))
            .map_err(|error| error.to_string())
    } else {
        let client =
            ghinvite_web::RestateClient::new(env.var("GHINVITE_RESTATE_INGRESS")?.to_string())
                .map_err(super::worker_err)?;
        let input =
            serde_json::json!({"operation_id": req.uri().query().unwrap_or("same-attempt")});
        match method {
            "send" => client
                .send("Fixture", "key", "mutate", &input)
                .await
                .map(|()| serde_json::Value::Null),
            "call" => client.call("Fixture", "key", "mutate", &input).await,
            _ => return Err(super::worker_err("unknown fixture operation")),
        }
        .map_err(|error| error.to_string())
    };
    use axum::response::IntoResponse;
    Ok(match result {
        Ok(value) => axum::Json(value).into_response(),
        Err(error) => (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            axum::Json(serde_json::json!({"error": error})),
        )
            .into_response(),
    })
}
