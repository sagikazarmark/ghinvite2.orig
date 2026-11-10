//! Requester-owned status and single-target observation, without a link bearer.
use crate::{AppState, middleware::csrf::CsrfForm, session};
use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{Html, IntoResponse, Response},
};
use dioxus::prelude::*;
use ghinvite_core::{
    RequestId, RequestState,
    delivery::{CreateOutcome, DeliverySnapshot, RepositoryRefresh},
    request_lifecycle::{RequesterStatus, RequesterView},
    targets::RepoTarget,
};

#[derive(serde::Deserialize)]
pub(super) struct RefreshForm {
    target: String,
}

fn unavailable() -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        "Current request status is unavailable. Check again later.",
    )
        .into_response()
}
fn path(id: RequestId) -> String {
    format!("/requests/{id}")
}

enum ReadError {
    Missing,
    Unavailable,
}
impl ReadError {
    fn response(self, session: &session::Session) -> Response {
        match self {
            Self::Missing => super::invitation_not_found_response(session),
            Self::Unavailable => unavailable(),
        }
    }
}

async fn authorized(
    state: &AppState,
    session: &session::Session,
    id: RequestId,
) -> Result<RequesterView, ReadError> {
    let view = match state
        .request_authority
        .requester_status(RequesterStatus {
            request_id: id,
            requester_id: session.user_id,
        })
        .await
    {
        Ok(view) => view,
        Err(crate::link_authority::AuthorityError::Missing) => {
            return Err(ReadError::Missing);
        }
        Err(_) => return Err(ReadError::Unavailable),
    };
    if view.request.request_id != id
        || view.request.requester_id != session.user_id
        || view.request.account_id == 0
        || view.targets.validate().is_err()
    {
        return Err(ReadError::Unavailable);
    }
    let max = ghinvite_core::storage::integer::MAX_EXACT;
    let mut repos = std::collections::BTreeSet::new();
    if view.repositories.iter().any(|p| {
        p.delivery_generation == 0
            || p.delivery_generation > max
            || !repos.insert(p.repo_id)
            || !view.targets.repos.iter().any(|t| t.repo_id == p.repo_id)
    }) {
        return Err(ReadError::Unavailable);
    }
    if let Some(progress) = &view.organization {
        let mut teams = progress.team_ids.clone();
        teams.sort_unstable();
        let mut expected: Vec<_> = view.targets.teams.iter().map(|t| t.team_id).collect();
        expected.sort_unstable();
        if !view
            .targets
            .organization
            .as_ref()
            .is_some_and(|t| t.org_id == progress.org_id && t.org_id == view.request.account_id)
            || teams != expected
            || progress.delivery_generation == 0
            || progress.delivery_generation > max
        {
            return Err(ReadError::Unavailable);
        }
    }
    Ok(view)
}

fn matches_repository(
    view: &RequesterView,
    target: &RepoTarget,
    generation: u64,
    snapshot: &DeliverySnapshot,
) -> bool {
    let c = &snapshot.create.command;
    c.request_id == view.request.request_id
        && c.link_id == view.request.link_id
        && c.batch == view.request.batch
        && c.account_id == view.request.account_id
        && c.requester_id == view.request.requester_id
        && c.repo_id == target.repo_id
        && c.repo_full_name == target.repo_full_name
        && c.permission == target.grant.into()
        && c.delivery_generation == generation
}
fn tracked(snapshot: &DeliverySnapshot) -> bool {
    snapshot.settlement.is_none()
        && snapshot.tracking_stopped.is_none()
        && matches!(
            snapshot.create.outcome,
            CreateOutcome::Created { .. } | CreateOutcome::AlreadyPending { .. }
        )
}
fn matches_organization(
    view: &RequesterView,
    snapshot: &ghinvite_core::org_delivery::OrganizationDeliverySnapshot,
) -> bool {
    let c = &snapshot.create.command;
    c.validate().is_ok()
        && c.request_id == view.request.request_id
        && c.link_id == view.request.link_id
        && c.batch == view.request.batch
        && c.account_id == view.request.account_id
        && c.requester_id == view.request.requester_id
        && view.targets.organization.as_ref() == Some(&c.organization)
        && c.teams == view.targets.teams
        && view
            .organization
            .as_ref()
            .is_some_and(|p| p.delivery_generation == c.delivery_generation)
}

pub(super) async fn show(
    State(state): State<AppState>,
    tower: tower_sessions::Session,
    Path(raw): Path<String>,
) -> Response {
    let Ok(id) = raw.parse::<RequestId>() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let mut session = match session::load(&tower).await {
        Ok(s) => s,
        Err(_) => return unavailable(),
    };
    if !session.is_authenticated() {
        return super::redirect_to_login(&path(id));
    }
    if session.csrf_token.is_none() {
        session.csrf_token = Some(crate::middleware::csrf::generate_token());
        if session::save(&tower, &session).await.is_err() {
            return unavailable();
        }
    }
    let view = match authorized(&state, &session, id).await {
        Ok(v) => v,
        Err(r) => return r.response(&session),
    };
    let mut rows = Vec::new();
    for target in &view.targets.repos {
        let progress = view
            .repositories
            .iter()
            .find(|p| p.repo_id == target.repo_id);
        let mut snapshots = Vec::new();
        let mut unavailable = false;
        if let Some(progress) = progress {
            match state
                .delivery_authority
                .snapshot(id, target.repo_id, progress.delivery_generation)
                .await
            {
                Ok(Some(s))
                    if matches_repository(&view, target, progress.delivery_generation, &s) =>
                {
                    snapshots.push(s)
                }
                Ok(_) | Err(_) => unavailable = true,
            }
        }
        let repo = ghinvite_core::InvitationLinkRepo {
            repo_id: target.repo_id,
            repo_full_name: target.repo_full_name.clone(),
        };
        let row = ghinvite_ui::invitation::delivery_presentation(
            &repo,
            &snapshots,
            &view.repositories,
            unavailable,
        );
        let refreshable =
            view.request.state == RequestState::Approved && snapshots.first().is_some_and(tracked);
        rows.push((target.repo_id, row, refreshable));
    }
    let mut organization = None;
    let mut org_refreshable = false;
    if let Some(progress) = &view.organization {
        let mut snapshots = Vec::new();
        let mut unavailable = false;
        match state
            .delivery_authority
            .organization_snapshot(id, progress.org_id, progress.delivery_generation)
            .await
        {
            Ok(Some(s)) if matches_organization(&view, &s) => {
                org_refreshable =
                    view.request.state == RequestState::Approved && s.needs_tracking();
                snapshots.push(s);
            }
            Ok(_) | Err(_) => unavailable = true,
        }
        organization = ghinvite_ui::organization_delivery::presentation(
            &view.targets,
            &snapshots,
            Some(progress),
            unavailable,
        );
    } else if view.targets.organization.is_some() {
        organization =
            ghinvite_ui::organization_delivery::presentation(&view.targets, &[], None, false);
    }
    let request = view.request;
    let login = session.login.clone();
    let href = path(id);
    let html = crate::render::render_with_csrf(session.csrf_token.clone(), move || {
        rsx! {
            ghinvite_ui::layouts::InvitationLayout {signed_in_login:Some(login.clone()),title:"Request status".to_owned(),account_login:None,active_nav:None,flash:None,
                h1 {"Request status"}
                p {"Request: {request.state}"}
                if let Some(deadline)=request.decision_deadline.filter(|_|request.state==RequestState::Pending) {p {"Decision deadline: {deadline.to_rfc3339()}"}}
                if request.state==RequestState::Declined {
                    p {"An account admin declined this request."}
                    if let Some(message)=request.decision.as_ref().and_then(|d|d.requester_message.as_ref()) {
                        p {class:"whitespace-pre-wrap","Message from the reviewer: {message}"}
                    }
                }
                if request.state==RequestState::Lapsed {p {"Your invitation expired before it was accepted. Open the invitation link to check whether another request is available."}}
                if request.state==RequestState::Expired {p {"The decision deadline passed before approval."}}
                if request.state==RequestState::Cancelled {p {"This request was cancelled."}}
                p {"Approval does not establish access. Delivery outcomes below are verified separately with GitHub."}
                ul {aria_label:"Repository delivery",for (repo_id,row,refreshable) in &rows {
                    ghinvite_ui::invitation::DeliveryRow {row:row.clone(),login:login.clone()}
                    if *refreshable {li {form {method:"post",action:"{href}/refresh",
                        ghinvite_ui::csrf::CsrfField {}
                        input {r#type:"hidden",name:"target",value:format!("repo:{repo_id}")}
                        button {r#type:"submit",class:"btn btn-outline","Refresh repository delivery"}
                    }}}
                }}
                if let Some(view)=&organization {
                    ghinvite_ui::organization_delivery::OrganizationDelivery {view:view.clone()}
                    if org_refreshable {form {method:"post",action:"{href}/refresh",
                        ghinvite_ui::csrf::CsrfField {}
                        input {r#type:"hidden",name:"target",value:"organization"}
                        button {r#type:"submit",class:"btn btn-outline","Refresh organization delivery"}
                    }}
                }
                a {href:href.clone(),class:"btn btn-outline","Check again"}
                p {"A refresh requests a read-only check, at most once per minute for each delivery. Check again shortly for verified updates."}
            }
        }
    });
    Html(html).into_response()
}

pub(super) async fn refresh(
    State(state): State<AppState>,
    tower: tower_sessions::Session,
    Path(raw): Path<String>,
    headers: HeaderMap,
    CsrfForm(form): CsrfForm<RefreshForm>,
) -> Response {
    let Ok(id) = raw.parse::<RequestId>() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let session = match session::load(&tower).await {
        Ok(s) => s,
        Err(_) => return unavailable(),
    };
    let view = match authorized(&state, &session, id).await {
        Ok(v) => v,
        Err(r) => return r.response(&session),
    };
    if view.request.state != RequestState::Approved {
        return answer(&headers, &session, id, RepositoryRefresh::NotTracked);
    }
    let result = if form.target == "organization" {
        let Some(progress) = &view.organization else {
            return answer(&headers, &session, id, RepositoryRefresh::NotTracked);
        };
        match state
            .delivery_authority
            .organization_snapshot(id, progress.org_id, progress.delivery_generation)
            .await
        {
            Ok(Some(s)) if matches_organization(&view, &s) => {}
            Ok(None) => return answer(&headers, &session, id, RepositoryRefresh::NotTracked),
            _ => return unavailable(),
        }
        match state
            .delivery_authority
            .refresh_organization(id, progress.org_id, progress.delivery_generation)
            .await
        {
            Ok(ghinvite_core::org_delivery::OrganizationRefresh::Requested) => {
                Ok(RepositoryRefresh::Requested)
            }
            Ok(ghinvite_core::org_delivery::OrganizationRefresh::NotTracked) => {
                Ok(RepositoryRefresh::NotTracked)
            }
            Ok(ghinvite_core::org_delivery::OrganizationRefresh::Throttled { retry_at }) => {
                Ok(RepositoryRefresh::Throttled { retry_at })
            }
            Err(e) => Err(e),
        }
    } else {
        let Some(repo_id) = form
            .target
            .strip_prefix("repo:")
            .and_then(|v| v.parse::<u64>().ok())
        else {
            return super::invitation_not_found_response(&session);
        };
        let Some(target) = view.targets.repos.iter().find(|t| t.repo_id == repo_id) else {
            return super::invitation_not_found_response(&session);
        };
        let Some(progress) = view.repositories.iter().find(|p| p.repo_id == repo_id) else {
            return answer(&headers, &session, id, RepositoryRefresh::NotTracked);
        };
        match state
            .delivery_authority
            .snapshot(id, repo_id, progress.delivery_generation)
            .await
        {
            Ok(Some(s)) if matches_repository(&view, target, progress.delivery_generation, &s) => {
                if !tracked(&s) {
                    return answer(&headers, &session, id, RepositoryRefresh::NotTracked);
                }
            }
            Ok(None) => return answer(&headers, &session, id, RepositoryRefresh::NotTracked),
            _ => return unavailable(),
        }
        state
            .delivery_authority
            .refresh_repository(id, repo_id, progress.delivery_generation)
            .await
    };
    match result {
        Ok(r) => answer(&headers, &session, id, r),
        Err(_) => unavailable(),
    }
}
fn answer(
    headers: &HeaderMap,
    session: &session::Session,
    id: RequestId,
    result: RepositoryRefresh,
) -> Response {
    let status = if matches!(result, RepositoryRefresh::Throttled { .. }) {
        StatusCode::TOO_MANY_REQUESTS
    } else {
        StatusCode::OK
    };
    if headers
        .get("accept")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.split(',').any(|v| v.trim() == "application/json"))
    {
        return (status, Json(result)).into_response();
    }
    let message = match result {
        RepositoryRefresh::Requested => {
            "Refresh requested. Check again shortly for verified updates.".to_owned()
        }
        RepositoryRefresh::NotTracked => {
            "No tracked invitation is available to refresh. This does not establish access."
                .to_owned()
        }
        RepositoryRefresh::Throttled { retry_at } => format!(
            "A refresh was requested recently. Try again no earlier than {}.",
            retry_at.to_rfc3339()
        ),
    };
    let href = path(id);
    let login = session.login.clone();
    let html = crate::render::render_with_csrf(
        session.csrf_token.clone(),
        move || rsx! {ghinvite_ui::layouts::InvitationLayout {signed_in_login:Some(login.clone()),title:"Delivery refresh".to_owned(),account_login:None,active_nav:None,flash:None,h1 {"Delivery refresh"} p {role:"status","{message}"} a {href:href.clone(),class:"btn btn-outline","Check again"}}},
    );
    (status, Html(html)).into_response()
}
