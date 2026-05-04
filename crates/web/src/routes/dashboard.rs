//! `/accounts/{login}/...` routes. Plan 5.

use crate::middleware::auth::RequireAdminOf;
use crate::session;
use crate::state::AppState;
use crate::views::render::render;
use axum::Router;
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse};
use axum::routing::get;
use chrono::Utc;
use dioxus::prelude::*;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/accounts/{login}", get(overview))
        .route("/accounts/{login}/links/new", get(stub))
        .route("/accounts/{login}/links/{link_id}", get(stub))
        .route("/accounts/{login}/requests", get(stub))
        .route("/accounts/{login}/audit", get(stub))
        .route("/accounts/{login}/settings", get(stub))
}

async fn overview(
    axum::extract::State(state): axum::extract::State<AppState>,
    admin: RequireAdminOf,
) -> impl IntoResponse {
    let now = Utc::now();
    let pending = state
        .storage
        .list_pending_requests_for_account(admin.account.account_id)
        .await
        .map(|v| v.len() as u64)
        .unwrap_or(0);
    let all_links = state
        .storage
        .list_share_links_for_account(admin.account.account_id)
        .await
        .unwrap_or_default();
    let active_links = all_links.iter().filter(|l| l.is_active(now)).count() as u64;
    let recent_links = {
        let mut v = all_links.clone();
        v.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        v.truncate(5);
        v
    };

    let flash = session::take_flash(&admin.tower).await.unwrap_or(None);
    let signed_in_login = Some(admin.session.login.clone());
    let account_login = admin.account.account_login.clone();
    let account_type = admin.account.account_type.to_string();

    let html = render(move || {
        rsx! {
            crate::views::dashboard::OverviewPage {
                signed_in_login: signed_in_login.clone(),
                flash: flash.clone(),
                account_login: account_login.clone(),
                account_type: account_type.clone(),
                pending_requests: pending,
                active_links,
                recent_links: recent_links.clone(),
                now,
            }
        }
    });
    Html(html).into_response()
}

async fn stub() -> impl IntoResponse {
    (
        StatusCode::NOT_IMPLEMENTED,
        "Plan 5 will fill in the rest of the dashboard routes.",
    )
}
