//! Fragment capabilities are exchanged only in CSRF-protected request bodies.
//! The browser session retains a hash and generation; `/join` has no bearer URL.

use crate::render::render_with_csrf as render;
use crate::session;
use crate::state::AppState;
use axum::Router;
use axum::extract::State;
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{any, get};
use dioxus::prelude::*;
use tower_sessions::Session as TowerSession;

mod attempt;
mod domains;
mod offer;
mod organization_refresh;
mod request_status;

#[derive(serde::Deserialize)]
struct SubmitForm {
    #[serde(default)]
    operation_id: String,
    #[serde(default)]
    justification: Option<String>,
}

#[derive(serde::Deserialize)]
struct SecretForm {
    secret: String,
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/i", get(bootstrap).post(select_invitation))
        .route("/code", get(manual_code).post(select_invitation))
        .route("/join", get(invitation_page).post(submit_request))
        .route("/requests/{request_id}", get(request_status::show))
        .route(
            "/requests/{request_id}/refresh",
            axum::routing::post(request_status::refresh),
        )
        .route(
            "/join/organization/refresh",
            axum::routing::post(organization_refresh::refresh),
        )
        .route("/i/{slug}", any(unknown_invitation))
        .route("/i/{slug}/{*rest}", any(unknown_invitation))
}

pub(super) fn invitation_not_found_response(session: &session::Session) -> Response {
    let signed_in_login = session.is_authenticated().then(|| session.login.clone());
    let html = render(session.csrf_token.clone(), move || {
        rsx! {
            ghinvite_ui::not_found::InvitationNotFoundPage {
                signed_in_login: signed_in_login.clone(),
            }
        }
    });
    (axum::http::StatusCode::NOT_FOUND, Html(html)).into_response()
}

async fn entry_page(tower: TowerSession, bootstrap: bool) -> Response {
    let mut session = match session::load(&tower).await {
        Ok(session) => session,
        Err(error) => return crate::WebError::Session(error.to_string()).into_response(),
    };
    if session.csrf_token.is_none() {
        session.csrf_token = Some(crate::middleware::csrf::generate_token());
        if let Err(error) = session::save(&tower, &session).await {
            return crate::WebError::Session(error.to_string()).into_response();
        }
    }
    // This page has no application shell or other scripts. The synchronous
    // head script erases the fragment before DOM parsing loads any resources.
    let csrf = session.csrf_token.as_deref().unwrap_or_default();
    let script = if bootstrap {
        "<script src=\"/static/i.js\"></script>"
    } else {
        ""
    };
    let action = if bootstrap { "/i" } else { "/code" };
    let html = format!(
        r#"<!DOCTYPE html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1"/><meta name="referrer" content="no-referrer"><meta name="robots" content="noindex,nofollow"><title>Open invitation · ghinvite</title>{script}</head><body><main><h1>Open invitation</h1><form id="invitation-secret-form" method="post" action="{action}"><input type="hidden" name="csrf_token" value="{csrf}"><label>Invitation code <input type="text" name="secret" autocomplete="off" spellcheck="false" maxlength="43" required></label><button type="submit">Open invitation</button></form><p>If the invitation does not open, <a href="/code">enter its code manually</a>. The code is the part after # in your invitation URL.</p></main></body></html>"#
    );
    Html(html).into_response()
}

async fn bootstrap(tower: TowerSession) -> Response {
    entry_page(tower, true).await
}
async fn manual_code(tower: TowerSession) -> Response {
    entry_page(tower, false).await
}

async fn select_invitation(
    State(state): State<AppState>,
    tower: TowerSession,
    crate::middleware::csrf::PublicCsrfForm(form): crate::middleware::csrf::PublicCsrfForm<
        SecretForm,
    >,
) -> Response {
    let mut session = match session::load(&tower).await {
        Ok(session) => session,
        Err(error) => return crate::WebError::Session(error.to_string()).into_response(),
    };
    let supplied = zeroize::Zeroizing::new(form.secret);
    let Ok(secret) = crate::link_secret::LinkSecret::parse(&supplied) else {
        return invitation_not_found_response(&session);
    };
    let hash = secret.hash();
    let binding = match state.storage.resolve_link_secret(&hash).await {
        Ok(Some(binding)) => binding,
        Ok(None) => return invitation_not_found_response(&session),
        Err(error) => return crate::WebError::from(error).into_response(),
    };
    session.pending_link = Some(session::PendingLink {
        link_id: binding.link_id,
        secret_sha256: hash,
        secret_generation: binding.secret_generation,
    });
    if let Err(error) = session::save(&tower, &session).await {
        return crate::WebError::Session(error.to_string()).into_response();
    }
    Redirect::to("/join").into_response()
}

fn redirect_to_login(return_to: &str) -> Response {
    let encoded: String = url::form_urlencoded::byte_serialize(return_to.as_bytes()).collect();
    Redirect::to(&format!("/login?return_to={encoded}")).into_response()
}

async fn invitation_page(
    State(state): State<AppState>,
    tower: TowerSession,
    axum::extract::Query(query): axum::extract::Query<StatusQuery>,
) -> Response {
    let session = match session::load(&tower).await {
        Ok(session) => session,
        Err(error) => return crate::WebError::Session(error.to_string()).into_response(),
    };
    let Some(capability) = &session.pending_link else {
        return invitation_not_found_response(&session);
    };
    if !session.is_authenticated() {
        let mut path = "/join".to_owned();
        if let Some(id) = &query.operation_id {
            let encoded: String = url::form_urlencoded::byte_serialize(id.as_bytes()).collect();
            path.push_str(&format!("?operation_id={encoded}"));
        }
        return offer::page(&state, &session, capability, &path).await;
    }
    attempt::page(
        &state,
        &tower,
        &session,
        capability,
        query.operation_id.as_deref(),
        query.fresh,
    )
    .await
}

#[derive(serde::Deserialize)]
struct StatusQuery {
    operation_id: Option<String>,
    #[serde(default)]
    fresh: bool,
}

async fn unknown_invitation(tower: TowerSession) -> Response {
    let session = session::load(&tower).await.unwrap_or_default();
    invitation_not_found_response(&session)
}

async fn submit_request(
    State(state): State<AppState>,
    tower: TowerSession,
    crate::middleware::csrf::CsrfForm(form): crate::middleware::csrf::CsrfForm<SubmitForm>,
) -> Response {
    let session = match session::load(&tower).await {
        Ok(session) => session,
        Err(error) => return crate::WebError::Session(error.to_string()).into_response(),
    };
    let Some(capability) = &session.pending_link else {
        return invitation_not_found_response(&session);
    };
    attempt::submit(
        &state,
        &tower,
        &session,
        capability,
        &form.operation_id,
        form.justification,
    )
    .await
}
