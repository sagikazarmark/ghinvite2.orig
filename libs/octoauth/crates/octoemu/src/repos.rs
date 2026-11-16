//! The repository surface (GE-03): `GET /repos/{owner}/{repo}`, the
//! collaborator routes, the repository-invitation routes and their
//! `/repositories/{repository_id}/invitations` alias. Thin adapters over
//! `crate::collaboration`.
//!
//! Every route takes an installation token. A private (or internal)
//! repository outside the token's repositories is a 404 (documented); a
//! public one outside them is a 403 on the collaborator and invitation
//! routes (behaviour `out-of-scope-repository`).
#![allow(
    clippy::result_large_err,
    reason = "a rejection is the response to send, built once per request"
)]

use crate::access;
use crate::apps::view;
use crate::auth::{
    self, AnyCaller, Caller, InstallationCaller, Permission, forbidden, in_scope, visible_repo,
};
use crate::collaboration::{self, Actor, Ctx, Failure, PutOutcome};
use crate::paginate::Pagination;
use crate::response::{self, FieldError, json, json_with_link};
use crate::seed::{Access, Repo, RepositoryInvitation, Role, Seed};
use crate::state::{Shared, SharedState, WITHHELD};
use crate::tokens::TokenRecord;
use crate::views::{self, View, expired};
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

const COLLABORATORS_DOCS: &str = "https://docs.github.com/rest/collaborators/collaborators";
const INVITATIONS_DOCS: &str = "https://docs.github.com/rest/collaborators/invitations";

pub(crate) const ADMINISTRATION_READ: Permission = Permission::new("administration", Access::Read);
pub(crate) const ADMINISTRATION_WRITE: Permission =
    Permission::new("administration", Access::Write);
pub(crate) const METADATA_READ: Permission = Permission::new("metadata", Access::Read);

/// The repository a collaborator or invitation route acts on.
fn scoped_repo<'a>(
    shared: &Shared,
    world: &'a Seed,
    token: &TokenRecord,
    owner: &str,
    name: &str,
    need: Permission,
) -> Result<&'a Repo, Response> {
    let repo = visible_repo(world, token, owner, name)?;
    if in_scope(world, token, repo) {
        Ok(repo)
    } else {
        Err(shared
            .refuse("out-of-scope-repository")
            .unwrap_or_else(|| forbidden(need)))
    }
}

/// The response for a failed operation.
fn failure(shared: &Shared, failure: Failure, docs: &str) -> Response {
    match failure {
        Failure::NotFound | Failure::Expired => auth::not_found(),
        Failure::InvalidRole { field } => unprocessable(
            shared,
            field,
            "invalid",
            &format!("{field} is not a valid role for this repository"),
            docs,
        ),
        Failure::InvitationLimit => unprocessable(
            shared,
            "base",
            "custom",
            "You have exceeded the limit of 50 invitations to this repository in 24 hours",
            docs,
        ),
        Failure::Refused(behaviour) => shared.refuse(behaviour).unwrap_or_else(|| {
            response::error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "octoemu: refusal without a refused behaviour",
                response::OCTOEMU_DOCS,
            )
        }),
    }
}

/// A 422 (documented status) with `Validation Failed` and an error whose
/// details are guesses (behaviour `repo-error-messages`): withheld in
/// refuse-assumed mode.
fn unprocessable(shared: &Shared, field: &str, code: &str, message: &str, docs: &str) -> Response {
    if !shared.allows("repo-error-messages") {
        return response::error(StatusCode::UNPROCESSABLE_ENTITY, WITHHELD, docs);
    }
    response::validation_failed(
        &[FieldError {
            resource: "Repository".into(),
            field: field.into(),
            code: code.into(),
            message: Some(message.into()),
        }],
        docs,
    )
}

/// A JSON object body; empty means `{}`. Malformed JSON is GitHub's 400.
pub(crate) fn object(body: &[u8], docs: &str) -> Result<Value, Response> {
    if body.iter().all(u8::is_ascii_whitespace) {
        return Ok(json!({}));
    }
    match serde_json::from_slice::<Value>(body) {
        Ok(value @ Value::Object(_)) => Ok(value),
        _ => Err(response::error(
            StatusCode::BAD_REQUEST,
            "Problems parsing JSON",
            docs,
        )),
    }
}

/// A string field of `body`: `Ok(None)` when absent or null, `Err` when
/// present with another type.
pub(crate) fn string_field<'a>(body: &'a Value, field: &str) -> Result<Option<&'a str>, ()> {
    match body.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value)),
        Some(_) => Err(()),
    }
}

fn actor(token: &TokenRecord) -> Actor {
    Actor::App {
        app_id: token.app_id,
    }
}

/// The ID in `{invitation_id}`; anything else names no invitation.
fn numeric(raw: &str) -> Option<u64> {
    raw.parse().ok()
}

/// Renders invitations, refusing the assumed parts they would show.
pub(crate) fn invitations_json(
    shared: &Shared,
    view: &View<'_>,
    invitations: &[&RepositoryInvitation],
) -> Result<Vec<Value>, Response> {
    for invitation in invitations {
        if matches!(invitation.role, Role::Custom(_))
            && let Some(refusal) = shared.refuse("custom-role-invitation-permissions")
        {
            return Err(refusal);
        }
        if expired(invitation, view.start, view.now)
            && let Some(refusal) = shared.refuse("expired-invitations-listed")
        {
            return Err(refusal);
        }
    }
    Ok(invitations.iter().map(|i| view.invitation(i)).collect())
}

// --- repositories --------------------------------------------------------------

/// `GET /repos/{owner}/{repo}`.
///
/// With an installation token or a user token: a user token reaches a
/// private repository the user can access and, for a GitHub App's token, an
/// installation of the App covers.
pub(crate) async fn get_repo(
    State(shared): State<SharedState>,
    AnyCaller(caller): AnyCaller,
    Path((owner, name)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    let origin = views::origin(&shared, &headers);
    let state = shared.state();
    let found = match &caller {
        Caller::Installation(token) => visible_repo(&state.world, token, &owner, &name),
        Caller::User(token) => auth::visible_repo_for_user(&state.world, token, &owner, &name),
        _ => Err(auth::not_found()),
    };
    match found {
        Ok(repo) => json(StatusCode::OK, &view(&state, &origin).full_repo(repo)),
        Err(hidden) => hidden,
    }
}

// --- collaborators ---------------------------------------------------------------

/// `PUT /repos/{owner}/{repo}/collaborators/{username}`: 201 with an
/// invitation, or 204.
pub(crate) async fn put_collaborator(
    State(shared): State<SharedState>,
    InstallationCaller(token): InstallationCaller,
    Path((owner, name, username)): Path<(String, String, String)>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let docs =
        "https://docs.github.com/rest/collaborators/collaborators#add-a-repository-collaborator";
    let body = match object(&body, docs) {
        Ok(body) => body,
        Err(response) => return response,
    };
    let origin = views::origin(&shared, &headers);
    let allows = |id: &str| shared.allows(id);
    let ctx = Ctx {
        origin: &origin,
        allows: &allows,
    };
    let mut state = shared.state();
    let repo_id = match scoped_repo(
        &shared,
        &state.world,
        &token,
        &owner,
        &name,
        ADMINISTRATION_WRITE,
    ) {
        Ok(repo) => repo.id,
        Err(response) => return response,
    };
    let Ok(permission) = string_field(&body, "permission") else {
        return failure(
            &shared,
            Failure::InvalidRole {
                field: "permission",
            },
            docs,
        );
    };
    let outcome = collaboration::put_collaborator(
        &mut state,
        &ctx,
        repo_id,
        &username,
        permission,
        &actor(&token),
    );
    match outcome {
        Ok(PutOutcome::NoContent) => StatusCode::NO_CONTENT.into_response(),
        Ok(PutOutcome::Invited(id)) => {
            let view = view(&state, &origin);
            let Some(invitation) = state
                .world
                .repository_invitations
                .iter()
                .find(|i| i.id == id)
            else {
                return auth::not_found();
            };
            match invitations_json(&shared, &view, &[invitation]) {
                Ok(mut rendered) => json(StatusCode::CREATED, &rendered.remove(0)),
                Err(refusal) => refusal,
            }
        }
        Err(error) => failure(&shared, error, docs),
    }
}

/// `DELETE /repos/{owner}/{repo}/collaborators/{username}`: 204.
pub(crate) async fn remove_collaborator(
    State(shared): State<SharedState>,
    InstallationCaller(token): InstallationCaller,
    Path((owner, name, username)): Path<(String, String, String)>,
    headers: HeaderMap,
) -> Response {
    let origin = views::origin(&shared, &headers);
    let allows = |id: &str| shared.allows(id);
    let ctx = Ctx {
        origin: &origin,
        allows: &allows,
    };
    let mut state = shared.state();
    let repo_id = match scoped_repo(
        &shared,
        &state.world,
        &token,
        &owner,
        &name,
        ADMINISTRATION_WRITE,
    ) {
        Ok(repo) => repo.id,
        Err(response) => return response,
    };
    match collaboration::remove_collaborator(&mut state, &ctx, repo_id, &username, &actor(&token)) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => failure(&shared, error, COLLABORATORS_DOCS),
    }
}

/// `GET /repos/{owner}/{repo}/collaborators/{username}/permission`: the
/// highest role across every source.
pub(crate) async fn collaborator_permission(
    State(shared): State<SharedState>,
    InstallationCaller(token): InstallationCaller,
    Path((owner, name, username)): Path<(String, String, String)>,
    headers: HeaderMap,
) -> Response {
    let origin = views::origin(&shared, &headers);
    let state = shared.state();
    let repo = match scoped_repo(&shared, &state.world, &token, &owner, &name, METADATA_READ) {
        Ok(repo) => repo,
        Err(response) => return response,
    };
    let Some(user) = access::user(&state.world, &username) else {
        return shared
            .refuse("unknown-user-404")
            .unwrap_or_else(auth::not_found);
    };
    let view = view(&state, &origin);
    let body = match access::effective(&state.world, repo, &user.login) {
        Some(grant) => json!({
            "permission": access::legacy_permission(&state.world, repo, &grant.role),
            "role_name": access::role_name(&state.world, repo, &grant.role),
            "user": view.collaborator(repo, &user.login, &grant.role),
        }),
        None => {
            if let Some(refusal) = shared.refuse("permission-without-access") {
                return refusal;
            }
            let mut user_body = view.account(&user.login).unwrap_or_default();
            user_body["permissions"] = json!({
                "admin": false, "maintain": false, "push": false, "triage": false, "pull": false,
            });
            user_body["role_name"] = "none".into();
            json!({"permission": "none", "role_name": "none", "user": user_body})
        }
    };
    json(StatusCode::OK, &body)
}

/// `affiliation` of `GET …/collaborators`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Affiliation {
    Direct,
    Outside,
    All,
}

fn affiliation(uri: &Uri) -> Option<Affiliation> {
    let value = uri
        .query()
        .unwrap_or_default()
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .find(|(name, _)| *name == "affiliation")
        .map(|(_, value)| value);
    match value {
        None | Some("all") => Some(Affiliation::All),
        Some("direct") => Some(Affiliation::Direct),
        Some("outside") => Some(Affiliation::Outside),
        Some(_) => None,
    }
}

/// `GET /repos/{owner}/{repo}/collaborators?affiliation=`, paged, each
/// with `permissions` and `role_name`.
pub(crate) async fn list_collaborators(
    State(shared): State<SharedState>,
    InstallationCaller(token): InstallationCaller,
    Path((owner, name)): Path<(String, String)>,
    headers: HeaderMap,
    uri: Uri,
) -> Response {
    let origin = views::origin(&shared, &headers);
    let state = shared.state();
    let world = &state.world;
    let repo = match scoped_repo(&shared, world, &token, &owner, &name, METADATA_READ) {
        Ok(repo) => repo,
        Err(response) => return response,
    };
    let Some(affiliation) = affiliation(&uri) else {
        return unprocessable(
            &shared,
            "affiliation",
            "invalid",
            "affiliation must be outside, direct or all",
            COLLABORATORS_DOCS,
        );
    };
    if affiliation != Affiliation::Direct
        && let Some(refusal) = shared.refuse("collaborator-affiliation")
    {
        return refusal;
    }
    let full_name = repo.full_name();
    let org_owned = access::org_owned(world, repo);
    let mut rows: Vec<(u64, String, Role)> = world
        .users
        .iter()
        .filter_map(|user| {
            let direct = world.collaborators.iter().find(|c| {
                c.repo.eq_ignore_ascii_case(&full_name) && c.user.eq_ignore_ascii_case(&user.login)
            });
            let role = match affiliation {
                Affiliation::Direct => direct.map(|c| c.role.clone()),
                Affiliation::Outside => direct
                    .filter(|_| !(org_owned && access::org_member(world, &repo.owner, &user.login)))
                    .map(|c| c.role.clone()),
                Affiliation::All => {
                    access::effective(world, repo, &user.login).map(|grant| grant.role)
                }
            }?;
            Some((user.id, user.login.clone(), role))
        })
        .collect();
    rows.sort_by_key(|(id, _, _)| *id);
    let paged = Pagination::of(&uri).apply(rows);
    let view = view(&state, &origin);
    let body: Vec<Value> = paged
        .items
        .iter()
        .map(|(_, login, role)| view.collaborator(repo, login, role))
        .collect();
    json_with_link(StatusCode::OK, &body, paged.link(&origin, &uri))
}

// --- invitations -------------------------------------------------------------------

/// `GET /repos/{owner}/{repo}/invitations`, paged.
pub(crate) async fn list_invitations(
    State(shared): State<SharedState>,
    InstallationCaller(token): InstallationCaller,
    Path((owner, name)): Path<(String, String)>,
    headers: HeaderMap,
    uri: Uri,
) -> Response {
    invitations(&shared, &token, &headers, &uri, |_| Some((owner, name)))
}

/// `GET /repositories/{repository_id}/invitations`: the same list by
/// repository ID.
pub(crate) async fn list_invitations_by_id(
    State(shared): State<SharedState>,
    InstallationCaller(token): InstallationCaller,
    Path(raw): Path<String>,
    headers: HeaderMap,
    uri: Uri,
) -> Response {
    invitations(&shared, &token, &headers, &uri, |world| {
        numeric(&raw)
            .and_then(|id| access::repo_by_id(world, id))
            .map(|repo| (repo.owner.clone(), repo.name.clone()))
    })
}

/// The invitation list of the repository `locate` names. Its `Link` URLs
/// are on `/repositories/{id}/invitations` whichever path was asked
/// (behaviour `invitation-alias-links`).
fn invitations(
    shared: &Shared,
    token: &TokenRecord,
    headers: &HeaderMap,
    uri: &Uri,
    locate: impl FnOnce(&Seed) -> Option<(String, String)>,
) -> Response {
    let origin = views::origin(shared, headers);
    let state = shared.state();
    let Some((owner, name)) = locate(&state.world) else {
        return auth::not_found();
    };
    let repo = match scoped_repo(
        shared,
        &state.world,
        token,
        &owner,
        &name,
        ADMINISTRATION_READ,
    ) {
        Ok(repo) => repo,
        Err(response) => return response,
    };
    let full_name = repo.full_name();
    let mut list: Vec<&RepositoryInvitation> = state
        .world
        .repository_invitations
        .iter()
        .filter(|i| i.repo.eq_ignore_ascii_case(&full_name))
        .collect();
    list.sort_by_key(|i| i.id);
    let paged = Pagination::of(uri).apply(list);
    let view = view(&state, &origin);
    let body = match invitations_json(shared, &view, &paged.items) {
        Ok(body) => body,
        Err(refusal) => return refusal,
    };
    let alias = format!("/repositories/{}/invitations", repo.id);
    if alias != uri.path()
        && paged.last_page() > 1
        && let Some(refusal) = shared.refuse("invitation-alias-links")
    {
        return refusal;
    }
    json_with_link(StatusCode::OK, &body, paged.link_at(&origin, &alias, uri))
}

/// `PATCH /repos/{owner}/{repo}/invitations/{invitation_id}` with
/// `permissions`: 200 with the invitation.
pub(crate) async fn update_invitation(
    State(shared): State<SharedState>,
    InstallationCaller(token): InstallationCaller,
    Path((owner, name, raw)): Path<(String, String, String)>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let docs =
        "https://docs.github.com/rest/collaborators/invitations#update-a-repository-invitation";
    let body = match object(&body, docs) {
        Ok(body) => body,
        Err(response) => return response,
    };
    let origin = views::origin(&shared, &headers);
    let mut state = shared.state();
    let repo_id = match scoped_repo(
        &shared,
        &state.world,
        &token,
        &owner,
        &name,
        ADMINISTRATION_WRITE,
    ) {
        Ok(repo) => repo.id,
        Err(response) => return response,
    };
    let Some(id) = numeric(&raw) else {
        return auth::not_found();
    };
    let Ok(permissions) = string_field(&body, "permissions") else {
        return failure(
            &shared,
            Failure::InvalidRole {
                field: "permissions",
            },
            docs,
        );
    };
    if let Err(error) = collaboration::update_invitation(&mut state, repo_id, id, permissions) {
        return failure(&shared, error, docs);
    }
    let view = view(&state, &origin);
    let Some(invitation) = state
        .world
        .repository_invitations
        .iter()
        .find(|i| i.id == id)
    else {
        return auth::not_found();
    };
    match invitations_json(&shared, &view, &[invitation]) {
        Ok(mut rendered) => json(StatusCode::OK, &rendered.remove(0)),
        Err(refusal) => refusal,
    }
}

/// `DELETE /repos/{owner}/{repo}/invitations/{invitation_id}`: 204.
pub(crate) async fn delete_invitation(
    State(shared): State<SharedState>,
    InstallationCaller(token): InstallationCaller,
    Path((owner, name, raw)): Path<(String, String, String)>,
) -> Response {
    let mut state = shared.state();
    let repo_id = match scoped_repo(
        &shared,
        &state.world,
        &token,
        &owner,
        &name,
        ADMINISTRATION_WRITE,
    ) {
        Ok(repo) => repo.id,
        Err(response) => return response,
    };
    let Some(id) = numeric(&raw) else {
        return auth::not_found();
    };
    match collaboration::delete_invitation(&mut state, repo_id, id) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => failure(&shared, error, INVITATIONS_DOCS),
    }
}
