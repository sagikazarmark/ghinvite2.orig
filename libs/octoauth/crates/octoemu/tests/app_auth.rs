//! App auth end to end (GE-02): octoauth's App client and installation
//! tokens and octotoken's provider against an embedded emulator, plus the
//! negative JWT and token cases octoauth never produces.
//!
//! Keys: `fixtures/rsa2048.pem` is octoauth's test key (its public half is
//! `rsa2048.pub.pem`); `rotated2048.pem` is a second test-only key pair for
//! key rotation.
#![cfg(not(target_arch = "wasm32"))]

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::{DateTime, TimeDelta, Utc};
use octoauth::{
    Access as Level, ApiError, App as AppClient, Error, ErrorBody, ForbiddenReason,
    OrganizationPermission, Permissions, RepositoryPermission, RepositorySelection, SigningKey,
};
use octoemu::Emulator;
use octoemu::calls::Identity;
use octoemu::seed::{
    Access, App, Installation, InstallationRequest, Org, Repo, Seed, User, Webhook,
};
use octotoken::{AppTokenProvider, TokenScope, with_token};
use reqwest::{Client, StatusCode};
use rsa::pkcs1v15::SigningKey as RsaSigningKey;
use rsa::pkcs8::DecodePrivateKey;
use rsa::signature::{SignatureEncoding, Signer};
use serde_json::{Value, json};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

const PRIVATE_KEY: &str = include_str!("fixtures/rsa2048.pem");
const PUBLIC_KEY: &str = include_str!("fixtures/rsa2048.pub.pem");
const ROTATED_PRIVATE_KEY: &str = include_str!("fixtures/rotated2048.pem");
const ROTATED_PUBLIC_KEY: &str = include_str!("fixtures/rotated2048.pub.pem");

/// App 4 (`Iv23.fixture`, both keys registered) installed on acme (5, every
/// repository; members:write + metadata:read of the App's wider grant) and
/// on octocat (6, members only, no metadata); App 7 (rotated key only)
/// installed on globex (8). One installation request for App 4.
fn seed() -> Seed {
    let mut seed = Seed::new()
        .user(User::new(1, "octocat"))
        .user(User::new(3, "hubot"))
        .org(Org::new(2, "acme"))
        .org(Org::new(9, "globex"))
        .repo(Repo::new(201, "globex", "gadgets").private())
        .app(
            App::new(4, "invite-bot", "Iv23.fixture", "fixture-client-secret")
                .public_key(ROTATED_PUBLIC_KEY)
                .public_key(PUBLIC_KEY)
                .webhook(Webhook::new("http://127.0.0.1:9/hooks", "fixture-hook"))
                .permission("members", Access::Write)
                .permission("metadata", Access::Read)
                .permission("administration", Access::Write)
                .event("member"),
        )
        .app(
            App::new(7, "other-bot", "Iv23.other", "other-client-secret")
                .public_key(ROTATED_PUBLIC_KEY)
                .permission("metadata", Access::Read),
        )
        .installation(
            Installation::new(5, 4, "acme")
                .permissions([("members", Access::Write), ("metadata", Access::Read)]),
        )
        .installation(Installation::new(6, 4, "octocat").permissions([("members", Access::Write)]))
        .installation(Installation::new(8, 7, "globex"))
        .installation_request(InstallationRequest::new(10, 4, "globex", "hubot"));
    for n in 1..=5 {
        seed = seed.repo(Repo::new(100 + n, "acme", format!("r{n:02}")).private());
    }
    seed
}

async fn start() -> Emulator {
    Emulator::start(seed()).await.unwrap()
}

fn app_client(emulator: &Emulator) -> AppClient {
    AppClient::builder(4, SigningKey::from_pem(PRIVATE_KEY).unwrap())
        .base_url(emulator.api_base())
        .build()
        .unwrap()
}

fn invite_permissions() -> Permissions {
    Permissions::builder()
        .grant(OrganizationPermission::Members, Level::Write)
        .grant(RepositoryPermission::Metadata, Level::Read)
        .build()
        .unwrap()
}

/// A JWT signed with `pem` (RS256 unless `header` says otherwise).
fn jwt_with(pem: &str, header: &Value, claims: &Value) -> String {
    let key = RsaSigningKey::<sha2::Sha256>::new(rsa::RsaPrivateKey::from_pkcs8_pem(pem).unwrap());
    let input = format!(
        "{}.{}",
        URL_SAFE_NO_PAD.encode(header.to_string()),
        URL_SAFE_NO_PAD.encode(claims.to_string())
    );
    let signature = key.sign(input.as_bytes()).to_vec();
    format!("{input}.{}", URL_SAFE_NO_PAD.encode(signature))
}

fn jwt(pem: &str, claims: &Value) -> String {
    jwt_with(pem, &json!({"alg": "RS256", "typ": "JWT"}), claims)
}

/// Claims GitHub's docs recommend, on the emulator's clock.
fn claims(emulator: &Emulator, iss: Value) -> Value {
    let now = emulator.now().timestamp();
    json!({"iat": now - 60, "exp": now + 540, "iss": iss})
}

struct Reply {
    status: StatusCode,
    headers: reqwest::header::HeaderMap,
    body: Value,
}

impl Reply {
    fn api_error(&self) -> ApiError {
        let bytes = self.body.to_string();
        ApiError::classify(
            self.status,
            &self.headers,
            ErrorBody::Bytes(bytes.as_bytes()),
        )
    }
}

async fn call(
    method: reqwest::Method,
    url: &str,
    authorization: Option<&str>,
    body: Option<Value>,
) -> Reply {
    let mut request = Client::new().request(method, url);
    if let Some(authorization) = authorization {
        request = request.header("authorization", authorization);
    }
    if let Some(body) = body {
        request = request
            .header("content-type", "application/json")
            .body(body.to_string());
    }
    let response = request.send().await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let text = response.text().await.unwrap();
    let body = if text.is_empty() {
        Value::Null
    } else {
        serde_json::from_str(&text).unwrap()
    };
    Reply {
        status,
        headers,
        body,
    }
}

async fn get(emulator: &Emulator, path: &str, authorization: &str) -> Reply {
    call(
        reqwest::Method::GET,
        &format!("{}{path}", emulator.api_base()),
        Some(authorization),
        None,
    )
    .await
}

fn bearer(token: &str) -> String {
    format!("Bearer {token}")
}

/// A raw token request with an App JWT.
async fn create_token(emulator: &Emulator, installation: u64, body: Value) -> Reply {
    let credential = jwt(PRIVATE_KEY, &claims(emulator, json!(4)));
    call(
        reqwest::Method::POST,
        &format!(
            "{}/app/installations/{installation}/access_tokens",
            emulator.api_base()
        ),
        Some(&bearer(&credential)),
        Some(body),
    )
    .await
}

/// Every repository name, following `Link: rel="next"` from `first`.
async fn walk_repositories(emulator: &Emulator, first: &str, token: &str) -> (Vec<String>, usize) {
    let mut url = format!("{}{first}", emulator.api_base());
    let mut names = Vec::new();
    let mut pages = 0;
    loop {
        let reply = call(reqwest::Method::GET, &url, Some(&bearer(token)), None).await;
        assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
        pages += 1;
        assert_eq!(reply.body["total_count"], 5);
        for repo in reply.body["repositories"].as_array().unwrap() {
            names.push(repo["name"].as_str().unwrap().to_owned());
        }
        let next = reply.headers.get("link").and_then(|link| {
            link.to_str().unwrap().split(", ").find_map(|rel| {
                rel.strip_suffix("; rel=\"next\"")
                    .map(|url| url.trim_matches(['<', '>']).to_owned())
            })
        });
        match next {
            Some(next) => url = next,
            None => return (names, pages),
        }
    }
}

#[tokio::test]
async fn octoauth_mints_a_token_that_lists_repositories_page_by_page() {
    let emulator = start().await;
    let token = app_client(&emulator)
        .installation(5)
        .access_token(&invite_permissions())
        .await
        .unwrap();
    let secret = token.expose_secret();
    assert!(secret.starts_with("ghs_4_"), "{secret}");
    // GitHub's are 383 characters.
    assert!((383..391).contains(&secret.len()), "{}", secret.len());
    let lifetime = token
        .expires_at()
        .duration_since(token.issued_at().unwrap())
        .unwrap();
    assert_eq!(lifetime, Duration::from_secs(3600));

    let (names, pages) =
        walk_repositories(&emulator, "/installation/repositories?per_page=2", secret).await;
    assert_eq!(names, ["r01", "r02", "r03", "r04", "r05"]);
    assert_eq!(pages, 3);
    // Above 100 silently becomes 100; the default is 30.
    let (names, pages) =
        walk_repositories(&emulator, "/installation/repositories?per_page=500", secret).await;
    assert_eq!((names.len(), pages), (5, 1));

    // The call log records verified identities, never the secret.
    let calls = emulator.calls();
    assert_eq!(
        calls[0].route.as_deref(),
        Some("POST /app/installations/{installation_id}/access_tokens")
    );
    assert_eq!(calls[0].status, 201);
    assert_eq!(calls[0].identity, Some(Identity::App { app_id: 4 }));
    assert_eq!(
        calls[1].identity,
        Some(Identity::Installation {
            app_id: 4,
            installation_id: 5,
            token_id: token.id().to_string(),
        })
    );
    let state = emulator.state();
    assert!(!state.to_string().contains(secret));
    assert_eq!(state["tokens"][0]["id"], token.id().to_string());
    assert_eq!(state["tokens"][0]["installation_id"], 5);
}

#[tokio::test]
async fn the_client_id_is_an_issuer_too_and_installations_have_ghinvites_fields() {
    let emulator = start().await;
    let app = AppClient::builder(4, SigningKey::from_pem(PRIVATE_KEY).unwrap())
        .client_id("Iv23.fixture")
        .base_url(emulator.api_base())
        .build()
        .unwrap();
    let info = app.installation(5).get().await.unwrap();
    assert_eq!(info.account.login, "acme");
    assert_eq!(info.account.id, 2);
    assert_eq!(info.account.kind, octoauth::AccountKind::Organization);
    assert_eq!(info.repository_selection, RepositorySelection::All);
    assert_eq!(info.permission("members"), Some("write"));
    assert_eq!(
        info.permission("administration"),
        None,
        "the installation's grant, not the App's"
    );
    assert_eq!(info.suspended_at, None);

    let credential = bearer(app.credential().unwrap().expose_secret());
    let raw = get(&emulator, "/app/installations/5", &credential).await;
    for field in ["created_at", "updated_at"] {
        let at = raw.body[field].as_str().unwrap();
        assert!(
            at.ends_with('Z') && at.parse::<DateTime<Utc>>().is_ok(),
            "{at}"
        );
    }
    assert_eq!(raw.body["app_id"], 4);
    assert_eq!(raw.body["target_type"], "Organization");
    assert_eq!(raw.body["target_id"], 2);
    assert_eq!(raw.body["events"], json!(["member"]));
    assert_eq!(raw.body["suspended_by"], Value::Null);
    assert_eq!(raw.body["account"]["type"], "Organization");

    let me = get(&emulator, "/app", &credential).await;
    assert_eq!(me.status, StatusCode::OK);
    assert_eq!(me.body["slug"], "invite-bot");
    assert_eq!(me.body["client_id"], "Iv23.fixture");
    assert_eq!(me.body["installations_count"], 2);

    let page = get(&emulator, "/app/installations?per_page=1", &credential).await;
    assert_eq!(page.body.as_array().unwrap().len(), 1);
    assert_eq!(page.body[0]["id"], 5);
    let link = page.headers["link"].to_str().unwrap();
    assert!(link.contains("page=2>; rel=\"next\""), "{link}");

    let requests = get(&emulator, "/app/installation-requests", &credential).await;
    assert_eq!(requests.status, StatusCode::OK);
    assert_eq!(requests.body[0]["id"], 10);
    assert_eq!(requests.body[0]["account"]["login"], "globex");
    assert_eq!(requests.body[0]["requester"]["login"], "hubot");
    assert_eq!(requests.body[0]["requester"]["id"], 3);
    assert!(requests.body[0]["created_at"].is_string());
}

#[tokio::test]
async fn the_app_its_bot_and_its_installations_in_githubs_shapes() {
    let mut world = seed();
    world.apps[0].external_url = Some("https://example.test/invite-bot".into());
    world.apps[0].events = vec!["organization".into(), "member".into()];
    world.repos.push(
        Repo::new(150, "acme", "quiet")
            .private()
            .has_wiki(false)
            .has_downloads(false)
            .allow_forking(false),
    );
    let emulator = Emulator::start(world).await.unwrap();
    let app = bearer(&jwt(PRIVATE_KEY, &claims(&emulator, json!(4))));

    // Events sorted; the registered homepage URL.
    let me = get(&emulator, "/app", &app).await;
    assert_eq!(me.body["events"], json!(["member", "organization"]));
    assert_eq!(me.body["external_url"], "https://example.test/invite-bot");
    let org = get(&emulator, "/app/installations/5", &app).await;
    assert_eq!(org.body["events"], json!(["member", "organization"]));
    // An organization's installation settings live under it.
    let base = emulator.api_base();
    assert_eq!(
        org.body["html_url"],
        format!("{base}/organizations/acme/settings/installations/5")
    );
    let user = get(&emulator, "/app/installations/6", &app).await;
    assert_eq!(
        user.body["html_url"],
        format!("{base}/settings/installations/6")
    );

    // Suspended by the App itself: its bot, as GET /users answers it.
    let suspend_url = format!("{base}/app/installations/5/suspended");
    let suspended = call(reqwest::Method::PUT, &suspend_url, Some(&app), None).await;
    assert_eq!(suspended.status, StatusCode::NO_CONTENT);
    let shown = get(&emulator, "/app/installations/5", &app).await;
    let bot = &shown.body["suspended_by"];
    assert_eq!(bot["login"], "invite-bot[bot]");
    assert_eq!(bot["type"], "Bot");
    assert_eq!(bot["url"], format!("{base}/users/invite-bot%5Bbot%5D"));
    assert_eq!(
        bot["repos_url"],
        format!("{base}/users/invite-bot%5Bbot%5D/repos")
    );
    assert_eq!(bot["html_url"], format!("{base}/apps/invite-bot"));
    let profile = call(
        reqwest::Method::GET,
        &format!("{base}/users/invite-bot%5Bbot%5D"),
        None,
        None,
    )
    .await;
    assert_eq!(profile.status, StatusCode::OK);
    assert_eq!(profile.body["id"], bot["id"]);
    assert_eq!(profile.body["type"], "Bot");
    // Logins are case-insensitive, the `[bot]` suffix too.
    let mixed = call(
        reqwest::Method::GET,
        &format!("{base}/users/Invite-Bot%5BBot%5D"),
        None,
        None,
    )
    .await;
    assert_eq!(mixed.status, StatusCode::OK);
    assert_eq!(mixed.body["id"], bot["id"]);
    let lifted = call(reqwest::Method::DELETE, &suspend_url, Some(&app), None).await;
    assert_eq!(lifted.status, StatusCode::NO_CONTENT);
    let shown = get(&emulator, "/app/installations/5", &app).await;
    assert_eq!(shown.body["suspended_by"], Value::Null);

    // Seeded repository settings.
    let token = create_token(&emulator, 5, json!({})).await;
    let token = bearer(token.body["token"].as_str().unwrap());
    let quiet = get(&emulator, "/repos/acme/quiet", &token).await;
    for flag in ["has_wiki", "has_downloads", "allow_forking"] {
        assert_eq!(quiet.body[flag], false, "{flag}");
    }
    let default = get(&emulator, "/repos/acme/r01", &token).await;
    for flag in ["has_wiki", "has_downloads", "allow_forking"] {
        assert_eq!(default.body[flag], true, "{flag}");
    }
}

/// Lists installation 5's repositories through `with_token`, counting the
/// attempts; the token's ID.
async fn list_with_provider(
    emulator: &Emulator,
    provider: &AppTokenProvider,
    scope: &TokenScope,
    attempts: &AtomicUsize,
) -> octoauth::TokenId {
    with_token(
        provider,
        octoauth::InstallationId::new(5),
        scope,
        ApiError::is_unauthorized,
        |token| {
            attempts.fetch_add(1, Ordering::SeqCst);
            async move {
                let auth = bearer(token.expose_secret());
                let reply = get(emulator, "/installation/repositories", &auth).await;
                if reply.status == StatusCode::OK {
                    Ok(token.id())
                } else {
                    Err(reply.api_error())
                }
            }
        },
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn octotoken_replaces_a_token_that_expired_on_the_clock() {
    let emulator = start().await;
    let provider = AppTokenProvider::new(app_client(&emulator));
    let scope = TokenScope::new(invite_permissions());
    let attempts = AtomicUsize::new(0);
    let list = || list_with_provider(&emulator, &provider, &scope, &attempts);
    let first = list().await;
    assert_eq!(attempts.swap(0, Ordering::SeqCst), 1);
    assert_eq!(list().await, first, "cached");
    assert_eq!(attempts.swap(0, Ordering::SeqCst), 1);

    // An hour later on GitHub's clock the cached token is expired: one 401,
    // one replacement (whose JWT octoauth re-signs for the skew), one retry.
    emulator.advance_clock(Duration::from_secs(3601));
    let replaced = list().await;
    assert_ne!(replaced, first);
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
    let statuses: Vec<u16> = emulator.calls().iter().map(|call| call.status).collect();
    assert_eq!(
        &statuses[statuses.len() - 4..],
        [401, 401, 201, 200],
        "{statuses:?}"
    );
}

#[tokio::test]
async fn revoked_tokens_are_refused() {
    let emulator = start().await;
    let token = app_client(&emulator)
        .installation(5)
        .access_token(&invite_permissions())
        .await
        .unwrap();
    let auth = format!("token {}", token.expose_secret());
    assert_eq!(
        get(&emulator, "/installation/repositories", &auth)
            .await
            .status,
        StatusCode::OK
    );
    let revoke = call(
        reqwest::Method::DELETE,
        &format!("{}/installation/token", emulator.api_base()),
        Some(&auth),
        None,
    )
    .await;
    assert_eq!(revoke.status, StatusCode::NO_CONTENT);
    // GitHub records the revocation at once: a second DELETE is a 401.
    let again = call(
        reqwest::Method::DELETE,
        &format!("{}/installation/token", emulator.api_base()),
        Some(&auth),
        None,
    )
    .await;
    assert_eq!(again.status, StatusCode::UNAUTHORIZED);
    assert_eq!(again.body["message"], "Bad credentials");
    // Other routes see it once it has propagated (500 ms on the clock).
    let during = get(&emulator, "/installation/repositories", &auth).await;
    assert_eq!(during.status, StatusCode::OK);
    emulator.advance_clock(Duration::from_secs(1));
    let after = get(&emulator, "/installation/repositories", &auth).await;
    assert_eq!(after.status, StatusCode::UNAUTHORIZED);
    assert_eq!(after.body["message"], "Bad credentials");
    assert_eq!(after.api_error(), ApiError::Unauthorized);
    assert_eq!(emulator.state()["tokens"][0]["revoked"], true);
}

#[tokio::test]
async fn suspension_blocks_tokens_and_octoauth_classifies_it() {
    let emulator = start().await;
    let app = app_client(&emulator);
    let installation = app.installation(5);
    let token = installation
        .access_token(&invite_permissions())
        .await
        .unwrap();
    let auth = bearer(token.expose_secret());
    let jwt = bearer(app.credential().unwrap().expose_secret());
    let suspend_url = format!("{}/app/installations/5/suspended", emulator.api_base());

    let suspended = call(reqwest::Method::PUT, &suspend_url, Some(&jwt), None).await;
    assert_eq!(suspended.status, StatusCode::NO_CONTENT);
    assert_eq!(
        installation
            .access_token(&invite_permissions())
            .await
            .unwrap_err(),
        Error::InstallationSuspended
    );
    let info = installation.get().await.unwrap();
    assert!(info.suspended_at.is_some());
    let refused = get(&emulator, "/installation/repositories", &auth).await;
    assert_eq!(refused.status, StatusCode::FORBIDDEN);
    assert_eq!(
        refused.body["message"],
        "This GitHub App installation is currently suspended."
    );
    assert_eq!(
        refused.api_error(),
        ApiError::Forbidden {
            reason: ForbiddenReason::Other
        }
    );

    let lifted = call(reqwest::Method::DELETE, &suspend_url, Some(&jwt), None).await;
    assert_eq!(lifted.status, StatusCode::NO_CONTENT);
    assert_eq!(
        get(&emulator, "/installation/repositories", &auth)
            .await
            .status,
        StatusCode::OK
    );
    assert!(
        installation
            .access_token(&invite_permissions())
            .await
            .is_ok()
    );

    // The admin API and the handle do the same.
    let admin = format!("{}/installations/5/suspend", emulator.admin_base());
    assert_eq!(
        call(reqwest::Method::POST, &admin, None, None).await.status,
        StatusCode::NO_CONTENT
    );
    assert!(installation.is_suspended().await.unwrap());
    assert!(emulator.unsuspend_installation(5));
    assert!(!installation.is_suspended().await.unwrap());
    assert!(emulator.suspend_installation(5));
    assert!(installation.is_suspended().await.unwrap());
    assert!(!emulator.suspend_installation(404));
}

#[tokio::test]
async fn unknown_foreign_and_uninstalled_installations_are_gone() {
    let emulator = start().await;
    let app = app_client(&emulator);
    let permissions = invite_permissions();
    for id in [999, 8] {
        assert_eq!(
            app.installation(id)
                .access_token(&permissions)
                .await
                .unwrap_err(),
            Error::InstallationGone,
            "{id}"
        );
        assert_eq!(
            app.installation(id).get().await.unwrap_err(),
            Error::InstallationGone,
            "{id}"
        );
    }
    let token = app
        .installation(5)
        .access_token(&permissions)
        .await
        .unwrap();
    let uninstall = format!("{}/installations/5", emulator.admin_base());
    assert_eq!(
        call(reqwest::Method::DELETE, &uninstall, None, None)
            .await
            .status,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        call(reqwest::Method::DELETE, &uninstall, None, None)
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        app.installation(5).get().await.unwrap_err(),
        Error::InstallationGone
    );
    let after = get(
        &emulator,
        "/installation/repositories",
        &bearer(token.expose_secret()),
    )
    .await;
    assert_eq!(after.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn the_app_uninstalls_itself_and_its_tokens_stop_working() {
    let emulator = start().await;
    let jwt = bearer(&jwt(PRIVATE_KEY, &claims(&emulator, json!(4))));
    let minted = create_token(&emulator, 5, json!({})).await;
    let token = minted.body["token"].as_str().unwrap().to_owned();
    let url = |id: u64| format!("{}/app/installations/{id}", emulator.api_base());
    // Another App's installation and an unknown one are not this App's.
    for id in [8, 999] {
        let refused = call(reqwest::Method::DELETE, &url(id), Some(&jwt), None).await;
        assert_eq!(refused.status, StatusCode::NOT_FOUND, "{id}");
    }
    let deleted = call(reqwest::Method::DELETE, &url(5), Some(&jwt), None).await;
    assert_eq!(deleted.status, StatusCode::NO_CONTENT);
    assert_eq!(
        get(&emulator, "/app/installations/5", &jwt).await.status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        get(&emulator, "/installation/repositories", &bearer(&token))
            .await
            .status,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        create_token(&emulator, 5, json!({})).await.status,
        StatusCode::NOT_FOUND
    );
    let again = call(reqwest::Method::DELETE, &url(5), Some(&jwt), None).await;
    assert_eq!(again.status, StatusCode::NOT_FOUND);
    // installation.deleted, sent by the App's bot.
    let deleted = emulator
        .events()
        .into_iter()
        .find(|e| e.event == "installation" && e.action.as_deref() == Some("deleted"))
        .unwrap();
    assert_eq!(deleted.payload["sender"]["login"], "invite-bot[bot]");
}

#[tokio::test]
async fn an_unsupported_api_version_is_a_400_on_the_api_only() {
    let emulator = start().await;
    let jwt = bearer(&jwt(PRIVATE_KEY, &claims(&emulator, json!(4))));
    let client = Client::new();
    let app = format!("{}/app", emulator.api_base());
    for version in ["2022-11-28", "2026-03-10"] {
        let ok = client
            .get(&app)
            .header("authorization", &jwt)
            .header("x-github-api-version", version)
            .send()
            .await
            .unwrap();
        assert_eq!(ok.status(), StatusCode::OK, "{version}");
    }
    for version in ["2021-01-01", "latest"] {
        let refused = client
            .get(&app)
            .header("x-github-api-version", version)
            .send()
            .await
            .unwrap();
        // Before the credential: none is sent.
        assert_eq!(refused.status(), StatusCode::BAD_REQUEST, "{version}");
        let body: Value = serde_json::from_str(&refused.text().await.unwrap()).unwrap();
        assert_eq!(
            body,
            json!({
                "message": "Bad Request",
                "errors": format!(
                    "The version you specified in the \"X-GitHub-API-Version\" request header, \
                     \"{version}\", is not a supported version. The following versions are \
                     currently supported: \"2026-03-10\" (most recent) and \"2022-11-28\"."
                ),
                "documentation_url": "https://docs.github.com/rest",
                "status": "400",
            })
        );
    }
    // The web routes ignore it.
    let web = client
        .post(format!("{}/login/oauth/access_token", emulator.web_base()))
        .header("x-github-api-version", "2021-01-01")
        .header("accept", "application/json")
        .form(&[("client_id", "Iv23.fixture"), ("code", "nope")])
        .send()
        .await
        .unwrap();
    assert_eq!(web.status(), StatusCode::OK);
    // Refuse-assumed mode refuses the guess.
    let strict = Emulator::builder(seed())
        .refuse_assumed()
        .start()
        .await
        .unwrap();
    let refused = client
        .get(format!("{}/app", strict.api_base()))
        .header("x-github-api-version", "2021-01-01")
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), StatusCode::NOT_IMPLEMENTED);
    let body: Value = serde_json::from_str(&refused.text().await.unwrap()).unwrap();
    assert_eq!(body["behaviour"], "unsupported-api-version");
}

#[tokio::test]
async fn permissions_and_repositories_narrow_and_never_widen() {
    let emulator = start().await;
    let app = app_client(&emulator);

    // Narrower: metadata only.
    let metadata = Permissions::new([(RepositoryPermission::Metadata, Level::Read)]).unwrap();
    let narrow = create_token(&emulator, 5, json!({"permissions": {"metadata": "read"}})).await;
    assert_eq!(narrow.status, StatusCode::CREATED);
    assert_eq!(narrow.body["permissions"], json!({"metadata": "read"}));
    // GitHub reports a token narrowed by permissions as `selected`.
    assert_eq!(narrow.body["repository_selection"], "selected");
    assert!(narrow.body.get("repositories").is_none());
    assert!(app.installation(5).access_token(&metadata).await.is_ok());

    // Members only: the answer lists what was asked; the token still
    // reads metadata (assumed).
    let members = create_token(&emulator, 5, json!({"permissions": {"members": "write"}})).await;
    assert_eq!(members.body["permissions"], json!({"members": "write"}));
    let token = bearer(members.body["token"].as_str().unwrap());
    let repo = get(&emulator, "/repos/acme/r01", &token).await;
    assert_eq!(repo.status, StatusCode::OK);

    // Ungranted (the App has it, the installation does not) or higher: 422,
    // which octoauth classifies.
    let ungranted =
        Permissions::new([(RepositoryPermission::Administration, Level::Write)]).unwrap();
    assert_eq!(
        app.installation(5)
            .access_token(&ungranted)
            .await
            .unwrap_err(),
        Error::PermissionsNotGranted
    );
    for (permissions, message) in [
        (
            json!({"issues": "write"}),
            "The permissions requested are not granted to this installation.",
        ),
        (
            json!({"members": "admin"}),
            "The level of access for permissions requested are not granted to this installation.",
        ),
        (
            json!({"metadata": "write"}),
            "The level of access for permissions requested are not granted to this installation.",
        ),
        (
            json!({"members": "superuser"}),
            "The level of access for permissions requested are not granted to this installation.",
        ),
    ] {
        let reply = create_token(&emulator, 5, json!({"permissions": permissions})).await;
        assert_eq!(
            reply.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{permissions}"
        );
        assert_eq!(reply.body["message"], message, "{permissions}");
        assert_eq!(
            reply.body["documentation_url"],
            "https://docs.github.com/rest/reference/apps#create-an-installation-access-token-for-an-app"
        );
    }

    // Repositories by name, by ID, both (the union), unknown, another
    // account's, too many.
    let named = create_token(&emulator, 5, json!({"repositories": ["r01", "R03"]})).await;
    assert_eq!(named.status, StatusCode::CREATED);
    assert_eq!(named.body["repository_selection"], "selected");
    let names: Vec<&str> = named.body["repositories"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["r01", "r03"]);
    let listed = get(
        &emulator,
        "/installation/repositories",
        &bearer(named.body["token"].as_str().unwrap()),
    )
    .await;
    assert_eq!(listed.body["total_count"], 2);
    assert_eq!(listed.body["repository_selection"], "selected");

    let both = create_token(
        &emulator,
        5,
        json!({"repositories": ["r01"], "repository_ids": [104]}),
    )
    .await;
    let ids: Vec<u64> = both.body["repositories"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["id"].as_u64().unwrap())
        .collect();
    assert_eq!(ids, [101, 104]);

    for body in [
        json!({"repositories": ["nope"]}),
        json!({"repositories": ["gadgets"]}),
        json!({"repository_ids": [201]}),
    ] {
        let reply = create_token(&emulator, 5, body.clone()).await;
        assert_eq!(reply.status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
        assert_eq!(
            reply.body["message"],
            "There is at least one repository that does not exist or is not accessible to the parent installation."
        );
    }
    // Each repository is checked before the count, as on GitHub.
    let too_many: Vec<u64> = (0..501).collect();
    let reply = create_token(&emulator, 5, json!({"repository_ids": too_many})).await;
    assert_eq!(reply.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        reply.body["message"],
        "There is at least one repository that does not exist or is not accessible to the parent installation."
    );

    let malformed = call(
        reqwest::Method::POST,
        &format!("{}/app/installations/5/access_tokens", emulator.api_base()),
        Some(&bearer(&jwt(PRIVATE_KEY, &claims(&emulator, json!(4))))),
        None,
    )
    .await;
    assert_eq!(
        malformed.status,
        StatusCode::CREATED,
        "an empty body asks for everything"
    );
}

#[tokio::test]
async fn a_token_without_the_routes_permission_is_forbidden() {
    let emulator = start().await;
    // Installation 6 grants members only, no metadata.
    let reply = create_token(&emulator, 6, json!({})).await;
    assert_eq!(reply.body["permissions"], json!({"members": "write"}));
    let token = bearer(reply.body["token"].as_str().unwrap());
    let refused = get(&emulator, "/installation/repositories", &token).await;
    assert_eq!(refused.status, StatusCode::FORBIDDEN);
    assert_eq!(
        refused.body["message"],
        "Resource not accessible by integration"
    );
    assert_eq!(
        refused.headers["x-accepted-github-permissions"],
        "metadata=read"
    );
    assert_eq!(
        refused.api_error(),
        ApiError::Forbidden {
            reason: ForbiddenReason::Other
        }
    );
    // A token without the permission may still revoke itself.
    let revoke = call(
        reqwest::Method::DELETE,
        &format!("{}/installation/token", emulator.api_base()),
        Some(&token),
        None,
    )
    .await;
    assert_eq!(revoke.status, StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn each_route_wants_its_kind_of_credential() {
    let emulator = start().await;
    let app_jwt = bearer(&jwt(PRIVATE_KEY, &claims(&emulator, json!(4))));
    let token = create_token(&emulator, 5, json!({})).await;
    let token = bearer(token.body["token"].as_str().unwrap());

    let jwt_on_token_route = get(&emulator, "/installation/repositories", &app_jwt).await;
    assert_eq!(jwt_on_token_route.status, StatusCode::UNAUTHORIZED);
    let token_on_jwt_route = get(&emulator, "/app/installations/5", &token).await;
    assert_eq!(token_on_jwt_route.status, StatusCode::UNAUTHORIZED);
    assert_eq!(
        token_on_jwt_route.body["message"],
        "A JSON web token could not be decoded"
    );

    let anonymous = call(
        reqwest::Method::GET,
        &format!("{}/app", emulator.api_base()),
        None,
        None,
    )
    .await;
    assert_eq!(anonymous.status, StatusCode::UNAUTHORIZED);
    assert_eq!(anonymous.body["message"], "Requires authentication");
    for garbage in [
        "Bearer ghs_4_nope",
        "token ghp_0123456789",
        "Bearer opaque",
        "Basic SXYyMzpz",
    ] {
        let reply = get(&emulator, "/app", garbage).await;
        assert_eq!(reply.status, StatusCode::UNAUTHORIZED, "{garbage}");
        assert_eq!(reply.body["message"], "Bad credentials", "{garbage}");
    }
}

#[tokio::test]
async fn octoauth_corrects_clock_skew_through_date() {
    // GitHub's clock 20 minutes ahead: octoauth's first JWT has expired there.
    let ahead = start().await;
    ahead.advance_clock(Duration::from_secs(1200));
    let info = app_client(&ahead).installation(5).get().await.unwrap();
    assert_eq!(info.id.get(), 5);
    let statuses: Vec<u16> = ahead.calls().iter().map(|call| call.status).collect();
    assert_eq!(statuses, [401, 200]);
    assert_eq!(
        ahead.calls()[0].identity,
        None,
        "a rejected JWT verifies no identity"
    );

    // 20 minutes behind: octoauth's first `iat` lies in GitHub's future.
    let behind = Emulator::start(seed().start_time(Utc::now() - TimeDelta::seconds(1200)))
        .await
        .unwrap();
    let token = app_client(&behind)
        .installation(5)
        .access_token(&invite_permissions())
        .await
        .unwrap();
    assert!(token.expose_secret().starts_with("ghs_4_"));
    let statuses: Vec<u16> = behind.calls().iter().map(|call| call.status).collect();
    assert_eq!(statuses, [401, 201]);
}

#[tokio::test]
async fn app_jwts_are_checked_strictly() {
    let emulator = start().await;
    let now = emulator.now().timestamp();
    let status_of = async |credential: &str| {
        let reply = get(&emulator, "/app", &bearer(credential)).await;
        (
            reply.status.as_u16(),
            reply.body["message"].as_str().unwrap_or("").to_owned(),
        )
    };
    let ok = (200, String::new());

    // Accepted: the App ID as a number or string, the client ID, either
    // registered key (rotation).
    for iss in [json!(4), json!("4"), json!("Iv23.fixture")] {
        assert_eq!(
            status_of(&jwt(PRIVATE_KEY, &claims(&emulator, iss.clone()))).await,
            ok,
            "{iss}"
        );
    }
    assert_eq!(
        status_of(&jwt(ROTATED_PRIVATE_KEY, &claims(&emulator, json!(4)))).await,
        ok
    );

    // GitHub's texts, as the fidelity suite measured them.
    let decode = "A JSON web token could not be decoded";
    let bad = "Bad credentials";
    let not_integer = "'Issuer' claim ('iss') must be an Integer";
    let no_iat = "Missing 'issued at' claim ('iat') in assertion";
    let iat = "'Issued at' claim ('iat') must be an Integer representing the time that the assertion was issued";
    let exp = "'Expiration time' claim ('exp') must be a numeric value representing the future time at which the assertion expires";
    let far = "'Expiration time' claim ('exp') is too far in the future";
    let cases: Vec<(&str, String, &str)> = vec![
        // App 7 registers only the rotated key.
        (
            "unregistered key",
            jwt(PRIVATE_KEY, &claims(&emulator, json!(7))),
            decode,
        ),
        (
            "unknown client ID",
            jwt(PRIVATE_KEY, &claims(&emulator, json!("Iv23.nope"))),
            not_integer,
        ),
        (
            "unknown App ID",
            jwt(PRIVATE_KEY, &claims(&emulator, json!(99))),
            "Integration not found",
        ),
        (
            "no iss",
            jwt(PRIVATE_KEY, &json!({"iat": now - 60, "exp": now + 540})),
            not_integer,
        ),
        (
            "exp too far",
            jwt(
                PRIVATE_KEY,
                &json!({"iat": now, "exp": now + 660, "iss": 4}),
            ),
            far,
        ),
        (
            "expired",
            jwt(
                PRIVATE_KEY,
                &json!({"iat": now - 700, "exp": now - 100, "iss": 4}),
            ),
            bad,
        ),
        (
            "no exp",
            jwt(PRIVATE_KEY, &json!({"iat": now - 60, "iss": 4})),
            bad,
        ),
        (
            "non-numeric exp",
            jwt(
                PRIVATE_KEY,
                &json!({"iat": now - 60, "exp": "soon", "iss": 4}),
            ),
            exp,
        ),
        (
            "no iat",
            jwt(PRIVATE_KEY, &json!({"exp": now + 540, "iss": 4})),
            no_iat,
        ),
        (
            "non-numeric iat",
            jwt(
                PRIVATE_KEY,
                &json!({"iat": "then", "exp": now + 540, "iss": 4}),
            ),
            iat,
        ),
        (
            "iat in the future",
            jwt(
                PRIVATE_KEY,
                &json!({"iat": now + 120, "exp": now + 540, "iss": 4}),
            ),
            bad,
        ),
        (
            "HS256",
            jwt_with(
                PRIVATE_KEY,
                &json!({"alg": "HS256", "typ": "JWT"}),
                &claims(&emulator, json!(4)),
            ),
            bad,
        ),
    ];
    for (case, credential, message) in cases {
        assert_eq!(
            status_of(&credential).await,
            (401, message.to_owned()),
            "{case}"
        );
    }

    let valid = jwt(PRIVATE_KEY, &claims(&emulator, json!(4)));
    let (input, signature) = valid.rsplit_once('.').unwrap();
    let mut signature = URL_SAFE_NO_PAD.decode(signature).unwrap();
    signature[0] ^= 1;
    let tampered = format!("{input}.{}", URL_SAFE_NO_PAD.encode(signature));
    assert_eq!(
        status_of(&tampered).await,
        (401, decode.to_owned()),
        "bad signature"
    );
    assert_eq!(status_of("eyJ.not.ajwt").await, (401, bad.to_owned()));

    // The `token` scheme is for tokens, not JWTs: on a JWT route GitHub
    // answers as for any credential that is not a JWT, elsewhere `Bad
    // credentials`.
    let reply = get(&emulator, "/app", &format!("token {valid}")).await;
    assert_eq!(reply.status, StatusCode::UNAUTHORIZED);
    assert_eq!(reply.body["message"], decode);
    let reply = get(
        &emulator,
        "/installation/repositories",
        &format!("token {valid}"),
    )
    .await;
    assert_eq!(reply.status, StatusCode::UNAUTHORIZED);
    assert_eq!(reply.body["message"], bad);

    // Accepted, as GitHub accepts them: an `iat` up to 60 s early (the
    // assumed tolerance), a numeric string `iat`, and an `exp` more than
    // 10 minutes after `iat` but within 10 minutes of now.
    for claims in [
        json!({"iat": now + 10, "exp": now + 540, "iss": 4}),
        json!({"iat": now + 55, "exp": now + 540, "iss": 4}),
        json!({"iat": (now - 60).to_string(), "exp": now + 540, "iss": 4}),
        json!({"iat": now - 120, "exp": now + 540, "iss": 4}),
    ] {
        assert_eq!(status_of(&jwt(PRIVATE_KEY, &claims)).await, ok, "{claims}");
    }
}

#[tokio::test]
async fn the_admin_api_mints_tokens_and_shows_only_their_ids() {
    let emulator = start().await;
    let mint = call(
        reqwest::Method::POST,
        &format!("{}/tokens/installation", emulator.admin_base()),
        None,
        Some(json!({"installation_id": 5, "repositories": ["r02"]})),
    )
    .await;
    assert_eq!(mint.status, StatusCode::CREATED);
    let token = mint.body["token"].as_str().unwrap();
    let listed = get(&emulator, "/installation/repositories", &bearer(token)).await;
    assert_eq!(listed.body["total_count"], 1);
    assert_eq!(listed.body["repositories"][0]["full_name"], "acme/r02");

    let state = call(
        reqwest::Method::GET,
        &format!("{}/state", emulator.admin_base()),
        None,
        None,
    )
    .await;
    assert!(!state.body.to_string().contains(token));
    assert_eq!(state.body["tokens"][0]["repository_ids"], json!([102]));
    assert_eq!(state.body["tokens"][0]["id"].as_str().unwrap().len(), 32);

    let missing = call(
        reqwest::Method::POST,
        &format!("{}/tokens/installation", emulator.admin_base()),
        None,
        Some(json!({"installation_id": 404})),
    )
    .await;
    assert_eq!(missing.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn refuse_assumed_mode_refuses_guesses_and_withholds_guessed_messages() {
    let emulator = Emulator::start(seed().refuse_assumed()).await.unwrap();
    let now = emulator.now().timestamp();

    // Documented flows still work.
    let token = app_client(&emulator)
        .installation(5)
        .access_token(&invite_permissions())
        .await
        .unwrap();
    let auth = bearer(token.expose_secret());
    assert_eq!(
        get(&emulator, "/installation/repositories", &auth)
            .await
            .status,
        StatusCode::OK
    );

    // A documented 401 keeps its status; its guessed text is withheld.
    let expired = jwt(
        PRIVATE_KEY,
        &json!({"iat": now - 700, "exp": now - 100, "iss": 4}),
    );
    let reply = get(&emulator, "/app", &bearer(&expired)).await;
    assert_eq!(reply.status, StatusCode::UNAUTHORIZED);
    assert_eq!(reply.body["message"], "octoemu: assumed message withheld");

    // Guessed outcomes are refused.
    let early = jwt(
        PRIVATE_KEY,
        &json!({"iat": now + 10, "exp": now + 540, "iss": 4}),
    );
    let reply = get(&emulator, "/app", &bearer(&early)).await;
    assert_eq!(reply.status, StatusCode::NOT_IMPLEMENTED);
    assert_eq!(reply.body["behaviour"], "jwt-iat-tolerance");

    assert!(emulator.suspend_installation(5));
    let reply = get(&emulator, "/installation/repositories", &auth).await;
    assert_eq!(reply.status, StatusCode::NOT_IMPLEMENTED);
    assert_eq!(reply.body["behaviour"], "suspended-installation-tokens");
    assert!(emulator.unsuspend_installation(5));

    let revoke = call(
        reqwest::Method::DELETE,
        &format!("{}/installation/token", emulator.api_base()),
        Some(&auth),
        None,
    )
    .await;
    assert_eq!(revoke.status, StatusCode::NO_CONTENT);
    let reply = get(&emulator, "/installation/repositories", &auth).await;
    assert_eq!(reply.status, StatusCode::NOT_IMPLEMENTED);
    assert_eq!(reply.body["behaviour"], "token-revocation-propagation");

    let long = jwt(
        PRIVATE_KEY,
        &json!({"iat": now - 120, "exp": now + 540, "iss": 4}),
    );
    let reply = get(&emulator, "/app", &bearer(&long)).await;
    assert_eq!(reply.status, StatusCode::NOT_IMPLEMENTED);
    assert_eq!(reply.body["behaviour"], "jwt-exp-after-iat");

    let members = create_token(&emulator, 5, json!({"permissions": {"members": "write"}})).await;
    assert_eq!(members.status, StatusCode::NOT_IMPLEMENTED);
    assert_eq!(members.body["behaviour"], "token-implicit-metadata");
}
