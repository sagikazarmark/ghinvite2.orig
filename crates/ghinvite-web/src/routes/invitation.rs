//! `/i/{slug}...` routes. Plan 6: recipient flow.

use crate::commands::SubmitInvitationRequest;
use crate::invitation_link_resolution::{
    resolve_public_invitation_link_context, select_requester_request_state,
};
use crate::session;
use crate::state::AppState;
use crate::views::render::render_with_csrf as render;
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

fn invitation_not_found_response(session: &session::Session) -> axum::response::Response {
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
        return redirect_to_login(&canonical_invitation_path(&slug));
    }

    let now = Utc::now();
    let context = match resolve_public_invitation_link_context(state.storage.as_ref(), &slug).await
    {
        Ok(context) => context,
        Err(_) => return invitation_not_found_response(&session),
    };
    let mut selection = select_requester_request_state(&context.requests, session.user_id);
    if let Some(lifecycle) = &state.request_lifecycle {
        let request_id = query.request_id.or_else(|| {
            context
                .requests
                .iter()
                .find(|r| r.requester_id == session.user_id)
                .map(|r| r.id)
        });
        if let Some(request_id) = request_id {
            let request = match lifecycle
                .status(ghinvite_core::request_lifecycle::RequestStatus {
                    link_id: context.link.id,
                    request_id,
                    requester_id: session.user_id,
                })
                .await
            {
                Ok(request) => request,
                Err(error) => return error.into_response(),
            };
            // Render only lifecycle state; justification/reason and admin facts
            // from the private command response never become requester copy.
            selection.current_status = Some(request.state);
            selection.retry_notice = None;
        }
    }
    if !context.link.is_active(now) && selection.current_status.is_none() {
        return invitation_not_found_response(&session);
    }

    let slug = context.slug.as_str().to_string();
    let request_id = ghinvite_core::RequestId::new().to_string();
    let flash = session::take_flash(&tower).await.unwrap_or(None);
    let signed_in_login = session.login.clone();
    let html = render(session.csrf_token.clone(), move || {
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

#[derive(serde::Deserialize)]
struct StatusQuery {
    request_id: Option<ghinvite_core::RequestId>,
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

    let now = Utc::now();
    let context = match resolve_public_invitation_link_context(state.storage.as_ref(), &slug).await
    {
        Ok(context) => context,
        Err(_) => return invitation_not_found_response(&session),
    };
    let selection = select_requester_request_state(&context.requests, session.user_id);
    if !context.link.is_active(now) && selection.current_status.is_none() {
        return invitation_not_found_response(&session);
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
    let request_id = ghinvite_core::RequestId::from_str(&form.request_id)
        .unwrap_or_else(|_| ghinvite_core::RequestId::new());

    if state
        .commands
        .submit_invitation_request(SubmitInvitationRequest::new(
            request_id,
            context.link.id,
            session.user_id,
            justification,
            now,
        ))
        .await
        .is_err()
    {
        tracing::warn!("submit invitation request command failed");
        let flash = Some(session::Flash {
            level: session::FlashLevel::Error,
            message: "Failed to submit request. Please try again.".into(),
        });
        let html = render(session.csrf_token.clone(), move || {
            rsx! {
                crate::views::invitation::RequestPage {
                    slug: slug.clone(),
                    link: context.link.clone(),
                    signed_in_login: session.login.clone(),
                    flash: flash.clone(),
                    request_id: request_id.to_string(),
                    justification: form.justification.clone().unwrap_or_default(),
                    current_status: selection.current_status,
                    retry_notice: selection.retry_notice,
                }
            }
        });
        return (axum::http::StatusCode::BAD_GATEWAY, Html(html)).into_response();
    }

    Redirect::to(&canonical_invitation_path(&slug)).into_response()
}
