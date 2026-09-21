//! `/i/{slug}...` routes: the requester's admission flow.

use crate::session;
use crate::state::AppState;
use crate::views::render::render_with_csrf as render;
use axum::Router;
use axum::extract::State;
use axum::http::Uri;
use axum::response::{Html, IntoResponse, Redirect};
use axum::routing::{any, get};
use dioxus::prelude::*;
use tower_sessions::Session as TowerSession;

mod attempt;

#[derive(serde::Deserialize)]
struct SubmitForm {
    #[serde(default)]
    operation_id: String,
    #[serde(default)]
    justification: Option<String>,
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/i/{slug}", get(invitation_page).post(submit_request))
        .route("/i/{slug}/{*rest}", any(unknown_nested))
}

pub(super) fn invitation_not_found_response(
    session: &session::Session,
) -> axum::response::Response {
    let signed_in_login = Some(session.login.clone());
    let html = render(session.csrf_token.clone(), move || {
        rsx! {
            crate::views::not_found::InvitationNotFoundPage {
                signed_in_login: signed_in_login.clone(),
            }
        }
    });
    (axum::http::StatusCode::NOT_FOUND, Html(html)).into_response()
}

fn redirect_to_login(return_to: &str) -> axum::response::Response {
    let encoded: String = url::form_urlencoded::byte_serialize(return_to.as_bytes()).collect();
    Redirect::to(&format!("/login?return_to={encoded}")).into_response()
}

fn canonical_invitation_path(slug: &str) -> String {
    format!("/i/{slug}")
}

async fn invitation_page(
    State(state): State<AppState>,
    tower: TowerSession,
    axum::extract::Path(slug): axum::extract::Path<String>,
    axum::extract::Query(query): axum::extract::Query<StatusQuery>,
) -> impl IntoResponse {
    let session = match session::load(&tower).await {
        Ok(session) => session,
        Err(error) => return crate::WebError::Session(error.to_string()).into_response(),
    };
    if !session.is_authenticated() {
        let mut path = canonical_invitation_path(&slug);
        if let Some(id) = &query.operation_id {
            let encoded: String = url::form_urlencoded::byte_serialize(id.as_bytes()).collect();
            path.push_str(&format!("?operation_id={encoded}"));
        }
        return redirect_to_login(&path);
    }

    attempt::page(
        &state,
        &tower,
        &state.admission,
        &session,
        &slug,
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

async fn unknown_nested(tower: TowerSession, uri: Uri) -> impl IntoResponse {
    let session = match session::load(&tower).await {
        Ok(session) => session,
        Err(error) => return crate::WebError::Session(error.to_string()).into_response(),
    };
    if !session.is_authenticated() {
        let return_to = uri
            .path_and_query()
            .map(|value| value.as_str())
            .unwrap_or("/");
        return redirect_to_login(return_to);
    }
    invitation_not_found_response(&session)
}

async fn submit_request(
    State(state): State<AppState>,
    tower: TowerSession,
    axum::extract::Path(slug): axum::extract::Path<String>,
    crate::middleware::csrf::CsrfForm(form): crate::middleware::csrf::CsrfForm<SubmitForm>,
) -> impl IntoResponse {
    let session = match session::load(&tower).await {
        Ok(session) => session,
        Err(error) => return crate::WebError::Session(error.to_string()).into_response(),
    };
    if !session.is_authenticated() {
        return redirect_to_login(&canonical_invitation_path(&slug));
    }

    attempt::submit(
        &state,
        &tower,
        &state.admission,
        &session,
        &slug,
        &form.operation_id,
        form.justification,
    )
    .await
}
