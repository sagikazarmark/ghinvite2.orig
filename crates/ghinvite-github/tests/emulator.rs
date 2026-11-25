//! ghinvite-github's repository endpoints against the GitHub emulator
//! (`octoemu`, GE-03): the real `GithubApps` (octotoken provider, octoauth
//! App JWTs, installation tokens), `RestEnv` and `ReqwestTransport`, each
//! test with its own embedded emulator so they run in parallel.
//!
//! Keys: octoemu's test key pair (public test material).
#![cfg(not(target_arch = "wasm32"))]

use ghinvite_github::endpoints::{self, CollaboratorWrite, Repository};
use ghinvite_github::profiles::{REPO_INVITE, REPO_READ};
use ghinvite_github::rest::RestEnv;
use ghinvite_github::transport::ReqwestTransport;
use ghinvite_github::{
    ApiError, AppSlot, CallError, GithubApp, GithubApps, InstalledApp, RepoRole, TokenCallError,
};
use octoemu::Emulator;
use octoemu::seed::{
    Access, App, Collaborator, Installation, Org, Repo, RepositoryInvitation, Role, Seed, User,
};
use std::sync::Arc;

const PRIVATE_KEY: &str =
    include_str!("../../../libs/octoemu/crates/octoemu/tests/fixtures/rsa2048.pem");
const PUBLIC_KEY: &str =
    include_str!("../../../libs/octoemu/crates/octoemu/tests/fixtures/rsa2048.pub.pem");

const INSTALLATION: u64 = 5;
const WIDGETS: Repository<'static> = Repository {
    owner: "acme",
    name: "widgets",
};

/// The repository App (4) installed on acme (5) for widgets only; monalisa
/// already writes to it; 120 invitations are pending, so the list spans
/// two pages of 100.
fn seed() -> Seed {
    let mut seed = Seed::new()
        .user(User::new(1, "octocat"))
        .user(User::new(3, "hubot"))
        .user(User::new(11, "monalisa"))
        .org(Org::new(2, "acme"))
        .repo(Repo::new(101, "acme", "widgets").private())
        .repo(Repo::new(102, "acme", "secret").private())
        .app(
            App::new(4, "ghinvite-repo", "Iv23.ghinvite", "fixture-client-secret")
                .public_key(PUBLIC_KEY)
                .permission("administration", Access::Write)
                .permission("metadata", Access::Read)
                .event("member"),
        )
        .installation(Installation::new(INSTALLATION, 4, "acme").selected(["acme/widgets"]))
        .collaborator(Collaborator::new("acme/widgets", "monalisa", Role::Push));
    for n in 1..=120 {
        seed = seed
            .user(User::new(1000 + n, format!("u{n:03}")))
            .repository_invitation(
                RepositoryInvitation::new(5000 + n, "acme/widgets", format!("u{n:03}"), Role::Pull)
                    .inviter("octocat"),
            );
    }
    seed
}

/// The repository app as ghinvite builds it, pointed at the emulator.
fn repo_app(emulator: &Emulator) -> GithubApp {
    let signer = octoauth::SigningKey::from_pem(PRIVATE_KEY).unwrap();
    let app = octoauth::App::builder(4, signer)
        .base_url(emulator.api_base())
        .build()
        .unwrap();
    let transport = Arc::new(ReqwestTransport::new().unwrap());
    let env = RestEnv::new(transport, emulator.api_base(), "ghinvite-tests").unwrap();
    let apps = GithubApps::new(
        env,
        [InstalledApp {
            slot: AppSlot::Repo,
            app,
            user_agent: "p1/repo".into(),
        }],
    )
    .unwrap();
    apps.app(AppSlot::Repo).unwrap()
}

type Outcome<T> = Result<T, TokenCallError>;

/// Runs one endpoint call with a `REPO_INVITE` token.
macro_rules! invite {
    ($app:expr, |$rest:ident| $body:expr) => {
        $app.write_once(INSTALLATION, &REPO_INVITE, |handle| async move {
            let $rest = handle.rest();
            $body
                .await
                .map_err(CallError::<std::convert::Infallible>::from)
        })
        .await
    };
}

/// Runs one endpoint call with a `REPO_READ` token.
macro_rules! read {
    ($app:expr, |$rest:ident| $body:expr) => {
        $app.read(INSTALLATION, &REPO_READ, |handle| async move {
            let $rest = handle.rest();
            $body
                .await
                .map_err(CallError::<std::convert::Infallible>::from)
        })
        .await
    };
}

fn role_of(app_result: Outcome<endpoints::CollaboratorPermission>) -> RepoRole {
    app_result.unwrap().role
}

#[tokio::test]
async fn repository_collaboration_end_to_end() {
    let emulator = Emulator::start(seed()).await.unwrap();
    let app = repo_app(&emulator);

    // Repository read.
    let repo = read!(app, |rest| endpoints::get_repository(&rest, WIDGETS)).unwrap();
    assert_eq!(
        (repo.id, repo.full_name.as_str(), repo.private),
        (101, "acme/widgets", true)
    );

    // PUT: 201 with a new invitation, then 204 for a collaborator.
    let created = invite!(app, |rest| endpoints::put_collaborator(
        &rest,
        WIDGETS,
        "hubot",
        &RepoRole::Triage
    ))
    .unwrap();
    let CollaboratorWrite::Created {
        invitation_id,
        created_at,
        permission,
    } = created.clone()
    else {
        panic!("expected an invitation, got {created:?}");
    };
    assert!(created_at.is_some_and(|at| at <= emulator.now()));
    assert_eq!(permission, RepoRole::Triage);
    // A re-PUT while it is pending answers 201 with the same invitation
    // at its original role: it does not change the role.
    let again = invite!(app, |rest| endpoints::put_collaborator(
        &rest,
        WIDGETS,
        "hubot",
        &RepoRole::Admin
    ))
    .unwrap();
    assert_eq!(again, created);
    let existing = invite!(app, |rest| endpoints::put_collaborator(
        &rest,
        WIDGETS,
        "monalisa",
        &RepoRole::Write
    ))
    .unwrap();
    assert_eq!(existing, CollaboratorWrite::NoContent);

    // Permission read.
    let monalisa = read!(app, |rest| endpoints::collaborator_permission(
        &rest, WIDGETS, "monalisa"
    ))
    .unwrap();
    assert_eq!((monalisa.user.id, monalisa.role), (11, RepoRole::Write));

    // The invitation list spans two pages; the second is on the alias.
    let invitations = read!(app, |rest| endpoints::list_repo_invitations(&rest, WIDGETS)).unwrap();
    assert_eq!(invitations.len(), 121);
    let hubot = invitations.iter().find(|i| i.id == invitation_id).unwrap();
    assert_eq!(hubot.invitee.as_ref().unwrap().login, "hubot");
    assert_eq!(hubot.permissions, RepoRole::Triage);
    assert!(!hubot.expired);
    let paths: Vec<String> = emulator.calls().into_iter().map(|c| c.path).collect();
    assert!(
        paths.contains(&"/repos/acme/widgets/invitations?per_page=100".to_owned()),
        "{paths:?}"
    );
    assert!(
        paths.contains(&"/repositories/101/invitations?per_page=100&page=2".to_owned()),
        "{paths:?}"
    );

    // PATCH, then DELETE another invitation.
    let patched = invite!(app, |rest| endpoints::update_repo_invitation(
        &rest,
        WIDGETS,
        invitation_id,
        &RepoRole::Maintain
    ))
    .unwrap();
    assert_eq!(
        (patched.id, patched.permissions),
        (invitation_id, RepoRole::Maintain)
    );
    invite!(app, |rest| endpoints::delete_repo_invitation(
        &rest, WIDGETS, 5001
    ))
    .unwrap();
    let invitations = read!(app, |rest| endpoints::list_repo_invitations(&rest, WIDGETS)).unwrap();
    assert_eq!(invitations.len(), 120);
    assert!(invitations.iter().all(|i| i.id != 5001));

    // The invitee accepts (admin API, until user tokens arrive in GE-05).
    let accepted = reqwest::Client::new()
        .post(format!(
            "{}/act/hubot/repository_invitations/{invitation_id}/accept",
            emulator.admin_base()
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(accepted.status(), 204);
    let hubot_role = role_of(read!(app, |rest| endpoints::collaborator_permission(
        &rest, WIDGETS, "hubot"
    )));
    assert_eq!(hubot_role, RepoRole::Maintain);
    let collaborators = read!(app, |rest| endpoints::list_collaborators(&rest, WIDGETS)).unwrap();
    let mut roles: Vec<(String, RepoRole)> = collaborators
        .into_iter()
        .map(|c| (c.user.login, c.role_name))
        .collect();
    roles.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(
        roles,
        [
            ("hubot".to_owned(), RepoRole::Maintain),
            ("monalisa".to_owned(), RepoRole::Write)
        ]
    );

    // DELETE the collaborator.
    invite!(app, |rest| endpoints::remove_collaborator(
        &rest, WIDGETS, "hubot"
    ))
    .unwrap();
    let removed = role_of(read!(app, |rest| endpoints::collaborator_permission(
        &rest, WIDGETS, "hubot"
    )));
    assert_eq!(removed, RepoRole::None);

    // GitHub would have announced the acceptance and the removal.
    let actions: Vec<_> = emulator
        .events()
        .into_iter()
        .map(|e| (e.event, e.action.unwrap_or_default()))
        .collect();
    assert_eq!(
        actions,
        [
            ("member".to_owned(), "added".to_owned()),
            ("member".to_owned(), "removed".to_owned())
        ]
    );
}

/// A real `AppTokenProvider` creates one token through the emulator's
/// token route, `read` calls an endpoint with it, the next read reuses the
/// cached token, and `get_installation` reads the installation with the
/// App's JWT.
#[tokio::test]
async fn the_provider_reuses_its_token() {
    let emulator = Emulator::start(seed()).await.unwrap();
    let app = repo_app(&emulator);
    for _ in 0..2 {
        let id = read!(app, |rest| endpoints::get_repository(&rest, WIDGETS)).unwrap();
        assert_eq!(id.id, 101);
    }
    let created = emulator
        .calls()
        .iter()
        .filter(|call| {
            call.method == "POST"
                && call.path == format!("/app/installations/{INSTALLATION}/access_tokens")
        })
        .count();
    assert_eq!(created, 1);
    let info = app.get_installation(INSTALLATION).await.unwrap();
    assert_eq!(
        info.id,
        ghinvite_github::ids::InstallationId::new(INSTALLATION)
    );
    assert_eq!(info.app_id, ghinvite_github::ids::AppId::new(4));
    assert_eq!(info.account.kind, octoauth::AccountKind::Organization);
}

#[tokio::test]
async fn users_resolve_by_login_and_id() {
    let emulator = Emulator::start(seed()).await.unwrap();
    let app = repo_app(&emulator);
    let by_login = read!(app, |rest| endpoints::get_user_by_login(&rest, "hubot")).unwrap();
    assert_eq!((by_login.id, by_login.login.as_str()), (3, "hubot"));
    assert_eq!(by_login.account_type.as_deref(), Some("User"));
    assert!(by_login.created_at.is_some());
    let by_id = read!(app, |rest| endpoints::get_user_by_id(&rest, 11)).unwrap();
    assert_eq!(by_id.login, "monalisa");
}

#[tokio::test]
async fn refusals_classify_as_ghinvite_expects() {
    let emulator = Emulator::start(seed()).await.unwrap();
    let app = repo_app(&emulator);
    let hidden = read!(app, |rest| endpoints::get_repository(
        &rest,
        Repository {
            owner: "acme",
            name: "secret",
        }
    ))
    .unwrap_err();
    assert!(
        matches!(
            hidden,
            TokenCallError::Call(CallError::Api(ApiError::NotFound))
        ),
        "{hidden:?}"
    );
    // A read-profile token cannot invite: 403 for the permission.
    let forbidden = read!(app, |rest| endpoints::put_collaborator(
        &rest,
        WIDGETS,
        "hubot",
        &RepoRole::Read
    ))
    .unwrap_err();
    assert!(
        matches!(
            forbidden,
            TokenCallError::Call(CallError::Api(ApiError::Forbidden { .. }))
        ),
        "{forbidden:?}"
    );
}
