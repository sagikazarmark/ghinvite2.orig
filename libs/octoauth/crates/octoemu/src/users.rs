//! Users (GE-03): `GET /users/{username}`, `GET /user/{account_id}` and the
//! avatar images at `/avatars/u/{id}`.

use crate::apps::view;
use crate::auth;
use crate::response::json;
use crate::state::SharedState;
use crate::views::{self, BOT_ID_OFFSET};
use axum::extract::{Path, State};
use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};

/// Every avatar: a 4×4 PNG in GitHub's dark grey (`#24292e`).
pub(crate) const AVATAR_PNG: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x04, 0x00, 0x00, 0x00, 0x04, 0x08, 0x02, 0x00, 0x00, 0x00, 0x26, 0x93, 0x09,
    0x29, 0x00, 0x00, 0x00, 0x10, 0x49, 0x44, 0x41, 0x54, 0x78, 0xda, 0x63, 0x50, 0xd1, 0xd4, 0x83,
    0x23, 0x06, 0xe2, 0x38, 0x00, 0xc7, 0x74, 0x07, 0xb1, 0xc7, 0x8e, 0x24, 0x61, 0x00, 0x00, 0x00,
    0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
];

/// `GET /users/{username}`: a user or organization.
pub(crate) async fn get_user(
    State(shared): State<SharedState>,
    Path(login): Path<String>,
    headers: HeaderMap,
) -> Response {
    let origin = views::origin(&shared, &headers);
    let state = shared.state();
    match view(&state, &origin).full_account(&login) {
        Some(body) => json(StatusCode::OK, &body),
        None => auth::not_found(),
    }
}

/// `GET /user/{account_id}`: the same, by numeric ID (ghinvite's spelling;
/// the route is `assumed`).
pub(crate) async fn get_user_by_id(
    State(shared): State<SharedState>,
    Path(raw): Path<String>,
    headers: HeaderMap,
) -> Response {
    let origin = views::origin(&shared, &headers);
    let state = shared.state();
    let login = raw.parse::<u64>().ok().and_then(|id| {
        state
            .world
            .users
            .iter()
            .find(|u| u.id == id)
            .map(|u| u.login.clone())
            .or_else(|| {
                state
                    .world
                    .orgs
                    .iter()
                    .find(|o| o.id == id)
                    .map(|o| o.login.clone())
            })
    });
    match login.and_then(|login| view(&state, &origin).full_account(&login)) {
        Some(body) => json(StatusCode::OK, &body),
        None => auth::not_found(),
    }
}

/// `GET /avatars/u/{id}`: a PNG for any seeded account or App bot; 404
/// otherwise. Query parameters (`s`, `v`) are ignored.
pub(crate) async fn avatar(State(shared): State<SharedState>, Path(raw): Path<String>) -> Response {
    let known = raw.parse::<u64>().ok().is_some_and(|id| {
        let state = shared.state();
        let world = &state.world;
        world.users.iter().any(|u| u.id == id)
            || world.orgs.iter().any(|o| o.id == id)
            || world.apps.iter().any(|a| a.id + BOT_ID_OFFSET == id)
    });
    if !known {
        return auth::not_found();
    }
    (
        StatusCode::OK,
        [
            (CONTENT_TYPE, HeaderValue::from_static("image/png")),
            (CACHE_CONTROL, HeaderValue::from_static("max-age=300")),
        ],
        AVATAR_PNG,
    )
        .into_response()
}
