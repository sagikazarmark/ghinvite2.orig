//! **Temporary.** The `ghinvite_github::stub` compatibility shim, so
//! ghinvite's workflow tests could move from the stub to the emulator before
//! they are rewritten against the native admin API. GE-09 deletes this
//! module, its routes and [`seed`].
//!
//! [`seed`] is the stub's default world. The stub's control routes live
//! under `/__octoemu/compat/` (`identity`, `outcomes`,
//! `installation-identity`, `installation-repositories`,
//! `invitation-expired`, `calls`, `reset`), each mapped onto emulator state
//! and fault rules labelled `compat:…`. See the README's "Compatibility
//! shim" section for the mapping and where it differs from the stub.

use crate::apps;
use crate::calls::Call;
use crate::faults::{Fault, FaultAction, Pattern, Reply};
use crate::response::{OCTOEMU_DOCS, error, json};
use crate::seed::{
    Access, App, Collaborator, Installation, Membership, Org, OrgRole, Repo, RepositorySelection,
    Role, Seed, User,
};
use crate::state::{Shared, SharedState};
use crate::users::rename_user;
use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use chrono::{DateTime, TimeDelta, Utc};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::{Arc, Weak};

/// The repository App's ID (`GH_REPO_APP_ID` in ghinvite's fixtures).
pub const REPO_APP_ID: u64 = 123;
/// The repository App's client ID.
pub const REPO_CLIENT_ID: &str = "Iv-p1-runtime-fixture";
/// The organization App's ID.
pub const ORG_APP_ID: u64 = 456;
/// The organization App's client ID.
pub const ORG_CLIENT_ID: &str = "Iv-p1-runtime-fixture-org";
/// The repository App's installation on `acme`.
pub const REPO_INSTALLATION_ID: u64 = 1;
/// The organization App's installation on `acme`.
pub const ORG_INSTALLATION_ID: u64 = 2;
/// The installation account, `acme` (an organization).
pub const ACCOUNT_ID: u64 = 100;
/// `creator`, an owner of `acme` (admin on every repository).
pub const CREATOR_ID: u64 = 7;
/// `alice`, the stub's default user.
pub const ALICE_ID: u64 = 8;
/// `acme/api`.
pub const API_REPO_ID: u64 = 10;
/// `acme/web`.
pub const WEB_REPO_ID: u64 = 11;

/// The public half of ghinvite's `test-fixtures/github-app-rsa.pem` (the same
/// public test key pair as `tests/fixtures/rsa2048.pem`).
pub const PUBLIC_KEY: &str = include_str!("../tests/fixtures/rsa2048.pub.pem");

/// The stub's default world: `acme` (100, an organization created in 2020)
/// with `acme/api` (10) and `acme/web` (11), both private; `creator` (7)
/// owns `acme`; `alice` (8); the repository App (123,
/// `Iv-p1-runtime-fixture`: `administration: write`, `metadata: read`)
/// installed on every `acme` repository as installation 1 (created
/// 2026-01-01, as the stub reported), and the
/// organization App (456, `Iv-p1-runtime-fixture-org`: `members: write`,
/// `metadata: read`) as installation 2. Both Apps verify JWTs against
/// [`PUBLIC_KEY`] and have no webhook.
pub fn seed() -> Seed {
    seed_with_installation(REPO_INSTALLATION_ID)
}

/// [`seed`] with the repository App installed as `installation_id`
/// (the stub answered for every installation ID).
pub fn seed_with_installation(installation_id: u64) -> Seed {
    let created: DateTime<Utc> = "2020-01-01T00:00:00Z".parse().unwrap_or_default();
    let mut seed = Seed::new()
        .user(User::new(CREATOR_ID, "creator"))
        .user(User::new(ALICE_ID, "alice"))
        .org(Org::new(ACCOUNT_ID, "acme").created_at(created))
        .membership(Membership::new("acme", "creator", OrgRole::Admin))
        .repo(Repo::new(API_REPO_ID, "acme", "api").private())
        .repo(Repo::new(WEB_REPO_ID, "acme", "web").private())
        .app(
            App::new(
                REPO_APP_ID,
                "ghinvite-repo-fixture",
                REPO_CLIENT_ID,
                "compat-repo-client-secret",
            )
            .public_key(PUBLIC_KEY)
            .permission("administration", Access::Write)
            .permission("metadata", Access::Read),
        )
        .app(
            App::new(
                ORG_APP_ID,
                "ghinvite-org-fixture",
                ORG_CLIENT_ID,
                "compat-org-client-secret",
            )
            .public_key(PUBLIC_KEY)
            .permission("members", Access::Write)
            .permission("metadata", Access::Read),
        )
        .installation(installed(installation_id, REPO_APP_ID));
    if installation_id != ORG_INSTALLATION_ID {
        seed = seed.installation(installed(ORG_INSTALLATION_ID, ORG_APP_ID));
    }
    seed
}

/// An installation on every `acme` repository, created when the stub said
/// (2026-01-01).
fn installed(id: u64, app_id: u64) -> Installation {
    let mut installation = Installation::new(id, app_id, "acme");
    installation.created_at = "2026-01-01T00:00:00Z".parse().ok();
    installation
}

/// What the shim remembers: the world to reset to.
#[derive(Debug, Default)]
pub(crate) struct CompatState {
    pub(crate) initial: Option<Seed>,
}

pub(crate) fn router() -> Router<SharedState> {
    Router::new()
        .route("/__octoemu/compat/identity", post(identity))
        .route(
            "/__octoemu/compat/installation-identity",
            post(installation_identity),
        )
        .route(
            "/__octoemu/compat/installation-repositories",
            post(installation_repositories),
        )
        .route("/__octoemu/compat/outcomes", post(outcomes))
        .route(
            "/__octoemu/compat/invitation-expired",
            post(invitation_expired),
        )
        .route("/__octoemu/compat/calls", get(calls))
        .route("/__octoemu/compat/reset", delete(reset))
}

fn bad_request(message: &str) -> Response {
    error(
        StatusCode::BAD_REQUEST,
        &format!("octoemu compat: {message}"),
        OCTOEMU_DOCS,
    )
}

fn not_found(message: &str) -> Response {
    error(
        StatusCode::NOT_FOUND,
        &format!("octoemu compat: {message}"),
        OCTOEMU_DOCS,
    )
}

fn parse<T: for<'de> Deserialize<'de>>(bytes: &Bytes) -> Result<T, Box<Response>> {
    serde_json::from_slice(bytes).map_err(|e| Box::new(bad_request(&format!("invalid body: {e}"))))
}

const IDENTITY_LABEL: &str = "compat:identity";
const INSTALLATION_IDENTITY_LABEL: &str = "compat:installation-identity";

/// The first free user ID at or above 1000.
fn fresh_user_id(world: &Seed) -> u64 {
    let mut id = 1000;
    while world.users.iter().any(|u| u.id == id) || world.orgs.iter().any(|o| o.id == id) {
        id += 1;
    }
    id
}

/// Makes user `id` carry `login`, creating them if needed and renaming
/// away (to `{login}-former-{id}`) whoever held the login before.
fn ensure_user(world: &mut Seed, id: u64, login: &str) -> Result<(), String> {
    if let Some(holder) = world
        .users
        .iter()
        .find(|u| u.login.eq_ignore_ascii_case(login) && u.id != id)
        .map(|u| (u.id, u.login.clone()))
    {
        rename_user(
            world,
            &holder.1,
            &format!("{}-former-{}", holder.1, holder.0),
        )?;
    }
    match world
        .users
        .iter()
        .find(|u| u.id == id)
        .map(|u| u.login.clone())
    {
        Some(current) => rename_user(world, &current, login),
        None => {
            if world.orgs.iter().any(|o| o.id == id) {
                return Err(format!("{id} is an organization"));
            }
            world.users.push(User::new(id, login));
            Ok(())
        }
    }
}

#[derive(Deserialize)]
struct IdentityInput {
    login: Option<String>,
    addressed_id: Option<u64>,
    role_name: Option<String>,
}

/// The stub's `role_name` as a role (`none`: no access).
fn role_of(name: &str) -> Option<Role> {
    match name {
        "none" => None,
        "read" => Some(Role::Pull),
        "write" => Some(Role::Push),
        other => Some(Role::from(other.to_owned())),
    }
}

/// `POST /__octoemu/compat/identity` `{"login"?, "addressed_id"?,
/// "role_name"?}`, the stub's `/identity`: from now on `GET /user/{id}`
/// answers `login` (default `alice`) for every ID and `GET
/// /users/{username}` answers `addressed_id` (default 8) for every
/// username (custom fault rules, as the stub did). In the world, user
/// `addressed_id` carries `login`; with `role_name` they are a direct
/// collaborator with that role on every repository (a pending invitation
/// of theirs is accepted), without it they lose their direct access.
async fn identity(State(shared): State<SharedState>, bytes: Bytes) -> Response {
    let input: IdentityInput = match parse(&bytes) {
        Ok(input) => input,
        Err(response) => return *response,
    };
    let login = input.login.unwrap_or_else(|| "alice".to_owned());
    let addressed = input.addressed_id.unwrap_or(ALICE_ID);
    let role = input.role_name.as_deref().and_then(role_of);
    {
        let mut state = shared.state();
        let mut world = state.world.clone();
        if let Err(problem) = ensure_user(&mut world, addressed, &login) {
            return bad_request(&problem);
        }
        let repos: Vec<String> = world.repos.iter().map(Repo::full_name).collect();
        let is_user = |other: &str| other.eq_ignore_ascii_case(&login);
        world.collaborators.retain(|c| !is_user(&c.user));
        if let Some(role) = role {
            world
                .repository_invitations
                .retain(|i| !is_user(&i.invitee));
            for repo in repos {
                world
                    .collaborators
                    .push(Collaborator::new(repo, login.clone(), role.clone()));
            }
        }
        if let Err(rejected) = world.validate() {
            return bad_request(&rejected.to_string());
        }
        state.world = world;
        state.faults.remove_labelled(IDENTITY_LABEL);
    }
    let weak = Arc::downgrade(&shared);
    let by_id = {
        let weak = weak.clone();
        let login = login.clone();
        FaultAction::custom(move |request| {
            let id = request
                .params
                .get("account_id")
                .and_then(|id| id.parse::<u64>().ok());
            match id {
                Some(id) => Reply::new(200).json(account(&weak, id, &login)),
                None => Reply::new(404).json(json!({"message": "Not Found"})),
            }
        })
    };
    let by_login = FaultAction::custom(move |request| {
        let username = request.params.get("username").cloned().unwrap_or_default();
        Reply::new(200).json(account(&weak, addressed, &username))
    });
    let mut state = shared.state();
    for fault in [
        Fault::on("GET", "/user/{account_id}", by_id),
        Fault::on("GET", "/users/{username}", by_login),
    ] {
        if let Err(refused) = state.faults.add(fault.label(IDENTITY_LABEL)) {
            return bad_request(&refused.to_string());
        }
    }
    StatusCode::NO_CONTENT.into_response()
}

/// User `id` as `GET /users/{username}` shows them, with `login` instead of
/// their own (the stub answered whatever login it was configured with).
fn account(shared: &Weak<Shared>, id: u64, login: &str) -> Value {
    let fallback = json!({"id": id, "login": login, "type": "User"});
    let Some(shared) = shared.upgrade() else {
        return fallback;
    };
    let state = shared.state();
    let view = apps::view(&state, &shared.root);
    let Some(mut body) = state
        .world
        .users
        .iter()
        .find(|u| u.id == id)
        .and_then(|user| view.full_account(&user.login))
    else {
        return fallback;
    };
    body["login"] = login.into();
    body
}

/// `POST /__octoemu/compat/installation-identity` `<account>`: `GET
/// /app/installations/{id}` reports this `account` object (a custom fault
/// rule; the rest of the body is the real installation). The real account
/// again removes the rule.
async fn installation_identity(State(shared): State<SharedState>, bytes: Bytes) -> Response {
    let account: Value = match parse(&bytes) {
        Ok(Value::Object(account)) => Value::Object(account),
        Ok(_) => return bad_request("the account must be an object"),
        Err(response) => return *response,
    };
    let mut state = shared.state();
    state.faults.remove_labelled(INSTALLATION_IDENTITY_LABEL);
    let real = state.world.orgs.iter().any(|o| {
        Some(o.id) == account["id"].as_u64() && Some(o.login.as_str()) == account["login"].as_str()
    });
    if real {
        return StatusCode::NO_CONTENT.into_response();
    }
    let weak = Arc::downgrade(&shared);
    let action = FaultAction::custom(move |request| {
        let id = request
            .params
            .get("installation_id")
            .and_then(|id| id.parse::<u64>().ok());
        let Some(shared) = weak.upgrade() else {
            return Reply::new(503);
        };
        let state = shared.state();
        let view = apps::view(&state, &shared.root);
        match id.and_then(|id| state.world.installations.iter().find(|i| i.id == id)) {
            Some(installation) => {
                let mut body = view.installation(installation);
                body["account"] = account.clone();
                Reply::new(200).json(body)
            }
            None => Reply::new(404).json(json!({
                "message": "Not Found",
                "documentation_url": "https://docs.github.com/rest/apps/apps#get-an-installation-for-the-authenticated-app",
            })),
        }
    });
    match state.faults.add(
        Fault::on("GET", "/app/installations/{installation_id}", action)
            .label(INSTALLATION_IDENTITY_LABEL),
    ) {
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(refused) => bad_request(&refused.to_string()),
    }
}

/// `POST /__octoemu/compat/installation-repositories` `[repository IDs]`:
/// every installation of the repository App selects exactly these
/// repositories (an unknown ID becomes `acme/repo-{id}`), with the
/// `installation_repositories` events the change implies.
async fn installation_repositories(State(shared): State<SharedState>, bytes: Bytes) -> Response {
    let ids: Vec<u64> = match parse(&bytes) {
        Ok(ids) => ids,
        Err(response) => return *response,
    };
    let (installations, names) = {
        let mut state = shared.state();
        let mut world = state.world.clone();
        let mut names = Vec::new();
        for id in ids {
            let name = match world.repos.iter().find(|r| r.id == id) {
                Some(repo) => repo.full_name(),
                None => {
                    world
                        .repos
                        .push(Repo::new(id, "acme", format!("repo-{id}")).private());
                    format!("acme/repo-{id}")
                }
            };
            names.push(name);
        }
        if let Err(rejected) = world.validate() {
            return bad_request(&rejected.to_string());
        }
        state.world = world;
        let installations: Vec<u64> = state
            .world
            .installations
            .iter()
            .filter(|i| i.app_id == REPO_APP_ID)
            .map(|i| i.id)
            .collect();
        (installations, names)
    };
    for id in installations {
        if let Err(problem) = apps::set_repositories(
            &shared,
            id,
            RepositorySelection::Selected,
            names.clone(),
            None,
        ) {
            return bad_request(&format!("{problem:?}"));
        }
    }
    StatusCode::NO_CONTENT.into_response()
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum Outcome {
    AlreadyCollaborator,
    TerminalFailure,
    TransientOnce,
    CreatedResponseLost,
    CreatedThenDeclined,
    AccessLostOnce,
    ThrottledOnce,
}

#[derive(Deserialize)]
struct OutcomeInput {
    owner: String,
    repo: String,
    user: String,
    outcome: Outcome,
}

/// The stub's body for a controlled failure.
fn stub_failure() -> Value {
    json!({"message": "Controlled stub failure"})
}

/// `POST /__octoemu/compat/outcomes` `{"owner", "repo", "user",
/// "outcome"}`, the stub's `/outcomes` for `PUT
/// /repos/{owner}/{repo}/collaborators/{user}`, replacing that triple's
/// earlier outcome:
///
/// - `already_collaborator`: the user is a direct collaborator (`pull`)
///   in the world, so every PUT answers 204;
/// - `terminal_failure`: every PUT answers 422 (rule);
/// - `transient_once`: the next PUT answers 502 (rule, once);
/// - `created_response_lost`: every PUT runs, then answers 502 (rule,
///   `apply_then_lose`);
/// - `created_then_declined`: every PUT answers 502 and nothing is left
///   behind (rule; the stub created and dropped an invitation);
/// - `access_lost_once`: the next PUT answers 403 (rule, once);
/// - `throttled_once`: the next PUT answers 403 with `Retry-After: 1` and
///   the secondary-rate-limit message (rule, once).
async fn outcomes(State(shared): State<SharedState>, bytes: Bytes) -> Response {
    let input: OutcomeInput = match parse(&bytes) {
        Ok(input) => input,
        Err(response) => return *response,
    };
    let path = format!(
        "/repos/{}/{}/collaborators/{}",
        input.owner, input.repo, input.user
    );
    if Pattern::parse(&path).is_err() || path.contains(['{', '}', '*']) {
        return bad_request("owner, repo and user must be plain path segments");
    }
    let label = format!("compat:outcome:{path}");
    let mut state = shared.state();
    state.faults.remove_labelled(&label);
    let rule = |action| Fault::on("PUT", path.clone(), action).label(label.clone());
    let fault = match input.outcome {
        Outcome::AlreadyCollaborator => {
            let full_name = format!("{}/{}", input.owner, input.repo);
            let mut world = state.world.clone();
            if !world
                .repos
                .iter()
                .any(|r| r.full_name().eq_ignore_ascii_case(&full_name))
            {
                return not_found(&format!("no repository {full_name}"));
            }
            if crate::access::user(&world, &input.user).is_none() {
                let id = fresh_user_id(&world);
                world.users.push(User::new(id, input.user.clone()));
            }
            let pair = |c: &Collaborator| {
                c.repo.eq_ignore_ascii_case(&full_name) && c.user.eq_ignore_ascii_case(&input.user)
            };
            if !world.collaborators.iter().any(pair) {
                world.repository_invitations.retain(|i| {
                    !(i.repo.eq_ignore_ascii_case(&full_name)
                        && i.invitee.eq_ignore_ascii_case(&input.user))
                });
                world.collaborators.push(Collaborator::new(
                    full_name,
                    input.user.clone(),
                    Role::Pull,
                ));
            }
            if let Err(rejected) = world.validate() {
                return bad_request(&rejected.to_string());
            }
            state.world = world;
            return StatusCode::NO_CONTENT.into_response();
        }
        Outcome::TerminalFailure => rule(FaultAction::respond_json(422, stub_failure())),
        Outcome::TransientOnce => rule(FaultAction::respond_json(502, stub_failure())).once(),
        Outcome::CreatedResponseLost => rule(FaultAction::ApplyThenLose { status: Some(502) }),
        Outcome::CreatedThenDeclined => rule(FaultAction::respond_json(502, stub_failure())),
        Outcome::AccessLostOnce => rule(FaultAction::respond_json(403, stub_failure())).once(),
        Outcome::ThrottledOnce => rule(FaultAction::Respond(
            Reply::new(403)
                .header("retry-after", "1")
                .json(json!({"message": "You have exceeded a secondary rate limit"})),
        ))
        .once(),
    };
    match state.faults.add(fault) {
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(refused) => bad_request(&refused.to_string()),
    }
}

#[derive(Deserialize)]
struct ExpiredInput {
    owner: String,
    repo: String,
    id: u64,
    expired: bool,
}

/// `POST /__octoemu/compat/invitation-expired` `{"owner", "repo", "id",
/// "expired"}`: dates the invitation back past its 7 days (or to now), so
/// GitHub's own expiry reports it.
async fn invitation_expired(State(shared): State<SharedState>, bytes: Bytes) -> Response {
    let input: ExpiredInput = match parse(&bytes) {
        Ok(input) => input,
        Err(response) => return *response,
    };
    let full_name = format!("{}/{}", input.owner, input.repo);
    let mut state = shared.state();
    let now = state.clock.now();
    let Some(invitation) = state
        .world
        .repository_invitations
        .iter_mut()
        .find(|i| i.id == input.id && i.repo.eq_ignore_ascii_case(&full_name))
    else {
        return not_found("no such invitation");
    };
    invitation.created_at = Some(if input.expired {
        now - crate::views::INVITATION_LIFETIME - TimeDelta::seconds(1)
    } else {
        now
    });
    StatusCode::NO_CONTENT.into_response()
}

/// The routes the stub recorded in its ledger.
const RECORDED: &[(&str, &str)] = &[
    ("POST", "/app/installations/{id}/access_tokens"),
    ("PUT", "/repos/{owner}/{repo}/collaborators/{user}"),
    ("GET", "/repos/{owner}/{repo}/collaborators/{user}"),
    ("DELETE", "/repos/{owner}/{repo}/collaborators/{user}"),
    ("GET", "/repos/{owner}/{repo}/invitations"),
    ("GET", "/repositories/{id}/invitations"),
    ("DELETE", "/repos/{owner}/{repo}/invitations/{id}"),
];

fn stub_record(call: &Call) -> Option<Value> {
    let path = call.path.split('?').next().unwrap_or_default();
    let recorded = RECORDED.iter().any(|(method, pattern)| {
        *method == call.method
            && Pattern::parse(pattern).is_ok_and(|pattern| pattern.matches(path).is_some())
    });
    if !recorded {
        return None;
    }
    let permission = call
        .body_fields
        .as_ref()
        .and_then(|fields| fields.get("permission"))
        .cloned()
        .filter(|_| call.method == "PUT")
        .unwrap_or(Value::Null);
    // The stub logged the status its handler chose, even when it then lost
    // the response.
    let status = call
        .fault
        .as_ref()
        .filter(|fault| fault.action == "apply_then_lose")
        .and_then(|fault| fault.handler_status)
        .unwrap_or(call.status);
    Some(json!({"method": call.method, "path": path, "permission": permission, "status": status}))
}

/// `GET /__octoemu/compat/calls`: the stub's ledger, `{"count", "requests":
/// [{"method", "path", "permission", "status"}]}`, from the call log:
/// token minting, collaborator writes and reads, and invitation reads and
/// deletions only, without query strings.
async fn calls(State(shared): State<SharedState>) -> Response {
    let requests: Vec<Value> = shared
        .state()
        .calls
        .calls()
        .iter()
        .filter_map(stub_record)
        .collect();
    json(
        StatusCode::OK,
        &json!({"count": requests.len(), "requests": requests}),
    )
}

/// `DELETE /__octoemu/compat/reset`, the stub's `/reset`: removes the
/// `compat:` rules and empties the call log; users get their starting
/// logins back (others holding one are renamed away), and collaborators,
/// invitations, memberships and the repository App's repository selection
/// return to the starting world. Installations, tokens, the clock and
/// invitation IDs (never reused) are left alone. 200, as the stub answered.
async fn reset(State(shared): State<SharedState>) -> Response {
    let mut state = shared.state();
    let Some(initial) = state.compat.initial.clone() else {
        return StatusCode::OK.into_response();
    };
    let mut world = state.world.clone();
    world.collaborators.clear();
    world.repository_invitations.clear();
    world.memberships.clear();
    world.team_memberships.clear();
    world.org_invitations.clear();
    for user in &initial.users {
        if let Err(problem) = ensure_user(&mut world, user.id, &user.login) {
            return bad_request(&problem);
        }
    }
    world.collaborators = initial.collaborators.clone();
    world.repository_invitations = initial.repository_invitations.clone();
    world.memberships = initial.memberships.clone();
    world.team_memberships = initial.team_memberships.clone();
    world.org_invitations = initial.org_invitations.clone();
    for installation in &mut world.installations {
        if installation.app_id != REPO_APP_ID {
            continue;
        }
        match initial
            .installations
            .iter()
            .find(|i| i.id == installation.id)
        {
            Some(start) => {
                installation.repository_selection = start.repository_selection;
                installation.repositories = start.repositories.clone();
            }
            None => {
                installation.repository_selection = RepositorySelection::All;
                installation.repositories.clear();
            }
        }
    }
    if let Err(rejected) = world.validate() {
        return bad_request(&rejected.to_string());
    }
    state.world = world;
    state.calls.clear();
    let labels: Vec<String> = state
        .faults
        .list()
        .into_iter()
        .filter_map(|fault| fault.label)
        .filter(|label| label.starts_with("compat:"))
        .collect();
    for label in labels {
        state.faults.remove_labelled(&label);
    }
    StatusCode::OK.into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_seed_is_valid_for_any_installation_id() {
        seed().validate().unwrap();
        seed_with_installation(9).validate().unwrap();
        seed_with_installation(ORG_INSTALLATION_ID)
            .validate()
            .unwrap();
    }

    #[test]
    fn renames_users_and_their_references() {
        let mut world = seed();
        world
            .collaborators
            .push(Collaborator::new("acme/api", "alice", Role::Push));
        ensure_user(&mut world, 99, "alice").unwrap();
        assert!(world.users.iter().any(|u| u.id == 99 && u.login == "alice"));
        assert!(
            world
                .users
                .iter()
                .any(|u| u.id == 8 && u.login == "alice-former-8")
        );
        assert_eq!(world.collaborators[0].user, "alice-former-8");
        world.validate().unwrap();
        assert!(rename_user(&mut world, "creator", "alice").is_err());
        assert!(rename_user(&mut world, "nobody", "x").is_err());
    }
}
