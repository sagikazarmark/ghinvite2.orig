//! The admin API under `/__octoemu/`: the emulator's own controls, never part
//! of GitHub's surface.

use crate::clock::MAX_ADVANCE_SECONDS;
use crate::collaboration::Actor;
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
use axum::routing::{delete, get, post, put};
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
        .route("/__octoemu/tokens/user", post(mint_user_token))
        .route("/__octoemu/oauth", get(oauth).post(set_oauth))
        .route("/__octoemu/oauth/deny", post(deny).delete(allow))
        .route("/__octoemu/installations", post(install))
        .route("/__octoemu/installations/{id}", delete(uninstall))
        .route("/__octoemu/installations/{id}/suspend", post(suspend))
        .route("/__octoemu/installations/{id}/unsuspend", post(unsuspend))
        .route(
            "/__octoemu/installations/{id}/permissions",
            post(accept_permissions),
        )
        .route(
            "/__octoemu/installations/{id}/repositories",
            post(set_repositories),
        )
        .route(
            "/__octoemu/apps/{id}/webhook",
            put(set_webhook).delete(remove_webhook),
        )
        .route("/__octoemu/events", get(events))
        .route("/__octoemu/deliveries", get(deliveries))
        .route("/__octoemu/deliveries/dispatch", post(dispatch))
        .route("/__octoemu/deliveries/wait", post(wait))
        .route("/__octoemu/deliveries/mode", post(set_mode))
        .route(
            "/__octoemu/act/{login}/repository_invitations/{id}/{action}",
            post(act_on_invitation),
        )
        .route(
            "/__octoemu/act/{login}/repos/{owner}/{repo}/leave",
            post(leave_repository),
        )
        .route(
            "/__octoemu/act/{login}/orgs/{org}/{action}",
            post(act_on_org),
        )
        .route("/__octoemu/orgs/{org}/spammed", post(set_spammed))
        .route("/__octoemu/users/{login}/rename", post(rename_user))
        .route(
            "/__octoemu/repository_invitations/{id}/expire",
            post(expire_invitation),
        )
        .route(
            "/__octoemu/faults",
            get(list_faults).post(add_fault).delete(clear_faults),
        )
        .route(
            "/__octoemu/faults/{id}",
            get(get_fault).delete(remove_fault),
        )
        .route("/__octoemu/faults/{id}/release", post(release_fault))
        .route("/__octoemu/faults/{id}/wait", post(wait_for_held))
}

fn no_such_fault() -> Response {
    error(
        StatusCode::NOT_FOUND,
        "octoemu: no such fault rule",
        OCTOEMU_DOCS,
    )
}

/// `POST /__octoemu/faults`: adds a rule (`crate::faults::Fault`); 201
/// with it as listed, 422 for a bad pattern, status or header.
async fn add_fault(State(shared): State<SharedState>, bytes: Bytes) -> Response {
    let fault: crate::faults::Fault = match body(&bytes) {
        Ok(fault) => fault,
        Err(response) => return *response,
    };
    let mut state = shared.state();
    match state.faults.add(fault) {
        Ok(id) => json(StatusCode::CREATED, &state.faults.get(id)),
        Err(refused) => error(
            StatusCode::UNPROCESSABLE_ENTITY,
            &refused.to_string(),
            OCTOEMU_DOCS,
        ),
    }
}

async fn list_faults(State(shared): State<SharedState>) -> Response {
    json(
        StatusCode::OK,
        &json!({"faults": shared.state().faults.list()}),
    )
}

async fn clear_faults(State(shared): State<SharedState>) -> Response {
    shared.state().faults.clear();
    StatusCode::NO_CONTENT.into_response()
}

async fn get_fault(State(shared): State<SharedState>, Path(id): Path<u64>) -> Response {
    match shared.state().faults.get(id) {
        Some(status) => json(StatusCode::OK, &status),
        None => no_such_fault(),
    }
}

async fn remove_fault(State(shared): State<SharedState>, Path(id): Path<u64>) -> Response {
    if shared.state().faults.remove(id) {
        StatusCode::NO_CONTENT.into_response()
    } else {
        no_such_fault()
    }
}

async fn release_fault(State(shared): State<SharedState>, Path(id): Path<u64>) -> Response {
    if shared.state().faults.release(id) {
        StatusCode::NO_CONTENT.into_response()
    } else {
        no_such_fault()
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WaitHeld {
    #[serde(default = "one")]
    held: usize,
    #[serde(default = "thirty_seconds")]
    timeout_ms: u64,
}

fn one() -> usize {
    1
}

fn thirty_seconds() -> u64 {
    30_000
}

/// `POST /__octoemu/faults/{id}/wait` `{"held"?: n (1), "timeout_ms"?}`:
/// 200 `{"held": n}` once the hold rule holds at least `n` requests, 504 at
/// the timeout.
async fn wait_for_held(
    State(shared): State<SharedState>,
    Path(id): Path<u64>,
    bytes: Bytes,
) -> Response {
    let WaitHeld { held, timeout_ms } = if bytes.is_empty() {
        WaitHeld {
            held: 1,
            timeout_ms: thirty_seconds(),
        }
    } else {
        match body(&bytes) {
            Ok(wait) => wait,
            Err(response) => return *response,
        }
    };
    let Some(gate) = shared.state().faults.gate(id) else {
        return no_such_fault();
    };
    if gate
        .held(held, std::time::Duration::from_millis(timeout_ms))
        .await
    {
        json(StatusCode::OK, &json!({"held": held}))
    } else {
        error(
            StatusCode::GATEWAY_TIMEOUT,
            "octoemu: timed out waiting for held requests",
            OCTOEMU_DOCS,
        )
    }
}

/// `POST /__octoemu/act/{login}/orgs/{org}/accept|decline|leave`: the
/// user's side of an organization membership, as `PATCH
/// /user/memberships/orgs/{org}` does with their user token (accept), plus
/// declining an invitation and leaving.
async fn act_on_org(
    State(shared): State<SharedState>,
    Path((login, org, action)): Path<(String, String, String)>,
    headers: HeaderMap,
) -> Response {
    let origin = crate::views::origin(&shared, &headers);
    crate::orgs::act(&shared, &origin, &login, &org, &action)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Spammed {
    spammed: bool,
}

/// `POST /__octoemu/orgs/{org}/spammed` `{"spammed": bool}`: the
/// organization's invitation writes (create, cancel) answer GitHub's
/// `spammed` 422 (assumed shape) while on. 204; 404 for an unknown
/// organization.
async fn set_spammed(
    State(shared): State<SharedState>,
    Path(org): Path<String>,
    bytes: Bytes,
) -> Response {
    let Spammed { spammed } = match body(&bytes) {
        Ok(body) => body,
        Err(response) => return *response,
    };
    if crate::orgs::set_spammed(&shared, &org, spammed) {
        StatusCode::NO_CONTENT.into_response()
    } else {
        error(
            StatusCode::NOT_FOUND,
            "octoemu: no such organization",
            OCTOEMU_DOCS,
        )
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Rename {
    login: String,
}

/// `POST /__octoemu/users/{login}/rename` `{"login"}`: the user changes
/// their login; their ID, collaborations, memberships and invitations stay
/// theirs (`user-rename`). 204; 404 for an unknown user; 422 for a login
/// that is taken, or a user owning repositories, an App or an installation
/// (an emulator limitation).
async fn rename_user(
    State(shared): State<SharedState>,
    Path(login): Path<String>,
    bytes: Bytes,
) -> Response {
    let Rename { login: new } = match body(&bytes) {
        Ok(rename) => rename,
        Err(response) => return *response,
    };
    if new.is_empty() || !new.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return unprocessable("a login is letters, digits and hyphens");
    }
    let mut state = shared.state();
    if !state
        .world
        .users
        .iter()
        .any(|user| user.login.eq_ignore_ascii_case(&login))
    {
        return error(StatusCode::NOT_FOUND, "octoemu: no such user", OCTOEMU_DOCS);
    }
    let mut world = state.world.clone();
    match crate::users::rename_user(&mut world, &login, &new) {
        Ok(()) => {
            state.world = world;
            StatusCode::NO_CONTENT.into_response()
        }
        Err(problem) => unprocessable(&problem),
    }
}

/// `POST /__octoemu/repository_invitations/{id}/expire`: dates the
/// invitation back past its 7 days on the virtual clock, so it is expired
/// now without moving the clock for everything else. 204; 404 if unknown.
async fn expire_invitation(State(shared): State<SharedState>, Path(id): Path<String>) -> Response {
    let mut state = shared.state();
    let now = state.clock.now();
    let invitation = id.parse::<u64>().ok().and_then(|id| {
        state
            .world
            .repository_invitations
            .iter_mut()
            .find(|invitation| invitation.id == id)
    });
    match invitation {
        Some(invitation) => {
            invitation.created_at =
                Some(now - crate::views::INVITATION_LIFETIME - chrono::TimeDelta::seconds(1));
            StatusCode::NO_CONTENT.into_response()
        }
        None => error(
            StatusCode::NOT_FOUND,
            "octoemu: no such invitation",
            OCTOEMU_DOCS,
        ),
    }
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

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MintUser {
    login: String,
    #[serde(default)]
    app_id: Option<u64>,
    /// `user_token` (default) or `pat`.
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    scopes: Option<Vec<String>>,
}

fn unprocessable(message: &str) -> Response {
    error(
        StatusCode::UNPROCESSABLE_ENTITY,
        &format!("octoemu: {message}"),
        OCTOEMU_DOCS,
    )
}

/// `POST /__octoemu/tokens/user`: `{"login", "app_id"}` mints a `ghu_`
/// user access token of that App (with a `ghr_` refresh token when the
/// App's tokens expire); `{"login", "kind": "pat", "scopes"}` a classic
/// `ghp_` personal access token. 201 `{token, token_id, expires_at,
/// refresh_token?}`.
async fn mint_user_token(State(shared): State<SharedState>, bytes: Bytes) -> Response {
    use crate::user_tokens::MintRequest;
    let mint: MintUser = match body(&bytes) {
        Ok(mint) => mint,
        Err(response) => return *response,
    };
    let request = match (mint.kind.as_deref(), mint.app_id, mint.scopes) {
        (None | Some("user_token"), Some(app_id), None) => MintRequest::UserToken { app_id },
        (Some("pat"), None, scopes) => MintRequest::PersonalAccessToken {
            scopes: scopes.unwrap_or_default(),
        },
        _ => {
            return unprocessable(
                "send {\"login\", \"app_id\"} or {\"login\", \"kind\": \"pat\", \"scopes\"}",
            );
        }
    };
    let minted = crate::user_tokens::mint_for(&mut shared.state(), &mint.login, request);
    match minted {
        Ok(minted) => json(
            StatusCode::CREATED,
            &json!({
                "token": minted.token,
                "token_id": minted.record.id,
                "expires_at": minted.record.expires_at,
                "refresh_token": minted.refresh_token,
            }),
        ),
        Err(message) => unprocessable(&message),
    }
}

fn oauth_body(shared: &SharedState) -> Response {
    let state = shared.state();
    json(StatusCode::OK, &state.oauth.view(&state.world))
}

/// `GET /__octoemu/oauth`: the authorize page's settings.
async fn oauth(State(shared): State<SharedState>) -> Response {
    oauth_body(&shared)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OAuthSettings {
    #[serde(default)]
    auto_consent: Option<bool>,
    #[serde(default)]
    deny_all: Option<bool>,
    #[serde(default)]
    denied_users: Option<Vec<String>>,
    #[serde(default)]
    user_tokens_per_hour: Option<u32>,
}

/// `POST /__octoemu/oauth`: `{"auto_consent"?, "deny_all"?,
/// "denied_users"?, "user_tokens_per_hour"?}` changes what is given.
async fn set_oauth(State(shared): State<SharedState>, bytes: Bytes) -> Response {
    let settings: OAuthSettings = match body(&bytes) {
        Ok(settings) => settings,
        Err(response) => return *response,
    };
    {
        let mut state = shared.state();
        if let Some(on) = settings.auto_consent {
            state.oauth.auto_consent = on;
        }
        if let Some(on) = settings.deny_all {
            state.oauth.deny_all = on;
        }
        if let Some(users) = settings.denied_users {
            state.oauth.denied = users.iter().map(|u| u.to_lowercase()).collect();
        }
        if let Some(limit) = settings.user_tokens_per_hour {
            state.world.settings.user_tokens_per_hour = Some(limit);
        }
    }
    oauth_body(&shared)
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Deny {
    #[serde(default)]
    login: Option<String>,
}

/// `POST /__octoemu/oauth/deny`: `{"login"}` makes that user decline every
/// authorization (`error=access_denied`); an empty body makes everyone.
async fn deny(State(shared): State<SharedState>, bytes: Bytes) -> Response {
    let Deny { login } = match optional_body(&bytes) {
        Ok(deny) => deny,
        Err(response) => return *response,
    };
    {
        let mut state = shared.state();
        match login {
            None => state.oauth.deny_all = true,
            Some(login) => {
                if !state
                    .world
                    .users
                    .iter()
                    .any(|u| u.login.eq_ignore_ascii_case(&login))
                {
                    return unprocessable(&format!("no user {login:?}"));
                }
                state.oauth.denied.insert(login.to_lowercase());
            }
        }
    }
    oauth_body(&shared)
}

/// `DELETE /__octoemu/oauth/deny`: nobody declines any more.
async fn allow(State(shared): State<SharedState>) -> Response {
    {
        let mut state = shared.state();
        state.oauth.deny_all = false;
        state.oauth.denied.clear();
    }
    oauth_body(&shared)
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

/// An optional `{"sender": login}` body: who an installation change is
/// attributed to (otherwise `events::default_sender`).
#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct By {
    #[serde(default)]
    sender: Option<String>,
}

/// The body (empty allowed) as `T`.
fn optional_body<T: DeserializeOwned + Default>(bytes: &Bytes) -> Result<T, Box<Response>> {
    if bytes.iter().all(u8::is_ascii_whitespace) {
        Ok(T::default())
    } else {
        body(bytes)
    }
}

/// `sender` as an actor; a login that names no account is a 422.
fn actor(shared: &SharedState, sender: Option<String>) -> Result<Option<Actor>, Box<Response>> {
    let Some(login) = sender else {
        return Ok(None);
    };
    let state = shared.state();
    let known = state
        .world
        .users
        .iter()
        .map(|u| &u.login)
        .chain(state.world.orgs.iter().map(|o| &o.login))
        .find(|l| l.eq_ignore_ascii_case(&login))
        .cloned();
    match known {
        Some(login) => Ok(Some(Actor::User(login))),
        None => Err(Box::new(error(
            StatusCode::UNPROCESSABLE_ENTITY,
            &format!("octoemu: no account {login:?} to send the event"),
            OCTOEMU_DOCS,
        ))),
    }
}

/// The parsed body's sender, or the response to send instead.
macro_rules! sender_of {
    ($shared:expr, $bytes:expr) => {
        match optional_body::<By>(&$bytes).and_then(|by| actor(&$shared, by.sender)) {
            Ok(actor) => actor,
            Err(response) => return *response,
        }
    };
}

async fn suspend(State(shared): State<SharedState>, Path(id): Path<u64>, bytes: Bytes) -> Response {
    let actor = sender_of!(shared, bytes);
    done(crate::apps::set_suspended(&shared, id, None, true, actor))
}

async fn unsuspend(
    State(shared): State<SharedState>,
    Path(id): Path<u64>,
    bytes: Bytes,
) -> Response {
    let actor = sender_of!(shared, bytes);
    done(crate::apps::set_suspended(&shared, id, None, false, actor))
}

async fn uninstall(
    State(shared): State<SharedState>,
    Path(id): Path<u64>,
    bytes: Bytes,
) -> Response {
    let actor = sender_of!(shared, bytes);
    done(crate::apps::uninstall(&shared, id, actor))
}

fn rejected(errors: &crate::seed::SeedError) -> Response {
    json(
        StatusCode::UNPROCESSABLE_ENTITY,
        &json!({
            "message": "octoemu: seed rejected",
            "errors": errors.problems(),
            "documentation_url": OCTOEMU_DOCS,
        }),
    )
}

fn lifecycle(result: Result<(), crate::apps::LifecycleError>) -> Response {
    match result {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(crate::apps::LifecycleError::NotFound) => no_installation(),
        Err(crate::apps::LifecycleError::Invalid(errors)) => rejected(&errors),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Install {
    installation: crate::seed::Installation,
    #[serde(default)]
    sender: Option<String>,
}

/// `POST /__octoemu/installations`: `{"installation": <seed installation>,
/// "sender"?}` installs an App (`installation.created`); 201, or 422 with
/// the seed problems.
async fn install(State(shared): State<SharedState>, bytes: Bytes) -> Response {
    let Install {
        installation,
        sender,
    } = match body(&bytes) {
        Ok(install) => install,
        Err(response) => return *response,
    };
    let actor = match actor(&shared, sender) {
        Ok(actor) => actor,
        Err(response) => return *response,
    };
    match crate::apps::install(&shared, installation, actor) {
        Ok(()) => StatusCode::CREATED.into_response(),
        Err(errors) => rejected(&errors),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Permissions {
    permissions: std::collections::BTreeMap<String, crate::seed::Access>,
    #[serde(default)]
    sender: Option<String>,
}

/// `POST /__octoemu/installations/{id}/permissions`: `{"permissions",
/// "sender"?}`, the owner accepts new permissions
/// (`installation.new_permissions_accepted`).
async fn accept_permissions(
    State(shared): State<SharedState>,
    Path(id): Path<u64>,
    bytes: Bytes,
) -> Response {
    let Permissions {
        permissions,
        sender,
    } = match body(&bytes) {
        Ok(permissions) => permissions,
        Err(response) => return *response,
    };
    let actor = match actor(&shared, sender) {
        Ok(actor) => actor,
        Err(response) => return *response,
    };
    lifecycle(crate::apps::accept_permissions(
        &shared,
        id,
        permissions,
        actor,
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Repositories {
    #[serde(default = "selected")]
    repository_selection: crate::seed::RepositorySelection,
    #[serde(default)]
    repositories: Vec<String>,
    #[serde(default)]
    sender: Option<String>,
}

fn selected() -> crate::seed::RepositorySelection {
    crate::seed::RepositorySelection::Selected
}

/// `POST /__octoemu/installations/{id}/repositories`:
/// `{"repository_selection"? (default selected), "repositories", "sender"?}`
/// replaces the selection (`installation_repositories.added` / `removed`).
async fn set_repositories(
    State(shared): State<SharedState>,
    Path(id): Path<u64>,
    bytes: Bytes,
) -> Response {
    let Repositories {
        repository_selection,
        repositories,
        sender,
    } = match body(&bytes) {
        Ok(repositories) => repositories,
        Err(response) => return *response,
    };
    let actor = match actor(&shared, sender) {
        Ok(actor) => actor,
        Err(response) => return *response,
    };
    lifecycle(crate::apps::set_repositories(
        &shared,
        id,
        repository_selection,
        repositories,
        actor,
    ))
}

fn no_app() -> Response {
    error(StatusCode::NOT_FOUND, "octoemu: no such App", OCTOEMU_DOCS)
}

fn replace_webhook(
    shared: &SharedState,
    id: u64,
    webhook: Option<crate::seed::Webhook>,
) -> Response {
    let mut state = shared.state();
    match state.world.apps.iter_mut().find(|app| app.id == id) {
        Some(app) => {
            app.webhook = webhook;
            StatusCode::NO_CONTENT.into_response()
        }
        None => no_app(),
    }
}

/// `PUT /__octoemu/apps/{id}/webhook`: a seed webhook (`{"url",
/// "secrets", "active"}`) replaces the App's; the first secret signs from
/// now on.
async fn set_webhook(
    State(shared): State<SharedState>,
    Path(id): Path<u64>,
    bytes: Bytes,
) -> Response {
    match body(&bytes) {
        Ok(webhook) => replace_webhook(&shared, id, Some(webhook)),
        Err(response) => *response,
    }
}

/// `DELETE /__octoemu/apps/{id}/webhook`: no more deliveries for the App.
async fn remove_webhook(State(shared): State<SharedState>, Path(id): Path<u64>) -> Response {
    replace_webhook(&shared, id, None)
}

/// `GET /__octoemu/deliveries`: every attempt, oldest first, with the
/// queue's state.
async fn deliveries(State(shared): State<SharedState>) -> Response {
    let mut state = shared.state();
    crate::webhooks::pump(&mut state);
    let deliveries = &state.deliveries;
    json(
        StatusCode::OK,
        &json!({
            "deliveries": deliveries.attempts().collect::<Vec<_>>(),
            "mode": deliveries.mode,
            "held": deliveries.held(),
            "pending": deliveries.pending(),
        }),
    )
}

/// `POST /__octoemu/deliveries/dispatch`: sends what manual mode holds.
async fn dispatch(State(shared): State<SharedState>) -> Response {
    let released = crate::webhooks::dispatch(&shared);
    json(StatusCode::OK, &json!({"dispatched": released}))
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Wait {
    #[serde(default)]
    timeout_ms: Option<u64>,
}

/// `POST /__octoemu/deliveries/wait` (`{"timeout_ms"?}`, default 30 000):
/// 200 once nothing is queued or being sent (held deliveries excepted),
/// else 504 at the timeout.
async fn wait(State(shared): State<SharedState>, bytes: Bytes) -> Response {
    let Wait { timeout_ms } = match optional_body(&bytes) {
        Ok(wait) => wait,
        Err(response) => return *response,
    };
    let timeout = std::time::Duration::from_millis(timeout_ms.unwrap_or(30_000));
    if crate::webhooks::wait_idle(&shared, timeout).await {
        let state = shared.state();
        json(
            StatusCode::OK,
            &json!({"idle": true, "held": state.deliveries.held()}),
        )
    } else {
        error(
            StatusCode::GATEWAY_TIMEOUT,
            "octoemu: deliveries still in flight",
            OCTOEMU_DOCS,
        )
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Mode {
    mode: crate::webhooks::Dispatch,
}

/// `POST /__octoemu/deliveries/mode` (`{"mode": "auto"|"manual"}`);
/// switching to auto sends what was held.
async fn set_mode(State(shared): State<SharedState>, bytes: Bytes) -> Response {
    let Mode { mode } = match body(&bytes) {
        Ok(mode) => mode,
        Err(response) => return *response,
    };
    shared.state().deliveries.set_mode(mode);
    shared.wake.notify_one();
    json(StatusCode::OK, &json!({"mode": mode}))
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
