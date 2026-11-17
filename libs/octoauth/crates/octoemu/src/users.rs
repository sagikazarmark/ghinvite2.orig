//! Users (GE-03): `GET /users/{username}`, `GET /user/{account_id}` and the
//! avatar images at `/avatars/u/{id}`; the signed-in user (GE-05): `GET
//! /user` and `GET /user/emails`.

use crate::apps::view;
use crate::auth::{self, Permission, UserCaller};
use crate::paginate::Pagination;
use crate::response::{json, json_with_link};
use crate::state::SharedState;
use crate::views::{self, BOT_ID_OFFSET};
use axum::extract::{Path, State};
use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE};
use axum::http::{HeaderMap, HeaderValue, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

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

/// `email_addresses: read`, which `GET /user/emails` needs (documented).
pub(crate) const EMAIL_ADDRESSES_READ: Permission =
    Permission::new("email_addresses", crate::seed::Access::Read);

/// `GET /user`: the signed-in user, as `GET /users/{username}` reports
/// them (`email` is the public primary address, else null).
pub(crate) async fn get_authenticated(
    State(shared): State<SharedState>,
    UserCaller(token): UserCaller,
    headers: HeaderMap,
) -> Response {
    let origin = views::origin(&shared, &headers);
    let state = shared.state();
    match view(&state, &origin).full_account(&token.login) {
        Some(body) => json(StatusCode::OK, &body),
        None => auth::not_found(),
    }
}

/// `GET /user/emails`, paged: `{email, primary, verified, visibility}`.
pub(crate) async fn emails(
    State(shared): State<SharedState>,
    UserCaller(token): UserCaller,
    headers: HeaderMap,
    uri: Uri,
) -> Response {
    let origin = views::origin(&shared, &headers);
    let addresses = {
        let state = shared.state();
        match crate::access::user(&state.world, &token.login) {
            Some(user) => user.email_addresses(),
            None => return auth::not_found(),
        }
    };
    if addresses
        .iter()
        .any(|e| e.email.ends_with("@users.noreply.github.com"))
        && let Some(refusal) = shared.refuse("noreply-email")
    {
        return refusal;
    }
    let paged = Pagination::of(&uri).apply(addresses);
    let body: Vec<Value> = paged
        .items
        .iter()
        .map(|e| {
            json!({
                "email": e.email,
                "primary": e.primary,
                "verified": e.verified,
                "visibility": e.visibility,
            })
        })
        .collect();
    json_with_link(StatusCode::OK, &body, paged.link(&origin, &uri))
}

/// Renames user `old` to `new`: the user keeps their ID, and every
/// reference (collaborations, memberships, invitations, installation
/// requests) follows them. Refused for a login already taken and, as an
/// emulator limitation (GitHub redirects their repositories), for a user
/// owning repositories, an App or an installation.
pub(crate) fn rename_user(
    world: &mut crate::seed::Seed,
    old: &str,
    new: &str,
) -> Result<(), String> {
    let same = |login: &str| login.eq_ignore_ascii_case(old);
    if !world.users.iter().any(|u| same(&u.login)) {
        return Err(format!("no user {old:?}"));
    }
    if old.eq_ignore_ascii_case(new) {
        if let Some(user) = world.users.iter_mut().find(|u| same(&u.login)) {
            user.login = new.to_owned();
        }
        return Ok(());
    }
    if world
        .users
        .iter()
        .any(|u| u.login.eq_ignore_ascii_case(new))
        || world.orgs.iter().any(|o| o.login.eq_ignore_ascii_case(new))
    {
        return Err(format!("the login {new:?} is taken"));
    }
    if world.repos.iter().any(|r| same(&r.owner))
        || world
            .apps
            .iter()
            .any(|a| a.owner.as_deref().is_some_and(same))
        || world.installations.iter().any(|i| same(&i.account))
    {
        return Err(format!(
            "{old:?} owns repositories, an App or an installation"
        ));
    }
    let rename = |login: &mut String| {
        if same(login) {
            *login = new.to_owned();
        }
    };
    for user in &mut world.users {
        rename(&mut user.login);
    }
    for collaborator in &mut world.collaborators {
        rename(&mut collaborator.user);
    }
    for membership in &mut world.memberships {
        rename(&mut membership.user);
    }
    for membership in &mut world.team_memberships {
        rename(&mut membership.user);
    }
    for invitation in &mut world.repository_invitations {
        rename(&mut invitation.invitee);
        if let Some(inviter) = invitation.inviter.as_mut() {
            rename(inviter);
        }
    }
    for invitation in &mut world.org_invitations {
        if let Some(invitee) = invitation.invitee.as_mut() {
            rename(invitee);
        }
        if let Some(inviter) = invitation.inviter.as_mut() {
            rename(inviter);
        }
    }
    for request in &mut world.installation_requests {
        rename(&mut request.requester);
    }
    for installation in &mut world.installations {
        if let Some(by) = installation.suspended_by.as_mut() {
            rename(by);
        }
    }
    Ok(())
}
