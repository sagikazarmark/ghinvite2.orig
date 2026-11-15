//! The App and installation surface: `/app`, `/app/installations…`,
//! `/app/installation-requests`, `/installation/repositories` and
//! `/installation/token`, plus the installation logic the admin API shares
//! (minting, suspension, uninstalling).

use crate::auth::{self, AppCaller, InstallationCaller, SUSPENDED, find_installation};
use crate::collaboration::Actor;
use crate::events;
use crate::paginate::Pagination;
use crate::response::{self, FieldError, json, json_with_link};
use crate::seed::{Access, Installation, Repo, RepositorySelection, Seed};
use crate::state::{SharedState, State as World};
use crate::tokens::TokenRecord;
use crate::views::{self, View, timestamp};
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use chrono::DateTime;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

/// The most repositories a token may be narrowed to (documented).
pub(crate) const MAX_TOKEN_REPOSITORIES: usize = 500;

const CREATE_TOKEN_DOCS: &str =
    "https://docs.github.com/rest/apps/apps#create-an-installation-access-token-for-an-app";

/// The installation's granted permissions: its own, or its App's.
pub(crate) fn granted(world: &Seed, installation: &Installation) -> BTreeMap<String, Access> {
    installation.permissions.clone().unwrap_or_else(|| {
        world
            .apps
            .iter()
            .find(|app| app.id == installation.app_id)
            .map(|app| app.permissions.clone())
            .unwrap_or_default()
    })
}

/// Every repository the installation can access, by ID.
pub(crate) fn installation_repos<'a>(
    world: &'a Seed,
    installation: &Installation,
) -> Vec<&'a Repo> {
    let mut repos: Vec<&Repo> = match installation.repository_selection {
        RepositorySelection::All => world
            .repos
            .iter()
            .filter(|repo| repo.owner.eq_ignore_ascii_case(&installation.account))
            .collect(),
        RepositorySelection::Selected => world
            .repos
            .iter()
            .filter(|repo| {
                installation
                    .repositories
                    .iter()
                    .any(|name| name.eq_ignore_ascii_case(&repo.full_name()))
            })
            .collect(),
    };
    repos.sort_by_key(|repo| repo.id);
    repos
}

pub(crate) fn view<'a>(state: &'a World, origin: &'a str) -> View<'a> {
    View {
        world: &state.world,
        origin,
        start: state.clock.start(),
        now: state.clock.now(),
    }
}

/// `{installation_id}` as a number; anything else names no installation.
fn installation_id(raw: &str) -> Option<u64> {
    raw.parse().ok()
}

// --- JWT routes ------------------------------------------------------------

/// `GET /app`.
pub(crate) async fn get_app(
    State(shared): State<SharedState>,
    AppCaller(app_id): AppCaller,
    headers: HeaderMap,
) -> Response {
    let origin = views::origin(&shared, &headers);
    let state = shared.state();
    match state.world.apps.iter().find(|app| app.id == app_id) {
        Some(app) => json(StatusCode::OK, &view(&state, &origin).app(app)),
        None => auth::not_found(),
    }
}

/// `GET /app/installations`, paged.
pub(crate) async fn list_installations(
    State(shared): State<SharedState>,
    AppCaller(app_id): AppCaller,
    headers: HeaderMap,
    uri: Uri,
) -> Response {
    let origin = views::origin(&shared, &headers);
    let state = shared.state();
    let view = view(&state, &origin);
    let mut installations: Vec<&Installation> = state
        .world
        .installations
        .iter()
        .filter(|i| i.app_id == app_id)
        .collect();
    installations.sort_by_key(|i| i.id);
    let paged = Pagination::of(&uri).apply(installations);
    let body: Vec<Value> = paged.items.iter().map(|i| view.installation(i)).collect();
    json_with_link(StatusCode::OK, &body, paged.link(&origin, &uri))
}

/// `GET /app/installations/{installation_id}`.
pub(crate) async fn get_installation(
    State(shared): State<SharedState>,
    AppCaller(app_id): AppCaller,
    Path(raw): Path<String>,
    headers: HeaderMap,
) -> Response {
    let origin = views::origin(&shared, &headers);
    let state = shared.state();
    match installation_id(&raw)
        .and_then(|id| find_installation(&state.world, id))
        .filter(|i| i.app_id == app_id)
    {
        Some(installation) => json(
            StatusCode::OK,
            &view(&state, &origin).installation(installation),
        ),
        None => auth::not_found(),
    }
}

/// `POST /app/installations/{installation_id}/access_tokens`.
pub(crate) async fn create_token(
    State(shared): State<SharedState>,
    AppCaller(app_id): AppCaller,
    Path(raw): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let Some(id) = installation_id(&raw) else {
        return auth::not_found();
    };
    let request = match TokenRequest::parse(&body) {
        Ok(request) => request,
        Err(response) => return *response,
    };
    let origin = views::origin(&shared, &headers);
    issue(&shared, Some(app_id), id, &request, &origin)
}

/// `PUT /app/installations/{installation_id}/suspended`.
pub(crate) async fn suspend(
    State(shared): State<SharedState>,
    AppCaller(app_id): AppCaller,
    Path(raw): Path<String>,
) -> Response {
    no_content_or_404(installation_id(&raw).is_some_and(|id| {
        set_suspended(&shared, id, Some(app_id), true, Some(Actor::App { app_id }))
    }))
}

/// `DELETE /app/installations/{installation_id}/suspended`.
pub(crate) async fn unsuspend(
    State(shared): State<SharedState>,
    AppCaller(app_id): AppCaller,
    Path(raw): Path<String>,
) -> Response {
    no_content_or_404(installation_id(&raw).is_some_and(|id| {
        set_suspended(
            &shared,
            id,
            Some(app_id),
            false,
            Some(Actor::App { app_id }),
        )
    }))
}

fn no_content_or_404(found: bool) -> Response {
    if found {
        StatusCode::NO_CONTENT.into_response()
    } else {
        auth::not_found()
    }
}

/// `GET /app/installation-requests`, paged.
pub(crate) async fn installation_requests(
    State(shared): State<SharedState>,
    AppCaller(app_id): AppCaller,
    headers: HeaderMap,
    uri: Uri,
) -> Response {
    let origin = views::origin(&shared, &headers);
    let state = shared.state();
    let view = view(&state, &origin);
    let mut requests: Vec<_> = state
        .world
        .installation_requests
        .iter()
        .filter(|r| r.app_id == app_id)
        .collect();
    requests.sort_by_key(|r| r.id);
    let paged = Pagination::of(&uri).apply(requests);
    let body: Vec<Value> = paged
        .items
        .iter()
        .map(|r| view.installation_request(r))
        .collect();
    json_with_link(StatusCode::OK, &body, paged.link(&origin, &uri))
}

// --- installation-token routes ----------------------------------------------

/// `GET /installation/repositories`, paged; only the token's repositories.
pub(crate) async fn repositories(
    State(shared): State<SharedState>,
    InstallationCaller(token): InstallationCaller,
    headers: HeaderMap,
    uri: Uri,
) -> Response {
    let origin = views::origin(&shared, &headers);
    let state = shared.state();
    let Some(installation) = find_installation(&state.world, token.installation_id) else {
        return auth::not_found();
    };
    let repos: Vec<&Repo> = installation_repos(&state.world, installation)
        .into_iter()
        .filter(|repo| token.covers(repo.id))
        .collect();
    let selection = selection_of(installation, &token);
    let paged = Pagination::of(&uri).apply(repos);
    let view = view(&state, &origin);
    let body = json!({
        "total_count": paged.total,
        "repository_selection": selection,
        "repositories": paged.items.iter().map(|r| view.repo(r)).collect::<Vec<_>>(),
    });
    json_with_link(StatusCode::OK, &body, paged.link(&origin, &uri))
}

/// A narrowed token reports `selected`; otherwise the installation's
/// selection.
fn selection_of(installation: &Installation, token: &TokenRecord) -> RepositorySelection {
    if token.repository_ids.is_some() {
        RepositorySelection::Selected
    } else {
        installation.repository_selection
    }
}

/// `DELETE /installation/token`: revokes the token in use.
pub(crate) async fn revoke_token(
    State(shared): State<SharedState>,
    InstallationCaller(token): InstallationCaller,
) -> Response {
    shared.state().tokens.revoke(&token.id);
    StatusCode::NO_CONTENT.into_response()
}

// --- minting ----------------------------------------------------------------

/// The body of `POST …/access_tokens` (and the admin API's mint).
#[derive(Debug, Default, Deserialize)]
pub(crate) struct TokenRequest {
    #[serde(default)]
    pub(crate) permissions: Option<BTreeMap<String, String>>,
    #[serde(default)]
    pub(crate) repositories: Option<Vec<String>>,
    #[serde(default)]
    pub(crate) repository_ids: Option<Vec<u64>>,
}

impl TokenRequest {
    /// An empty body asks for everything the installation has; malformed
    /// JSON is GitHub's 400 `Problems parsing JSON`.
    pub(crate) fn parse(body: &[u8]) -> Result<Self, Box<Response>> {
        if body.iter().all(u8::is_ascii_whitespace) {
            return Ok(Self::default());
        }
        serde_json::from_slice(body).map_err(|_| {
            Box::new(response::error(
                StatusCode::BAD_REQUEST,
                "Problems parsing JSON",
                CREATE_TOKEN_DOCS,
            ))
        })
    }
}

/// Why a token was not minted.
enum Refused {
    NotFound,
    Suspended,
    PermissionsNotGranted,
    RepositoryInaccessible,
    TooManyRepositories,
}

/// What a token will hold.
struct Grant {
    app_id: u64,
    permissions: BTreeMap<String, Access>,
    repositories: Option<Vec<u64>>,
    /// Both `repositories` and `repository_ids` were given (assumed: union).
    union: bool,
    /// `metadata: read` added though not requested (assumed).
    implicit_metadata: bool,
}

fn level(name: &str) -> Option<Access> {
    match name {
        "read" => Some(Access::Read),
        "write" => Some(Access::Write),
        "admin" => Some(Access::Admin),
        _ => None,
    }
}

/// What `request` may have of installation `id` (of App `app_id`, unless
/// the admin API asks).
fn plan(
    world: &Seed,
    app_id: Option<u64>,
    id: u64,
    request: &TokenRequest,
) -> Result<Grant, Refused> {
    let installation = find_installation(world, id)
        .filter(|i| app_id.is_none_or(|app| app == i.app_id))
        .ok_or(Refused::NotFound)?;
    if installation.suspended_at.is_some() {
        return Err(Refused::Suspended);
    }
    let granted = granted(world, installation);
    let mut implicit_metadata = false;
    let permissions = match &request.permissions {
        Some(asked) if !asked.is_empty() => {
            let mut permissions = BTreeMap::new();
            for (name, asked) in asked {
                let asked = level(asked).ok_or(Refused::PermissionsNotGranted)?;
                match granted.get(name) {
                    Some(held) if *held >= asked => {
                        permissions.insert(name.clone(), asked);
                    }
                    _ => return Err(Refused::PermissionsNotGranted),
                }
            }
            if !permissions.contains_key("metadata") && granted.contains_key("metadata") {
                permissions.insert("metadata".into(), Access::Read);
                implicit_metadata = true;
            }
            permissions
        }
        _ => granted,
    };
    let reachable = installation_repos(world, installation);
    let names = request.repositories.as_deref().unwrap_or_default();
    let ids = request.repository_ids.as_deref().unwrap_or_default();
    let repositories = if names.is_empty() && ids.is_empty() {
        None
    } else {
        if names.len() + ids.len() > MAX_TOKEN_REPOSITORIES {
            return Err(Refused::TooManyRepositories);
        }
        let mut chosen = BTreeSet::new();
        for name in names {
            let repo = reachable
                .iter()
                .find(|r| r.name.eq_ignore_ascii_case(name))
                .ok_or(Refused::RepositoryInaccessible)?;
            chosen.insert(repo.id);
        }
        for id in ids {
            if !reachable.iter().any(|r| r.id == *id) {
                return Err(Refused::RepositoryInaccessible);
            }
            chosen.insert(*id);
        }
        Some(chosen.into_iter().collect())
    };
    Ok(Grant {
        app_id: installation.app_id,
        permissions,
        repositories,
        union: !names.is_empty() && !ids.is_empty(),
        implicit_metadata,
    })
}

/// Mints a token for installation `id`, answering as
/// `POST /app/installations/{id}/access_tokens` does. `app_id` is the
/// calling App (`None` for the admin API, which may mint for any App).
pub(crate) fn issue(
    shared: &SharedState,
    app_id: Option<u64>,
    id: u64,
    request: &TokenRequest,
    origin: &str,
) -> Response {
    let mut state = shared.state();
    let grant = match plan(&state.world, app_id, id, request) {
        Ok(grant) => grant,
        Err(refused) => {
            drop(state);
            return refusal(shared, refused);
        }
    };
    if let Some(refusal) = (grant.union)
        .then(|| shared.refuse("token-repositories-union"))
        .flatten()
        .or_else(|| {
            (grant.implicit_metadata)
                .then(|| shared.refuse("token-implicit-metadata"))
                .flatten()
        })
    {
        return refusal;
    }
    let now = state.clock.now();
    let state = &mut *state;
    let (token, record) =
        state
            .tokens
            .mint(grant.app_id, id, grant.permissions, grant.repositories, now);
    let Some(installation) = find_installation(&state.world, id) else {
        return auth::not_found();
    };
    let view = view(state, origin);
    let mut body = json!({
        "token": token,
        "expires_at": timestamp(record.expires_at),
        "permissions": record.permissions,
        "repository_selection": selection_of(installation, &record),
    });
    if record.repository_ids.is_some() {
        body["repositories"] = installation_repos(&state.world, installation)
            .into_iter()
            .filter(|repo| record.covers(repo.id))
            .map(|repo| view.repo(repo))
            .collect();
    }
    json(StatusCode::CREATED, &body)
}

fn refusal(shared: &SharedState, refused: Refused) -> Response {
    let unprocessable = |message: &str| {
        response::error(
            StatusCode::UNPROCESSABLE_ENTITY,
            &shared.message("token-error-messages", message),
            CREATE_TOKEN_DOCS,
        )
    };
    match refused {
        Refused::NotFound => response::error(StatusCode::NOT_FOUND, "Not Found", CREATE_TOKEN_DOCS),
        Refused::Suspended => response::error(
            StatusCode::FORBIDDEN,
            &shared.message("suspended-installation", SUSPENDED),
            CREATE_TOKEN_DOCS,
        ),
        Refused::PermissionsNotGranted => {
            unprocessable("The permissions requested are not granted to this installation.")
        }
        Refused::RepositoryInaccessible => unprocessable(
            "There is at least one repository that does not exist or is not accessible to the parent installation.",
        ),
        Refused::TooManyRepositories => {
            if let Some(refusal) = shared.refuse("validation-errors") {
                return refusal;
            }
            response::validation_failed(
                &[FieldError {
                    resource: "Integration".into(),
                    field: "repositories".into(),
                    code: "custom".into(),
                    message: Some(
                        shared
                            .message(
                                "token-error-messages",
                                "Too many repositories: a token covers at most 500",
                            )
                            .into_owned(),
                    ),
                }],
                CREATE_TOKEN_DOCS,
            )
        }
    }
}

// --- installation lifecycle ------------------------------------------------------
//
// Each change announces itself (`crate::events`): `installation.*` and
// `installation_repositories.*`, attributed to `actor` or, without one, to
// `events::default_sender`.

/// Suspends or unsuspends installation `id` (of App `app_id`, unless the
/// admin API asks), announcing `installation.suspend` / `unsuspend` when
/// it changes. `false` when there is no such installation.
pub(crate) fn set_suspended(
    shared: &SharedState,
    id: u64,
    app_id: Option<u64>,
    suspended: bool,
    actor: Option<Actor>,
) -> bool {
    let mut state = shared.state();
    let now = DateTime::from_timestamp(state.clock.now().timestamp(), 0);
    let Some(index) = state
        .world
        .installations
        .iter()
        .position(|i| i.id == id && app_id.is_none_or(|app| app == i.app_id))
    else {
        return false;
    };
    let actor = actor.unwrap_or_else(|| {
        events::default_sender(&state.world, &state.world.installations[index].account)
    });
    let installation = &mut state.world.installations[index];
    let action = match (suspended, installation.suspended_at.is_some()) {
        (true, false) => {
            installation.suspended_at = now;
            installation.suspended_by = match &actor {
                Actor::User(login) => Some(login.clone()),
                Actor::App { .. } => None,
            };
            installation.updated_at = now;
            "suspend"
        }
        (false, true) => {
            installation.suspended_at = None;
            installation.suspended_by = None;
            installation.updated_at = now;
            "unsuspend"
        }
        _ => return true,
    };
    let installation = installation.clone();
    events::emit_installation(&mut state, &shared.root, action, &installation, &actor);
    true
}

/// Removes installation `id` and revokes its tokens, announcing
/// `installation.deleted`. `false` when there is no such installation.
pub(crate) fn uninstall(shared: &SharedState, id: u64, actor: Option<Actor>) -> bool {
    let mut state = shared.state();
    let Some(installation) = find_installation(&state.world, id).cloned() else {
        return false;
    };
    let actor =
        actor.unwrap_or_else(|| events::default_sender(&state.world, &installation.account));
    events::emit_installation(&mut state, &shared.root, "deleted", &installation, &actor);
    state.world.installations.retain(|i| i.id != id);
    state.tokens.revoke_installation(id);
    true
}

/// Why an admin lifecycle change was refused.
#[derive(Debug)]
pub(crate) enum LifecycleError {
    NotFound,
    Invalid(crate::seed::SeedError),
}

/// `world` with the installation of `changed`'s ID replaced by it,
/// validated.
fn validated(world: &Seed, changed: Installation) -> Result<Seed, LifecycleError> {
    let mut next = world.clone();
    match next.installations.iter_mut().find(|i| i.id == changed.id) {
        Some(slot) => *slot = changed,
        None => next.installations.push(changed),
    }
    next.validate().map_err(LifecycleError::Invalid)?;
    Ok(next)
}

/// Installs an App: adds `installation` (validated as a seed entry) and
/// announces `installation.created`.
pub(crate) fn install(
    shared: &SharedState,
    mut installation: Installation,
    actor: Option<Actor>,
) -> Result<(), crate::seed::SeedError> {
    let mut state = shared.state();
    let now = DateTime::from_timestamp(state.clock.now().timestamp(), 0);
    installation.created_at = installation.created_at.or(now);
    let world = state
        .world
        .merged(Seed::new().installation(installation.clone()))?;
    state.world = world;
    let actor =
        actor.unwrap_or_else(|| events::default_sender(&state.world, &installation.account));
    events::emit_installation(&mut state, &shared.root, "created", &installation, &actor);
    Ok(())
}

/// The installation's owner accepts new permissions: they become the
/// installation's grant (never wider than the App's), announced as
/// `installation.new_permissions_accepted`.
pub(crate) fn accept_permissions(
    shared: &SharedState,
    id: u64,
    permissions: BTreeMap<String, Access>,
    actor: Option<Actor>,
) -> Result<(), LifecycleError> {
    let mut state = shared.state();
    let mut installation = find_installation(&state.world, id)
        .cloned()
        .ok_or(LifecycleError::NotFound)?;
    installation.permissions = Some(permissions);
    installation.updated_at = DateTime::from_timestamp(state.clock.now().timestamp(), 0);
    state.world = validated(&state.world, installation.clone())?;
    let actor =
        actor.unwrap_or_else(|| events::default_sender(&state.world, &installation.account));
    events::emit_installation(
        &mut state,
        &shared.root,
        "new_permissions_accepted",
        &installation,
        &actor,
    );
    Ok(())
}

/// Changes which repositories installation `id` covers. Announces
/// `installation_repositories.added` for newly reachable repositories and
/// `removed` for lost ones, one event each (behaviour
/// `installation-repositories-events`).
pub(crate) fn set_repositories(
    shared: &SharedState,
    id: u64,
    selection: RepositorySelection,
    repositories: Vec<String>,
    actor: Option<Actor>,
) -> Result<(), LifecycleError> {
    let mut state = shared.state();
    let before = find_installation(&state.world, id)
        .cloned()
        .ok_or(LifecycleError::NotFound)?;
    let mut installation = before.clone();
    installation.repository_selection = selection;
    installation.repositories = match selection {
        RepositorySelection::All => Vec::new(),
        RepositorySelection::Selected => repositories,
    };
    installation.updated_at = DateTime::from_timestamp(state.clock.now().timestamp(), 0);
    state.world = validated(&state.world, installation.clone())?;
    let old: Vec<Repo> = installation_repos(&state.world, &before)
        .into_iter()
        .cloned()
        .collect();
    let new: Vec<Repo> = installation_repos(&state.world, &installation)
        .into_iter()
        .cloned()
        .collect();
    let added: Vec<Repo> = new
        .iter()
        .filter(|r| !old.iter().any(|o| o.id == r.id))
        .cloned()
        .collect();
    let removed: Vec<Repo> = old
        .iter()
        .filter(|r| !new.iter().any(|n| n.id == r.id))
        .cloned()
        .collect();
    let actor =
        actor.unwrap_or_else(|| events::default_sender(&state.world, &installation.account));
    if !added.is_empty() {
        events::emit_installation_repositories(
            &mut state,
            &shared.root,
            "added",
            &installation,
            &added,
            &[],
            &actor,
        );
    }
    if !removed.is_empty() {
        events::emit_installation_repositories(
            &mut state,
            &shared.root,
            "removed",
            &installation,
            &[],
            &removed,
            &actor,
        );
    }
    Ok(())
}
