//! The admin API under `/__octoemu/`: the emulator's own controls, never part
//! of GitHub's surface.

use crate::clock::MAX_ADVANCE_SECONDS;
use crate::response::{OCTOEMU_DOCS, error, json};
use crate::seed::Seed;
use crate::state::SharedState;
use axum::Router;
use axum::body::Bytes;
use axum::extract::Path;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::json;

pub(crate) fn router() -> Router<SharedState> {
    Router::new()
        .route("/__octoemu/health", get(health))
        .route("/__octoemu/state", get(state))
        .route("/__octoemu/seed", post(seed))
        .route("/__octoemu/clock", get(clock))
        .route("/__octoemu/clock/advance", post(advance))
        .route("/__octoemu/calls", get(calls).delete(clear_calls))
        .route("/__octoemu/fidelity", get(fidelity))
        .route(
            "/__octoemu/tokens/installation",
            post(mint_installation_token),
        )
        .route("/__octoemu/installations/{id}", delete(uninstall))
        .route("/__octoemu/installations/{id}/suspend", post(suspend))
        .route("/__octoemu/installations/{id}/unsuspend", post(unsuspend))
        .route("/__octoemu/events", get(events))
        .route(
            "/__octoemu/act/{login}/repository_invitations/{id}/{action}",
            post(act_on_invitation),
        )
        .route(
            "/__octoemu/act/{login}/repos/{owner}/{repo}/leave",
            post(leave_repository),
        )
}

/// A JSON body, or the 400 (in GitHub's error shape) to answer instead.
fn body<T: DeserializeOwned>(bytes: &Bytes) -> Result<T, Box<Response>> {
    serde_json::from_slice(bytes).map_err(|e| {
        Box::new(error(
            StatusCode::BAD_REQUEST,
            &format!("octoemu: invalid request body: {e}"),
            OCTOEMU_DOCS,
        ))
    })
}

async fn health() -> Response {
    json(
        StatusCode::OK,
        &json!({"status": "ok", "version": env!("CARGO_PKG_VERSION")}),
    )
}

async fn state(State(shared): State<SharedState>) -> Response {
    json(StatusCode::OK, &shared.dump())
}

async fn seed(State(shared): State<SharedState>, bytes: Bytes) -> Response {
    let more: Seed = match body(&bytes) {
        Ok(more) => more,
        Err(response) => return *response,
    };
    match shared.merge(more) {
        Ok(()) => json(StatusCode::OK, &shared.dump()),
        Err(rejected) => json(
            StatusCode::UNPROCESSABLE_ENTITY,
            &json!({
                "message": "octoemu: seed rejected",
                "errors": rejected.problems(),
                "documentation_url": OCTOEMU_DOCS,
            }),
        ),
    }
}

fn clock_body(shared: &SharedState) -> serde_json::Value {
    let state = shared.state();
    json!({
        "now": state.clock.now(),
        "advanced_seconds": state.clock.advanced_seconds(),
    })
}

async fn clock(State(shared): State<SharedState>) -> Response {
    json(StatusCode::OK, &clock_body(&shared))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Advance {
    seconds: u64,
}

async fn advance(State(shared): State<SharedState>, bytes: Bytes) -> Response {
    let Advance { seconds } = match body(&bytes) {
        Ok(advance) => advance,
        Err(response) => return *response,
    };
    if seconds > MAX_ADVANCE_SECONDS {
        return error(
            StatusCode::UNPROCESSABLE_ENTITY,
            &format!("octoemu: advance at most {MAX_ADVANCE_SECONDS} seconds at a time"),
            OCTOEMU_DOCS,
        );
    }
    shared.state().clock.advance(seconds);
    json(StatusCode::OK, &clock_body(&shared))
}

async fn calls(State(shared): State<SharedState>) -> Response {
    let state = shared.state();
    json(
        StatusCode::OK,
        &json!({"calls": state.calls.calls(), "dropped": state.calls.dropped()}),
    )
}

async fn clear_calls(State(shared): State<SharedState>) -> Response {
    shared.state().calls.clear();
    StatusCode::NO_CONTENT.into_response()
}

async fn fidelity(State(shared): State<SharedState>) -> Response {
    json(StatusCode::OK, shared.manifest())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Mint {
    installation_id: u64,
    #[serde(default)]
    permissions: Option<std::collections::BTreeMap<String, String>>,
    #[serde(default)]
    repositories: Option<Vec<String>>,
    #[serde(default)]
    repository_ids: Option<Vec<u64>>,
}

/// `POST /__octoemu/tokens/installation`: a token for any installation
/// without an App JWT, answered (and narrowed, and refused) exactly as
/// `POST /app/installations/{id}/access_tokens`.
async fn mint_installation_token(
    State(shared): State<SharedState>,
    headers: HeaderMap,
    bytes: Bytes,
) -> Response {
    let mint: Mint = match body(&bytes) {
        Ok(mint) => mint,
        Err(response) => return *response,
    };
    let request = crate::apps::TokenRequest {
        permissions: mint.permissions,
        repositories: mint.repositories,
        repository_ids: mint.repository_ids,
    };
    let origin = crate::views::origin(&shared, &headers);
    crate::apps::issue(&shared, None, mint.installation_id, &request, &origin)
}

fn no_installation() -> Response {
    error(
        StatusCode::NOT_FOUND,
        "octoemu: no such installation",
        OCTOEMU_DOCS,
    )
}

fn done(found: bool) -> Response {
    if found {
        StatusCode::NO_CONTENT.into_response()
    } else {
        no_installation()
    }
}

async fn suspend(State(shared): State<SharedState>, Path(id): Path<u64>) -> Response {
    done(crate::apps::set_suspended(&shared, id, None, true))
}

async fn unsuspend(State(shared): State<SharedState>, Path(id): Path<u64>) -> Response {
    done(crate::apps::set_suspended(&shared, id, None, false))
}

async fn uninstall(State(shared): State<SharedState>, Path(id): Path<u64>) -> Response {
    done(crate::apps::uninstall(&shared, id))
}

async fn events(State(shared): State<SharedState>) -> Response {
    json(
        StatusCode::OK,
        &json!({"events": shared.state().events.events()}),
    )
}

/// The answer to an `act` route: 204, or why nothing happened.
fn acted(result: Result<(), crate::collaboration::Failure>, shared: &SharedState) -> Response {
    use crate::collaboration::Failure;
    match result {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(Failure::NotFound) => error(
            StatusCode::NOT_FOUND,
            "octoemu: no such invitation, repository or collaborator for this user",
            OCTOEMU_DOCS,
        ),
        Err(Failure::Expired) => error(
            StatusCode::NOT_FOUND,
            "octoemu: the invitation has expired",
            OCTOEMU_DOCS,
        ),
        Err(Failure::Refused(behaviour)) => shared.refuse(behaviour).unwrap_or_else(|| {
            error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "octoemu: refused",
                OCTOEMU_DOCS,
            )
        }),
        Err(other) => error(
            StatusCode::UNPROCESSABLE_ENTITY,
            &format!("octoemu: {other:?}"),
            OCTOEMU_DOCS,
        ),
    }
}

/// `POST /__octoemu/act/{login}/repository_invitations/{id}/accept|decline`:
/// the invitee's side of an invitation, as `PATCH`/`DELETE
/// /user/repository_invitations/{id}` with their user token will do
/// (GE-05). Same domain operations, same events.
async fn act_on_invitation(
    State(shared): State<SharedState>,
    Path((login, id, action)): Path<(String, String, String)>,
    headers: HeaderMap,
) -> Response {
    let Ok(id) = id.parse::<u64>() else {
        return acted(Err(crate::collaboration::Failure::NotFound), &shared);
    };
    let origin = crate::views::origin(&shared, &headers);
    let allows = |behaviour: &str| shared.allows(behaviour);
    let ctx = crate::collaboration::Ctx {
        origin: &origin,
        allows: &allows,
    };
    let result = {
        let mut state = shared.state();
        match action.as_str() {
            "accept" => crate::collaboration::accept_invitation(&mut state, &ctx, &login, id),
            "decline" => crate::collaboration::decline_invitation(&mut state, &login, id),
            _ => {
                return error(
                    StatusCode::NOT_FOUND,
                    "octoemu: no such admin route",
                    OCTOEMU_DOCS,
                );
            }
        }
    };
    acted(result, &shared)
}

/// `POST /__octoemu/act/{login}/repos/{owner}/{repo}/leave`: a
/// collaborator leaves a repository (`member.removed`, sent by them).
async fn leave_repository(
    State(shared): State<SharedState>,
    Path((login, owner, repo)): Path<(String, String, String)>,
    headers: HeaderMap,
) -> Response {
    let origin = crate::views::origin(&shared, &headers);
    let allows = |behaviour: &str| shared.allows(behaviour);
    let ctx = crate::collaboration::Ctx {
        origin: &origin,
        allows: &allows,
    };
    let result = {
        let mut state = shared.state();
        let repo_id = state
            .world
            .repos
            .iter()
            .find(|r| r.owner.eq_ignore_ascii_case(&owner) && r.name.eq_ignore_ascii_case(&repo))
            .map(|r| r.id);
        match repo_id {
            Some(repo_id) => {
                crate::collaboration::leave_repository(&mut state, &ctx, &login, repo_id)
            }
            None => Err(crate::collaboration::Failure::NotFound),
        }
    };
    acted(result, &shared)
}
