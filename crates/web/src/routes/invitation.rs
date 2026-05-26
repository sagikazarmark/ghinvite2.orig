//! `/i/{slug}...` routes. Plan 6: recipient flow.

use crate::commands::SubmitInvitationRequest;
use crate::invitation_link_resolution::{
    resolve_public_invitation_link_context, select_requester_request_state,
};
use crate::session;
use crate::state::AppState;
use crate::views::render::render;
use axum::Router;
use axum::extract::State;
use axum::http::Uri;
use axum::response::{Html, IntoResponse, Redirect};
use axum::routing::{any, get};
use chrono::Utc;
use dioxus::prelude::*;
use tower_sessions::Session as TowerSession;

#[derive(serde::Deserialize)]
struct SubmitForm {
    /// Pre-generated ULID from GET /i/{slug} handler for double-submit dedup.
    #[serde(default)]
    request_id: String,
    #[serde(default)]
    justification: Option<String>,
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/i/{slug}", get(invitation_page).post(submit_request))
        .route("/i/{slug}/{*rest}", any(unknown_nested))
}

fn invitation_not_found_response(signed_in_login: Option<String>) -> axum::response::Response {
    let html = render(move || {
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
) -> impl IntoResponse {
    let session = session::load(&tower).await.unwrap_or_default();
    if !session.is_authenticated() {
        return redirect_to_login(&canonical_invitation_path(&slug));
    }

    let now = Utc::now();
    let context = match resolve_public_invitation_link_context(state.storage.as_ref(), &slug).await
    {
        Ok(context) => context,
        Err(_) => return invitation_not_found_response(Some(session.login.clone())),
    };
    let selection = select_requester_request_state(&context.requests, session.user_id);
    if !context.link.is_active(now) && selection.current_status.is_none() {
        return invitation_not_found_response(Some(session.login.clone()));
    }

    let slug = context.slug.as_str().to_string();
    let request_id = domain::RequestId::new().to_string();
    let flash = session::take_flash(&tower).await.unwrap_or(None);
    let signed_in_login = session.login.clone();
    let html = render(move || {
        rsx! {
            crate::views::invitation::RequestPage {
                slug: slug.clone(),
                link: context.link.clone(),
                signed_in_login: signed_in_login.clone(),
                flash: flash.clone(),
                request_id: request_id.clone(),
                current_status: selection.current_status,
                retry_notice: selection.retry_notice,
            }
        }
    });
    Html(html).into_response()
}

async fn unknown_nested(tower: TowerSession, uri: Uri) -> impl IntoResponse {
    let session = session::load(&tower).await.unwrap_or_default();
    if !session.is_authenticated() {
        let return_to = uri
            .path_and_query()
            .map(|value| value.as_str())
            .unwrap_or("/");
        return redirect_to_login(return_to);
    }
    invitation_not_found_response(Some(session.login.clone()))
}

async fn submit_request(
    State(state): State<AppState>,
    tower: TowerSession,
    axum::extract::Path(slug): axum::extract::Path<String>,
    // Use serde_qs::axum::QsForm, NOT axum::extract::Form - this workspace uses serde_qs.
    serde_qs::axum::QsForm(form): serde_qs::axum::QsForm<SubmitForm>,
) -> impl IntoResponse {
    let session = session::load(&tower).await.unwrap_or_default();
    if !session.is_authenticated() {
        return redirect_to_login(&canonical_invitation_path(&slug));
    }

    let now = Utc::now();
    let context = match resolve_public_invitation_link_context(state.storage.as_ref(), &slug).await
    {
        Ok(context) => context,
        Err(_) => return invitation_not_found_response(Some(session.login.clone())),
    };
    let selection = select_requester_request_state(&context.requests, session.user_id);
    if !context.link.is_active(now) && selection.current_status.is_none() {
        return invitation_not_found_response(Some(session.login.clone()));
    }

    let slug = context.slug.as_str().to_string();

    if selection.current_status.is_some() {
        return Redirect::to(&canonical_invitation_path(&slug)).into_response();
    }

    let justification = form
        .justification
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    // Use the ULID pre-generated in the GET handler for double-submit dedup.
    // Fallback to a fresh ULID if the hidden field was absent or tampered.
    use std::str::FromStr;
    let request_id =
        domain::RequestId::from_str(&form.request_id).unwrap_or_else(|_| domain::RequestId::new());

    if let Err(e) = state
        .commands
        .submit_invitation_request(SubmitInvitationRequest::new(
            request_id,
            context.link.id,
            session.user_id,
            justification,
            now,
        ))
        .await
    {
        tracing::warn!(error = ?e, "submit invitation request command failed");
        let _ = session::set_flash(
            &tower,
            session::Flash {
                level: session::FlashLevel::Error,
                message: "Failed to submit request. Please try again.".into(),
            },
        )
        .await;
        return Redirect::to(&canonical_invitation_path(&slug)).into_response();
    }

    Redirect::to(&canonical_invitation_path(&slug)).into_response()
}
