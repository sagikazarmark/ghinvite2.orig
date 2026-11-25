//! Organization membership as domain operations on the world (GE-06):
//! invitations (by user or email, with teams), memberships and their
//! `pending` → `active` acceptance, team memberships, removals, the
//! invitation budget and the seat check, and the `organization` /
//! `membership` events GitHub announces.
//!
//! The HTTP routes (`crate::orgs`, and the admin API's `act` routes) are
//! thin adapters over these functions, as `crate::repos` is over
//! `crate::collaboration`. Each operation checks the assumed behaviours it
//! relies on through [`Ctx::allows`] before changing anything.
//!
//! **State.** An active membership, or a pending one, is a
//! `world.memberships` entry (a seed may hold pending ones). An invitation
//! sent through the API is a `world.org_invitations` entry, and the
//! invitee's pending membership (and pending team memberships, from its
//! `team_ids`) are derived from it while it is open, so cancelling or
//! expiring it needs no second bookkeeping. Accepting turns either into
//! an active membership.

use crate::access;
use crate::collaboration::{Actor, Ctx};
use crate::events::{organization_targets, sender};
use crate::seed::{
    InvitationRole, Membership, MembershipState, Org, OrgInvitation, OrgRole, Seed, Team,
    TeamMembership, TeamRole,
};
use crate::state::State;
use crate::views::{View, org_invitation_open, org_role_name};
use chrono::{DateTime, Months, TimeDelta, Utc};
use serde_json::{Value, json};
use std::collections::BTreeMap;

/// Invitations a new free organization may send per 24 hours (documented).
pub(crate) const INVITATIONS_PER_DAY: usize = 50;

/// Invitations per 24 hours for an organization older than a month or on
/// a paid plan (documented).
pub(crate) const INVITATIONS_PER_DAY_ESTABLISHED: usize = 500;

/// Why an operation did nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum OrgFailure {
    /// No such organization, user, team, invitation or membership (a 404).
    NotFound,
    /// A 422 on `field` with `code`; `message` is a guess (behaviour
    /// `org-error-messages`).
    Invalid {
        field: &'static str,
        code: &'static str,
        message: &'static str,
    },
    /// The invitee is already a member (a 422, behaviour
    /// `org-error-messages`).
    AlreadyMember,
    /// The invitee already has an open invitation (a 422, behaviour
    /// `org-error-messages`).
    AlreadyInvited,
    /// The organization's invitation budget for 24 hours is spent.
    InvitationLimit { limit: usize },
    /// A paid plan without a free seat (behaviour `org-no-seat`).
    NoSeat,
    /// The organization's invitation writes answer `spammed` (behaviour
    /// `org-spammed`).
    Spammed,
    /// It would rest on the `assumed` behaviour named, and refuse-assumed
    /// mode is on.
    Refused(&'static str),
}

fn check(ctx: &Ctx<'_>, behaviour: &'static str) -> Result<(), OrgFailure> {
    if (ctx.allows)(behaviour) {
        Ok(())
    } else {
        Err(OrgFailure::Refused(behaviour))
    }
}

/// Who an invitation addresses.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Invitee {
    /// A user, by login.
    User(String),
    /// An email address.
    Email(String),
}

/// The role a new invitation asks for (`POST /orgs/{org}/invitations`'s
/// `role`, documented).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CreateRole {
    DirectMember,
    Admin,
    BillingManager,
    /// The role the invitee had before being removed.
    Reinstate,
}

impl CreateRole {
    /// `admin`, `direct_member`, `billing_manager` or `reinstate`.
    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value {
            "direct_member" => Some(Self::DirectMember),
            "admin" => Some(Self::Admin),
            "billing_manager" => Some(Self::BillingManager),
            "reinstate" => Some(Self::Reinstate),
            _ => None,
        }
    }
}

/// The canonical organization `login` names.
pub(crate) fn org<'a>(world: &'a Seed, login: &str) -> Option<&'a Org> {
    world
        .orgs
        .iter()
        .find(|o| o.login.eq_ignore_ascii_case(login))
}

fn org_login(state: &State, login: &str) -> Result<String, OrgFailure> {
    org(&state.world, login)
        .map(|o| o.login.clone())
        .ok_or(OrgFailure::NotFound)
}

fn user_login(state: &State, login: &str) -> Result<String, OrgFailure> {
    access::user(&state.world, login)
        .map(|u| u.login.clone())
        .ok_or(OrgFailure::NotFound)
}

/// The membership role an invitation role leads to.
pub(crate) fn invited_role_name(role: InvitationRole) -> &'static str {
    match role {
        InvitationRole::Admin => "admin",
        InvitationRole::DirectMember => "member",
        InvitationRole::BillingManager => "billing_manager",
        InvitationRole::HiringManager => "hiring_manager",
    }
}

/// The index of `login`'s open invitation to `org`.
pub(crate) fn open_invitation(state: &State, org: &str, login: &str) -> Option<usize> {
    let (start, now) = (state.clock.start(), state.clock.now());
    state.world.org_invitations.iter().position(|i| {
        i.org.eq_ignore_ascii_case(org)
            && i.invitee
                .as_deref()
                .is_some_and(|invitee| invitee.eq_ignore_ascii_case(login))
            && org_invitation_open(i, start, now)
    })
}

/// The index of an open email invitation to `org` addressed to one of
/// `login`'s verified addresses (behaviour `email-invitation-accept`).
fn email_invitation(state: &State, org: &str, login: &str) -> Option<usize> {
    let user = access::user(&state.world, login)?;
    let verified: Vec<String> = user
        .email_addresses()
        .into_iter()
        .filter(|e| e.verified)
        .map(|e| e.email.to_lowercase())
        .collect();
    let (start, now) = (state.clock.start(), state.clock.now());
    state.world.org_invitations.iter().position(|i| {
        i.org.eq_ignore_ascii_case(org)
            && i.email
                .as_deref()
                .is_some_and(|email| verified.contains(&email.to_lowercase()))
            && org_invitation_open(i, start, now)
    })
}

/// The index of `login`'s membership of `org`, any state.
fn membership_index(world: &Seed, org: &str, login: &str) -> Option<usize> {
    world
        .memberships
        .iter()
        .position(|m| m.org.eq_ignore_ascii_case(org) && m.user.eq_ignore_ascii_case(login))
}

/// `login`'s membership of `org` as the API reports it: `(role, state)`.
/// An open invitation is a pending membership with the invited role.
pub(crate) fn membership_of(
    state: &State,
    org: &str,
    login: &str,
) -> Option<(&'static str, MembershipState)> {
    if let Some(index) = membership_index(&state.world, org, login) {
        let membership = &state.world.memberships[index];
        return Some((org_role_name(membership.role), membership.state));
    }
    open_invitation(state, org, login).map(|index| {
        (
            invited_role_name(state.world.org_invitations[index].role),
            MembershipState::Pending,
        )
    })
}

/// `login`'s membership of `team` as the API reports it: a stored one, or
/// pending through an open invitation naming the team (behaviour
/// `invited-team-membership-pending`, checked by the caller).
pub(crate) fn team_membership_of(
    state: &State,
    team: &Team,
    login: &str,
) -> Option<(TeamRole, MembershipState, bool)> {
    let key = access::team_key(team);
    if let Some(stored) = state
        .world
        .team_memberships
        .iter()
        .find(|m| m.team.eq_ignore_ascii_case(&key) && m.user.eq_ignore_ascii_case(login))
    {
        return Some((stored.role, stored.state, false));
    }
    let index = open_invitation(state, &team.org, login)?;
    let invitation = &state.world.org_invitations[index];
    invitation.team_ids.contains(&team.id).then(|| {
        (
            invitation
                .team_roles
                .get(&team.id)
                .copied()
                .unwrap_or_default(),
            MembershipState::Pending,
            true,
        )
    })
}

/// Seats in use: the plan's pinned `filled_seats`, else active and pending
/// members plus open invitations that would make a member (behaviour
/// `org-no-seat`).
pub(crate) fn filled_seats(
    world: &Seed,
    org: &Org,
    start: DateTime<Utc>,
    now: DateTime<Utc>,
) -> u32 {
    if let Some(pinned) = org.plan.filled_seats {
        return pinned;
    }
    let members = world
        .memberships
        .iter()
        .filter(|m| m.org.eq_ignore_ascii_case(&org.login))
        .count();
    let invited = world
        .org_invitations
        .iter()
        .filter(|i| {
            i.org.eq_ignore_ascii_case(&org.login)
                && matches!(i.role, InvitationRole::DirectMember | InvitationRole::Admin)
                && org_invitation_open(i, start, now)
        })
        .count();
    u32::try_from(members + invited).unwrap_or(u32::MAX)
}

/// Whether `org` is on a paid plan (anything but `free`).
fn paid(org: &Org) -> bool {
    !org.plan.name.eq_ignore_ascii_case("free")
}

/// The organization's invitation budget per 24 hours (documented): 50, or
/// 500 once it is older than a month or on a paid plan.
pub(crate) fn invitation_budget(org: &Org, start: DateTime<Utc>, now: DateTime<Utc>) -> usize {
    let created = org.created_at.unwrap_or(start);
    let established = created
        .checked_add_months(Months::new(1))
        .is_some_and(|month| now > month);
    if established || paid(org) {
        INVITATIONS_PER_DAY_ESTABLISHED
    } else {
        INVITATIONS_PER_DAY
    }
}

/// Spends one invitation of `org`'s budget, or fails when it is spent.
fn spend_budget(state: &mut State, ctx: &Ctx<'_>, org: &Org) -> Result<(), OrgFailure> {
    let now = state.clock.now();
    let limit = invitation_budget(org, state.clock.start(), now);
    let since = now - TimeDelta::hours(24);
    let key = org.login.to_lowercase();
    let sent = state
        .org_invitations_sent
        .iter()
        .filter(|(o, at)| *o == key && *at > since)
        .count();
    if sent >= limit {
        check(ctx, "org-invitation-limit-response")?;
        return Err(OrgFailure::InvitationLimit { limit });
    }
    state.org_invitations_sent.push((key, now));
    Ok(())
}

/// The seat check for an invitation that would make a member: a paid plan
/// with every seat filled refuses it (behaviour `org-no-seat`).
fn check_seat(state: &State, ctx: &Ctx<'_>, org: &Org) -> Result<(), OrgFailure> {
    if !paid(org) || org.plan.seats == 0 {
        return Ok(());
    }
    let filled = filled_seats(&state.world, org, state.clock.start(), state.clock.now());
    if filled >= org.plan.seats {
        check(ctx, "org-no-seat")?;
        return Err(OrgFailure::NoSeat);
    }
    Ok(())
}

fn check_spammed(state: &State, ctx: &Ctx<'_>, org: &str) -> Result<(), OrgFailure> {
    if state.spammed_orgs.contains(&org.to_lowercase()) {
        check(ctx, "org-spammed")?;
        return Err(OrgFailure::Spammed);
    }
    Ok(())
}

/// What a new invitation carries besides its addressee.
pub(crate) struct NewInvitation {
    pub(crate) role: CreateRole,
    pub(crate) team_ids: Vec<u64>,
    pub(crate) team_roles: BTreeMap<u64, TeamRole>,
}

/// `POST /orgs/{org}/invitations` (and the invitations PUT membership and
/// team PUT send for non-members): validates, checks the spam flag, the
/// budget and the seats, records the invitation and emits
/// `organization.member_invited`. Returns its ID.
pub(crate) fn create_invitation(
    state: &mut State,
    ctx: &Ctx<'_>,
    org_name: &str,
    invitee: &Invitee,
    new: NewInvitation,
    actor: &Actor,
) -> Result<u64, OrgFailure> {
    let org = org(&state.world, org_name)
        .cloned()
        .ok_or(OrgFailure::NotFound)?;
    let invitee = match invitee {
        Invitee::User(login) => {
            let Some(user) = access::user(&state.world, login) else {
                return Err(OrgFailure::Invalid {
                    field: "invitee_id",
                    code: "invalid",
                    message: "Invitee does not exist",
                });
            };
            let login = user.login.clone();
            if membership_index(&state.world, &org.login, &login).is_some() {
                return Err(OrgFailure::AlreadyMember);
            }
            if open_invitation(state, &org.login, &login).is_some() {
                return Err(OrgFailure::AlreadyInvited);
            }
            Invitee::User(login)
        }
        Invitee::Email(email) => {
            let (start, now) = (state.clock.start(), state.clock.now());
            if state.world.org_invitations.iter().any(|i| {
                i.org.eq_ignore_ascii_case(&org.login)
                    && i.email
                        .as_deref()
                        .is_some_and(|e| e.eq_ignore_ascii_case(email))
                    && org_invitation_open(i, start, now)
            }) {
                return Err(OrgFailure::AlreadyInvited);
            }
            Invitee::Email(email.clone())
        }
    };
    let role = match new.role {
        CreateRole::DirectMember => InvitationRole::DirectMember,
        CreateRole::Admin => InvitationRole::Admin,
        CreateRole::BillingManager => InvitationRole::BillingManager,
        CreateRole::Reinstate => {
            check(ctx, "org-invitation-roles")?;
            let former =
                match &invitee {
                    Invitee::User(login) => state.former_members.iter().rev().find(|(o, u, _)| {
                        *o == org.login.to_lowercase() && *u == login.to_lowercase()
                    }),
                    Invitee::Email(_) => None,
                };
            match former {
                Some((_, _, OrgRole::Admin)) => InvitationRole::Admin,
                Some((_, _, OrgRole::Member)) => InvitationRole::DirectMember,
                None => {
                    return Err(OrgFailure::Invalid {
                        field: "role",
                        code: "invalid",
                        message: "Invitee was not previously a member of this organization",
                    });
                }
            }
        }
    };
    let mut team_ids = Vec::new();
    for id in new.team_ids {
        let in_org = state
            .world
            .teams
            .iter()
            .any(|t| t.id == id && t.org.eq_ignore_ascii_case(&org.login));
        if !in_org {
            return Err(OrgFailure::Invalid {
                field: "team_ids",
                code: "invalid",
                message: "One or more teams are not in this organization",
            });
        }
        if !team_ids.contains(&id) {
            team_ids.push(id);
        }
    }
    check_spammed(state, ctx, &org.login)?;
    if matches!(role, InvitationRole::DirectMember | InvitationRole::Admin) {
        check_seat(state, ctx, &org)?;
    }
    spend_budget(state, ctx, &org)?;

    let now = state.clock.now();
    let highest = state
        .world
        .org_invitations
        .iter()
        .map(|i| i.id)
        .max()
        .unwrap_or(0);
    let id = state.last_org_invitation_id.max(highest) + 1;
    state.last_org_invitation_id = id;
    let mut invitation = match &invitee {
        Invitee::User(login) => OrgInvitation::new(id, org.login.clone(), login.clone()),
        Invitee::Email(email) => OrgInvitation::by_email(id, org.login.clone(), email.clone()),
    }
    .role(role)
    .created_at(DateTime::from_timestamp(now.timestamp(), 0).unwrap_or(now));
    invitation.team_ids = team_ids;
    invitation.team_roles = new.team_roles;
    invitation
        .team_roles
        .retain(|id, _| invitation.team_ids.contains(id));
    match actor {
        Actor::App { app_id } => invitation.inviter_app_id = Some(*app_id),
        Actor::User(user) => invitation.inviter = Some(user.clone()),
    }
    state.world.org_invitations.push(invitation.clone());

    let view = view(state, ctx);
    let user = match &invitee {
        Invitee::User(login) => view.account(login).unwrap_or_default(),
        Invitee::Email(_) => Value::Null,
    };
    let extra = json!({"invitation": view.org_invitation(&invitation), "user": user});
    emit_organization(state, ctx, &org.login, "member_invited", extra, actor);
    Ok(id)
}

/// The index of open invitation `id` of `org`.
fn invitation_index(state: &State, org: &str, id: u64) -> Result<usize, OrgFailure> {
    let (start, now) = (state.clock.start(), state.clock.now());
    state
        .world
        .org_invitations
        .iter()
        .position(|i| {
            i.id == id && i.org.eq_ignore_ascii_case(org) && org_invitation_open(i, start, now)
        })
        .ok_or(OrgFailure::NotFound)
}

/// The teams of invitation `id` of `org` (open or failed).
pub(crate) fn invitation_teams<'a>(
    world: &'a Seed,
    org: &str,
    id: u64,
) -> Result<Vec<&'a Team>, OrgFailure> {
    let invitation = world
        .org_invitations
        .iter()
        .find(|i| i.id == id && i.org.eq_ignore_ascii_case(org))
        .ok_or(OrgFailure::NotFound)?;
    Ok(invitation
        .team_ids
        .iter()
        .filter_map(|id| world.teams.iter().find(|t| t.id == *id))
        .collect())
}

/// `DELETE /orgs/{org}/invitations/{invitation_id}`: the open invitation
/// disappears (it is not listed as failed; behaviour
/// `org-invitation-cancel`). No event: GitHub documents none.
pub(crate) fn cancel_invitation(
    state: &mut State,
    ctx: &Ctx<'_>,
    org: &str,
    id: u64,
) -> Result<(), OrgFailure> {
    let org = org_login(state, org)?;
    let index = invitation_index(state, &org, id)?;
    check_spammed(state, ctx, &org)?;
    check(ctx, "org-invitation-cancel")?;
    state.world.org_invitations.remove(index);
    Ok(())
}

/// `PUT /orgs/{org}/memberships/{username}` with `role`:
///
/// - a member: the role changes, the state stays (documented);
/// - an invitee (or a pending member): the pending role changes (behaviour
///   `org-reput-pending`);
/// - anyone else: an invitation (documented: "pending until they accept"),
///   which spends the budget and emits `organization.member_invited`.
pub(crate) fn put_membership(
    state: &mut State,
    ctx: &Ctx<'_>,
    org: &str,
    login: &str,
    role: OrgRole,
    actor: &Actor,
) -> Result<(), OrgFailure> {
    let org = org_login(state, org)?;
    let login = user_login(state, login)?;
    if let Some(index) = membership_index(&state.world, &org, &login) {
        if state.world.memberships[index].state == MembershipState::Pending {
            check(ctx, "org-reput-pending")?;
        }
        state.world.memberships[index].role = role;
        return Ok(());
    }
    if let Some(index) = open_invitation(state, &org, &login) {
        check(ctx, "org-reput-pending")?;
        state.world.org_invitations[index].role = match role {
            OrgRole::Admin => InvitationRole::Admin,
            OrgRole::Member => InvitationRole::DirectMember,
        };
        return Ok(());
    }
    let new = NewInvitation {
        role: match role {
            OrgRole::Admin => CreateRole::Admin,
            OrgRole::Member => CreateRole::DirectMember,
        },
        team_ids: Vec::new(),
        team_roles: BTreeMap::new(),
    };
    create_invitation(state, ctx, &org, &Invitee::User(login), new, actor).map(|_| ())
}

/// `DELETE /orgs/{org}/memberships/{username}` (documented): a member is
/// removed (with their team memberships; `organization.member_removed`);
/// an invitee's invitation is cancelled. Anyone else is a 404 (behaviour
/// `remove-org-non-member`).
pub(crate) fn remove_membership(
    state: &mut State,
    ctx: &Ctx<'_>,
    org: &str,
    login: &str,
    actor: &Actor,
) -> Result<(), OrgFailure> {
    let org = org_login(state, org)?;
    let login = user_login(state, login)?;
    if let Some(index) = membership_index(&state.world, &org, &login) {
        remove_member(state, ctx, &org, index, actor);
        return Ok(());
    }
    if let Some(index) = open_invitation(state, &org, &login) {
        state.world.org_invitations.remove(index);
        return Ok(());
    }
    check(ctx, "remove-org-non-member")?;
    Err(OrgFailure::NotFound)
}

/// Removes membership `index` of `org`, its team memberships, and
/// announces it when it was active.
fn remove_member(state: &mut State, ctx: &Ctx<'_>, org: &str, index: usize, actor: &Actor) {
    let membership = state.world.memberships.remove(index);
    let prefix = format!("{}/", org.to_lowercase());
    state.world.team_memberships.retain(|m| {
        !(m.user.eq_ignore_ascii_case(&membership.user)
            && m.team.to_lowercase().starts_with(&prefix))
    });
    if membership.state != MembershipState::Active {
        return;
    }
    state.former_members.push((
        org.to_lowercase(),
        membership.user.to_lowercase(),
        membership.role,
    ));
    let view = view(state, ctx);
    let extra = json!({"membership": membership_payload(&view, org, &membership.user, org_role_name(membership.role))});
    emit_organization(state, ctx, org, "member_removed", extra, actor);
}

/// `PATCH /user/memberships/orgs/{org}` with `state: active` (documented):
/// the user's open invitation (by login, or by a verified email address,
/// behaviour `email-invitation-accept`) or pending membership becomes an
/// active membership; the invitation's teams become active team
/// memberships. Emits `organization.member_added` and `membership.added`
/// per team. Already active: nothing changes.
pub(crate) fn accept(
    state: &mut State,
    ctx: &Ctx<'_>,
    org: &str,
    login: &str,
) -> Result<(), OrgFailure> {
    let org = org_login(state, org)?;
    let login = user_login(state, login)?;
    let actor = Actor::User(login.clone());
    if let Some(index) = membership_index(&state.world, &org, &login) {
        if state.world.memberships[index].state == MembershipState::Active {
            return Ok(());
        }
        state.world.memberships[index].state = MembershipState::Active;
        let role = state.world.memberships[index].role;
        let prefix = format!("{}/", org.to_lowercase());
        let mut activated = Vec::new();
        for membership in state.world.team_memberships.iter_mut().filter(|m| {
            m.user.eq_ignore_ascii_case(&login)
                && m.team.to_lowercase().starts_with(&prefix)
                && m.state == MembershipState::Pending
        }) {
            membership.state = MembershipState::Active;
            activated.push(membership.team.clone());
        }
        announce_added(state, ctx, &org, &login, role, &activated, &actor);
        return Ok(());
    }
    let index = match open_invitation(state, &org, &login) {
        Some(index) => index,
        None => {
            let index = email_invitation(state, &org, &login).ok_or(OrgFailure::NotFound)?;
            check(ctx, "email-invitation-accept")?;
            index
        }
    };
    let invitation = state.world.org_invitations[index].clone();
    let role = match invitation.role {
        InvitationRole::Admin => OrgRole::Admin,
        InvitationRole::DirectMember => OrgRole::Member,
        InvitationRole::BillingManager | InvitationRole::HiringManager => {
            // Not a membership: the invitation is used up.
            check(ctx, "org-invitation-roles")?;
            state.world.org_invitations.remove(index);
            return Ok(());
        }
    };
    state.world.org_invitations.remove(index);
    state
        .world
        .memberships
        .push(Membership::new(org.clone(), login.clone(), role));
    let mut joined = Vec::new();
    for id in &invitation.team_ids {
        let Some(team) = state.world.teams.iter().find(|t| t.id == *id) else {
            continue;
        };
        let key = access::team_key(team);
        let role = invitation.team_roles.get(id).copied().unwrap_or_default();
        state.world.team_memberships.retain(|m| {
            !(m.team.eq_ignore_ascii_case(&key) && m.user.eq_ignore_ascii_case(&login))
        });
        state
            .world
            .team_memberships
            .push(TeamMembership::new(key.clone(), login.clone(), role));
        joined.push(key);
    }
    announce_added(state, ctx, &org, &login, role, &joined, &actor);
    Ok(())
}

/// `organization.member_added`, then `membership.added` for each team in
/// `teams` (`org/slug`), sent by the user who joined (behaviour
/// `org-accept-team-events` for the team events).
fn announce_added(
    state: &mut State,
    ctx: &Ctx<'_>,
    org: &str,
    login: &str,
    role: OrgRole,
    teams: &[String],
    actor: &Actor,
) {
    let view = view(state, ctx);
    let extra = json!({"membership": membership_payload(&view, org, login, org_role_name(role))});
    emit_organization(state, ctx, org, "member_added", extra, actor);
    if !(ctx.allows)("org-accept-team-events") {
        return;
    }
    for key in teams {
        let Some(team) = key
            .split_once('/')
            .and_then(|(o, slug)| access::team(&state.world, o, slug))
            .cloned()
        else {
            continue;
        };
        emit_membership(state, ctx, &team, "added", login, actor);
    }
}

/// The invitee declines (the admin API's `act` route): the open
/// invitation, or the pending membership, disappears. No event (behaviour
/// `org-invitation-decline`).
pub(crate) fn decline(
    state: &mut State,
    ctx: &Ctx<'_>,
    org: &str,
    login: &str,
) -> Result<(), OrgFailure> {
    let org = org_login(state, org)?;
    let login = user_login(state, login)?;
    check(ctx, "org-invitation-decline")?;
    if let Some(index) = membership_index(&state.world, &org, &login)
        && state.world.memberships[index].state == MembershipState::Pending
    {
        state.world.memberships.remove(index);
        let prefix = format!("{}/", org.to_lowercase());
        state.world.team_memberships.retain(|m| {
            !(m.user.eq_ignore_ascii_case(&login) && m.team.to_lowercase().starts_with(&prefix))
        });
        return Ok(());
    }
    let index = open_invitation(state, &org, &login)
        .or_else(|| email_invitation(state, &org, &login))
        .ok_or(OrgFailure::NotFound)?;
    state.world.org_invitations.remove(index);
    Ok(())
}

/// A member leaves (the admin API's `act` route):
/// `organization.member_removed`, sent by them.
pub(crate) fn leave(
    state: &mut State,
    ctx: &Ctx<'_>,
    org: &str,
    login: &str,
) -> Result<(), OrgFailure> {
    let org = org_login(state, org)?;
    let login = user_login(state, login)?;
    let index = membership_index(&state.world, &org, &login)
        .filter(|i| state.world.memberships[*i].state == MembershipState::Active)
        .ok_or(OrgFailure::NotFound)?;
    remove_member(state, ctx, &org, index, &Actor::User(login));
    Ok(())
}

/// `PUT /orgs/{org}/teams/{team_slug}/memberships/{username}` with `role`
/// (documented):
///
/// - an active member of the organization joins (or changes role), active;
///   joining emits `membership.added`;
/// - a pending member gets a pending team membership;
/// - an invitee's open invitation gains the team (behaviour
///   `team-put-non-member`);
/// - anyone else is invited to the organization with the team: a visible
///   invitation that spends the budget and emits
///   `organization.member_invited` (behaviour `team-put-non-member`).
pub(crate) fn put_team_membership(
    state: &mut State,
    ctx: &Ctx<'_>,
    team: &Team,
    login: &str,
    role: TeamRole,
    actor: &Actor,
) -> Result<(), OrgFailure> {
    let login = user_login(state, login)?;
    let key = access::team_key(team);
    let org = team.org.clone();
    if let Some(index) = membership_index(&state.world, &org, &login) {
        let org_state = state.world.memberships[index].state;
        if let Some(existing) = state
            .world
            .team_memberships
            .iter_mut()
            .find(|m| m.team.eq_ignore_ascii_case(&key) && m.user.eq_ignore_ascii_case(&login))
        {
            existing.role = role;
            return Ok(());
        }
        let mut membership = TeamMembership::new(key, login.clone(), role);
        membership.state = org_state;
        state.world.team_memberships.push(membership);
        if org_state == MembershipState::Active {
            emit_membership(state, ctx, team, "added", &login, actor);
        }
        return Ok(());
    }
    check(ctx, "team-put-non-member")?;
    if let Some(index) = open_invitation(state, &org, &login) {
        let invitation = &mut state.world.org_invitations[index];
        if !invitation.team_ids.contains(&team.id) {
            invitation.team_ids.push(team.id);
        }
        if role == TeamRole::Member {
            invitation.team_roles.remove(&team.id);
        } else {
            invitation.team_roles.insert(team.id, role);
        }
        return Ok(());
    }
    let mut team_roles = BTreeMap::new();
    if role != TeamRole::Member {
        team_roles.insert(team.id, role);
    }
    let new = NewInvitation {
        role: CreateRole::DirectMember,
        team_ids: vec![team.id],
        team_roles,
    };
    create_invitation(state, ctx, &org, &Invitee::User(login), new, actor).map(|_| ())
}

/// `DELETE /orgs/{org}/teams/{team_slug}/memberships/{username}`
/// (documented: 204): an active team membership ends
/// (`membership.removed`); a pending one, or the team on an open
/// invitation, is dropped. Nothing to remove is a 404 (behaviour
/// `remove-team-non-member`).
pub(crate) fn remove_team_membership(
    state: &mut State,
    ctx: &Ctx<'_>,
    team: &Team,
    login: &str,
    actor: &Actor,
) -> Result<(), OrgFailure> {
    let login = user_login(state, login)?;
    let key = access::team_key(team);
    if let Some(index) = state
        .world
        .team_memberships
        .iter()
        .position(|m| m.team.eq_ignore_ascii_case(&key) && m.user.eq_ignore_ascii_case(&login))
    {
        let removed = state.world.team_memberships.remove(index);
        if removed.state == MembershipState::Active {
            emit_membership(state, ctx, team, "removed", &login, actor);
        }
        return Ok(());
    }
    if let Some(index) = open_invitation(state, &team.org, &login)
        && state.world.org_invitations[index]
            .team_ids
            .contains(&team.id)
    {
        check(ctx, "team-put-non-member")?;
        let invitation = &mut state.world.org_invitations[index];
        invitation.team_ids.retain(|id| *id != team.id);
        invitation.team_roles.remove(&team.id);
        return Ok(());
    }
    check(ctx, "remove-team-non-member")?;
    Err(OrgFailure::NotFound)
}

fn view<'a>(state: &'a State, ctx: &Ctx<'a>) -> View<'a> {
    View {
        world: &state.world,
        origin: ctx.origin,
        start: state.clock.start(),
        now: state.clock.now(),
    }
}

/// `membership` in `organization.member_added|member_removed` (documented
/// shape): `{url, state, role, organization_url, user}`.
fn membership_payload(view: &View<'_>, org: &str, login: &str, role: &str) -> Value {
    let mut membership = view.membership(org, login, role, MembershipState::Active);
    if let Some(object) = membership.as_object_mut() {
        object.remove("organization");
    }
    membership
}

/// Appends `organization.<action>` for `org` with `extra`'s fields,
/// `organization` and `sender`.
fn emit_organization(
    state: &mut State,
    ctx: &Ctx<'_>,
    org: &str,
    action: &str,
    extra: Value,
    actor: &Actor,
) {
    let view = view(state, ctx);
    let mut payload = json!({
        "action": action,
        "organization": view.organization(org),
        "sender": sender(&view, actor),
    });
    if let (Some(payload), Value::Object(extra)) = (payload.as_object_mut(), extra) {
        payload.extend(extra);
    }
    let targets = organization_targets(&state.world, org, "organization");
    let now = state.clock.now();
    state
        .events
        .push(now, "organization", Some(action), targets, payload);
}

/// Appends `membership.<action>` (`scope: "team"`) for `login` in `team`.
fn emit_membership(
    state: &mut State,
    ctx: &Ctx<'_>,
    team: &Team,
    action: &str,
    login: &str,
    actor: &Actor,
) {
    let view = view(state, ctx);
    let payload = json!({
        "action": action,
        "scope": "team",
        "member": view.account(login),
        "team": view.team(team),
        "organization": view.organization(&team.org),
        "sender": sender(&view, actor),
    });
    let targets = organization_targets(&state.world, &team.org, "membership");
    let now = state.clock.now();
    state
        .events
        .push(now, "membership", Some(action), targets, payload);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::seed::Plan;

    #[test]
    fn the_budget_grows_with_age_or_a_paid_plan() {
        let start: DateTime<Utc> = "2030-01-01T00:00:00Z".parse().unwrap();
        let young = Org::new(1, "young").created_at(start);
        assert_eq!(invitation_budget(&young, start, start), 50);
        let later = start + TimeDelta::days(32);
        assert_eq!(invitation_budget(&young, start, later), 500);
        let paid = Org::new(2, "paid")
            .created_at(start)
            .plan(Plan::new("team", 5));
        assert_eq!(invitation_budget(&paid, start, start), 500);
    }
}
