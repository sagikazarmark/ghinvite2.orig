//! Seed parsing, the builder, and validation errors.
#![cfg(not(target_arch = "wasm32"))]

use octoemu::seed::{
    Access, App, Collaborator, Installation, InstallationRequest, Membership, Org, OrgRole, Plan,
    Repo, Role, Seed, Team, TeamMembership, TeamRole, User, Webhook,
};

const PUBLIC_KEY: &str = include_str!("fixtures/rsa2048.pub.pem");

fn app(id: u64) -> App {
    App::new(id, format!("app-{id}"), format!("Iv23.{id}"), "secret")
        .public_key(PUBLIC_KEY)
        .permission("members", Access::Write)
        .permission("metadata", Access::Read)
        .event("member")
}

/// A valid world touching every entity.
fn world() -> Seed {
    Seed::new()
        .user(
            User::new(1, "octocat")
                .name("The Octocat")
                .email("octocat@example.com"),
        )
        .user(User::new(2, "hubot"))
        .org(Org::new(10, "acme").plan(Plan::new("team", 10).filled_seats(2)))
        .team(Team::new(20, "acme", "core"))
        .repo(Repo::new(30, "acme", "widgets").private())
        .repo(Repo::new(31, "octocat", "dotfiles"))
        .app(
            app(40)
                .owner("acme")
                .webhook(Webhook::new("https://hooks.example.com/gh", "s1").secret("s0"))
                .callback_url("http://127.0.0.1:8080/callback"),
        )
        .installation(
            Installation::new(50, 40, "acme")
                .selected(["acme/widgets"])
                .permissions([("members", Access::Read)]),
        )
        .installation(
            Installation::new(51, 40, "octocat")
                .suspended_at("2030-01-01T00:00:00Z".parse().unwrap())
                .suspended_by("octocat"),
        )
        .installation_request(InstallationRequest::new(60, 40, "acme", "hubot"))
        .collaborator(Collaborator::new("acme/widgets", "hubot", Role::Push))
        .collaborator(Collaborator::new(
            "acme/widgets",
            "octocat",
            Role::Custom("security-reviewer".into()),
        ))
        .membership(Membership::new("acme", "octocat", OrgRole::Admin))
        .membership(Membership::new("acme", "hubot", OrgRole::Member).pending())
        .team_membership(TeamMembership::new(
            "acme/core",
            "octocat",
            TeamRole::Maintainer,
        ))
        .team_membership(TeamMembership::new("acme/core", "hubot", TeamRole::Member).pending())
}

fn problems(seed: &Seed) -> Vec<String> {
    seed.validate()
        .unwrap_err()
        .problems()
        .iter()
        .map(ToString::to_string)
        .collect()
}

#[test]
fn a_full_world_is_valid_and_round_trips_through_json() {
    let seed = world();
    seed.validate().unwrap();
    let json = serde_json::to_string(&seed).unwrap();
    assert_eq!(Seed::from_json(&json).unwrap(), seed);
}

#[test]
fn json_seeds_use_defaults_and_reject_unknown_fields() {
    let seed = Seed::from_json(
        r#"{
            "users": [{"id": 1, "login": "octocat"}],
            "orgs": [{"id": 2, "login": "acme"}],
            "repos": [{"id": 3, "owner": "acme", "name": "widgets"}],
            "collaborators": [{"repo": "acme/widgets", "user": "octocat", "role": "maintain"}],
            "memberships": [{"org": "acme", "user": "octocat"}]
        }"#,
    )
    .unwrap();
    seed.validate().unwrap();
    assert_eq!(seed.orgs[0].plan, Plan::new("free", 0));
    assert_eq!(seed.collaborators[0].role, Role::Maintain);
    assert_eq!(seed.memberships[0].role, OrgRole::Member);
    assert!(!seed.settings.refuse_assumed);

    for bad in [
        r#"{"user": []}"#,
        r#"{"users": [{"id": 1, "login": "x", "admin": true}]}"#,
        r#"{"settings": {"refuse_assumptions": true}}"#,
        r#"{"apps": [{"id": 1, "slug": "a", "client_id": "c", "client_secret": "s", "permissions": {"members": "own"}}]}"#,
    ] {
        assert!(Seed::from_json(bad).is_err(), "{bad}");
    }
}

#[test]
fn reports_duplicates() {
    let seed = Seed::new()
        .user(User::new(1, "octocat"))
        .user(User::new(1, "OctoCat"))
        .org(Org::new(2, "octocat"))
        .org(Org::new(0, "zero"))
        .repo(Repo::new(3, "zero", "a"))
        .repo(Repo::new(3, "zero", "A"))
        .app(app(4))
        .app(app(4));
    assert_eq!(
        problems(&seed),
        [
            "users[1].id: account ID 1 is used twice",
            "users[1].login: login \"OctoCat\" is used twice (logins ignore case)",
            "orgs[0].login: login \"octocat\" is used twice (logins ignore case)",
            "orgs[1].id: account IDs start at 1",
            "repos[1].id: repository ID 3 is used twice",
            "repos[1].name: zero/A is used twice (names ignore case)",
            "apps[1].id: App ID 4 is used twice",
            "apps[1].slug: App slug \"app-4\" is used twice",
            "apps[1].client_id: client ID \"Iv23.4\" is used twice",
        ]
    );
}

#[test]
fn reports_dangling_and_mistyped_references() {
    let seed = Seed::new()
        .user(User::new(1, "octocat"))
        .org(Org::new(2, "acme"))
        .team(Team::new(3, "octocat", "core"))
        .repo(Repo::new(4, "ghost", "widgets"))
        .installation(Installation::new(5, 99, "nobody"))
        .collaborator(Collaborator::new("acme/missing", "acme", Role::Pull))
        .membership(Membership::new("octocat", "acme", OrgRole::Member))
        .team_membership(TeamMembership::new(
            "acme/none",
            "octocat",
            TeamRole::Member,
        ));
    assert_eq!(
        problems(&seed),
        [
            "teams[0].org: \"octocat\" is a user, not an organization",
            "repos[0].owner: no user or organization \"ghost\"",
            "installations[0].app_id: no App with ID 99",
            "installations[0].account: no user or organization \"nobody\"",
            "collaborators[0].repo: no repository \"acme/missing\"",
            "collaborators[0].user: \"acme\" is an organization, not a user",
            "memberships[0].org: \"octocat\" is a user, not an organization",
            "memberships[0].user: \"acme\" is an organization, not a user",
            "team_memberships[0].team: no team \"acme/none\" (teams are named org/slug)",
        ]
    );
}

#[test]
fn reports_installation_scope_problems() {
    let seed = Seed::new()
        .user(User::new(1, "octocat"))
        .org(Org::new(2, "acme"))
        .repo(Repo::new(3, "octocat", "dotfiles"))
        .repo(Repo::new(4, "acme", "widgets"))
        .app(app(5))
        .installation(
            Installation::new(6, 5, "acme")
                .selected([
                    "octocat/dotfiles",
                    "acme/nope",
                    "acme/widgets",
                    "acme/widgets",
                ])
                .permissions([
                    ("metadata", Access::Write),
                    ("administration", Access::Read),
                    ("members", Access::Read),
                ]),
        )
        .installation(Installation::new(7, 5, "ACME"))
        .installation({
            let mut all = Installation::new(8, 5, "octocat");
            all.repositories = vec!["octocat/dotfiles".into()];
            all.events = Some(vec!["push".into()]);
            all
        });
    assert_eq!(
        problems(&seed),
        [
            "installations[0].repositories[0]: \"octocat/dotfiles\" is not owned by the installation's account \"acme\"",
            "installations[0].repositories[1]: no repository \"acme/nope\"",
            "installations[0].repositories[3]: \"acme/widgets\" is listed twice",
            "installations[0].permissions.administration: App 5 does not request \"administration\"",
            "installations[0].permissions.metadata: write is wider than App 5's read",
            "installations[1].account: App 5 is already installed on \"ACME\"",
            "installations[2].repositories: only an installation with repository_selection \"selected\" lists repositories",
            "installations[2].events[0]: App 5 does not subscribe to \"push\"",
        ]
    );
}

#[test]
fn reports_installation_request_and_suspension_problems() {
    let seed = Seed::new()
        .user(User::new(1, "octocat"))
        .org(Org::new(2, "acme"))
        .app(app(5))
        .installation(Installation::new(6, 5, "acme").suspended_by("acme"))
        .installation_request(InstallationRequest::new(7, 9, "nobody", "acme"))
        .installation_request(InstallationRequest::new(7, 5, "acme", "octocat"));
    assert_eq!(
        problems(&seed),
        [
            "installations[0].suspended_by: only a suspended installation (with suspended_at) names who suspended it",
            "installations[0].suspended_by: \"acme\" is an organization, not a user",
            "installation_requests[0].app_id: no App with ID 9",
            "installation_requests[0].account: no user or organization \"nobody\"",
            "installation_requests[0].requester: \"acme\" is an organization, not a user",
            "installation_requests[1].id: installation request ID 7 is used twice",
        ]
    );
}

#[test]
fn reports_app_problems() {
    let mut keyless = App::new(1, "keyless", "Iv23.k", "");
    keyless.permissions.insert("Members".into(), Access::Read);
    let seed = Seed::new()
        .app(keyless.event("member").event("member"))
        .app(
            App::new(2, "bad-key", "Iv23.b", "s")
                .public_key("-----BEGIN PUBLIC KEY-----\nnope\n-----END PUBLIC KEY-----")
                .owner("ghost")
                .webhook(Webhook::new("ftp://hooks", ""))
                .callback_url("/relative"),
        );
    assert_eq!(
        problems(&seed),
        [
            "apps[0].client_secret: is empty",
            "apps[0].public_keys: an App needs at least one RSA public key",
            "apps[0].permissions: permission name \"Members\" is not snake_case",
            "apps[0].events[1]: event \"member\" is listed twice",
            "apps[1].owner: no user or organization \"ghost\"",
            "apps[1].public_keys[0]: not an RSA public key in PEM (SPKI `PUBLIC KEY` or PKCS#1 `RSA PUBLIC KEY`)",
            "apps[1].webhook.url: \"ftp://hooks\" is not an http(s) URL",
            "apps[1].webhook.secrets[0]: is empty",
            "apps[1].callback_urls[0]: \"/relative\" is not an http(s) URL",
        ]
    );
}

#[test]
fn reports_membership_problems() {
    let seed = Seed::new()
        .user(User::new(1, "octocat"))
        .user(User::new(2, "hubot"))
        .org(Org::new(3, "acme"))
        .team(Team::new(4, "acme", "core"))
        .team(Team::new(5, "acme", "CORE"))
        .repo(Repo::new(6, "acme", "widgets"))
        .collaborator(Collaborator::new("acme/widgets", "hubot", Role::Pull))
        .collaborator(Collaborator::new("acme/widgets", "HUBOT", Role::Admin))
        .membership(Membership::new("acme", "octocat", OrgRole::Member).pending())
        .membership(Membership::new("acme", "octocat", OrgRole::Admin))
        .team_membership(TeamMembership::new(
            "acme/core",
            "octocat",
            TeamRole::Member,
        ))
        .team_membership(TeamMembership::new("acme/core", "hubot", TeamRole::Member));
    assert_eq!(
        problems(&seed),
        [
            "teams[1].slug: acme/CORE is used twice",
            "collaborators[1].user: \"HUBOT\" is already a collaborator on \"acme/widgets\"",
            "memberships[1].user: \"octocat\" already has a membership of \"acme\"",
            "team_memberships[0].state: an active team membership needs an active organization membership; \"octocat\"'s is pending",
            "team_memberships[1].user: \"hubot\" has no membership of the team's organization",
        ]
    );
}

#[test]
fn errors_display_every_problem() {
    let error = Seed::new()
        .installation(Installation::new(1, 2, "ghost"))
        .validate()
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        "invalid seed: installations[0].app_id: no App with ID 2; \
         installations[0].account: no user or organization \"ghost\""
    );
}

#[test]
fn merging_validates_the_whole_and_turns_refuse_assumed_on() {
    let base = world();
    let merged = base
        .merged(Seed::new().user(User::new(3, "monalisa")).refuse_assumed())
        .unwrap();
    assert_eq!(merged.users.len(), 3);
    assert!(merged.settings.refuse_assumed);

    let error = base
        .merged(Seed::new().user(User::new(1, "dup")))
        .unwrap_err();
    assert_eq!(
        error.problems()[0].to_string(),
        "users[2].id: account ID 1 is used twice"
    );
    // PKCS#1 keys are accepted too.
    let pkcs1 = include_str!("fixtures/rsa2048.pkcs1.pub.pem");
    Seed::new()
        .app(App::new(1, "a", "c", "s").public_key(pkcs1))
        .validate()
        .unwrap();
}
