//! `GET /user/installations/{installation_id}/repositories` over HTTP: a
//! GitHub App's user access token lists the installation's repositories the
//! user has explicit access to, with the user's `permissions`, paged; other
//! installations are 404s.
#![cfg(not(target_arch = "wasm32"))]

use chrono::{DateTime, Utc};
use octoemu::Emulator;
use octoemu::seed::{
    Access, App, BasePermission, Collaborator, Installation, Membership, Org, OrgRole, Repo, Role,
    Seed, Team, TeamMembership, TeamRole, User,
};
use reqwest::{Client, StatusCode};
use serde_json::{Value, json};

const PUBLIC_KEY: &str = include_str!("fixtures/rsa2048.pub.pem");

/// App 4 installed on acme as 5 (selected: widgets, big, gadgets) and on
/// octocat as 6 (all); App 8 installed on acme as 9 (all), and App 4 on
/// `other` as 11 (all). acme has no base permission, `other` read (for
/// `orgdev`); `boss` owns acme; `monalisa` pushes to
/// widgets; `teamie` maintains gadgets through a team; `stranger` has no
/// access to anything. hubot's installation 7 (of App 4) selects nothing;
/// installation 10 (of App 4) on `frozen` is suspended.
fn seed() -> Seed {
    let start: DateTime<Utc> = "2030-01-01T00:00:00Z".parse().unwrap();
    Seed::new()
        .start_time(start)
        .user(User::new(1, "octocat"))
        .user(User::new(3, "hubot"))
        .user(User::new(11, "monalisa"))
        .user(User::new(12, "stranger"))
        .user(User::new(13, "orgdev"))
        .user(User::new(14, "boss"))
        .user(User::new(15, "teamie"))
        .user(User::new(16, "frozen"))
        .org(Org::new(2, "acme").default_repository_permission(BasePermission::None))
        .org(Org::new(20, "other").default_repository_permission(BasePermission::Read))
        .repo(Repo::new(101, "acme", "widgets").private())
        .repo(Repo::new(102, "acme", "public"))
        .repo(Repo::new(103, "acme", "secret").private())
        .repo(Repo::new(104, "acme", "big").private())
        .repo(Repo::new(105, "acme", "gadgets").private())
        .repo(Repo::new(201, "octocat", "dotfiles"))
        .repo(Repo::new(202, "octocat", "private").private())
        .repo(Repo::new(301, "frozen", "ice").private())
        .repo(Repo::new(401, "other", "shared").private())
        .app(
            App::new(4, "invite-bot", "Iv23.fixture", "fixture-client-secret")
                .public_key(PUBLIC_KEY)
                .permission("metadata", Access::Read),
        )
        .app(
            App::new(8, "other-bot", "Iv23.other", "other-client-secret")
                .public_key(PUBLIC_KEY)
                .permission("metadata", Access::Read),
        )
        .installation(Installation::new(5, 4, "acme").selected([
            "acme/widgets",
            "acme/big",
            "acme/gadgets",
        ]))
        .installation(Installation::new(6, 4, "octocat"))
        .installation(Installation::new(7, 4, "hubot").selected(Vec::<String>::new()))
        .installation(Installation::new(9, 8, "acme"))
        .installation(Installation::new(10, 4, "frozen").suspended_at(start))
        .installation(Installation::new(11, 4, "other"))
        .membership(Membership::new("acme", "orgdev", OrgRole::Member))
        .membership(Membership::new("acme", "boss", OrgRole::Admin))
        .membership(Membership::new("acme", "teamie", OrgRole::Member))
        .membership(Membership::new("other", "orgdev", OrgRole::Member))
        .team(Team::new(30, "acme", "builders").repository("acme/gadgets", Role::Maintain))
        .team_membership(TeamMembership::new(
            "acme/builders",
            "teamie",
            TeamRole::Member,
        ))
        .collaborator(Collaborator::new("acme/widgets", "monalisa", Role::Push))
}

struct Reply {
    status: StatusCode,
    link: Option<String>,
    body: Value,
}

async fn get(emulator: &Emulator, path: &str, token: Option<&str>) -> Reply {
    let mut request = Client::new().get(format!("{}{path}", emulator.api_base()));
    if let Some(token) = token {
        request = request.bearer_auth(token);
    }
    let response = request.send().await.unwrap();
    let status = response.status();
    let link = response
        .headers()
        .get("link")
        .map(|l| l.to_str().unwrap().to_owned());
    let text = response.text().await.unwrap();
    let body = serde_json::from_str(&text).unwrap_or(Value::String(text));
    Reply { status, link, body }
}

/// `login`'s user access token for App `app_id`.
fn user_token(emulator: &Emulator, login: &str, app_id: u64) -> String {
    emulator.mint_user_token(login, app_id).unwrap()
}

/// The `full_name`s of a listing.
fn names(body: &Value) -> Vec<&str> {
    body["repositories"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["full_name"].as_str().unwrap())
        .collect()
}

#[tokio::test]
async fn lists_the_installation_repositories_the_user_can_access() {
    let emulator = Emulator::start(seed()).await.unwrap();
    let path = "/user/installations/5/repositories";

    // A direct collaborator: only that repository, with its permissions.
    let monalisa = get(&emulator, path, Some(&user_token(&emulator, "monalisa", 4))).await;
    assert_eq!(monalisa.status, StatusCode::OK, "{}", monalisa.body);
    assert_eq!(monalisa.body["total_count"], 1);
    assert_eq!(monalisa.body["repository_selection"], "selected");
    assert_eq!(names(&monalisa.body), ["acme/widgets"]);
    let widgets = &monalisa.body["repositories"][0];
    assert_eq!(widgets["id"], 101);
    assert_eq!(widgets["private"], true);
    assert_eq!(widgets["owner"]["login"], "acme");
    assert_eq!(
        widgets["permissions"],
        json!({"admin": false, "maintain": false, "push": true, "triage": true, "pull": true})
    );

    // A team grant reaches gadgets; acme's base permission (none) nothing
    // else.
    let teamie = get(&emulator, path, Some(&user_token(&emulator, "teamie", 4))).await;
    assert_eq!(names(&teamie.body), ["acme/gadgets"]);
    assert_eq!(
        teamie.body["repositories"][0]["permissions"]["maintain"],
        true
    );
    assert_eq!(
        teamie.body["repositories"][0]["permissions"]["admin"],
        false
    );

    // An owner: every selected repository (not acme/secret or acme/public,
    // which the installation does not cover), as admin, by ID.
    let boss = get(&emulator, path, Some(&user_token(&emulator, "boss", 4))).await;
    assert_eq!(boss.body["total_count"], 3);
    assert_eq!(
        names(&boss.body),
        ["acme/widgets", "acme/big", "acme/gadgets"]
    );
    assert!(
        boss.body["repositories"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["permissions"]["admin"] == true)
    );

    // A user account's installation on all repositories: the owner's own.
    let octocat = get(
        &emulator,
        "/user/installations/6/repositories",
        Some(&user_token(&emulator, "octocat", 4)),
    )
    .await;
    assert_eq!(octocat.status, StatusCode::OK);
    assert_eq!(octocat.body["repository_selection"], "all");
    assert_eq!(
        names(&octocat.body),
        ["octocat/dotfiles", "octocat/private"]
    );

    // An organization's base permission counts as explicit access.
    let orgdev = get(
        &emulator,
        "/user/installations/11/repositories",
        Some(&user_token(&emulator, "orgdev", 4)),
    )
    .await;
    assert_eq!(names(&orgdev.body), ["other/shared"]);
    assert_eq!(orgdev.body["repositories"][0]["permissions"]["push"], false);
    assert_eq!(orgdev.body["repositories"][0]["permissions"]["pull"], true);

    let call = emulator.calls().into_iter().next().unwrap();
    assert_eq!(
        call.route.as_deref(),
        Some("GET /user/installations/{installation_id}/repositories")
    );
}

#[tokio::test]
async fn pages_with_a_link_header() {
    let emulator = Emulator::start(seed()).await.unwrap();
    let boss = user_token(&emulator, "boss", 4);
    let first = get(
        &emulator,
        "/user/installations/5/repositories?per_page=2",
        Some(&boss),
    )
    .await;
    assert_eq!(first.body["total_count"], 3);
    assert_eq!(names(&first.body), ["acme/widgets", "acme/big"]);
    let link = first.link.unwrap();
    assert!(link.contains("page=2>; rel=\"next\""), "{link}");
    let second = get(
        &emulator,
        "/user/installations/5/repositories?per_page=2&page=2",
        Some(&boss),
    )
    .await;
    assert_eq!(second.body["total_count"], 3);
    assert_eq!(names(&second.body), ["acme/gadgets"]);
}

#[tokio::test]
async fn installations_out_of_the_users_reach_are_404s() {
    let emulator = Emulator::start(seed()).await.unwrap();
    let stranger = user_token(&emulator, "stranger", 4);
    let boss = user_token(&emulator, "boss", 4);
    let cases = [
        // No repository of the installation is the user's.
        ("/user/installations/5/repositories", &stranger),
        // Another App's installation, which the user could access.
        ("/user/installations/9/repositories", &boss),
        // Unknown or malformed.
        ("/user/installations/404/repositories", &boss),
        ("/user/installations/x/repositories", &boss),
        // Suspended (assumed).
        (
            "/user/installations/10/repositories",
            &user_token(&emulator, "frozen", 4),
        ),
    ];
    for (path, token) in cases {
        let reply = get(&emulator, path, Some(token)).await;
        assert_eq!(
            reply.status,
            StatusCode::NOT_FOUND,
            "{path}: {}",
            reply.body
        );
        assert_eq!(reply.body["message"], "Not Found");
    }
    // Another App's token sees its own installation on acme.
    let other = get(
        &emulator,
        "/user/installations/9/repositories",
        Some(&user_token(&emulator, "boss", 8)),
    )
    .await;
    assert_eq!(other.status, StatusCode::OK);
    assert_eq!(other.body["total_count"], 5);

    // The account's owner sees an installation without repositories as an
    // empty list (assumed).
    let hubot = get(
        &emulator,
        "/user/installations/7/repositories",
        Some(&user_token(&emulator, "hubot", 4)),
    )
    .await;
    assert_eq!(hubot.status, StatusCode::OK, "{}", hubot.body);
    assert_eq!(hubot.body["total_count"], 0);
    assert_eq!(hubot.body["repositories"], json!([]));
}

#[tokio::test]
async fn only_a_github_app_user_token_may_call_it() {
    let emulator = Emulator::start(seed()).await.unwrap();
    let path = "/user/installations/5/repositories";
    let anonymous = get(&emulator, path, None).await;
    assert_eq!(anonymous.status, StatusCode::UNAUTHORIZED);
    assert_eq!(anonymous.body["message"], "Requires authentication");

    let pat = emulator
        .mint_personal_access_token("boss", &["repo"])
        .unwrap();
    let refused = get(&emulator, path, Some(&pat)).await;
    assert_eq!(refused.status, StatusCode::FORBIDDEN, "{}", refused.body);
    assert!(
        refused.body["message"]
            .as_str()
            .unwrap()
            .contains("authorized to a GitHub App")
    );

    let installation = Client::new()
        .post(format!("{}/tokens/installation", emulator.admin_base()))
        .header("content-type", "application/json")
        .body(json!({"installation_id": 5}).to_string())
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    let installation = serde_json::from_str::<Value>(&installation).unwrap()["token"]
        .as_str()
        .unwrap()
        .to_owned();
    let wrong_kind = get(&emulator, path, Some(&installation)).await;
    assert_eq!(wrong_kind.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn refuse_assumed_mode_keeps_only_the_documented_core() {
    let emulator = Emulator::start(seed().refuse_assumed()).await.unwrap();
    let listed = get(
        &emulator,
        "/user/installations/5/repositories",
        Some(&user_token(&emulator, "monalisa", 4)),
    )
    .await;
    assert_eq!(listed.status, StatusCode::OK, "{}", listed.body);
    assert_eq!(names(&listed.body), ["acme/widgets"]);
    assert!(listed.body.get("repository_selection").is_none());

    let pat = emulator
        .mint_personal_access_token("boss", &["repo"])
        .unwrap();
    let cases = [
        (
            "/user/installations/5/repositories",
            pat,
            "user-installations-pat",
        ),
        (
            "/user/installations/10/repositories",
            user_token(&emulator, "frozen", 4),
            "user-installation-access",
        ),
        (
            "/user/installations/7/repositories",
            user_token(&emulator, "hubot", 4),
            "user-installation-access",
        ),
        (
            "/user/installations/5/repositories",
            user_token(&emulator, "stranger", 4),
            "user-installation-access",
        ),
    ];
    for (path, token, behaviour) in cases {
        let refused = get(&emulator, path, Some(&token)).await;
        assert_eq!(refused.status, StatusCode::NOT_IMPLEMENTED, "{path}");
        assert_eq!(refused.body["behaviour"], behaviour, "{path}");
    }
    // Another App's installation is a documented 404 even so.
    let other = get(
        &emulator,
        "/user/installations/9/repositories",
        Some(&user_token(&emulator, "boss", 4)),
    )
    .await;
    assert_eq!(other.status, StatusCode::NOT_FOUND);
}
