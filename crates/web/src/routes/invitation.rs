//! `/i/{slug}...` routes. Plan 6: recipient flow.

use super::share_link_resolution::{PendingRequestPolicy, resolve_public_share_link};
use crate::commands::SubmitInvitationRequest;
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
        .route("/i/{slug}/request", get(request_form).post(submit_request))
        .route("/i/{slug}/pending/{request_id}", get(pending))
}

async fn landing(
    State(state): State<AppState>,
    tower: TowerSession,
    axum::extract::Path(slug): axum::extract::Path<String>,
) -> impl IntoResponse {
    let now = Utc::now();
    let resolved = match resolve_public_share_link(
        state.storage.as_ref(),
        &slug,
        now,
        PendingRequestPolicy::Ignore,
    )
    .await
    {
        Ok(resolved) => resolved,
        Err(error) => return error.into_response(),
    };
    let slug = resolved.slug.as_str().to_string();
    let link = resolved.link;
    let session = session::load(&tower).await.unwrap_or_default();
    let signed_in_login = if session.is_authenticated() {
        Some(session.login.clone())
    } else {
        None
    };
    let html = render(move || {
        rsx! {
            crate::views::invitation::LandingPage {
                slug: slug.clone(),
                link: link.clone(),
                signed_in_login: signed_in_login.clone(),
                now,
            }
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
    let resolved = match resolve_public_share_link(
        state.storage.as_ref(),
        &slug,
        now,
        PendingRequestPolicy::RedirectForRequester(session.user_id),
    )
    .await
    {
        Ok(resolved) => resolved,
        Err(error) => return error.into_response(),
    };

    if let Some(redirect) = resolved.pending_redirect() {
        return redirect.into_response();
    }

    let slug = resolved.slug.as_str().to_string();
    let link = resolved.link;
    let request_id = domain::RequestId::new();
    let flash = session::take_flash(&tower).await.unwrap_or(None);
    let signed_in_login = session.login.clone();
    let request_id_str = request_id.to_string();
    let html = render(move || {
        rsx! {
            crate::views::invitation::RequestFormPage {
                slug: slug.clone(),
                link: link.clone(),
                signed_in_login: signed_in_login.clone(),
                flash: flash.clone(),
                request_id: request_id_str.clone(),
            }
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
    let resolved = match resolve_public_share_link(
        state.storage.as_ref(),
        &slug,
        now,
        PendingRequestPolicy::RedirectForRequester(session.user_id),
    )
    .await
    {
        Ok(resolved) => resolved,
        Err(error) => return error.into_response(),
    };

    if let Some(redirect) = resolved.pending_redirect() {
        return redirect.into_response();
    }

    let slug = resolved.slug.as_str().to_string();
    let link = resolved.link;

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
        .submit_invitation_request(SubmitInvitationRequest {
            request_id,
            share_link_id: link.id,
            requester_id: session.user_id,
            justification,
            created_at: now,
        })
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
    State(state): State<AppState>,
    tower: TowerSession,
    axum::extract::Path((slug, request_id_str)): axum::extract::Path<(String, String)>,
) -> impl IntoResponse {
    // TODO(test): the requester_id ownership guard is security-critical and should be
    // covered by an integration test in Plan 8 (tests/invitation_ownership.rs).
    use std::str::FromStr;

    let session = session::load(&tower).await.unwrap_or_default();
    if !session.is_authenticated() {
        return Redirect::to(&format!(
            "/login?return_to=/i/{slug}/pending/{request_id_str}"
        ))
        .into_response();
    }

    let request_id = match domain::RequestId::from_str(&request_id_str) {
        Ok(id) => id,
        Err(_) => return crate::error::WebError::NotFound.into_response(),
    };

    // Load request. None = workflow just started, not yet in DB — show "processing" state.
    let request_state = match state.storage.get_invitation_request(request_id).await {
        Ok(Some(r)) if r.requester_id == session.user_id => Some(r.state),
        Ok(Some(_)) => return crate::error::WebError::NotFound.into_response(), // wrong user
        Ok(None) => None,                                                       // not yet committed
        Err(_) => return crate::error::WebError::NotFound.into_response(),
    };

    let signed_in_login = Some(session.login.clone());
    let html = render(move || {
        rsx! {
            crate::views::invitation::PendingPage {
                slug: slug.clone(),
                request_state,
                signed_in_login: signed_in_login.clone(),
            }
        }
    });
    Html(html).into_response()
}
