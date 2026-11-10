//! A refresh requests observation only. The owner enforces its durable throttle.
use crate::{AppState, middleware::csrf::CsrfForm, session};
use axum::{
    Json,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{Html, IntoResponse, Response},
};
use dioxus::prelude::*;
use ghinvite_core::{
    RequestId, RequestState, org_delivery::OrganizationRefresh, request_lifecycle::RequestStatus,
};

#[derive(serde::Deserialize)]
pub(super) struct RefreshForm {
    request_id: RequestId,
}

pub(super) async fn refresh(
    State(state): State<AppState>,
    tower: tower_sessions::Session,
    headers: HeaderMap,
    CsrfForm(form): CsrfForm<RefreshForm>,
) -> Response {
    let session = match session::load(&tower).await {
        Ok(s) => s,
        Err(_) => return unavailable(),
    };
    if !session.is_authenticated() {
        return super::redirect_to_login("/join");
    }
    let Some(capability) = &session.pending_link else {
        return super::invitation_not_found_response(&session);
    };
    let page = match state
        .link_authority
        .requester_page(capability, session.user_id, None)
        .await
    {
        Ok(page) => page,
        Err(crate::link_authority::AuthorityError::Missing) => {
            return super::invitation_not_found_response(&session);
        }
        Err(_) => return unavailable(),
    };
    if page.link_id != capability.link_id {
        return unavailable();
    }
    let Some(request) = page.request.as_ref().filter(|r| {
        r.request_id == form.request_id
            && r.link_id == Some(page.link_id)
            && r.requester_id == session.user_id
            && r.state == RequestState::Approved
    }) else {
        return super::invitation_not_found_response(&session);
    };
    let Some(org) = &page.targets.organization else {
        return super::invitation_not_found_response(&session);
    };
    let progress = match state
        .request_authority
        .organization_delivery_progress(RequestStatus {
            link_id: page.link_id,
            request_id: request.request_id,
            requester_id: session.user_id,
        })
        .await
    {
        Ok(Some(p)) => p,
        Ok(None) => return answer(&headers, &session, OrganizationRefresh::NotTracked),
        Err(_) => return unavailable(),
    };
    let mut expected: Vec<_> = page.targets.teams.iter().map(|t| t.team_id).collect();
    expected.sort_unstable();
    let mut actual = progress.team_ids.clone();
    actual.sort_unstable();
    if progress.org_id != org.org_id
        || actual != expected
        || progress.delivery_generation == 0
        || progress.delivery_generation > ghinvite_core::storage::integer::MAX_EXACT
    {
        return unavailable();
    }
    let snapshot = match state
        .delivery_authority
        .organization_snapshot(request.request_id, org.org_id, progress.delivery_generation)
        .await
    {
        Ok(Some(s)) => s,
        Ok(None) => return answer(&headers, &session, OrganizationRefresh::NotTracked),
        Err(_) => return unavailable(),
    };
    let command = &snapshot.create.command;
    if command.validate().is_err()
        || command.link_id != Some(page.link_id)
        || command.request_id != request.request_id
        || command.requester_id != session.user_id
        || command.account_id != org.org_id
        || command.organization != *org
        || command.teams != page.targets.teams
        || command.delivery_generation != progress.delivery_generation
    {
        return unavailable();
    }
    match state
        .delivery_authority
        .refresh_organization(request.request_id, org.org_id, progress.delivery_generation)
        .await
    {
        Ok(result) => answer(&headers, &session, result),
        Err(_) => unavailable(),
    }
}
fn unavailable() -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        "Organization refresh is unavailable. Check again later.",
    )
        .into_response()
}
fn answer(
    headers: &HeaderMap,
    session: &session::Session,
    result: OrganizationRefresh,
) -> Response {
    let status = if matches!(result, OrganizationRefresh::Throttled { .. }) {
        StatusCode::TOO_MANY_REQUESTS
    } else {
        StatusCode::OK
    };
    if headers
        .get(axum::http::header::ACCEPT)
        .and_then(|h| h.to_str().ok())
        .is_some_and(|h| h.split(',').any(|v| v.trim() == "application/json"))
    {
        return (status, Json(result)).into_response();
    }
    let message = match result {
        OrganizationRefresh::Requested => "Refresh requested. Check again shortly for verified organization and team updates.".to_owned(),
        OrganizationRefresh::Throttled{retry_at} => format!("A refresh was requested recently. Try again no earlier than {}.",retry_at.to_rfc3339()),
        OrganizationRefresh::NotTracked => "No tracked organization invitation is available to refresh yet. This does not establish current access.".to_owned(),
    };
    let login = session.login.clone();
    let html = crate::render::render_with_csrf(session.csrf_token.clone(), move || {
        rsx! {
            ghinvite_ui::layouts::InvitationLayout {signed_in_login:Some(login.clone()),title:"Organization delivery refresh".to_owned(),account_login:None,active_nav:None,flash:None,
                h1 { "Organization delivery refresh" } p { role:"status", "{message}" } a {href:"/join",class:"btn btn-outline","Check again"}
            }
        }
    });
    (status, Html(html)).into_response()
}
