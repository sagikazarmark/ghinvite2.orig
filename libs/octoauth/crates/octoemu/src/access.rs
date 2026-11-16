//! Who has which role on a repository, and the names each API reports for
//! a role.
//!
//! Roles are GitHub's five (`pull < triage < push < maintain < admin`) or
//! an organization's custom role over a base (`seed::RepositoryRole`),
//! ranked as its base. A user's effective role is the highest of their
//! [`grants`]: a direct collaboration, the repository's ownership, the
//! owning organization's ownership or base permission, and its teams
//! (an active team membership, inherited by child teams).

use crate::seed::{
    BasePermission, MembershipState, OrgRole, Repo, RepositoryRole, Role, Seed, Team, User,
};
use serde_json::{Value, json};

/// Where a user's access to a repository comes from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Source {
    /// A direct collaborator (`seed.collaborators`).
    Direct,
    /// The user owns the repository.
    Owner,
    /// An owner of the organization that owns the repository.
    OrgOwner,
    /// A member of that organization, through its base permission.
    OrgBase,
    /// An active member of a team (or of one of its child teams) with a
    /// role on the repository; the team's ID.
    Team(u64),
}

/// One source of access and the role it confers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Grant {
    pub(crate) source: Source,
    pub(crate) role: Role,
}

/// The user with `login` (ignoring case).
pub(crate) fn user<'a>(world: &'a Seed, login: &str) -> Option<&'a User> {
    world
        .users
        .iter()
        .find(|u| u.login.eq_ignore_ascii_case(login))
}

/// The repository with `id`.
pub(crate) fn repo_by_id(world: &Seed, id: u64) -> Option<&Repo> {
    world.repos.iter().find(|r| r.id == id)
}

/// Whether the repository's owner is an organization.
pub(crate) fn org_owned(world: &Seed, repo: &Repo) -> bool {
    world
        .orgs
        .iter()
        .any(|o| o.login.eq_ignore_ascii_case(&repo.owner))
}

/// Whether `login` is an active member of organization `org`. The hook
/// organization-side behaviour (GE-06) builds on.
pub(crate) fn org_member(world: &Seed, org: &str, login: &str) -> bool {
    world.memberships.iter().any(|m| {
        m.org.eq_ignore_ascii_case(org)
            && m.user.eq_ignore_ascii_case(login)
            && m.state == MembershipState::Active
    })
}

/// The team `slug` of organization `org`.
pub(crate) fn team<'a>(world: &'a Seed, org: &str, slug: &str) -> Option<&'a Team> {
    world
        .teams
        .iter()
        .find(|t| t.org.eq_ignore_ascii_case(org) && t.slug.eq_ignore_ascii_case(slug))
}

/// `team` named as seed entries name it: `org/slug`.
pub(crate) fn team_key(team: &Team) -> String {
    format!("{}/{}", team.org, team.slug)
}

/// The teams `login` is an active member of, plus their ancestors (a child
/// team's members inherit the parent's access; documented).
pub(crate) fn active_teams<'a>(world: &'a Seed, org: &str, login: &str) -> Vec<&'a Team> {
    let mut teams: Vec<&Team> = Vec::new();
    for membership in world
        .team_memberships
        .iter()
        .filter(|m| m.user.eq_ignore_ascii_case(login) && m.state == MembershipState::Active)
    {
        let Some((team_org, slug)) = membership.team.split_once('/') else {
            continue;
        };
        if !team_org.eq_ignore_ascii_case(org) {
            continue;
        }
        let mut current = team(world, org, slug);
        // Bounded: seed validation refuses parent cycles.
        for _ in 0..=world.teams.len() {
            let Some(found) = current else { break };
            if teams.iter().any(|t| t.id == found.id) {
                break;
            }
            teams.push(found);
            current = found.parent.as_deref().and_then(|p| team(world, org, p));
        }
    }
    teams
}

/// The custom role `name` of the organization owning `repo`.
pub(crate) fn custom_role<'a>(
    world: &'a Seed,
    repo: &Repo,
    name: &str,
) -> Option<&'a RepositoryRole> {
    world
        .repository_roles
        .iter()
        .find(|r| r.org.eq_ignore_ascii_case(&repo.owner) && r.name.eq_ignore_ascii_case(name))
}

/// The built-in role `role` stands for: itself, or a custom role's base.
/// An unknown custom role counts as `pull` (seed validation prevents it).
pub(crate) fn base(world: &Seed, repo: &Repo, role: &Role) -> Role {
    match role {
        Role::Custom(name) => {
            custom_role(world, repo, name).map_or(Role::Pull, |custom| custom.base_role.role())
        }
        builtin => builtin.clone(),
    }
}

/// A built-in role's rank, `pull` (1) to `admin` (5).
fn rank_of_builtin(role: &Role) -> u8 {
    match role {
        Role::Pull | Role::Custom(_) => 1,
        Role::Triage => 2,
        Role::Push => 3,
        Role::Maintain => 4,
        Role::Admin => 5,
    }
}

/// `role`'s rank; a custom role ranks as its base.
pub(crate) fn rank(world: &Seed, repo: &Repo, role: &Role) -> u8 {
    rank_of_builtin(&base(world, repo, role))
}

/// The legacy `permission` (`admin`, `write` or `read`): maintain is write,
/// triage is read (documented).
pub(crate) fn legacy_permission(world: &Seed, repo: &Repo, role: &Role) -> &'static str {
    match base(world, repo, role) {
        Role::Admin => "admin",
        Role::Maintain | Role::Push => "write",
        _ => "read",
    }
}

/// `role_name`, as the permission and collaborator APIs report it: `read`,
/// `triage`, `write`, `maintain`, `admin`, or the custom role's name.
pub(crate) fn role_name(world: &Seed, repo: &Repo, role: &Role) -> String {
    match role {
        Role::Pull => "read".into(),
        Role::Triage => "triage".into(),
        Role::Push => "write".into(),
        Role::Maintain => "maintain".into(),
        Role::Admin => "admin".into(),
        Role::Custom(name) => {
            custom_role(world, repo, name).map_or_else(|| name.clone(), |r| r.name.clone())
        }
    }
}

/// The `permissions` booleans of a collaborator with `role`.
pub(crate) fn permission_flags(world: &Seed, repo: &Repo, role: &Role) -> Value {
    let rank = rank(world, repo, role);
    json!({
        "admin": rank >= 5,
        "maintain": rank >= 4,
        "push": rank >= 3,
        "triage": rank >= 2,
        "pull": rank >= 1,
    })
}

/// The role a `PUT …/collaborators/{username}` `permission` names: one of
/// `pull`, `triage`, `push`, `maintain`, `admin` (documented), or a custom
/// role of the owning organization. `None` for anything else.
pub(crate) fn parse_put_permission(world: &Seed, repo: &Repo, value: &str) -> Option<Role> {
    match value {
        "pull" => Some(Role::Pull),
        "triage" => Some(Role::Triage),
        "push" => Some(Role::Push),
        "maintain" => Some(Role::Maintain),
        "admin" => Some(Role::Admin),
        name => custom_role(world, repo, name).map(|custom| Role::Custom(custom.name.clone())),
    }
}

/// The role a `PATCH …/invitations/{id}` `permissions` names: `read`,
/// `write`, `maintain`, `triage` or `admin` (documented).
pub(crate) fn parse_invitation_permission(value: &str) -> Option<Role> {
    match value {
        "read" => Some(Role::Pull),
        "triage" => Some(Role::Triage),
        "write" => Some(Role::Push),
        "maintain" => Some(Role::Maintain),
        "admin" => Some(Role::Admin),
        _ => None,
    }
}

/// Every source of `login`'s access to `repo`, direct first: a direct
/// collaboration, ownership, the organization's ownership and base
/// permission (active members), then team grants (active team members,
/// and members of child teams).
pub(crate) fn grants(world: &Seed, repo: &Repo, login: &str) -> Vec<Grant> {
    let mut grants = Vec::new();
    let full_name = repo.full_name();
    if let Some(direct) = world
        .collaborators
        .iter()
        .find(|c| c.repo.eq_ignore_ascii_case(&full_name) && c.user.eq_ignore_ascii_case(login))
    {
        grants.push(Grant {
            source: Source::Direct,
            role: direct.role.clone(),
        });
    }
    if repo.owner.eq_ignore_ascii_case(login) {
        grants.push(Grant {
            source: Source::Owner,
            role: Role::Admin,
        });
    }
    if let Some(org) = world
        .orgs
        .iter()
        .find(|o| o.login.eq_ignore_ascii_case(&repo.owner))
        && let Some(membership) = world.memberships.iter().find(|m| {
            m.org.eq_ignore_ascii_case(&org.login)
                && m.user.eq_ignore_ascii_case(login)
                && m.state == MembershipState::Active
        })
    {
        if membership.role == OrgRole::Admin {
            grants.push(Grant {
                source: Source::OrgOwner,
                role: Role::Admin,
            });
        }
        let base = match org.default_repository_permission {
            BasePermission::None => None,
            BasePermission::Read => Some(Role::Pull),
            BasePermission::Write => Some(Role::Push),
            BasePermission::Admin => Some(Role::Admin),
        };
        if let Some(role) = base {
            grants.push(Grant {
                source: Source::OrgBase,
                role,
            });
        }
        for team in active_teams(world, &org.login, login) {
            if let Some(grant) = team
                .repositories
                .iter()
                .find(|r| r.repo.eq_ignore_ascii_case(&full_name))
            {
                grants.push(Grant {
                    source: Source::Team(team.id),
                    role: grant.role.clone(),
                });
            }
        }
    }
    grants
}

/// The highest of `login`'s grants (on a tie the earlier, so a direct
/// custom role keeps its name); `None` without access.
pub(crate) fn effective(world: &Seed, repo: &Repo, login: &str) -> Option<Grant> {
    grants(world, repo, login)
        .into_iter()
        .fold(None, |best: Option<Grant>, grant| match best {
            Some(best) if rank(world, repo, &best.role) >= rank(world, repo, &grant.role) => {
                Some(best)
            }
            _ => Some(grant),
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::seed::{BaseRole, Collaborator, Membership, Org, User};

    fn world() -> Seed {
        Seed::new()
            .user(User::new(1, "ann"))
            .user(User::new(2, "bob"))
            .user(User::new(3, "cat"))
            .org(Org::new(10, "acme").default_repository_permission(BasePermission::Read))
            .repo(Repo::new(20, "acme", "w").private())
            .repository_role(RepositoryRole::new(
                30,
                "acme",
                "auditor",
                BaseRole::Maintain,
            ))
            .collaborator(Collaborator::new(
                "acme/w",
                "ann",
                Role::Custom("auditor".into()),
            ))
            .collaborator(Collaborator::new("acme/w", "bob", Role::Triage))
            .membership(Membership::new("acme", "bob", OrgRole::Admin))
    }

    #[test]
    fn names_and_ranks() {
        let world = world();
        let repo = &world.repos[0];
        let auditor = Role::Custom("auditor".into());
        assert_eq!(legacy_permission(&world, repo, &auditor), "write");
        assert_eq!(role_name(&world, repo, &auditor), "auditor");
        assert_eq!(legacy_permission(&world, repo, &Role::Triage), "read");
        assert_eq!(legacy_permission(&world, repo, &Role::Maintain), "write");
        assert_eq!(role_name(&world, repo, &Role::Pull), "read");
        assert_eq!(role_name(&world, repo, &Role::Push), "write");
        assert_eq!(
            parse_put_permission(&world, repo, "AUDITOR"),
            Some(Role::Custom("auditor".into()))
        );
        assert_eq!(parse_put_permission(&world, repo, "read"), None);
        assert_eq!(parse_invitation_permission("write"), Some(Role::Push));
        assert_eq!(parse_invitation_permission("push"), None);
    }

    #[test]
    fn the_highest_grant_wins() {
        let world = world();
        let repo = &world.repos[0];
        let ann = effective(&world, repo, "ann").unwrap();
        assert_eq!(
            (ann.source, ann.role),
            (Source::Direct, Role::Custom("auditor".into()))
        );
        // Triage directly, but an organization owner.
        let bob = effective(&world, repo, "bob").unwrap();
        assert_eq!((bob.source, bob.role), (Source::OrgOwner, Role::Admin));
        assert_eq!(effective(&world, repo, "cat"), None);
    }
}
