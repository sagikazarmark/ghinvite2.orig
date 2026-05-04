//! `/i/{slug}...` routes. Plan 6: recipient flow.

use crate::session;
use crate::state::AppState;
use crate::views::render::render;
use axum::Router;
use axum::extract::State;
use axum::response::{Html, IntoResponse, Redirect};
use axum::routing::get;
use chrono::Utc;
use dioxus::prelude::*;
use tower_sessions::Session as TowerSession;

#[derive(serde::Deserialize)]
struct SubmitForm {
    /// Pre-generated ULID from GET /request handler for double-submit dedup.
    #[serde(default)]
    request_id: String,
    #[serde(default)]
    justification: Option<String>,
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/i/{slug}", get(landing))
        .route(
            "/i/{slug}/request",
            get(request_form).post(submit_request),
        )
        .route("/i/{slug}/pending/{request_id}", get(pending))
}

async fn landing(
    State(state): State<AppState>,
    tower: TowerSession,
    axum::extract::Path(slug): axum::extract::Path<String>,
) -> impl IntoResponse {
    let now = Utc::now();
    let link = match state.storage.get_share_link_by_slug(&slug).await {
        Ok(Some(l)) if l.is_active(now) => l,
        _ => return crate::error::WebError::NotFound.into_response(),
    };
    let session = session::load(&tower).await.unwrap_or_default();
    let signed_in_login = if session.is_authenticated() {
        Some(session.login.clone())
    } else {
        None
    };
    let html = render(move || rsx! {
        crate::views::invitation::LandingPage {
            slug: slug.clone(),
            link: link.clone(),
            signed_in_login: signed_in_login.clone(),
            now,
        }
    });
    Html(html).into_response()
}

async fn request_form(
    State(state): State<AppState>,
    tower: TowerSession,
    axum::extract::Path(slug): axum::extract::Path<String>,
) -> impl IntoResponse {
    let session = session::load(&tower).await.unwrap_or_default();
    if !session.is_authenticated() {
        return Redirect::to(&format!("/login?return_to=/i/{slug}/request")).into_response();
    }
    let now = Utc::now();
    let link = match state.storage.get_share_link_by_slug(&slug).await {
        Ok(Some(l)) if l.is_active(now) => l,
        _ => return crate::error::WebError::NotFound.into_response(),
    };

    // Redirect to pending if requester already has a pending request (prevents duplicate Restate workflows).
    if let Ok(requests) = state.storage.list_requests_for_link(link.id).await {
        if let Some(existing) = requests
            .iter()
            .find(|r| r.requester_id == session.user_id && r.state == domain::RequestState::Pending)
        {
            return Redirect::to(&format!("/i/{slug}/pending/{}", existing.id)).into_response();
        }
    }

    let request_id = domain::RequestId::new();
    let flash = session::take_flash(&tower).await.unwrap_or(None);
    let signed_in_login = session.login.clone();
    let request_id_str = request_id.to_string();
    let html = render(move || rsx! {
        crate::views::invitation::RequestFormPage {
            slug: slug.clone(),
            link: link.clone(),
            signed_in_login: signed_in_login.clone(),
            flash: flash.clone(),
            request_id: request_id_str.clone(),
        }
    });
    Html(html).into_response()
}

async fn submit_request(
    State(state): State<AppState>,
    tower: TowerSession,
    axum::extract::Path(slug): axum::extract::Path<String>,
    // Use serde_qs::axum::QsForm, NOT axum::extract::Form — this workspace uses serde_qs.
    serde_qs::axum::QsForm(form): serde_qs::axum::QsForm<SubmitForm>,
) -> impl IntoResponse {
    let session = session::load(&tower).await.unwrap_or_default();
    if !session.is_authenticated() {
        return Redirect::to(&format!("/login?return_to=/i/{slug}/request")).into_response();
    }
    let now = Utc::now();
    let link = match state.storage.get_share_link_by_slug(&slug).await {
        Ok(Some(l)) if l.is_active(now) => l,
        _ => return crate::error::WebError::NotFound.into_response(),
    };

    let justification = form.justification
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    // Use the ULID pre-generated in the GET handler for double-submit dedup.
    // Fallback to a fresh ULID if the hidden field was absent or tampered.
    use std::str::FromStr;
    let request_id = domain::RequestId::from_str(&form.request_id)
        .unwrap_or_else(|_| domain::RequestId::new());

    let input = serde_json::json!({
        "request_id": request_id.to_string(),
        "share_link_id": link.id.to_string(),
        "requester_id": session.user_id,
        "justification": justification,
        "created_at": now,
    });

    if let Err(e) = state
        .restate
        .send("InvitationRequest", &request_id.to_string(), "submit", &input)
        .await
    {
        tracing::warn!(error = ?e, "InvitationRequest::submit send failed");
        let _ = session::set_flash(
            &tower,
            session::Flash {
                level: session::FlashLevel::Error,
                message: "Failed to submit request. Please try again.".into(),
            },
        )
        .await;
        return Redirect::to(&format!("/i/{slug}/request")).into_response();
    }

    Redirect::to(&format!("/i/{slug}/pending/{request_id}")).into_response()
}

async fn pending(
    State(_state): State<AppState>,
    _tower: TowerSession,
    axum::extract::Path(_path): axum::extract::Path<(String, String)>,
) -> impl IntoResponse {
    (axum::http::StatusCode::NOT_IMPLEMENTED, "TODO Task 8")
}
