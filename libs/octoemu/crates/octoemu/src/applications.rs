//! An App's OAuth token management (GE-05), with its client credentials
//! (`Basic`): `POST` (check), `PATCH` (reset) and `DELETE` (revoke)
//! `/applications/{client_id}/token`, and `DELETE
//! /applications/{client_id}/grant` (revoke every token of the user).
//!
//! Documented: <https://docs.github.com/en/rest/apps/oauth-applications>.
//! A token that is not a live user access token of this App, or a
//! `{client_id}` other than the credentials', is a 404 (behaviour
//! `applications-errors`).

#![allow(
    clippy::result_large_err,
    reason = "a rejection is the response to send, built once per request"
)]
use crate::apps::view;
use crate::auth::{ClientCaller, not_found};
use crate::response::{self, FieldError, json};
use crate::state::{Shared, SharedState};
use crate::user_tokens::UserTokenRecord;
use crate::views::{self, timestamp};
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use base64::Engine as _;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const DOCS: &str = "https://docs.github.com/rest/apps/oauth-applications";

/// What a route does with the token.
enum Action {
    Check,
    Reset,
    Delete,
    DeleteGrant,
}

/// A 404, or the refusal of that guess in refuse-assumed mode.
fn missing(shared: &Shared) -> Response {
    shared
        .refuse("applications-errors")
        .unwrap_or_else(not_found)
}

/// `access_token` from the JSON body, or the 422 (documented status).
fn access_token(shared: &Shared, body: &[u8]) -> Result<String, Response> {
    let token = serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(|body| body.get("access_token")?.as_str().map(str::to_owned))
        .filter(|token| !token.is_empty());
    token.ok_or_else(|| {
        if !shared.allows("applications-errors") {
            return response::error(
                StatusCode::UNPROCESSABLE_ENTITY,
                crate::state::WITHHELD,
                DOCS,
            );
        }
        response::validation_failed(
            &[FieldError {
                resource: "OauthAccess".into(),
                field: "access_token".into(),
                code: "missing_field".into(),
                message: None,
            }],
            DOCS,
        )
    })
}

fn handle(
    shared: &Shared,
    app_id: u64,
    client_id: &str,
    headers: &HeaderMap,
    body: &[u8],
    action: Action,
) -> Response {
    let token = match access_token(shared, body) {
        Ok(token) => token,
        Err(response) => return response,
    };
    let origin = views::origin(shared, headers);
    let mut state = shared.state();
    let now = state.clock.now();
    let Some(app) = state
        .world
        .apps
        .iter()
        .find(|app| app.id == app_id && app.client_id == client_id)
        .cloned()
    else {
        return missing(shared);
    };
    let Some(record) = state
        .user_tokens
        .verify(&token, now)
        .ok()
        .filter(|record| record.app_id() == Some(app.id))
        .cloned()
    else {
        return missing(shared);
    };
    match action {
        Action::Delete => {
            state.user_tokens.revoke(&record.id);
            StatusCode::NO_CONTENT.into_response()
        }
        Action::DeleteGrant => {
            state.user_tokens.revoke_grant(app.id, record.user_id);
            StatusCode::NO_CONTENT.into_response()
        }
        Action::Check => json(
            StatusCode::OK,
            &authorization(&state, &origin, &app, &record, &token),
        ),
        Action::Reset => {
            state.user_tokens.revoke(&record.id);
            let minted = state.user_tokens.mint_user_token(
                app.id,
                record.user_id,
                &record.login,
                app.expire_user_tokens,
                false,
                now,
            );
            json(
                StatusCode::OK,
                &authorization(&state, &origin, &app, &minted.record, &minted.token),
            )
        }
    }
}

/// The `authorization` body of the check and reset routes.
fn authorization(
    state: &crate::state::State,
    origin: &str,
    app: &crate::seed::App,
    record: &UserTokenRecord,
    token: &str,
) -> Value {
    let view = view(state, origin);
    let number = record.number();
    json!({
        "id": number,
        "url": format!("{origin}/authorizations/{number}"),
        "scopes": [],
        "token": token,
        "token_last_eight": record.last_eight,
        "hashed_token": base64::engine::general_purpose::STANDARD.encode(Sha256::digest(token.as_bytes())),
        "app": {
            "client_id": app.client_id,
            "name": app.name.as_deref().unwrap_or(&app.slug),
            "url": format!("{origin}/apps/{}", app.slug),
        },
        "note": null,
        "note_url": null,
        "fingerprint": null,
        "created_at": timestamp(record.issued_at),
        "updated_at": timestamp(record.issued_at),
        "expires_at": record.expires_at.map(timestamp),
        "user": view.account(&record.login),
        "installation": null,
    })
}

/// `POST /applications/{client_id}/token`: 200 with the authorization, 404
/// for an invalid token.
pub(crate) async fn check_token(
    State(shared): State<SharedState>,
    ClientCaller(app_id): ClientCaller,
    Path(client_id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    handle(&shared, app_id, &client_id, &headers, &body, Action::Check)
}

/// `PATCH /applications/{client_id}/token`: the token is replaced; 200
/// with the new one.
pub(crate) async fn reset_token(
    State(shared): State<SharedState>,
    ClientCaller(app_id): ClientCaller,
    Path(client_id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    handle(&shared, app_id, &client_id, &headers, &body, Action::Reset)
}

/// `DELETE /applications/{client_id}/token`: 204, the token (and its
/// refresh token) stop working.
pub(crate) async fn delete_token(
    State(shared): State<SharedState>,
    ClientCaller(app_id): ClientCaller,
    Path(client_id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    handle(&shared, app_id, &client_id, &headers, &body, Action::Delete)
}

/// `DELETE /applications/{client_id}/grant`: 204, every token the App holds
/// for the token's user stops working.
pub(crate) async fn delete_grant(
    State(shared): State<SharedState>,
    ClientCaller(app_id): ClientCaller,
    Path(client_id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    handle(
        &shared,
        app_id,
        &client_id,
        &headers,
        &body,
        Action::DeleteGrant,
    )
}
