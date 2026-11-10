//! Runtime-tests-only entry points exercising the production token context
//! (`GithubApps` over octotoken's `AppTokenProvider`) in workerd. The context
//! is built per incoming request, as the web Worker builds it; its providers
//! are isolate-scope (PD-4).
//! This PEM is the committed public test fixture, never a deployment credential.
use axum::response::IntoResponse;
use futures_util::future::{Either, join_all, select};
use ghinvite_github::{
    AppSlot, CallError, GithubApp, GithubApps, HttpTransport, InstalledApp,
    profiles::CONSOLE_REPO_READ,
    rest::{PathTemplate, RestEnv},
    transport::{Method, Request},
    worker::WorkerFetchTransport,
};
use std::{convert::Infallible, sync::Arc, time::UNIX_EPOCH};
use worker::HttpRequest;

/// Where the fixture app creates tokens and calls the API.
const BASE: &str = "https://broker.test";

/// A new `GithubApps` for this request, over the isolate's token cache.
fn app() -> Result<GithubApp, String> {
    let key =
        octoauth::SigningKey::from_pem(include_str!("../../../test-fixtures/github-app-rsa.pem"))
            .map_err(|_| "fixture key")?;
    let app = octoauth::App::builder(99001, key)
        .client_id("Iv-runtime-broker")
        .user_agent("runtime-broker-fixture")
        .base_url(BASE)
        .build()
        .map_err(|_| "fixture app")?;
    let env = RestEnv::new(Arc::new(WorkerFetchTransport::new()), BASE, "runtime")
        .map_err(|e| e.code().to_owned())?;
    let installed = InstalledApp {
        slot: AppSlot::Repo,
        app,
        user_agent: "runtime-broker-fixture".into(),
    };
    GithubApps::new(env, [installed])
        .map_err(|e| e.code().to_owned())?
        .app(AppSlot::Repo)
        .map_err(|e| e.to_string())
}
/// Drop a token creation once the outbound control request answers.
async fn cancel_creation(app: &GithubApp, installation: u64) -> Result<(), String> {
    let transport = WorkerFetchTransport::new();
    let control = transport.send(Request::new(
        Method::Get,
        format!("{BASE}/control/cancel/{installation}"),
    ));
    match select(
        Box::pin(app.ensure(installation, &CONSOLE_REPO_READ)),
        Box::pin(control),
    )
    .await
    {
        Either::Right((Ok(response), pending)) if response.status == 200 => {
            drop(pending);
            Ok(())
        }
        Either::Right((_error, pending)) => {
            drop(pending);
            Err("control failed".into())
        }
        Either::Left((_completed, pending)) => {
            drop(pending);
            Err("token creation completed before cancellation".into())
        }
    }
}
pub async fn fetch(req: HttpRequest) -> worker::Result<axum::response::Response> {
    let result = run(req).await;
    Ok(match result {
        Ok(value) => axum::Json(value).into_response(),
        Err(error) => (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            axum::Json(serde_json::json!({"error":error})),
        )
            .into_response(),
    })
}
async fn run(req: HttpRequest) -> Result<serde_json::Value, String> {
    let case = req.uri().path().trim_start_matches("/__fixture/broker/");
    let installation = req
        .uri()
        .query()
        .and_then(|query| query.strip_prefix("installation="))
        .and_then(|id| id.parse::<u64>().ok())
        .ok_or("invalid installation")?;
    let scope = &*CONSOLE_REPO_READ;
    let app = app()?;
    match case {
        // Concurrent misses in one request: on Workers each may create its
        // own token (no coalescing); all must succeed.
        "coalesce" => {
            for result in join_all((0..20).map(|_| app.ensure(installation, scope))).await {
                result.map_err(|e| e.code().to_owned())?;
            }
        }
        "observed-read" => {
            app.read(installation, scope, |handle| async move {
                handle
                    .rest()
                    .call::<(), serde_json::Value>(
                        Method::Get,
                        PathTemplate::new("/installation/repositories"),
                        &[],
                        None,
                    )
                    .await
                    .map_err(CallError::<Infallible>::Api)?;
                Ok::<_, CallError<Infallible>>(())
            })
            .await
            .map_err(|error| error.code().to_owned())?;
        }
        "ensure" => app
            .ensure(installation, scope)
            .await
            .map_err(|e| e.code().to_owned())?,
        "cancel" => {
            cancel_creation(&app, installation).await?;
            return Ok(serde_json::json!({"cancelled":true}));
        }
        "cancel-late" => {
            cancel_creation(&app, installation).await?;
            app.ensure(installation, scope)
                .await
                .map_err(|e| e.code().to_owned())?;
        }
        _ => return Err("unknown broker fixture".into()),
    }
    // The token a later read would use, as seconds; never the secret.
    let expires_at = app
        .read(installation, scope, |handle| async move {
            Ok::<_, CallError<Infallible>>(handle.credential().expires_at())
        })
        .await
        .map_err(|e| e.code().to_owned())?
        .duration_since(UNIX_EPOCH)
        .map_err(|_| "expiry before epoch")?
        .as_secs();
    Ok(serde_json::json!({"ok":true,"expires_at":expires_at}))
}
