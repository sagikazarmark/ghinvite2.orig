//! The scenarios. Each has a stable ID listed in octoemu's `fidelity.toml`
//! `[scenarios]` table with the entries it measures, so a live run can
//! stamp them `verified`.
//!
//! Steps use public GitHub routes only (the emulator's clock is the one
//! exception, [`Ctx::advance_clock`]). Scenarios that use up real budget
//! are `destructive`: a live run takes them only on request. Those that
//! need days of real time are `slow` (live-only, skipped by default).
//! Anything only GitHub's consent screen can provide is skipped with a
//! reason.

mod auth;
mod oauth;
mod orgs;
mod repos;

use crate::ctx::{Ctx, Req, Stop};
use crate::target::EmulatorSetup;
use std::future::Future;
use std::pin::Pin;

/// A scenario's body.
pub type BoxFuture<'a> = Pin<Box<dyn Future<Output = Result<(), Stop>> + Send + 'a>>;

/// A scenario's steps.
pub type Run = for<'a> fn(&'a mut Ctx) -> BoxFuture<'a>;

/// One fidelity scenario.
#[derive(Clone, Copy)]
pub struct Scenario {
    /// The stable ID (a `fidelity.toml` `[scenarios]` key).
    pub id: &'static str,
    /// What it measures.
    pub summary: &'static str,
    /// Uses up real budget or breaks the live setup: live only on request.
    pub destructive: bool,
    /// Needs long real waits: live only on request.
    pub slow: bool,
    /// GitHub-side state the emulator must be put in first.
    pub setup: Option<EmulatorSetup>,
    /// The steps.
    pub run: Run,
}

impl std::fmt::Debug for Scenario {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Scenario")
            .field("id", &self.id)
            .field("destructive", &self.destructive)
            .field("slow", &self.slow)
            .finish_non_exhaustive()
    }
}

const fn scenario(id: &'static str, summary: &'static str, run: Run) -> Scenario {
    Scenario {
        id,
        summary,
        destructive: false,
        slow: false,
        setup: None,
        run,
    }
}

const fn destructive(mut scenario: Scenario) -> Scenario {
    scenario.destructive = true;
    scenario
}

const fn slow(mut scenario: Scenario) -> Scenario {
    scenario.slow = true;
    scenario
}

const fn with_setup(mut scenario: Scenario, setup: EmulatorSetup) -> Scenario {
    scenario.setup = Some(setup);
    scenario
}

/// Every scenario, in run order (the ones that break a live setup last).
pub fn all() -> Vec<Scenario> {
    vec![
        scenario(
            "jwt-error-matrix",
            "App JWT rejections: malformed, unregistered key, alg none, unknown iss, missing or bad claims, the token scheme, wrong credential kinds",
            auth::jwt_error_matrix,
        ),
        scenario(
            "jwt-skew-tolerance",
            "How far ahead of GitHub's clock iat and exp may be",
            auth::jwt_skew_tolerance,
        ),
        scenario(
            "token-downscoping-errors",
            "Narrowing an installation token: what is granted, what is refused and how",
            auth::token_downscoping_errors,
        ),
        scenario(
            "token-revocation-delete",
            "A stateless installation token's shape, and DELETE /installation/token taking effect",
            auth::token_revocation_delete,
        ),
        scenario(
            "suspended-installation",
            "Suspending and unsuspending an installation: the installation, its tokens, minting",
            auth::suspended_installation,
        ),
        scenario(
            "oauth-exchange-errors",
            "The status and body of user-token exchange errors at /login/oauth/access_token",
            oauth::exchange_errors,
        ),
        scenario(
            "user-permissionless",
            "GET /user and /user/emails with a user access token of an App without user permissions",
            oauth::user_permissionless,
        ),
        scenario(
            "reput-pending-invitation",
            "PUT collaborator again while the invitation is pending, with the same and another role",
            repos::reput_pending_invitation,
        ),
        slow(scenario(
            "expired-invitation",
            "A repository invitation past its 7 days: listed, accepted, re-PUT",
            repos::expired_invitation,
        )),
        destructive(scenario(
            "repo-invitation-limit",
            "The 50-invitations-per-repository-per-day limit and its body",
            repos::invitation_limit,
        )),
        scenario(
            "outside-collaborator-policy",
            "PUT collaborator for a non-member where only owners may invite outside collaborators",
            repos::outside_collaborator_policy,
        ),
        scenario(
            "member-edited-changes",
            "The member.edited webhook payload's changes after a role change",
            repos::member_edited_changes,
        ),
        scenario(
            "org-membership-accept",
            "An organization invitation accepted by the invitee: the pending and active membership and the accept's status",
            orgs::membership_accept,
        ),
        destructive(scenario(
            "org-invitation-limit",
            "The organization invitation budget and its body",
            orgs::invitation_limit,
        )),
        destructive(with_setup(
            scenario(
                "org-spammed",
                "An organization invitation while GitHub has flagged the organization",
                orgs::spammed,
            ),
            EmulatorSetup::OrgSpammed,
        )),
        scenario(
            "org-no-seat",
            "An organization invitation on a paid plan with every seat filled",
            orgs::no_seat,
        ),
        scenario(
            "team-put-non-member",
            "PUT team membership for a user outside the organization",
            orgs::team_put_non_member,
        ),
        scenario(
            "api-version-unsupported",
            "X-GitHub-Api-Version values GitHub does not serve: 400 or 410",
            auth::api_version_unsupported,
        ),
        // Last: on live they leave the setup rate-limited or uninstalled.
        destructive(scenario(
            "secondary-limit-shape",
            "The status, headers and body of a secondary rate limit",
            auth::secondary_limit_shape,
        )),
        destructive(scenario(
            "token-revocation-uninstall",
            "Tokens after the App uninstalls itself (live: reinstall by hand afterwards)",
            auth::token_revocation_uninstall,
        )),
    ]
}

// --- Helpers shared by the scenarios -------------------------------------------

fn repo_path(ctx: &Ctx) -> String {
    let world = ctx.world();
    format!("/repos/{}/{}", world.org, world.repo)
}

/// Removes the invitee's collaboration and pending invitations on the
/// world's repository (unrecorded), so a scenario starts clean and leaves
/// nothing behind.
async fn clear_repository(ctx: &mut Ctx) -> Result<(), Stop> {
    let token = ctx.token().await?;
    let repo = repo_path(ctx);
    let invitee = ctx.world().invitee.clone();
    let list = ctx
        .call(Req::get(format!("{repo}/invitations?per_page=100")).bearer(&token))
        .await?;
    for invitation in list.body.as_array().into_iter().flatten() {
        let theirs = invitation["invitee"]["login"]
            .as_str()
            .is_some_and(|login| login.eq_ignore_ascii_case(&invitee));
        if let (true, Some(id)) = (theirs, invitation["id"].as_u64()) {
            ctx.call(Req::delete(format!("{repo}/invitations/{id}")).bearer(&token))
                .await?;
        }
    }
    ctx.call(Req::delete(format!("{repo}/collaborators/{invitee}")).bearer(&token))
        .await?;
    ctx.revoke(&token).await;
    Ok(())
}

/// Removes the invitee's organization membership or invitation
/// (unrecorded).
async fn clear_organization(ctx: &mut Ctx) -> Result<(), Stop> {
    let token = ctx.token().await?;
    let world = ctx.world().clone();
    ctx.call(
        Req::delete(format!("/orgs/{}/memberships/{}", world.org, world.invitee)).bearer(&token),
    )
    .await?;
    ctx.revoke(&token).await;
    Ok(())
}

/// Runs `steps`, then `cleanup` whatever happened; the steps' outcome wins.
async fn then_cleanup(
    ctx: &mut Ctx,
    steps: Result<(), Stop>,
    cleanup: for<'a> fn(&'a mut Ctx) -> BoxFuture<'a>,
) -> Result<(), Stop> {
    let cleaned = cleanup(ctx).await;
    steps.and(cleaned)
}

fn clear_repository_boxed(ctx: &mut Ctx) -> BoxFuture<'_> {
    Box::pin(clear_repository(ctx))
}

fn clear_organization_boxed(ctx: &mut Ctx) -> BoxFuture<'_> {
    Box::pin(clear_organization(ctx))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn ids_are_unique_and_match_the_fidelity_manifest() {
        let ids: Vec<&str> = all().iter().map(|s| s.id).collect();
        let unique: BTreeSet<&str> = ids.iter().copied().collect();
        assert_eq!(unique.len(), ids.len(), "duplicate scenario IDs");
        let manifest = octoemu::fidelity::Manifest::embedded().unwrap();
        let listed: BTreeSet<&str> = manifest.scenarios.keys().map(String::as_str).collect();
        assert_eq!(
            unique, listed,
            "the scenarios and fidelity.toml's [scenarios] table disagree"
        );
    }
}
