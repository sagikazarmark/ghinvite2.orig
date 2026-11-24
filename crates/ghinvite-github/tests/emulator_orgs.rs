//! ghinvite-github's organization client against the GitHub emulator
//! (`octoemu`, GE-06): the GitHub side of the JS org drivers
//! (`tests/worker/organization-*.mjs`, `org-observation.mjs`,
//! `access-removal.mjs`, `email-delivery.mjs`) rebuilt over the real
//! `GithubApps` (octotoken, octoauth), `RestEnv` and `ReqwestTransport`.
//! Each test has its own embedded emulator, so they run in parallel.
//!
//! Where a driver's inline fake disagrees with GitHub's documentation, these
//! follow the documentation (see each test).
//!
//! Keys: octoemu's test key pair (public test material).
#![cfg(not(target_arch = "wasm32"))]

use ghinvite_github::endpoints::organization::{
    self, CreateInvitationRole, InvitationRole, Invitee, OrganizationInvitationInput,
};
use ghinvite_github::endpoints::{authority, authority_teams};
use ghinvite_github::profiles::{ORG_INVITE, ORG_READ};
use ghinvite_github::rest::RestEnv;
use ghinvite_github::roles::{MembershipState, OrgRole, TeamRole};
use ghinvite_github::transport::ReqwestTransport;
use ghinvite_github::{
    ApiError, AppSlot, CallError, GithubApp, GithubApps, InstalledApp, TokenCallError,
    email_invitation,
};
use octoemu::Emulator;
use octoemu::seed::{
    Access, App, Email, EmailVisibility, Installation, Membership, Org, OrgInvitation,
    OrgRole as SeedOrgRole, Plan, Repo, Role, Seed, Team, TeamMembership, TeamRole as SeedTeamRole,
    User,
};
use serde_json::{Value, json};
use std::sync::Arc;

const PRIVATE_KEY: &str =
    include_str!("../../../libs/octoauth/crates/octoemu/tests/fixtures/rsa2048.pem");
const PUBLIC_KEY: &str =
    include_str!("../../../libs/octoauth/crates/octoemu/tests/fixtures/rsa2048.pub.pem");

/// The org App's ID (`GH_ORG_APP_ID` in the drivers).
const ORG_APP: u64 = 456;
const INSTALLATION: u64 = 8402;
const ORG: &str = "org-fixture";

/// The drivers' organization fixture: org-fixture (840, created long ago,
/// free plan), owner (7) its owner, alice (8) and bob (9) outsiders, carol
/// (10) a member in devs; team devs (50, "Developers", write on
/// org-fixture/api). The org App (456: members write, metadata read)
/// installed as 8402.
fn seed() -> Seed {
    Seed::new()
        .user(User::new(7, "owner"))
        .user(
            User::new(8, "alice")
                .email_address(Email::new("alice@example.test").primary(EmailVisibility::Private)),
        )
        .user(User::new(9, "bob"))
        .user(User::new(10, "carol"))
        .org(Org::new(840, ORG).created_at("2020-01-01T00:00:00Z".parse().unwrap()))
        .repo(Repo::new(10, ORG, "api").private())
        .team(
            Team::new(50, ORG, "devs")
                .name("Developers")
                .repository("org-fixture/api", Role::Push),
        )
        .app(
            App::new(ORG_APP, "ghinvite-org", "Iv.fixture-org", "fixture-secret")
                .public_key(PUBLIC_KEY)
                .permission("members", Access::Write)
                .permission("metadata", Access::Read)
                .permission("organization_plan", Access::Read)
                .event("organization")
                .event("membership"),
        )
        .installation(Installation::new(INSTALLATION, ORG_APP, ORG))
        .membership(Membership::new(ORG, "owner", SeedOrgRole::Admin))
        .membership(Membership::new(ORG, "carol", SeedOrgRole::Member))
        .team_membership(TeamMembership::new(
            "org-fixture/devs",
            "carol",
            SeedTeamRole::Member,
        ))
}

/// The organization app as ghinvite builds it, pointed at the emulator.
fn org_app(emulator: &Emulator) -> GithubApp {
    let signer = octoauth::SigningKey::from_pem(PRIVATE_KEY).unwrap();
    let app = octoauth::App::builder(ORG_APP, signer)
        .base_url(emulator.api_base())
        .build()
        .unwrap();
    let transport = Arc::new(ReqwestTransport::new().unwrap());
    let env = RestEnv::new(transport, emulator.api_base(), "ghinvite-tests").unwrap();
    let apps = GithubApps::new(
        env,
        [InstalledApp {
            slot: AppSlot::Org,
            app,
            user_agent: "p1/org".into(),
        }],
    )
    .unwrap();
    apps.app(AppSlot::Org).unwrap()
}

/// Runs one endpoint call with an `ORG_INVITE` token.
macro_rules! invite {
    ($app:expr, |$rest:ident| $body:expr) => {
        $app.write_once(INSTALLATION, &ORG_INVITE, |handle| async move {
            let $rest = handle.rest();
            $body
                .await
                .map_err(CallError::<std::convert::Infallible>::from)
        })
        .await
    };
}

/// Runs one endpoint call with an `ORG_READ` token.
macro_rules! read {
    ($app:expr, |$rest:ident| $body:expr) => {
        $app.read(INSTALLATION, &ORG_READ, |handle| async move {
            let $rest = handle.rest();
            $body
                .await
                .map_err(CallError::<std::convert::Infallible>::from)
        })
        .await
    };
}

fn api_error<T: std::fmt::Debug>(result: Result<T, TokenCallError>) -> ApiError {
    match result {
        Err(TokenCallError::Call(CallError::Api(error))) => error,
        other => panic!("expected an API error, got {other:?}"),
    }
}

fn direct_member(invitee: Invitee, team_ids: Vec<u64>) -> OrganizationInvitationInput {
    OrganizationInvitationInput {
        invitee,
        role: CreateInvitationRole::DirectMember,
        team_ids,
    }
}

/// The invitee's side, which ghinvite has no client for: `PATCH
/// /user/memberships/orgs/{org}` with their user token.
async fn accept(emulator: &Emulator, login: &str) {
    let token = emulator.mint_user_token(login, ORG_APP).unwrap();
    let response = reqwest::Client::new()
        .patch(format!(
            "{}/user/memberships/orgs/{ORG}",
            emulator.api_base()
        ))
        .bearer_auth(token)
        .header("content-type", "application/json")
        .body(json!({"state": "active"}).to_string())
        .send()
        .await
        .unwrap();
    // GitHub answers the accept with 202, the membership already active.
    assert_eq!(response.status(), 202);
}

async fn admin(emulator: &Emulator, path: &str, body: Value) -> u16 {
    reqwest::Client::new()
        .post(format!("{}{path}", emulator.admin_base()))
        .header("content-type", "application/json")
        .body(body.to_string())
        .send()
        .await
        .unwrap()
        .status()
        .as_u16()
}

fn events(emulator: &Emulator) -> Vec<(String, String)> {
    emulator
        .events()
        .into_iter()
        .map(|e| (e.event, e.action.unwrap_or_default()))
        .collect()
}

/// organization-delivery.mjs and organization-webhooks.mjs: eligibility
/// reads, one POST with the role and team, the pending observation, the
/// invitee's acceptance, then the active org and team memberships and the
/// `organization.member_added` / `membership.added` events.
///
/// Disagreements with the drivers' fake: `GET /orgs/{org}` shows `plan`
/// only to a token with `organization_plan` (documented); ORG_READ narrows
/// it away, so ghinvite never sees seats with its profiles (the fake always
/// sends them). The fake answers 404 for the team membership while the
/// invitation is pending; octoemu answers `pending` (assumed).
#[tokio::test]
async fn invitation_with_team_through_acceptance() {
    let emulator = Emulator::start(seed()).await.unwrap();
    let app = org_app(&emulator);

    let org = read!(app, |rest| organization::get_organization(&rest, ORG)).unwrap();
    assert_eq!((org.id, org.login.as_str()), (840, ORG));
    assert_eq!(org.created_at.to_rfc3339(), "2020-01-01T00:00:00+00:00");
    assert!(org.plan.is_none(), "ORG_READ holds no organization_plan");
    assert!(org.two_factor_requirement_enabled.is_none());

    let absent = api_error(read!(app, |rest| organization::organization_membership(
        &rest, ORG, "alice"
    )));
    assert!(matches!(absent, ApiError::NotFound), "{absent:?}");
    let identity = read!(app, |rest| authority_teams::team_identity(
        &rest, ORG, "devs"
    ))
    .unwrap();
    assert_eq!((identity.team.id, identity.organization.id), (50, 840));
    let devs = read!(app, |rest| organization::get_team(&rest, ORG, "devs")).unwrap();
    assert_eq!((devs.id, devs.name.as_str()), (50, "Developers"));
    assert!(!devs.externally_managed);

    let created = invite!(app, |rest| organization::create_invitation(
        &rest,
        ORG,
        &direct_member(Invitee::User { invitee_id: 8 }, vec![50])
    ))
    .unwrap();
    assert_eq!(created.login.as_deref(), Some("alice"));
    assert_eq!(created.role, InvitationRole::DirectMember);
    assert_eq!(created.team_count, 1);
    assert_eq!(created.inviter.login, "ghinvite-org[bot]");

    let pending = read!(app, |rest| organization::list_invitations(&rest, ORG)).unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].id, created.id);
    let teams = read!(app, |rest| organization::list_invitation_teams(
        &rest, ORG, created.id
    ))
    .unwrap();
    assert_eq!(teams.iter().map(|t| t.id).collect::<Vec<_>>(), [50]);
    let membership = read!(app, |rest| organization::organization_membership(
        &rest, ORG, "alice"
    ))
    .unwrap();
    assert_eq!(
        (
            membership.state,
            membership.role,
            membership.user.id,
            membership.organization.id
        ),
        (MembershipState::Pending, OrgRole::Member, 8, 840)
    );
    let team_pending = read!(app, |rest| organization::team_membership(
        &rest, ORG, "devs", "alice"
    ))
    .unwrap();
    assert_eq!(team_pending.state, MembershipState::Pending);

    accept(&emulator, "alice").await;

    let active = read!(app, |rest| organization::organization_membership(
        &rest, ORG, "alice"
    ))
    .unwrap();
    assert_eq!(active.state, MembershipState::Active);
    let team_active = read!(app, |rest| organization::team_membership(
        &rest, ORG, "devs", "alice"
    ))
    .unwrap();
    assert_eq!(
        (team_active.role, team_active.state),
        (TeamRole::Member, MembershipState::Active)
    );
    assert!(
        read!(app, |rest| organization::list_invitations(&rest, ORG))
            .unwrap()
            .is_empty()
    );
    assert!(
        read!(app, |rest| organization::list_failed_invitations(
            &rest, ORG
        ))
        .unwrap()
        .is_empty()
    );
    assert_eq!(
        events(&emulator),
        [
            ("organization".to_owned(), "member_invited".to_owned()),
            ("organization".to_owned(), "member_added".to_owned()),
            ("membership".to_owned(), "added".to_owned()),
        ]
    );
    // The fields ghinvite-web's webhook intake reads.
    let all = emulator.events();
    assert_eq!(all[1].payload["membership"]["user"]["id"], 8);
    assert_eq!(all[1].payload["organization"]["id"], 840);
    assert_eq!(all[2].payload["scope"], "team");
    assert_eq!(all[2].payload["member"]["id"], 8);
    assert_eq!(all[2].payload["team"]["id"], 50);
}

/// org-observation.mjs: owner, member, pending and absent memberships as
/// the authority read sees them.
#[tokio::test]
async fn membership_observation() {
    let seed = seed().membership(Membership::new(ORG, "bob", SeedOrgRole::Admin).pending());
    let emulator = Emulator::start(seed).await.unwrap();
    let app = org_app(&emulator);
    for (login, role, state) in [
        ("owner", "admin", "active"),
        ("carol", "member", "active"),
        ("bob", "admin", "pending"),
    ] {
        let membership = read!(app, |rest| authority::organization_membership(
            &rest, ORG, login
        ))
        .unwrap();
        assert_eq!(
            (membership.role.as_str(), membership.state.as_str()),
            (role, state),
            "{login}"
        );
        assert_eq!(membership.organization.id, 840);
    }
    let absent = api_error(read!(app, |rest| authority::organization_membership(
        &rest, ORG, "alice"
    )));
    assert!(matches!(absent, ApiError::NotFound), "{absent:?}");
    let missing = api_error(read!(app, |rest| organization::get_organization(
        &rest, "nobody"
    )));
    assert!(matches!(missing, ApiError::NotFound), "{missing:?}");
}

/// organization-cancellation.mjs: cancelling an owned invitation (204, then
/// gone), a decline (membership 404 and no failed entry), expiry (the
/// failed list, `failed_at` set; assumed), and the `spammed` DELETE.
///
/// Disagreements with the fake: its failed entries lack `failed_reason`
/// and stamp `failed_at` with the current time; octoemu sends both, with
/// the expiry time (assumed).
#[tokio::test]
async fn cancellation_decline_expiry_and_spam() {
    let emulator = Emulator::start(seed()).await.unwrap();
    let app = org_app(&emulator);
    let invite_user = |id: u64| direct_member(Invitee::User { invitee_id: id }, vec![]);

    // Owned revoke: exactly one DELETE, then nothing to cancel.
    let owned = invite!(app, |rest| organization::create_invitation(
        &rest,
        ORG,
        &invite_user(8)
    ))
    .unwrap();
    invite!(app, |rest| organization::cancel_invitation(
        &rest, ORG, owned.id
    ))
    .unwrap();
    let again = api_error(invite!(app, |rest| organization::cancel_invitation(
        &rest, ORG, owned.id
    )));
    assert!(matches!(again, ApiError::NotFound), "{again:?}");
    assert!(
        read!(app, |rest| organization::list_invitations(&rest, ORG))
            .unwrap()
            .is_empty()
    );

    // Declined: no invitation, no membership.
    invite!(app, |rest| organization::create_invitation(
        &rest,
        ORG,
        &invite_user(8)
    ))
    .unwrap();
    assert_eq!(
        admin(&emulator, "/act/alice/orgs/org-fixture/decline", json!({})).await,
        204
    );
    let declined = api_error(read!(app, |rest| organization::organization_membership(
        &rest, ORG, "alice"
    )));
    assert!(matches!(declined, ApiError::NotFound));

    // Expired: an invitation sent eight days before the clock started has
    // moved to the failed list. (The clock cannot jump a week here: octoauth
    // corrects only a bounded skew for App JWTs.)
    let sent = emulator.now() - chrono::TimeDelta::days(8);
    let sent = chrono::DateTime::from_timestamp(sent.timestamp(), 0).unwrap();
    emulator
        .seed(
            Seed::new().org_invitation(
                OrgInvitation::new(9000, ORG, "bob")
                    .inviter("owner")
                    .created_at(sent),
            ),
        )
        .unwrap();
    assert!(
        read!(app, |rest| organization::list_invitations(&rest, ORG))
            .unwrap()
            .is_empty()
    );
    let failed = read!(app, |rest| organization::list_failed_invitations(
        &rest, ORG
    ))
    .unwrap();
    assert_eq!(failed.len(), 1);
    assert_eq!(failed[0].invitation.id, 9000);
    assert_eq!(failed[0].invitation.inviter.login, "owner");
    assert_eq!(failed[0].failed_at, Some(sent + chrono::TimeDelta::days(7)));
    let gone = api_error(read!(app, |rest| organization::organization_membership(
        &rest, ORG, "bob"
    )));
    assert!(matches!(gone, ApiError::NotFound), "{gone:?}");

    // Spammed DELETE: ghinvite classifies it as Spammed.
    let pending = invite!(app, |rest| organization::create_invitation(
        &rest,
        ORG,
        &invite_user(8)
    ))
    .unwrap();
    assert_eq!(
        admin(
            &emulator,
            "/orgs/org-fixture/spammed",
            json!({"spammed": true})
        )
        .await,
        204
    );
    let spammed = api_error(invite!(app, |rest| organization::cancel_invitation(
        &rest, ORG, pending.id
    )));
    assert!(matches!(spammed, ApiError::Spammed), "{spammed:?}");
    let spammed_post = api_error(invite!(app, |rest| organization::create_invitation(
        &rest,
        ORG,
        &invite_user(9)
    )));
    assert!(
        matches!(spammed_post, ApiError::Spammed),
        "{spammed_post:?}"
    );
}

/// access-removal.mjs: removing granted team access (204, then 404) and
/// organization membership; the repository access through the team goes
/// with it; `membership.removed` and `organization.member_removed`.
#[tokio::test]
async fn access_removal() {
    let emulator = Emulator::start(seed()).await.unwrap();
    let app = org_app(&emulator);
    // A team PUT for an active member is active at once.
    let added = invite!(app, |rest| organization::put_team_membership(
        &rest,
        ORG,
        "devs",
        "owner",
        TeamRole::Maintainer
    ))
    .unwrap();
    assert_eq!(
        (added.role, added.state),
        (TeamRole::Maintainer, MembershipState::Active)
    );
    let carol = read!(app, |rest| organization::team_membership(
        &rest, ORG, "devs", "carol"
    ))
    .unwrap();
    assert_eq!(carol.state, MembershipState::Active);

    invite!(app, |rest| organization::remove_team_membership(
        &rest, ORG, "devs", "carol"
    ))
    .unwrap();
    let gone = api_error(read!(app, |rest| organization::team_membership(
        &rest, ORG, "devs", "carol"
    )));
    assert!(matches!(gone, ApiError::NotFound), "{gone:?}");

    invite!(app, |rest| organization::remove_organization_member(
        &rest, ORG, "carol"
    ))
    .unwrap();
    let removed = api_error(read!(app, |rest| organization::organization_membership(
        &rest, ORG, "carol"
    )));
    assert!(matches!(removed, ApiError::NotFound), "{removed:?}");
    assert_eq!(
        events(&emulator),
        [
            ("membership".to_owned(), "added".to_owned()),
            ("membership".to_owned(), "removed".to_owned()),
            ("organization".to_owned(), "member_removed".to_owned()),
        ]
    );
    let member_removed = emulator.events().pop().unwrap();
    assert_eq!(member_removed.payload["membership"]["user"]["id"], 10);
    assert_eq!(member_removed.payload["organization"]["id"], 840);

    // A team PUT for a non-member: pending, through a visible invitation.
    let pending = invite!(app, |rest| organization::put_team_membership(
        &rest,
        ORG,
        "devs",
        "bob",
        TeamRole::Member
    ))
    .unwrap();
    assert_eq!(pending.state, MembershipState::Pending);
    let invitations = read!(app, |rest| organization::list_invitations(&rest, ORG)).unwrap();
    assert_eq!(invitations.len(), 1);
    assert_eq!(invitations[0].login.as_deref(), Some("bob"));
    assert_eq!(invitations[0].team_count, 1);
}

/// email-delivery.mjs: an email invitation (201 with `{id, created_at}`),
/// listed with `login: null`, then accepted by the user with that verified
/// address (assumed) and gone from the pending list.
///
/// Disagreements with the fake: its 201 body is only `{id, created_at,
/// email}` and its pending listing has `email: null`; GitHub's documented
/// invitation has every field, and the email for an email invitee. Its 422
/// has no `errors`.
#[tokio::test]
async fn email_invitation() {
    let emulator = Emulator::start(seed()).await.unwrap();
    let app = org_app(&emulator);
    let receipt = invite!(app, |rest| email_invitation::create(
        &rest,
        ORG,
        "alice@example.test",
        &[]
    ))
    .unwrap();
    assert!(receipt.id > 0);
    let now = emulator.now();
    assert_eq!(
        receipt.created_at,
        chrono::DateTime::from_timestamp(now.timestamp(), 0).unwrap()
    );
    let teams = read!(app, |rest| organization::list_teams(&rest, ORG)).unwrap();
    assert_eq!(teams.len(), 1);
    let pending = read!(app, |rest| organization::list_invitations(&rest, ORG)).unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].id, receipt.id);
    assert_eq!(pending[0].login, None);
    assert_eq!(pending[0].email.as_deref(), Some("alice@example.test"));
    assert_eq!(pending[0].team_count, 0);
    // A second invitation to the same address is a 422.
    let duplicate = api_error(invite!(app, |rest| email_invitation::create(
        &rest,
        ORG,
        "alice@example.test",
        &[]
    )));
    assert!(
        matches!(duplicate, ApiError::Validation { .. }),
        "{duplicate:?}"
    );

    accept(&emulator, "alice").await;
    assert!(
        read!(app, |rest| organization::list_invitations(&rest, ORG))
            .unwrap()
            .is_empty()
    );
    let membership = read!(app, |rest| organization::organization_membership(
        &rest, ORG, "alice"
    ))
    .unwrap();
    assert_eq!(membership.state, MembershipState::Active);
}

/// The organization budget (organization-delivery.mjs's bucket): a young
/// free organization gets 50 invitations per 24 hours, then a 422
/// (assumed shape); an existing member is a 422 too.
#[tokio::test]
async fn invitation_budget_and_existing_members() {
    let mut seed = seed();
    // Created when the clock starts: a young organization.
    seed.orgs[0].created_at = None;
    let emulator = Emulator::start(seed).await.unwrap();
    let app = org_app(&emulator);
    let member = api_error(invite!(app, |rest| organization::create_invitation(
        &rest,
        ORG,
        &direct_member(Invitee::User { invitee_id: 10 }, vec![])
    )));
    assert!(matches!(member, ApiError::Validation { .. }), "{member:?}");
    for n in 0..50 {
        invite!(app, |rest| email_invitation::create(
            &rest,
            ORG,
            &format!("p{n}@example.test"),
            &[]
        ))
        .unwrap();
    }
    let over = api_error(invite!(app, |rest| email_invitation::create(
        &rest,
        ORG,
        "p50@example.test",
        &[]
    )));
    assert!(matches!(over, ApiError::Validation { .. }), "{over:?}");
}

/// organization-delivery.mjs's `no_seat`: a paid plan with every seat
/// taken refuses the POST (assumed 422). A token with `organization_plan`
/// would see the seats; ghinvite's profiles do not ask for it.
#[tokio::test]
async fn no_seat_on_a_paid_plan() {
    let mut seed = seed();
    seed.orgs[0].plan = Plan::new("enterprise", 2);
    let emulator = Emulator::start(seed).await.unwrap();
    let app = org_app(&emulator);
    let blocked = api_error(invite!(app, |rest| organization::create_invitation(
        &rest,
        ORG,
        &direct_member(Invitee::User { invitee_id: 8 }, vec![50])
    )));
    assert!(
        matches!(blocked, ApiError::Validation { .. }),
        "{blocked:?}"
    );
    let org = read!(app, |rest| organization::get_organization(&rest, ORG)).unwrap();
    assert!(org.plan.is_none());
}

/// A read profile cannot write: 403 for `members: write`.
#[tokio::test]
async fn read_profile_cannot_invite() {
    let emulator = Emulator::start(seed()).await.unwrap();
    let app = org_app(&emulator);
    let forbidden = api_error(read!(app, |rest| organization::create_invitation(
        &rest,
        ORG,
        &direct_member(Invitee::User { invitee_id: 8 }, vec![])
    )));
    assert!(
        matches!(forbidden, ApiError::Forbidden { .. }),
        "{forbidden:?}"
    );
}
