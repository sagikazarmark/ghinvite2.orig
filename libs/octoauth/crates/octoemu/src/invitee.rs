//! The invitee's side of repository invitations (GE-05), with a user token:
//! `GET /user/repository_invitations` (paged), `PATCH
//! /user/repository_invitations/{invitation_id}` (accept, `member.added`)
//! and `DELETE …` (decline). Thin adapters over `crate::collaboration`, the
//! same operations the admin API's `act` routes run.
//!
//! Documented: <https://docs.github.com/en/rest/collaborators/invitations>.
//! A classic PAT may call them (documented: the `repo:invite` scope); a
//! GitHub App's user access token may too (behaviour
//! `user-token-invitations`).

use crate::apps::view;
use crate::auth::{self, UserCaller};
use crate::collaboration::{self, Ctx, Failure};
use crate::paginate::Pagination;
use crate::repos::invitations_json;
use crate::response::{self, json_with_link};
use crate::seed::RepositoryInvitation;
use crate::state::{Shared, SharedState};
use crate::user_tokens::{Grant, UserTokenRecord};
use crate::views;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::response::{IntoResponse, Response};

/// Refuses a GitHub App user token in refuse-assumed mode: whether GitHub
/// lets one act on invitations is unmeasured.
fn refuse_app_token(shared: &Shared, token: &UserTokenRecord) -> Option<Response> {
    match token.grant {
        Grant::UserToken { .. } => shared.refuse("user-token-invitations"),
        Grant::PersonalAccessToken { .. } => None,
    }
}

/// `GET /user/repository_invitations`, paged, oldest first.
pub(crate) async fn list(
    State(shared): State<SharedState>,
    UserCaller(token): UserCaller,
    headers: HeaderMap,
    uri: Uri,
) -> Response {
    if let Some(refusal) = refuse_app_token(&shared, &token) {
        return refusal;
    }
    let origin = views::origin(&shared, &headers);
    let state = shared.state();
    let mut mine: Vec<&RepositoryInvitation> = state
        .world
        .repository_invitations
        .iter()
        .filter(|i| i.invitee.eq_ignore_ascii_case(&token.login))
        .collect();
    mine.sort_by_key(|i| i.id);
    let paged = Pagination::of(&uri).apply(mine);
    let view = view(&state, &origin);
    match invitations_json(&shared, &view, &paged.items) {
        Ok(body) => json_with_link(StatusCode::OK, &body, paged.link(&origin, &uri)),
        Err(refusal) => refusal,
    }
}

/// The answer to an accept or decline: 204, or 404 for an invitation that
/// is not the user's (or has expired, behaviour `accept-expired-invitation`).
fn answer(shared: &Shared, result: Result<(), Failure>) -> Response {
    match result {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(Failure::Refused(behaviour)) => shared.refuse(behaviour).unwrap_or_else(|| {
            response::error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "octoemu: refusal without a refused behaviour",
                response::OCTOEMU_DOCS,
            )
        }),
        Err(_) => auth::not_found(),
    }
}

/// `PATCH /user/repository_invitations/{invitation_id}`: accept; 204.
pub(crate) async fn accept(
    State(shared): State<SharedState>,
    UserCaller(token): UserCaller,
    Path(raw): Path<String>,
    headers: HeaderMap,
) -> Response {
    if let Some(refusal) = refuse_app_token(&shared, &token) {
        return refusal;
    }
    let Ok(id) = raw.parse::<u64>() else {
        return auth::not_found();
    };
    let origin = views::origin(&shared, &headers);
    let allows = |behaviour: &str| shared.allows(behaviour);
    let ctx = Ctx {
        origin: &origin,
        allows: &allows,
    };
    let result = {
        let mut state = shared.state();
        collaboration::accept_invitation(&mut state, &ctx, &token.login, id)
    };
    answer(&shared, result)
}

/// `DELETE /user/repository_invitations/{invitation_id}`: decline; 204.
pub(crate) async fn decline(
    State(shared): State<SharedState>,
    UserCaller(token): UserCaller,
    Path(raw): Path<String>,
) -> Response {
    if let Some(refusal) = refuse_app_token(&shared, &token) {
        return refusal;
    }
    let Ok(id) = raw.parse::<u64>() else {
        return auth::not_found();
    };
    let result = {
        let mut state = shared.state();
        collaboration::decline_invitation(&mut state, &token.login, id)
    };
    answer(&shared, result)
}
