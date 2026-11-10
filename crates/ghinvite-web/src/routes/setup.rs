//! App-scoped setup callback; identity comes from an app-JWT verified read.
use crate::{
    error::{Result, WebError},
    installed_apps, session,
    state::AppState,
};
use axum::{
    Router,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Redirect, Response},
    routing::get,
};
use ghinvite_core::{
    account::{InstallationRef, OnboardInput},
    admission::AdmissionOperationId,
};
use serde::Deserialize;
use tower_sessions::Session as TowerSession;

pub fn router() -> Router<AppState> {
    Router::new().route("/setup/{app}", get(handle_setup))
}
#[derive(Deserialize)]
struct SetupQuery {
    installation_id: Option<u64>,
    setup_action: Option<String>,
}
async fn handle_setup(
    State(state): State<AppState>,
    Path(name): Path<String>,
    tower: TowerSession,
    Query(query): Query<SetupQuery>,
) -> Result<Response> {
    let app = installed_apps::app(&name).ok_or(WebError::NotFound)?;
    if state.github_apps.slots().next().is_none() {
        return Ok(StatusCode::SERVICE_UNAVAILABLE.into_response());
    }
    if installed_apps::configured(&state, app).is_none() {
        return Err(WebError::NotFound);
    }
    if query.setup_action.as_deref() == Some("request") {
        // Browser callback is a hint, never proof of a pending request or grant.
        // The listing authenticates the viewer and binds numeric requester ID.
        return Ok(
            Redirect::to(&format!("/onboarding/requests?app={}", app.as_str())).into_response(),
        );
    }
    let installation_id = query
        .installation_id
        .ok_or_else(|| WebError::BadRequest("missing installation_id".into()))?;
    InstallationRef {
        app,
        installation_id,
    }
    .key()
    .map_err(|_| WebError::BadRequest("invalid installation identity".into()))?;
    let action = if query.setup_action.as_deref() == Some("update") {
        "update"
    } else {
        "install"
    };
    let session = session::load(&tower)
        .await
        .map_err(|e| WebError::Session(e.to_string()))?;
    if !session.is_authenticated() {
        let return_to = format!(
            "/setup/{}?installation_id={installation_id}&setup_action={action}",
            app.as_str()
        );
        let encoded: String = url::form_urlencoded::byte_serialize(return_to.as_bytes()).collect();
        return Ok(Redirect::to(&format!("/login?return_to={encoded}")).into_response());
    }
    let verified = match installed_apps::verify(&state, app, installation_id).await {
        Ok(facts) => facts,
        Err(status) => {
            if status == StatusCode::NOT_FOUND {
                return Err(WebError::NotFound);
            }
            return Ok((status, axum::response::Html(r#"<!doctype html><html lang="en"><head><meta charset="utf-8"><title>Setup unavailable</title></head><body><main><h1>Setup temporarily unavailable</h1><p>We could not verify the installation. Please try again.</p><a href="/">Go home</a></main></body></html>"#.to_owned())).into_response());
        }
    };
    let account_login = verified.account.account_login.clone();
    // The same verified command handles install/update, and deliberately cannot
    // carry the signed-in viewer as an installer or confer Console authority.
    state
        .commands
        .onboard_installation(OnboardInput {
            operation_id: AdmissionOperationId::try_from(
                ghinvite_core::RequestId::new().to_string(),
            )
            .expect("generated operation ID"),
            verified,
        })
        .await?;
    Ok(Redirect::to(&format!("/console/accounts/{account_login}")).into_response())
}
