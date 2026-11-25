//! Repository collaboration as domain operations on the world: inviting
//! and re-inviting, changing and removing collaborators, editing and
//! deleting invitations, and the invitee's side (accept, decline, leave).
//!
//! The HTTP routes (`crate::repos`, and the admin API's `act` routes) are
//! thin adapters over these functions; GE-05's user-token routes
//! (`PATCH`/`DELETE /user/repository_invitations/{id}`) call
//! [`accept_invitation`] and [`decline_invitation`] the same way. Each
//! operation checks the assumed behaviours it relies on through
//! [`Ctx::allows`] before changing anything, and appends the events GitHub
//! would announce to the outbox (`crate::events`).

use crate::access::{self, Source};
use crate::events::repository_targets;
use crate::seed::{Collaborator, Repo, RepositoryInvitation, Role};
use crate::state::State;
use crate::views::{View, expired};
use chrono::{DateTime, TimeDelta, Utc};
use serde_json::{Value, json};

/// Repository invitations a repository may receive per 24 hours
/// (documented).
pub(crate) const INVITATIONS_PER_DAY: usize = 50;

/// Who acts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Actor {
    /// An installation token: the App's bot account acts.
    App { app_id: u64 },
    /// A user (the admin API's `act` routes; user tokens from GE-05).
    User(String),
}

/// What an operation needs besides the state: the origin for URLs in event
/// payloads, and which behaviours it may rely on (`Shared::allows`).
pub(crate) struct Ctx<'a> {
    pub(crate) origin: &'a str,
    pub(crate) allows: &'a dyn Fn(&str) -> bool,
}

/// Why an operation did nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Failure {
    /// No such user, invitation or collaborator (a 404).
    NotFound,
    /// The role is not one the route accepts (a 422 on `field`).
    InvalidRole { field: &'static str },
    /// The repository received its 50 invitations in the last 24 hours.
    InvitationLimit,
    /// The invitation has expired (behaviour `accept-expired-invitation`).
    Expired,
    /// The organization lets only owners invite outside collaborators
    /// (behaviour `outside-collaborator-policy`).
    OutsideCollaboratorsRestricted,
    /// It would rest on the `assumed` behaviour named, and refuse-assumed
    /// mode is on.
    Refused(&'static str),
}

/// What a collaborator `PUT` did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PutOutcome {
    /// An invitation, new or (re-PUT) the pending one: 201 with it.
    Invited(u64),
    /// The user already has access, perhaps with a changed role: 204.
    NoContent,
}

fn check(ctx: &Ctx<'_>, behaviour: &'static str) -> Result<(), Failure> {
    if (ctx.allows)(behaviour) {
        Ok(())
    } else {
        Err(Failure::Refused(behaviour))
    }
}

fn repo(state: &State, repo_id: u64) -> Result<Repo, Failure> {
    access::repo_by_id(&state.world, repo_id)
        .cloned()
        .ok_or(Failure::NotFound)
}

/// The canonical login of an existing user. An unknown login is a 404
/// (behaviour `unknown-user-404`).
fn user_login(state: &State, ctx: &Ctx<'_>, login: &str) -> Result<String, Failure> {
    match access::user(&state.world, login) {
        Some(user) => Ok(user.login.clone()),
        None => {
            check(ctx, "unknown-user-404")?;
            Err(Failure::NotFound)
        }
    }
}

fn is_pair(repo: &Repo, login: &str) -> impl Fn(&str, &str) -> bool + use<> {
    let full_name = repo.full_name();
    let login = login.to_owned();
    move |r: &str, u: &str| r.eq_ignore_ascii_case(&full_name) && u.eq_ignore_ascii_case(&login)
}

/// `PUT /repos/{owner}/{repo}/collaborators/{username}` with `permission`
/// (default `push`), by `actor`:
///
/// - a direct collaborator: their role becomes `permission` (a change
///   emits `member.edited`); 204 (documented);
/// - a user with access otherwise (an organization member through the
///   base permission or ownership): added as a direct collaborator without
///   an invitation, `member.added`; 204 (documented);
/// - a pending invitation: 201 with it, its role unchanged whatever
///   `permission` asks (behaviour `reput-pending-invitation`; measured:
///   PATCH the invitation to change it);
/// - an expired invitation: replaced by a new one (behaviour
///   `reput-expired-invitation`);
/// - otherwise a new invitation, 201, at most 50 per repository per 24 h.
///
/// On a user-owned repository `permission` is ignored (documented).
pub(crate) fn put_collaborator(
    state: &mut State,
    ctx: &Ctx<'_>,
    repo_id: u64,
    login: &str,
    permission: Option<&str>,
    actor: &Actor,
) -> Result<PutOutcome, Failure> {
    let repo = repo(state, repo_id)?;
    let login = user_login(state, ctx, login)?;
    let org_owned = access::org_owned(&state.world, &repo);
    let role = match permission {
        Some(value) if org_owned => access::parse_put_permission(&state.world, &repo, value)
            .ok_or(Failure::InvalidRole {
                field: "permission",
            })?,
        _ => Role::Push,
    };
    let now = state.clock.now();
    let pair = is_pair(&repo, &login);

    let grants = access::grants(&state.world, &repo, &login);
    if let Some(direct) = grants.iter().find(|g| g.source == Source::Direct) {
        if direct.role != role {
            let from = direct.role.clone();
            if let Some(collaborator) = state
                .world
                .collaborators
                .iter_mut()
                .find(|c| pair(&c.repo, &c.user))
            {
                collaborator.role = role.clone();
            }
            // Measured: no `old_permission` (behaviour `member-edited-changes`).
            let changes = (ctx.allows)("member-edited-changes").then(|| {
                json!({
                    "permission": {
                        "from": access::role_name(&state.world, &repo, &from),
                        "to": access::role_name(&state.world, &repo, &role),
                    },
                })
            });
            emit_member(state, ctx, &repo, "edited", &login, changes, actor, now);
        }
        return Ok(PutOutcome::NoContent);
    }
    if !grants.is_empty() {
        if grants.iter().any(|g| g.source == Source::Owner) || !org_owned {
            return Ok(PutOutcome::NoContent);
        }
        state.world.collaborators.push(Collaborator::new(
            repo.full_name(),
            login.clone(),
            role.clone(),
        ));
        if (ctx.allows)("member-added-directly") {
            let changes = added_changes(state, &repo, &role);
            emit_member(
                state,
                ctx,
                &repo,
                "added",
                &login,
                Some(changes),
                actor,
                now,
            );
        }
        return Ok(PutOutcome::NoContent);
    }
    if org_owned && outside_restricted(state, &repo, &login) {
        check(ctx, "outside-collaborator-policy")?;
        return Err(Failure::OutsideCollaboratorsRestricted);
    }

    let start = state.clock.start();
    if let Some(index) = state
        .world
        .repository_invitations
        .iter()
        .position(|i| pair(&i.repo, &i.invitee))
    {
        if expired(&state.world.repository_invitations[index], start, now) {
            check(ctx, "reput-expired-invitation")?;
            check_limit(state, repo.id, now)?;
            state.world.repository_invitations.remove(index);
        } else {
            check(ctx, "reput-pending-invitation")?;
            return Ok(PutOutcome::Invited(
                state.world.repository_invitations[index].id,
            ));
        }
    } else {
        check_limit(state, repo.id, now)?;
    }

    let highest = state
        .world
        .repository_invitations
        .iter()
        .map(|i| i.id)
        .max()
        .unwrap_or(0);
    let id = state.last_invitation_id.max(highest) + 1;
    state.last_invitation_id = id;
    let mut invitation = RepositoryInvitation::new(id, repo.full_name(), login, role)
        .created_at(DateTime::from_timestamp(now.timestamp(), 0).unwrap_or(now));
    match actor {
        Actor::App { app_id } => invitation.inviter_app_id = Some(*app_id),
        Actor::User(user) => invitation.inviter = Some(user.clone()),
    }
    state.world.repository_invitations.push(invitation);
    state.invitations_sent.push((repo.id, now));
    Ok(PutOutcome::Invited(id))
}

/// Whether inviting `login` to `repo` would make an outside collaborator
/// of an organization that lets only its owners do that. The caller of the
/// collaborator routes is an App, never an owner.
fn outside_restricted(state: &State, repo: &Repo, login: &str) -> bool {
    state.world.orgs.iter().any(|org| {
        org.login.eq_ignore_ascii_case(&repo.owner) && org.outside_collaborators_restricted
    }) && !access::org_member(&state.world, &repo.owner, login)
}

/// The 50-invitations-per-24-hours limit (documented), on the virtual
/// clock, counting invitations sent through the API.
fn check_limit(state: &State, repo_id: u64, now: DateTime<Utc>) -> Result<(), Failure> {
    let since = now - TimeDelta::hours(24);
    let sent = state
        .invitations_sent
        .iter()
        .filter(|(repo, at)| *repo == repo_id && *at > since)
        .count();
    if sent >= INVITATIONS_PER_DAY {
        Err(Failure::InvitationLimit)
    } else {
        Ok(())
    }
}

/// `DELETE /repos/{owner}/{repo}/collaborators/{username}`: removes a
/// direct collaborator (`member.removed`). Removing someone who is not
/// one changes nothing (behaviour `remove-non-collaborator`).
pub(crate) fn remove_collaborator(
    state: &mut State,
    ctx: &Ctx<'_>,
    repo_id: u64,
    login: &str,
    actor: &Actor,
) -> Result<(), Failure> {
    let repo = repo(state, repo_id)?;
    let login = user_login(state, ctx, login)?;
    let pair = is_pair(&repo, &login);
    let Some(index) = state
        .world
        .collaborators
        .iter()
        .position(|c| pair(&c.repo, &c.user))
    else {
        return check(ctx, "remove-non-collaborator");
    };
    state.world.collaborators.remove(index);
    let now = state.clock.now();
    emit_member(state, ctx, &repo, "removed", &login, None, actor, now);
    Ok(())
}

fn invitation_index(state: &State, repo: &Repo, id: u64) -> Result<usize, Failure> {
    let full_name = repo.full_name();
    state
        .world
        .repository_invitations
        .iter()
        .position(|i| i.id == id && i.repo.eq_ignore_ascii_case(&full_name))
        .ok_or(Failure::NotFound)
}

/// `PATCH /repos/{owner}/{repo}/invitations/{id}` with `permissions`
/// (`read`, `write`, `maintain`, `triage` or `admin`; absent keeps the
/// role).
pub(crate) fn update_invitation(
    state: &mut State,
    repo_id: u64,
    id: u64,
    permissions: Option<&str>,
) -> Result<(), Failure> {
    let repo = repo(state, repo_id)?;
    let index = invitation_index(state, &repo, id)?;
    if let Some(value) = permissions {
        let role = access::parse_invitation_permission(value).ok_or(Failure::InvalidRole {
            field: "permissions",
        })?;
        state.world.repository_invitations[index].role = role;
    }
    Ok(())
}

/// `DELETE /repos/{owner}/{repo}/invitations/{id}`.
pub(crate) fn delete_invitation(state: &mut State, repo_id: u64, id: u64) -> Result<(), Failure> {
    let repo = repo(state, repo_id)?;
    let index = invitation_index(state, &repo, id)?;
    state.world.repository_invitations.remove(index);
    Ok(())
}

/// The invitation `id` addressed to `login`.
fn own_invitation(state: &State, login: &str, id: u64) -> Result<usize, Failure> {
    state
        .world
        .repository_invitations
        .iter()
        .position(|i| i.id == id && i.invitee.eq_ignore_ascii_case(login))
        .ok_or(Failure::NotFound)
}

/// The invitee accepts invitation `id` (`PATCH
/// /user/repository_invitations/{id}`): they become a collaborator with
/// the invited role, and `member.added` is emitted ("accepted an
/// invitation to a repository"). An expired invitation cannot be accepted
/// (behaviour `accept-expired-invitation`).
pub(crate) fn accept_invitation(
    state: &mut State,
    ctx: &Ctx<'_>,
    login: &str,
    id: u64,
) -> Result<(), Failure> {
    let index = own_invitation(state, login, id)?;
    let now = state.clock.now();
    if expired(
        &state.world.repository_invitations[index],
        state.clock.start(),
        now,
    ) {
        check(ctx, "accept-expired-invitation")?;
        return Err(Failure::Expired);
    }
    let invitation = state.world.repository_invitations.remove(index);
    let Some(repo) = state
        .world
        .repos
        .iter()
        .find(|r| r.full_name().eq_ignore_ascii_case(&invitation.repo))
        .cloned()
    else {
        return Err(Failure::NotFound);
    };
    let login = invitation.invitee.clone();
    let pair = is_pair(&repo, &login);
    match state
        .world
        .collaborators
        .iter_mut()
        .find(|c| pair(&c.repo, &c.user))
    {
        Some(collaborator) => collaborator.role = invitation.role.clone(),
        None => state.world.collaborators.push(Collaborator::new(
            repo.full_name(),
            login.clone(),
            invitation.role.clone(),
        )),
    }
    let changes = added_changes(state, &repo, &invitation.role);
    let actor = Actor::User(login.clone());
    emit_member(
        state,
        ctx,
        &repo,
        "added",
        &login,
        Some(changes),
        &actor,
        now,
    );
    Ok(())
}

/// The invitee declines invitation `id` (`DELETE
/// /user/repository_invitations/{id}`). No event: GitHub announces none.
pub(crate) fn decline_invitation(state: &mut State, login: &str, id: u64) -> Result<(), Failure> {
    let index = own_invitation(state, login, id)?;
    state.world.repository_invitations.remove(index);
    Ok(())
}

/// A collaborator leaves repository `repo_id` (`member.removed`, sent by
/// them).
pub(crate) fn leave_repository(
    state: &mut State,
    ctx: &Ctx<'_>,
    login: &str,
    repo_id: u64,
) -> Result<(), Failure> {
    let repo = repo(state, repo_id)?;
    let pair = is_pair(&repo, login);
    let index = state
        .world
        .collaborators
        .iter()
        .position(|c| pair(&c.repo, &c.user))
        .ok_or(Failure::NotFound)?;
    let login = state.world.collaborators.remove(index).user;
    let now = state.clock.now();
    let actor = Actor::User(login.clone());
    emit_member(state, ctx, &repo, "removed", &login, None, &actor, now);
    Ok(())
}

/// `member.added`'s `changes` (documented): the granted permission and
/// role name.
fn added_changes(state: &State, repo: &Repo, role: &Role) -> Value {
    json!({
        "permission": {"to": access::legacy_permission(&state.world, repo, role)},
        "role_name": {"to": access::role_name(&state.world, repo, role)},
    })
}

/// Appends a `member` event for `login` on `repo`.
#[allow(
    clippy::too_many_arguments,
    reason = "one call site per action; a struct would only rename them"
)]
fn emit_member(
    state: &mut State,
    ctx: &Ctx<'_>,
    repo: &Repo,
    action: &str,
    login: &str,
    changes: Option<Value>,
    actor: &Actor,
    now: DateTime<Utc>,
) {
    let view = View {
        world: &state.world,
        origin: ctx.origin,
        start: state.clock.start(),
        now,
    };
    let mut payload = json!({
        "action": action,
        "member": view.account(login),
        "repository": view.full_repo(repo),
        "sender": match actor {
            Actor::App { app_id } => view.app_bot(*app_id),
            Actor::User(user) => view.account(user).unwrap_or_default(),
        },
    });
    if let Some(changes) = changes {
        payload["changes"] = changes;
    }
    if let Some(org) = view.organization(&repo.owner) {
        payload["organization"] = org;
    }
    let targets = repository_targets(&state.world, repo, "member");
    state
        .events
        .push(now, "member", Some(action), targets, payload);
}
