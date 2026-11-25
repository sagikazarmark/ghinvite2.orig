//! The organization surface (GE-06): `GET /orgs/{org}`, members and
//! memberships, organization invitations (pending, failed, their teams),
//! teams and team memberships, with installation tokens; and the user's
//! side, `GET /user/memberships/orgs[/{org}]` and `PATCH
//! /user/memberships/orgs/{org}`, with user tokens. Thin adapters over
//! `crate::organization`.
//!
//! The installation-token routes need the `members` organization
//! permission (documented) and an installation on the organization; a
//! token of another account's installation is a 403 (behaviour
//! `org-out-of-scope`). An unknown organization is a 404.
#![allow(
    clippy::result_large_err,
    reason = "a rejection is the response to send, built once per request"
)]

use crate::apps::view;
use crate::auth::{self, InstallationCaller, Permission, UserCaller, find_installation, forbidden};
use crate::collaboration::{Actor, Ctx};
use crate::organization::{self, CreateRole, Invitee, NewInvitation, OrgFailure};
use crate::paginate::{Pagination, query_pairs};
use crate::repos::{object, string_field};
use crate::response::{self, FieldError, json, json_with_link};
use crate::seed::{Access, MembershipState, Org, OrgInvitation, OrgRole, Seed, Team, TeamRole};
use crate::state::{Shared, SharedState, WITHHELD};
use crate::tokens::TokenRecord;
use crate::user_tokens::{Grant, UserTokenRecord};
use crate::views::{self, View, org_invitation_expired, org_invitation_open};
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use serde_json::Value;
use std::collections::BTreeMap;

pub(crate) const MEMBERS_READ: Permission = Permission::new("members", Access::Read);
pub(crate) const MEMBERS_WRITE: Permission = Permission::new("members", Access::Write);
/// `organization_plan: read`: what shows `plan` on `GET /orgs/{org}`
/// (documented).
const PLAN_READ: Permission = Permission::new("organization_plan", Access::Read);
/// `organization_administration: read`: what shows the owner-only
/// settings on `GET /orgs/{org}` (behaviour `org-admin-fields`).
const ADMINISTRATION_READ: Permission =
    Permission::new("organization_administration", Access::Read);

const MEMBERS_DOCS: &str = "https://docs.github.com/rest/orgs/members";
const TEAMS_DOCS: &str = "https://docs.github.com/rest/teams/members";

/// The organization an installation-token route acts on: it exists (else
/// 404) and the token's installation is on it (else 403, behaviour
/// `org-out-of-scope`).
fn scoped_org<'a>(
    shared: &Shared,
    world: &'a Seed,
    token: &TokenRecord,
    login: &str,
    need: Permission,
) -> Result<&'a Org, Response> {
    let Some(org) = organization::org(world, login) else {
        return Err(auth::not_found());
    };
    let on_org = find_installation(world, token.installation_id)
        .is_some_and(|i| i.account.eq_ignore_ascii_case(&org.login));
    if on_org {
        Ok(org)
    } else {
        Err(shared
            .refuse("org-out-of-scope")
            .unwrap_or_else(|| forbidden(need)))
    }
}

fn ctx<'a>(origin: &'a str, allows: &'a dyn Fn(&str) -> bool) -> Ctx<'a> {
    Ctx { origin, allows }
}

fn app_actor(token: &TokenRecord) -> Actor {
    Actor::App {
        app_id: token.app_id,
    }
}

/// A 422 whose details are guesses (behaviour `org-error-messages`):
/// withheld in refuse-assumed mode.
fn unprocessable(
    shared: &Shared,
    resource: &str,
    field: &str,
    code: &str,
    message: &str,
    docs: &str,
) -> Response {
    if !shared.allows("org-error-messages") {
        return response::error(StatusCode::UNPROCESSABLE_ENTITY, WITHHELD, docs);
    }
    response::validation_failed(
        &[FieldError {
            resource: resource.into(),
            field: field.into(),
            code: code.into(),
            message: Some(message.into()),
        }],
        docs,
    )
}

/// The response for a failed operation.
fn failure(shared: &Shared, failure: OrgFailure, docs: &str) -> Response {
    const INVITATION: &str = "OrganizationInvitation";
    match failure {
        OrgFailure::NotFound => auth::not_found(),
        OrgFailure::Invalid {
            field,
            code,
            message,
        } => unprocessable(shared, INVITATION, field, code, message, docs),
        OrgFailure::AlreadyMember => unprocessable(
            shared,
            INVITATION,
            "base",
            "custom",
            "Invitee is already a part of this organization",
            docs,
        ),
        OrgFailure::AlreadyInvited => unprocessable(
            shared,
            INVITATION,
            "base",
            "custom",
            "Invitee has already been invited to this organization",
            docs,
        ),
        // The domain checked `org-invitation-limit-response`, `org-no-seat`
        // and `org-spammed` before failing; these shapes are guesses.
        OrgFailure::InvitationLimit { .. } => response::validation_failed(
            &[FieldError {
                resource: INVITATION.into(),
                field: "base".into(),
                code: "custom".into(),
                message: Some("Over invitation rate limit".into()),
            }],
            docs,
        ),
        OrgFailure::NoSeat => response::validation_failed(
            &[FieldError {
                resource: INVITATION.into(),
                field: "base".into(),
                code: "custom".into(),
                message: Some(
                    "You must purchase at least one more seat to add this user as a member.".into(),
                ),
            }],
            docs,
        ),
        OrgFailure::Spammed => {
            #[derive(serde::Serialize)]
            struct Spam {
                resource: &'static str,
                code: &'static str,
            }
            json(
                StatusCode::UNPROCESSABLE_ENTITY,
                &serde_json::json!({
                    "message": "Validation Failed",
                    "errors": [Spam { resource: INVITATION, code: "spammed" }],
                    "documentation_url": docs,
                    "status": response::status_text(StatusCode::UNPROCESSABLE_ENTITY),
                }),
            )
        }
        OrgFailure::Refused(behaviour) => shared.refuse(behaviour).unwrap_or_else(|| {
            response::error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "octoemu: refusal without a refused behaviour",
                response::OCTOEMU_DOCS,
            )
        }),
    }
}

/// The value of query parameter `name`.
fn query<'a>(uri: &'a Uri, name: &str) -> Option<&'a str> {
    query_pairs(uri).find(|(n, _)| *n == name).map(|(_, v)| v)
}

fn numeric(raw: &str) -> Option<u64> {
    raw.parse().ok()
}

// --- the organization ----------------------------------------------------------

/// `GET /orgs/{org}`. Public fields for any installation token; `plan`
/// with `organization_plan: read` on the organization's installation
/// (documented); the owner-only settings with
/// `organization_administration: read` (behaviour `org-admin-fields`).
pub(crate) async fn get_org(
    State(shared): State<SharedState>,
    InstallationCaller(token): InstallationCaller,
    Path(login): Path<String>,
    headers: HeaderMap,
) -> Response {
    let origin = views::origin(&shared, &headers);
    let state = shared.state();
    let Some(org) = organization::org(&state.world, &login) else {
        return auth::not_found();
    };
    let on_org = find_installation(&state.world, token.installation_id)
        .is_some_and(|i| i.account.eq_ignore_ascii_case(&org.login));
    let plan = on_org && token.has(PLAN_READ.name, PLAN_READ.access);
    let admin = on_org
        && token.has(ADMINISTRATION_READ.name, ADMINISTRATION_READ.access)
        && shared.allows("org-admin-fields");
    json(
        StatusCode::OK,
        &view(&state, &origin).full_org(org, plan, admin),
    )
}

// --- members and memberships ---------------------------------------------------

/// `GET /orgs/{org}/members?role=all|admin|member`, paged: active members.
pub(crate) async fn list_members(
    State(shared): State<SharedState>,
    InstallationCaller(token): InstallationCaller,
    Path(login): Path<String>,
    headers: HeaderMap,
    uri: Uri,
) -> Response {
    let origin = views::origin(&shared, &headers);
    let state = shared.state();
    let org = match scoped_org(&shared, &state.world, &token, &login, MEMBERS_READ) {
        Ok(org) => org,
        Err(response) => return response,
    };
    let role = match query(&uri, "role") {
        None | Some("all") => None,
        Some("admin") => Some(OrgRole::Admin),
        Some("member") => Some(OrgRole::Member),
        Some(_) => {
            return unprocessable(
                &shared,
                "Organization",
                "role",
                "invalid",
                "role must be all, admin or member",
                MEMBERS_DOCS,
            );
        }
    };
    let mut members: Vec<(u64, String)> = state
        .world
        .memberships
        .iter()
        .filter(|m| {
            m.org.eq_ignore_ascii_case(&org.login)
                && m.state == MembershipState::Active
                && role.is_none_or(|r| m.role == r)
        })
        .filter_map(|m| crate::access::user(&state.world, &m.user).map(|u| (u.id, u.login.clone())))
        .collect();
    members.sort();
    let paged = Pagination::of(&uri).apply(members);
    let view = view(&state, &origin);
    let body: Vec<Value> = paged
        .items
        .iter()
        .filter_map(|(_, login)| view.account(login))
        .collect();
    json_with_link(StatusCode::OK, &body, paged.link(&origin, &uri))
}

/// `GET /orgs/{org}/memberships/{username}`: active or pending (an open
/// invitation is a pending membership), else 404.
pub(crate) async fn get_membership(
    State(shared): State<SharedState>,
    InstallationCaller(token): InstallationCaller,
    Path((login, username)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    let origin = views::origin(&shared, &headers);
    let state = shared.state();
    let org = match scoped_org(&shared, &state.world, &token, &login, MEMBERS_READ) {
        Ok(org) => org.login.clone(),
        Err(response) => return response,
    };
    let Some(user) = crate::access::user(&state.world, &username) else {
        return auth::not_found();
    };
    match organization::membership_of(&state, &org, &user.login) {
        Some((role, membership_state)) => json(
            StatusCode::OK,
            &view(&state, &origin).membership(&org, &user.login, role, membership_state),
        ),
        None => auth::not_found(),
    }
}

/// `PUT /orgs/{org}/memberships/{username}` with `{role}` (`admin` or
/// `member`, the default): 200 with the membership; a non-member is
/// invited and `pending` (documented).
pub(crate) async fn put_membership(
    State(shared): State<SharedState>,
    InstallationCaller(token): InstallationCaller,
    Path((login, username)): Path<(String, String)>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let docs = "https://docs.github.com/rest/orgs/members#set-organization-membership-for-a-user";
    let body = match object(&body, docs) {
        Ok(body) => body,
        Err(response) => return response,
    };
    let role = match string_field(&body, "role") {
        Ok(None | Some("member")) => OrgRole::Member,
        Ok(Some("admin")) => OrgRole::Admin,
        _ => {
            return unprocessable(
                &shared,
                "Membership",
                "role",
                "invalid",
                "role must be admin or member",
                docs,
            );
        }
    };
    let origin = views::origin(&shared, &headers);
    let allows = |id: &str| shared.allows(id);
    let ctx = ctx(&origin, &allows);
    let mut state = shared.state();
    let org = match scoped_org(&shared, &state.world, &token, &login, MEMBERS_WRITE) {
        Ok(org) => org.login.clone(),
        Err(response) => return response,
    };
    if let Err(error) =
        organization::put_membership(&mut state, &ctx, &org, &username, role, &app_actor(&token))
    {
        return failure(&shared, error, docs);
    }
    let Some(user) = crate::access::user(&state.world, &username).map(|u| u.login.clone()) else {
        return auth::not_found();
    };
    match organization::membership_of(&state, &org, &user) {
        Some((role, membership_state)) => json(
            StatusCode::OK,
            &view(&state, &origin).membership(&org, &user, role, membership_state),
        ),
        None => auth::not_found(),
    }
}

/// `DELETE /orgs/{org}/memberships/{username}`: 204.
pub(crate) async fn remove_membership(
    State(shared): State<SharedState>,
    InstallationCaller(token): InstallationCaller,
    Path((login, username)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    let origin = views::origin(&shared, &headers);
    let allows = |id: &str| shared.allows(id);
    let ctx = ctx(&origin, &allows);
    let mut state = shared.state();
    let org = match scoped_org(&shared, &state.world, &token, &login, MEMBERS_WRITE) {
        Ok(org) => org.login.clone(),
        Err(response) => return response,
    };
    match organization::remove_membership(&mut state, &ctx, &org, &username, &app_actor(&token)) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => failure(&shared, error, MEMBERS_DOCS),
    }
}

// --- invitations ---------------------------------------------------------------

/// `GET /orgs/{org}/invitations?role=`, paged: open invitations, oldest
/// first. An expired invitation leaves this list (documented 7-day
/// expiry) for `failed_invitations`.
pub(crate) async fn list_invitations(
    State(shared): State<SharedState>,
    InstallationCaller(token): InstallationCaller,
    Path(login): Path<String>,
    headers: HeaderMap,
    uri: Uri,
) -> Response {
    let origin = views::origin(&shared, &headers);
    let state = shared.state();
    let org = match scoped_org(&shared, &state.world, &token, &login, MEMBERS_READ) {
        Ok(org) => org.login.clone(),
        Err(response) => return response,
    };
    let role = query(&uri, "role").filter(|r| *r != "all");
    let (start, now) = (state.clock.start(), state.clock.now());
    let mut list: Vec<&OrgInvitation> = state
        .world
        .org_invitations
        .iter()
        .filter(|i| {
            i.org.eq_ignore_ascii_case(&org)
                && org_invitation_open(i, start, now)
                && role.is_none_or(|r| i.role.as_str() == r)
        })
        .collect();
    list.sort_by_key(|i| i.id);
    let paged = Pagination::of(&uri).apply(list);
    let view = view(&state, &origin);
    let body: Vec<Value> = paged.items.iter().map(|i| view.org_invitation(i)).collect();
    json_with_link(StatusCode::OK, &body, paged.link(&origin, &uri))
}

/// `GET /orgs/{org}/failed_invitations`, paged: invitations with
/// `failed_at`, and expired ones (behaviour
/// `expired-org-invitations-failed`).
pub(crate) async fn list_failed_invitations(
    State(shared): State<SharedState>,
    InstallationCaller(token): InstallationCaller,
    Path(login): Path<String>,
    headers: HeaderMap,
    uri: Uri,
) -> Response {
    let origin = views::origin(&shared, &headers);
    let state = shared.state();
    let org = match scoped_org(&shared, &state.world, &token, &login, MEMBERS_READ) {
        Ok(org) => org.login.clone(),
        Err(response) => return response,
    };
    let (start, now) = (state.clock.start(), state.clock.now());
    let mut list: Vec<&OrgInvitation> = state
        .world
        .org_invitations
        .iter()
        .filter(|i| i.org.eq_ignore_ascii_case(&org) && !org_invitation_open(i, start, now))
        .collect();
    if list
        .iter()
        .any(|i| i.failed_at.is_none() && org_invitation_expired(i, start, now))
        && let Some(refusal) = shared.refuse("expired-org-invitations-failed")
    {
        return refusal;
    }
    list.sort_by_key(|i| i.id);
    let paged = Pagination::of(&uri).apply(list);
    let view = view(&state, &origin);
    let body: Vec<Value> = paged.items.iter().map(|i| view.org_invitation(i)).collect();
    json_with_link(StatusCode::OK, &body, paged.link(&origin, &uri))
}

/// `GET /orgs/{org}/invitations/{invitation_id}/teams`, paged.
pub(crate) async fn invitation_teams(
    State(shared): State<SharedState>,
    InstallationCaller(token): InstallationCaller,
    Path((login, raw)): Path<(String, String)>,
    headers: HeaderMap,
    uri: Uri,
) -> Response {
    let origin = views::origin(&shared, &headers);
    let state = shared.state();
    let org = match scoped_org(&shared, &state.world, &token, &login, MEMBERS_READ) {
        Ok(org) => org.login.clone(),
        Err(response) => return response,
    };
    let Some(id) = numeric(&raw) else {
        return auth::not_found();
    };
    let teams = match organization::invitation_teams(&state.world, &org, id) {
        Ok(teams) => teams,
        Err(error) => return failure(&shared, error, MEMBERS_DOCS),
    };
    let paged = Pagination::of(&uri).apply(teams);
    let view = view(&state, &origin);
    let body: Vec<Value> = paged.items.iter().map(|t| view.team(t)).collect();
    json_with_link(StatusCode::OK, &body, paged.link(&origin, &uri))
}

/// The parsed body of `POST /orgs/{org}/invitations`.
struct CreateBody {
    invitee: Result<u64, String>,
    role: CreateRole,
    team_ids: Vec<u64>,
}

fn invalid(field: &'static str, message: &'static str) -> OrgFailure {
    OrgFailure::Invalid {
        field,
        code: "invalid",
        message,
    }
}

fn create_body(body: &Value) -> Result<CreateBody, OrgFailure> {
    let invitee = match (body.get("invitee_id"), body.get("email")) {
        (Some(Value::Number(id)), None | Some(Value::Null)) => Ok(id
            .as_u64()
            .ok_or_else(|| invalid("invitee_id", "invitee_id must be a user ID"))?),
        (None | Some(Value::Null), Some(Value::String(email))) => {
            if email.is_empty() || !email.contains('@') || email.chars().any(char::is_whitespace) {
                return Err(invalid("email", "email is not a valid email address"));
            }
            Err(email.clone())
        }
        (Some(_), Some(Value::String(_))) => {
            return Err(invalid(
                "base",
                "Exactly one of invitee_id and email is required",
            ));
        }
        _ => {
            return Err(invalid(
                "base",
                "Exactly one of invitee_id and email is required",
            ));
        }
    };
    let role = match string_field(body, "role") {
        Ok(None) => CreateRole::DirectMember,
        Ok(Some(value)) => CreateRole::parse(value).ok_or_else(|| {
            invalid(
                "role",
                "role must be admin, direct_member, billing_manager or reinstate",
            )
        })?,
        Err(()) => return Err(invalid("role", "role must be a string")),
    };
    let team_ids = match body.get("team_ids") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(ids)) => ids
            .iter()
            .map(|id| id.as_u64().filter(|id| *id > 0))
            .collect::<Option<Vec<u64>>>()
            .ok_or_else(|| invalid("team_ids", "team_ids must be team IDs"))?,
        Some(_) => return Err(invalid("team_ids", "team_ids must be team IDs")),
    };
    Ok(CreateBody {
        invitee,
        role,
        team_ids,
    })
}

/// `POST /orgs/{org}/invitations` with `{invitee_id | email, role,
/// team_ids}`: 201 with the invitation, `organization.member_invited`.
pub(crate) async fn create_invitation(
    State(shared): State<SharedState>,
    InstallationCaller(token): InstallationCaller,
    Path(login): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let docs = "https://docs.github.com/rest/orgs/members#create-an-organization-invitation";
    let body = match object(&body, docs) {
        Ok(body) => body,
        Err(response) => return response,
    };
    let origin = views::origin(&shared, &headers);
    let allows = |id: &str| shared.allows(id);
    let ctx = ctx(&origin, &allows);
    let mut state = shared.state();
    let org = match scoped_org(&shared, &state.world, &token, &login, MEMBERS_WRITE) {
        Ok(org) => org.login.clone(),
        Err(response) => return response,
    };
    let parsed = match create_body(&body) {
        Ok(parsed) => parsed,
        Err(error) => return failure(&shared, error, docs),
    };
    let invitee = match parsed.invitee {
        Ok(id) => match state.world.users.iter().find(|u| u.id == id) {
            Some(user) => Invitee::User(user.login.clone()),
            None => {
                return failure(
                    &shared,
                    invalid("invitee_id", "Invitee does not exist"),
                    docs,
                );
            }
        },
        Err(email) => Invitee::Email(email),
    };
    let new = NewInvitation {
        role: parsed.role,
        team_ids: parsed.team_ids,
        team_roles: BTreeMap::new(),
    };
    match organization::create_invitation(&mut state, &ctx, &org, &invitee, new, &app_actor(&token))
    {
        Ok(id) => {
            let view = view(&state, &origin);
            match state.world.org_invitations.iter().find(|i| i.id == id) {
                Some(invitation) => json(StatusCode::CREATED, &view.org_invitation(invitation)),
                None => auth::not_found(),
            }
        }
        Err(error) => failure(&shared, error, docs),
    }
}

/// `DELETE /orgs/{org}/invitations/{invitation_id}`: 204.
pub(crate) async fn cancel_invitation(
    State(shared): State<SharedState>,
    InstallationCaller(token): InstallationCaller,
    Path((login, raw)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    let origin = views::origin(&shared, &headers);
    let allows = |id: &str| shared.allows(id);
    let ctx = ctx(&origin, &allows);
    let mut state = shared.state();
    let org = match scoped_org(&shared, &state.world, &token, &login, MEMBERS_WRITE) {
        Ok(org) => org.login.clone(),
        Err(response) => return response,
    };
    let Some(id) = numeric(&raw) else {
        return auth::not_found();
    };
    match organization::cancel_invitation(&mut state, &ctx, &org, id) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => failure(&shared, error, MEMBERS_DOCS),
    }
}

// --- teams ---------------------------------------------------------------------

/// `GET /orgs/{org}/teams`, paged, by ID.
pub(crate) async fn list_teams(
    State(shared): State<SharedState>,
    InstallationCaller(token): InstallationCaller,
    Path(login): Path<String>,
    headers: HeaderMap,
    uri: Uri,
) -> Response {
    let origin = views::origin(&shared, &headers);
    let state = shared.state();
    let org = match scoped_org(&shared, &state.world, &token, &login, MEMBERS_READ) {
        Ok(org) => org.login.clone(),
        Err(response) => return response,
    };
    let mut teams: Vec<&Team> = state
        .world
        .teams
        .iter()
        .filter(|t| t.org.eq_ignore_ascii_case(&org))
        .collect();
    teams.sort_by_key(|t| t.id);
    let paged = Pagination::of(&uri).apply(teams);
    let view = view(&state, &origin);
    let body: Vec<Value> = paged.items.iter().map(|t| view.team(t)).collect();
    json_with_link(StatusCode::OK, &body, paged.link(&origin, &uri))
}

/// The team a team route names, or its 404.
fn scoped_team<'a>(
    shared: &Shared,
    world: &'a Seed,
    token: &TokenRecord,
    login: &str,
    slug: &str,
    need: Permission,
) -> Result<&'a Team, Response> {
    let org = scoped_org(shared, world, token, login, need)?;
    crate::access::team(world, &org.login, slug).ok_or_else(auth::not_found)
}

/// `GET /orgs/{org}/teams/{team_slug}`.
pub(crate) async fn get_team(
    State(shared): State<SharedState>,
    InstallationCaller(token): InstallationCaller,
    Path((login, slug)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    let origin = views::origin(&shared, &headers);
    let state = shared.state();
    match scoped_team(&shared, &state.world, &token, &login, &slug, MEMBERS_READ) {
        Ok(team) => json(StatusCode::OK, &view(&state, &origin).full_team(team)),
        Err(response) => response,
    }
}

/// Renders `login`'s membership of `team`, or 404.
fn team_membership_response(
    shared: &Shared,
    state: &crate::state::State,
    origin: &str,
    team: &Team,
    login: &str,
) -> Response {
    match organization::team_membership_of(state, team, login) {
        Some((role, membership_state, via_invitation)) => {
            if via_invitation
                && let Some(refusal) = shared.refuse("invited-team-membership-pending")
            {
                return refusal;
            }
            json(
                StatusCode::OK,
                &view(state, origin).team_membership(team, login, role, membership_state),
            )
        }
        None => auth::not_found(),
    }
}

/// `GET /orgs/{org}/teams/{team_slug}/memberships/{username}`.
pub(crate) async fn get_team_membership(
    State(shared): State<SharedState>,
    InstallationCaller(token): InstallationCaller,
    Path((login, slug, username)): Path<(String, String, String)>,
    headers: HeaderMap,
) -> Response {
    let origin = views::origin(&shared, &headers);
    let state = shared.state();
    let team = match scoped_team(&shared, &state.world, &token, &login, &slug, MEMBERS_READ) {
        Ok(team) => team.clone(),
        Err(response) => return response,
    };
    let Some(user) = crate::access::user(&state.world, &username).map(|u| u.login.clone()) else {
        return auth::not_found();
    };
    team_membership_response(&shared, &state, &origin, &team, &user)
}

/// `PUT /orgs/{org}/teams/{team_slug}/memberships/{username}` with
/// `{role}` (`member`, the default, or `maintainer`): 200 `{url, role,
/// state}`.
pub(crate) async fn put_team_membership(
    State(shared): State<SharedState>,
    InstallationCaller(token): InstallationCaller,
    Path((login, slug, username)): Path<(String, String, String)>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let docs =
        "https://docs.github.com/rest/teams/members#add-or-update-team-membership-for-a-user";
    let body = match object(&body, docs) {
        Ok(body) => body,
        Err(response) => return response,
    };
    let role = match string_field(&body, "role") {
        Ok(None | Some("member")) => TeamRole::Member,
        Ok(Some("maintainer")) => TeamRole::Maintainer,
        _ => {
            return unprocessable(
                &shared,
                "TeamMember",
                "role",
                "invalid",
                "role must be member or maintainer",
                docs,
            );
        }
    };
    let origin = views::origin(&shared, &headers);
    let allows = |id: &str| shared.allows(id);
    let ctx = ctx(&origin, &allows);
    let mut state = shared.state();
    let team = match scoped_team(&shared, &state.world, &token, &login, &slug, MEMBERS_WRITE) {
        Ok(team) => team.clone(),
        Err(response) => return response,
    };
    let Some(user) = crate::access::user(&state.world, &username).map(|u| u.login.clone()) else {
        return auth::not_found();
    };
    if let Err(error) =
        organization::put_team_membership(&mut state, &ctx, &team, &user, role, &app_actor(&token))
    {
        return failure(&shared, error, docs);
    }
    team_membership_response(&shared, &state, &origin, &team, &user)
}

/// `DELETE /orgs/{org}/teams/{team_slug}/memberships/{username}`: 204.
pub(crate) async fn remove_team_membership(
    State(shared): State<SharedState>,
    InstallationCaller(token): InstallationCaller,
    Path((login, slug, username)): Path<(String, String, String)>,
    headers: HeaderMap,
) -> Response {
    let origin = views::origin(&shared, &headers);
    let allows = |id: &str| shared.allows(id);
    let ctx = ctx(&origin, &allows);
    let mut state = shared.state();
    let team = match scoped_team(&shared, &state.world, &token, &login, &slug, MEMBERS_WRITE) {
        Ok(team) => team.clone(),
        Err(response) => return response,
    };
    match organization::remove_team_membership(
        &mut state,
        &ctx,
        &team,
        &username,
        &app_actor(&token),
    ) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => failure(&shared, error, TEAMS_DOCS),
    }
}

// --- the user's side -------------------------------------------------------------

/// `login`'s memberships (and pending invitations by login), as views,
/// ordered by organization ID.
fn user_memberships(
    state: &crate::state::State,
    view: &View<'_>,
    login: &str,
    filter: Option<MembershipState>,
) -> Vec<Value> {
    let mut orgs: Vec<&Org> = state.world.orgs.iter().collect();
    orgs.sort_by_key(|o| o.id);
    orgs.into_iter()
        .filter_map(|org| {
            let (role, membership_state) = organization::membership_of(state, &org.login, login)?;
            filter
                .is_none_or(|f| f == membership_state)
                .then(|| view.membership(&org.login, login, role, membership_state))
        })
        .collect()
}

/// `GET /user/memberships/orgs?state=active|pending`, paged. A GitHub App
/// user token may call it (behaviour `user-token-org-membership-list`; the
/// route is not in GitHub's permission table).
pub(crate) async fn list_user_memberships(
    State(shared): State<SharedState>,
    UserCaller(token): UserCaller,
    headers: HeaderMap,
    uri: Uri,
) -> Response {
    if matches!(token.grant, Grant::UserToken { .. })
        && let Some(refusal) = shared.refuse("user-token-org-membership-list")
    {
        return refusal;
    }
    let filter = match query(&uri, "state") {
        None => None,
        Some("active") => Some(MembershipState::Active),
        Some("pending") => Some(MembershipState::Pending),
        Some(_) => {
            return unprocessable(
                &shared,
                "Membership",
                "state",
                "invalid",
                "state must be active or pending",
                MEMBERS_DOCS,
            );
        }
    };
    let origin = views::origin(&shared, &headers);
    let state = shared.state();
    let view = view(&state, &origin);
    let all = user_memberships(&state, &view, &token.login, filter);
    let paged = Pagination::of(&uri).apply(all);
    json_with_link(StatusCode::OK, &paged.items, paged.link(&origin, &uri))
}

fn user_membership(
    shared: &Shared,
    token: &UserTokenRecord,
    login: &str,
    origin: &str,
) -> Response {
    let state = shared.state();
    let Some(org) = organization::org(&state.world, login).map(|o| o.login.clone()) else {
        return auth::not_found();
    };
    match organization::membership_of(&state, &org, &token.login) {
        Some((role, membership_state)) => json(
            StatusCode::OK,
            &view(&state, origin).membership(&org, &token.login, role, membership_state),
        ),
        None => auth::not_found(),
    }
}

/// `GET /user/memberships/orgs/{org}`: 200 or 404.
pub(crate) async fn get_user_membership(
    State(shared): State<SharedState>,
    UserCaller(token): UserCaller,
    Path(login): Path<String>,
    headers: HeaderMap,
) -> Response {
    let origin = views::origin(&shared, &headers);
    user_membership(&shared, &token, &login, &origin)
}

/// `PATCH /user/memberships/orgs/{org}` with `{"state": "active"}`: accepts
/// the pending membership; 202 with it, already active (measured: GitHub
/// answers 202 though its docs say 200). 404 without a pending membership
/// or invitation.
pub(crate) async fn accept_membership(
    State(shared): State<SharedState>,
    UserCaller(token): UserCaller,
    Path(login): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let docs = "https://docs.github.com/rest/orgs/members#update-an-organization-membership-for-the-authenticated-user";
    let body = match object(&body, docs) {
        Ok(body) => body,
        Err(response) => return response,
    };
    if string_field(&body, "state") != Ok(Some("active")) {
        return unprocessable(
            &shared,
            "Membership",
            "state",
            "invalid",
            "state must be active",
            docs,
        );
    }
    let origin = views::origin(&shared, &headers);
    let allows = |id: &str| shared.allows(id);
    let ctx = ctx(&origin, &allows);
    let result = {
        let mut state = shared.state();
        organization::accept(&mut state, &ctx, &login, &token.login)
    };
    if let Err(error) = result {
        return failure(&shared, error, docs);
    }
    let state = shared.state();
    let Some(org) = organization::org(&state.world, &login).map(|o| o.login.clone()) else {
        return auth::not_found();
    };
    let view = view(&state, &origin);
    let body = match organization::membership_of(&state, &org, &token.login) {
        Some((role, membership_state)) => {
            view.membership(&org, &token.login, role, membership_state)
        }
        // A billing manager's invitation makes no membership.
        None => view.membership(
            &org,
            &token.login,
            "billing_manager",
            MembershipState::Active,
        ),
    };
    json(StatusCode::ACCEPTED, &body)
}

/// The admin API's `act` routes for organizations
/// (`POST /__octoemu/act/{login}/orgs/{org}/accept|decline|leave`): the
/// same domain operations as the user routes.
pub(crate) fn act(shared: &Shared, origin: &str, login: &str, org: &str, action: &str) -> Response {
    let allows = |id: &str| shared.allows(id);
    let ctx = ctx(origin, &allows);
    let result = {
        let mut state = shared.state();
        match action {
            "accept" => organization::accept(&mut state, &ctx, org, login),
            "decline" => organization::decline(&mut state, &ctx, org, login),
            "leave" => organization::leave(&mut state, &ctx, org, login),
            _ => {
                return response::error(
                    StatusCode::NOT_FOUND,
                    "octoemu: no such admin route",
                    response::OCTOEMU_DOCS,
                );
            }
        }
    };
    match result {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(OrgFailure::NotFound) => response::error(
            StatusCode::NOT_FOUND,
            "octoemu: no such organization, user, invitation or membership for this user",
            response::OCTOEMU_DOCS,
        ),
        Err(OrgFailure::Refused(behaviour)) => shared.refuse(behaviour).unwrap_or_else(|| {
            response::error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "octoemu: refused",
                response::OCTOEMU_DOCS,
            )
        }),
        Err(other) => response::error(
            StatusCode::UNPROCESSABLE_ENTITY,
            &format!("octoemu: {other:?}"),
            response::OCTOEMU_DOCS,
        ),
    }
}

/// `POST /__octoemu/orgs/{org}/spammed` `{"spammed": bool}`: whether the
/// organization's invitation writes answer `spammed` (behaviour
/// `org-spammed`). `false` when the organization is unknown.
pub(crate) fn set_spammed(shared: &Shared, org: &str, spammed: bool) -> bool {
    let mut state = shared.state();
    let Some(login) = organization::org(&state.world, org).map(|o| o.login.to_lowercase()) else {
        return false;
    };
    if spammed {
        state.spammed_orgs.insert(login);
    } else {
        state.spammed_orgs.remove(&login);
    }
    true
}
