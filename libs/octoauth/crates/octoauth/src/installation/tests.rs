//! `Installation` against a local GitHub stand-in (wiremock).

use super::*;
use crate::{
    Access, App, AppBuilder, AppId, AppKeySigner, ForbiddenReason, OrganizationPermission,
    RateLimitScope, RepositoryPermission, SigningError, SigningKey, TransientKind,
};

use std::sync::Arc;
use std::sync::atomic::Ordering::SeqCst;
use std::sync::atomic::{AtomicI64, AtomicUsize};
use std::time::Duration;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const PEM: &str = include_str!("../../tests/fixtures/rsa2048.pem");

const NOW: i64 = 1_800_000_000;

const CREATE: &str = "/app/installations/42/access_tokens";

const GET: &str = "/app/installations/42";

struct Fixed(i64);
impl crate::app::Clock for Fixed {
    fn now(&self) -> i64 {
        self.0
    }
}

fn builder_for(base: &str) -> AppBuilder {
    App::builder(7, SigningKey::from_pem(PEM).unwrap())
        .base_url(base)
        .clock(Arc::new(Fixed(NOW)))
}

fn app_for(base: &str) -> App {
    builder_for(base).build().unwrap()
}

fn permissions() -> Permissions {
    Permissions::builder()
        .grant(RepositoryPermission::Metadata, Access::Read)
        .grant(OrganizationPermission::Members, Access::Write)
        .build()
        .unwrap()
}

fn created(token: &str) -> ResponseTemplate {
    ResponseTemplate::new(201).set_body_json(serde_json::json!({
        "token": token,
        "expires_at": "2027-01-15T09:00:00Z",
        "permissions": {"members": "write", "metadata": "read"},
        "repository_selection": "all",
    }))
}

/// 2026-10-01T00:00:00Z, when the test installation was suspended.
const SUSPENDED_AT: u64 = 1_790_812_800;

/// A `GET /app/installations/42` body for App 7 as GitHub sends it, with
/// fields and permissions this crate does not know; suspended.
fn installation_body() -> serde_json::Value {
    serde_json::json!({
        "id": 42,
        "client_id": "Iv23liexample",
        "account": {
            "login": "acme",
            "id": 1001,
            "node_id": "O_kgDOAAAD6Q",
            "avatar_url": "https://avatars.githubusercontent.com/u/1001?v=4",
            "gravatar_id": "",
            "url": "https://api.github.com/users/acme",
            "html_url": "https://github.com/acme",
            "type": "Organization",
            "user_view_type": "public",
            "site_admin": false
        },
        "repository_selection": "selected",
        "access_tokens_url": "https://api.github.com/app/installations/42/access_tokens",
        "repositories_url": "https://api.github.com/installation/repositories",
        "html_url": "https://github.com/organizations/acme/settings/installations/42",
        "app_id": 7,
        "app_slug": "acme-invites",
        "target_id": 1001,
        "target_type": "Organization",
        "permissions": {
            "members": "write",
            "metadata": "read",
            "custom_properties_for_organizations": "admin"
        },
        "events": ["member", "organization"],
        "created_at": "2026-01-15T09:00:00.000Z",
        "updated_at": "2026-09-30T12:00:00.000Z",
        "single_file_name": null,
        "has_multiple_single_files": false,
        "single_file_paths": [],
        "suspended_by": {"login": "octocat", "id": 1, "type": "User"},
        "suspended_at": "2026-10-01T00:00:00Z"
    })
}

/// [`installation_body`], not suspended.
fn active_body() -> serde_json::Value {
    let mut body = installation_body();
    body["suspended_by"] = serde_json::Value::Null;
    body["suspended_at"] = serde_json::Value::Null;
    body
}

fn installed(body: serde_json::Value) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(body)
}

/// An HTTP `Date` header value for Unix time `seconds`.
fn http_date(seconds: i64) -> String {
    chrono::DateTime::from_timestamp(seconds, 0)
        .unwrap()
        .format("%a, %d %b %Y %H:%M:%S GMT")
        .to_string()
}

/// Mount each route. Responses carry `Date: NOW`, matching the test clock;
/// otherwise wiremock's real-time `Date` would look like clock skew.
async fn serve(routes: Vec<(&str, &str, ResponseTemplate)>) -> MockServer {
    let server = MockServer::start().await;
    for (verb, route, response) in routes {
        Mock::given(method(verb))
            .and(path(route))
            .respond_with(response.insert_header("date", http_date(NOW).as_str()))
            .mount(&server)
            .await;
    }
    server
}

async fn create_against(
    routes: Vec<(&str, &str, ResponseTemplate)>,
) -> (Result<InstallationToken, Error>, Vec<wiremock::Request>) {
    let server = serve(routes).await;
    let result = app_for(&server.uri())
        .installation(42)
        .access_token(&permissions())
        .await;
    (result, server.received_requests().await.unwrap())
}

fn header<'a>(request: &'a wiremock::Request, name: &str) -> &'a str {
    request.headers[name].to_str().unwrap()
}

fn bearer(request: &wiremock::Request) -> &str {
    header(request, "authorization")
        .strip_prefix("Bearer ")
        .unwrap()
}

/// The claims segment, unverified (signature checks live in the unit tests).
fn jwt_claims(jwt: &str) -> serde_json::Value {
    use base64::Engine as _;
    let claims = jwt.split('.').nth(1).unwrap();
    serde_json::from_slice(
        &base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(claims)
            .unwrap(),
    )
    .unwrap()
}

#[tokio::test]
async fn creates_with_the_documented_request() {
    let (result, seen) = create_against(vec![("POST", CREATE, created("ghs_abc123"))]).await;
    let token = result.unwrap();
    assert_eq!(token.app_id(), AppId::new(7));
    assert_eq!(token.installation_id(), InstallationId::new(42));
    assert_eq!(token.permissions(), &permissions());
    assert_eq!(
        token.issued_at(),
        Some(std::time::UNIX_EPOCH + Duration::from_secs(NOW as u64)),
        "from the response's Date"
    );
    assert_eq!(
        token.expires_at(),
        std::time::UNIX_EPOCH + Duration::from_secs(1_800_003_600)
    );

    let [request] = seen.as_slice() else {
        panic!("one request")
    };
    assert_eq!(request.method.as_str(), "POST");
    assert_eq!(header(request, "accept"), "application/vnd.github+json");
    assert_eq!(header(request, "x-github-api-version"), "2022-11-28");
    assert_eq!(header(request, "user-agent"), crate::DEFAULT_USER_AGENT);
    assert_eq!(header(request, "content-type"), "application/json");
    assert_eq!(
        request.body,
        br#"{"permissions":{"members":"write","metadata":"read"}}"#
    );
    let claims = jwt_claims(bearer(request));
    assert_eq!(claims["iss"], "7");
    assert_eq!(claims["iat"], NOW - 60);
    assert_eq!(claims["exp"], NOW + 540);
}

#[tokio::test]
async fn client_id_issuer_and_custom_signer() {
    struct Fixed256;
    impl AppKeySigner for Fixed256 {
        fn sign_rs256(&self, _: &[u8]) -> Result<Vec<u8>, SigningError> {
            Ok(vec![7; 256])
        }
    }
    let server = serve(vec![("POST", CREATE, created("ghs_x"))]).await;
    App::builder(7, Arc::new(Fixed256) as Arc<dyn AppKeySigner>)
        .client_id("Iv23.repo")
        .base_url(server.uri())
        .build()
        .unwrap()
        .installation(42)
        .access_token(&permissions())
        .await
        .unwrap();
    let seen = server.received_requests().await.unwrap();
    let jwt = bearer(&seen[0]);
    assert_eq!(jwt_claims(jwt)["iss"], "Iv23.repo");
    use base64::Engine as _;
    let signature = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode([7u8; 256]);
    assert!(jwt.ends_with(&format!(".{signature}")));
}

#[tokio::test]
async fn user_agent_sources() {
    let server = serve(vec![("POST", CREATE, created("ghs_x"))]).await;
    let custom = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .user_agent("custom-client/1")
        .build()
        .unwrap();
    builder_for(&server.uri())
        .client(custom.clone())
        .build()
        .unwrap()
        .installation(42)
        .access_token(&permissions())
        .await
        .unwrap();
    builder_for(&server.uri())
        .client(custom)
        .user_agent("override/2")
        .build()
        .unwrap()
        .installation(42)
        .access_token(&permissions())
        .await
        .unwrap();
    let seen = server.received_requests().await.unwrap();
    assert_eq!(header(&seen[0], "user-agent"), "custom-client/1");
    assert_eq!(header(&seen[1], "user-agent"), "override/2");
}

#[tokio::test]
async fn base_url_with_path() {
    let server = serve(vec![(
        "POST",
        "/api/v3/app/installations/42/access_tokens",
        created("ghs_x"),
    )])
    .await;
    app_for(&format!("{}/api/v3/", server.uri()))
        .installation(42)
        .access_token(&permissions())
        .await
        .unwrap();
}

#[tokio::test]
async fn statuses_are_classified() {
    let json = |status: u16, body: &str| {
        ResponseTemplate::new(status).set_body_raw(body.to_owned(), "application/json")
    };
    let cases: Vec<(ResponseTemplate, Error)> = vec![
        (
            json(401, r#"{"message":"Bad credentials"}"#),
            Error::AppCredentialsRejected { clock_skew: None },
        ),
        (ResponseTemplate::new(404), Error::InstallationGone),
        (ResponseTemplate::new(422), Error::PermissionsNotGranted),
        (
            ResponseTemplate::new(503),
            Error::Transient {
                kind: TransientKind::Server { status: 503 },
            },
        ),
        (
            ResponseTemplate::new(200),
            Error::Unexpected { status: 200 },
        ),
        (
            ResponseTemplate::new(429).insert_header("retry-after", "7"),
            Error::RateLimited {
                scope: RateLimitScope::Secondary,
                retry_after: Some(Duration::from_secs(7)),
            },
        ),
        (
            ResponseTemplate::new(403).insert_header("x-ratelimit-remaining", "0"),
            Error::RateLimited {
                scope: RateLimitScope::Primary,
                retry_after: None,
            },
        ),
        (
            ResponseTemplate::new(201).set_body_bytes(vec![b' '; 10 * 1024 * 1024 + 1]),
            Error::Decode,
        ),
    ];
    for (response, expected) in cases {
        let (result, seen) = create_against(vec![("POST", CREATE, response)]).await;
        assert_eq!(result.unwrap_err(), expected);
        assert_eq!(seen.len(), 1, "{expected:?} must not trigger a follow-up");
    }
}

#[tokio::test]
async fn forbidden_checks_suspension() {
    let ip = "Although you appear to have the correct authorization credentials, the `acme` organization has an IP allow list enabled, and your IP address is not permitted to access this resource.";
    let forbidden = |body: serde_json::Value| ResponseTemplate::new(403).set_body_json(body);
    let cases: Vec<(ResponseTemplate, ResponseTemplate, Error)> = vec![
        (
            forbidden(serde_json::json!({"message": "This installation has been suspended"})),
            installed(installation_body()),
            Error::InstallationSuspended,
        ),
        (
            forbidden(serde_json::json!({"message": "Resource not accessible by integration"})),
            installed(active_body()),
            Error::Forbidden {
                reason: ForbiddenReason::Other,
            },
        ),
        (
            // Suspended, but about another installation: not trusted.
            forbidden(serde_json::json!({"message": "This installation has been suspended"})),
            installed({
                let mut body = installation_body();
                body["id"] = 43.into();
                body
            }),
            Error::Forbidden {
                reason: ForbiddenReason::Other,
            },
        ),
        (
            ResponseTemplate::new(403).insert_header("x-github-sso", "required; url=https://x"),
            ResponseTemplate::new(500),
            Error::Forbidden {
                reason: ForbiddenReason::SsoRequired,
            },
        ),
        (
            forbidden(serde_json::json!({ "message": ip })),
            ResponseTemplate::new(404),
            Error::Forbidden {
                reason: ForbiddenReason::IpAllowList,
            },
        ),
    ];
    for (create, get, expected) in cases {
        let (result, seen) =
            create_against(vec![("POST", CREATE, create), ("GET", GET, get)]).await;
        assert_eq!(result.unwrap_err(), expected);
        assert_eq!(seen.len(), 2);
        assert_eq!(seen[1].method.as_str(), "GET");
        // Authenticated as the app (a freshly signed credential).
        assert!(bearer(&seen[1]).starts_with("eyJ"));
    }
}

#[tokio::test]
async fn malformed_success_bodies_are_decode_errors() {
    let long = format!("ghs_{}", "a".repeat(9000));
    let bodies = [
        serde_json::json!("not an object"),
        serde_json::json!({"token": "ghs_x"}),
        serde_json::json!({"expires_at": "2027-01-15T09:00:00Z"}),
        serde_json::json!({"token": "ghs_x", "expires_at": "tomorrow"}),
        serde_json::json!({"token": "", "expires_at": "2027-01-15T09:00:00Z"}),
        serde_json::json!({"token": "ghs_a b", "expires_at": "2027-01-15T09:00:00Z"}),
        serde_json::json!({"token": "ghs_a\nb", "expires_at": "2027-01-15T09:00:00Z"}),
        serde_json::json!({"token": long, "expires_at": "2027-01-15T09:00:00Z"}),
    ];
    for body in bodies {
        let (result, _) = create_against(vec![(
            "POST",
            CREATE,
            ResponseTemplate::new(201).set_body_json(&body),
        )])
        .await;
        assert_eq!(result.unwrap_err(), Error::Decode, "{body:.60}");
    }
}

#[tokio::test]
async fn redirects() {
    // The default client does not follow.
    let (result, seen) = create_against(vec![(
        "POST",
        CREATE,
        ResponseTemplate::new(307).insert_header("location", "/elsewhere"),
    )])
    .await;
    assert_eq!(result.unwrap_err(), Error::Unexpected { status: 307 });
    assert_eq!(seen.len(), 1);

    // A custom client that follows is caught, and its response discarded.
    let server = serve(vec![
        (
            "POST",
            CREATE,
            ResponseTemplate::new(307).insert_header("location", "/elsewhere"),
        ),
        ("POST", "/elsewhere", created("ghs_planted")),
    ])
    .await;
    let result = builder_for(&server.uri())
        .client(reqwest::Client::new())
        .user_agent("ua")
        .build()
        .unwrap()
        .installation(42)
        .access_token(&permissions())
        .await;
    assert_eq!(result.unwrap_err(), Error::Redirected);
    assert_eq!(server.received_requests().await.unwrap().len(), 2);
}

#[tokio::test]
async fn transport_failures() {
    // Refused connection: provably not sent.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let closed = format!("http://{}", listener.local_addr().unwrap());
    drop(listener);
    assert_eq!(
        app_for(&closed)
            .installation(42)
            .access_token(&permissions())
            .await
            .unwrap_err(),
        Error::Transient {
            kind: TransientKind::NotSent
        }
    );

    // Accepted, read, then hung up: may have been acted on.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        use tokio::io::AsyncReadExt;
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buffer = [0; 4096];
        let _ = socket.read(&mut buffer).await.unwrap();
    });
    assert_eq!(
        app_for(&base)
            .installation(42)
            .access_token(&permissions())
            .await
            .unwrap_err(),
        Error::Transient {
            kind: TransientKind::NoResponse
        }
    );
    server.await.unwrap();
}

#[tokio::test]
async fn is_suspended() {
    let mut unknown = installation_body();
    unknown.as_object_mut().unwrap().remove("suspended_at");
    let cases: Vec<(ResponseTemplate, Result<bool, Error>)> = vec![
        (installed(installation_body()), Ok(true)),
        (installed(active_body()), Ok(false)),
        (installed(unknown), Err(Error::Decode)),
        (ResponseTemplate::new(404), Err(Error::InstallationGone)),
        (
            ResponseTemplate::new(401),
            Err(Error::AppCredentialsRejected { clock_skew: None }),
        ),
    ];
    for (response, expected) in cases {
        let server = serve(vec![("GET", GET, response)]).await;
        let app = app_for(&server.uri());
        assert_eq!(app.installation(42).is_suspended().await, expected);
        let seen = server.received_requests().await.unwrap();
        assert_eq!(seen.len(), 1);
        assert!(bearer(&seen[0]).starts_with("eyJ"));
    }
}

/// A clock tests can move.
#[derive(Clone)]
struct Manual(Arc<AtomicI64>);

impl Manual {
    fn at(seconds: i64) -> Self {
        Self(Arc::new(AtomicI64::new(seconds)))
    }

    fn advance(&self, seconds: i64) {
        self.0.fetch_add(seconds, SeqCst);
    }
}

impl crate::app::Clock for Manual {
    fn now(&self) -> i64 {
        self.0.load(SeqCst)
    }
}

/// Counts signatures, delegating to the test key.
struct Counting(SigningKey, Arc<AtomicUsize>);

impl AppKeySigner for Counting {
    fn sign_rs256(&self, input: &[u8]) -> Result<Vec<u8>, SigningError> {
        self.1.fetch_add(1, SeqCst);
        self.0.sign_rs256(input)
    }
}

#[tokio::test]
async fn credential_is_reused_for_five_minutes() {
    let server = serve(vec![("POST", CREATE, created("ghs_x"))]).await;
    let clock = Manual::at(NOW);
    let signatures = Arc::new(AtomicUsize::new(0));
    let app = App::builder(
        7,
        Counting(SigningKey::from_pem(PEM).unwrap(), signatures.clone()),
    )
    .base_url(server.uri())
    .clock(Arc::new(clock.clone()))
    .build()
    .unwrap();
    let create = || async {
        // A clone shares the cache.
        app.clone()
            .installation(42)
            .access_token(&permissions())
            .await
            .unwrap();
    };
    create().await;
    clock.advance(299);
    create().await;
    clock.advance(1);
    create().await;
    clock.advance(-10);
    create().await;

    let seen = server.received_requests().await.unwrap();
    let credentials: Vec<_> = seen.iter().map(bearer).collect();
    assert_eq!(credentials[0], credentials[1]);
    assert_ne!(credentials[1], credentials[2], "re-signed after 300 s");
    assert_ne!(
        credentials[2], credentials[3],
        "re-signed when time moves back"
    );
    assert_eq!(signatures.load(SeqCst), 3);
}

/// A GitHub stand-in that validates the app credential's `iat`/`exp` against
/// its own clock, with the rules observed on api.github.com: `iat` at most
/// 60 s ahead, `exp` at most 60 s past and at most 600 s ahead. Every response
/// carries `Date` from that clock.
struct GitHub {
    /// GitHub's clock: our test clock plus `offset`.
    local: Manual,
    offset: AtomicI64,
    /// Seconds our test clock advances while the next request is in flight.
    delay_once: AtomicI64,
    accepted: ResponseTemplate,
}

impl GitHub {
    fn now(&self) -> i64 {
        crate::app::Clock::now(&self.local) + self.offset.load(SeqCst)
    }
}

impl wiremock::Respond for GitHub {
    fn respond(&self, request: &wiremock::Request) -> ResponseTemplate {
        self.local.advance(self.delay_once.swap(0, SeqCst));
        let now = self.now();
        let claims = jwt_claims(bearer(request));
        let (iat, exp) = (
            claims["iat"].as_i64().unwrap(),
            claims["exp"].as_i64().unwrap(),
        );
        let response = if iat > now + 60 || exp <= now - 60 || exp > now + 600 {
            ResponseTemplate::new(401).set_body_json(serde_json::json!({"message": "clock"}))
        } else {
            self.accepted.clone()
        };
        response.insert_header("date", http_date(now).as_str())
    }
}

/// An app whose local clock is `NOW`, against a GitHub stand-in `offset`
/// seconds ahead, creating tokens with `accepted` for valid credentials.
async fn skewed(offset: i64, accepted: ResponseTemplate) -> (App, Arc<GitHub>, MockServer, Manual) {
    skewed_at("POST", CREATE, offset, accepted).await
}

/// [`skewed`], serving `verb route` instead.
async fn skewed_at(
    verb: &str,
    route: &str,
    offset: i64,
    accepted: ResponseTemplate,
) -> (App, Arc<GitHub>, MockServer, Manual) {
    let clock = Manual::at(NOW);
    let github = Arc::new(GitHub {
        local: clock.clone(),
        offset: AtomicI64::new(offset),
        delay_once: AtomicI64::new(0),
        accepted,
    });
    let server = MockServer::start().await;
    Mock::given(method(verb))
        .and(path(route))
        .respond_with(SharedGitHub(github.clone()))
        .mount(&server)
        .await;
    let app = App::builder(7, SigningKey::from_pem(PEM).unwrap())
        .base_url(server.uri())
        .clock(Arc::new(clock.clone()))
        .build()
        .unwrap();
    (app, github, server, clock)
}

struct SharedGitHub(Arc<GitHub>);

impl wiremock::Respond for SharedGitHub {
    fn respond(&self, request: &wiremock::Request) -> ResponseTemplate {
        self.0.respond(request)
    }
}

async fn create(app: &App) -> Result<InstallationToken, Error> {
    app.installation(42).access_token(&permissions()).await
}

async fn requests(server: &MockServer) -> usize {
    server.received_requests().await.unwrap().len()
}

#[tokio::test]
async fn skew_github_tolerates_needs_no_retry() {
    // Our credential (iat now-60, exp now+540) is valid for GitHub from 60 s
    // behind to 540 s ahead; within that, no correction is needed.
    for offset in [-60, -30, 0, 300, 539] {
        let (app, _, server, _) = skewed(offset, created("ghs_x")).await;
        create(&app).await.unwrap();
        assert_eq!(requests(&server).await, 1, "offset {offset}");
    }
}

#[tokio::test]
async fn github_ahead_is_corrected_by_one_retry() {
    let (app, _, server, _) = skewed(660, created("ghs_x")).await;
    create(&app).await.unwrap();
    assert_eq!(requests(&server).await, 2);
    let seen = server.received_requests().await.unwrap();
    assert_eq!(jwt_claims(bearer(&seen[1]))["iat"], NOW + 660 - 60);

    // The corrected credential is reused.
    create(&app).await.unwrap();
    assert_eq!(requests(&server).await, 3);
}

#[tokio::test]
async fn github_behind_is_corrected_by_one_retry() {
    let (app, _, server, clock) = skewed(-300, created("ghs_x")).await;
    create(&app).await.unwrap();
    assert_eq!(requests(&server).await, 2);

    // A later credential keeps the correction: no new rejection.
    clock.advance(301);
    create(&app).await.unwrap();
    assert_eq!(requests(&server).await, 3);
}

#[tokio::test]
async fn up_to_an_hour_is_corrected() {
    for offset in [3600, 4000, -3600] {
        let (app, _, server, _) = skewed(offset, created("ghs_x")).await;
        create(&app).await.unwrap();
        assert_eq!(requests(&server).await, 2, "offset {offset}");
    }
}

#[tokio::test]
async fn uncorrectable_skew_is_reported_without_retrying() {
    // Two hours: a credential with the maximum correction would still be
    // rejected, so retrying cannot help.
    let (app, _, server, _) = skewed(7200, created("ghs_x")).await;
    for expected_requests in [1, 2] {
        assert_eq!(
            create(&app).await.unwrap_err(),
            Error::AppCredentialsRejected {
                clock_skew: Some(7200)
            }
        );
        assert_eq!(requests(&server).await, expected_requests);
    }
}

#[tokio::test]
async fn fixing_the_host_clock_heals_after_one_retry() {
    // Learn a correction for GitHub 660 s ahead…
    let (app, github, server, clock) = skewed(660, created("ghs_x")).await;
    create(&app).await.unwrap();
    assert_eq!(requests(&server).await, 2);

    // …then NTP fixes our clock: it jumps to GitHub's time.
    clock.advance(660);
    github.offset.store(0, SeqCst);
    create(&app).await.unwrap();
    assert_eq!(requests(&server).await, 4, "one rejection, one retry");
    let seen = server.received_requests().await.unwrap();
    assert_eq!(jwt_claims(bearer(&seen[3]))["iat"], NOW + 660 - 60);

    create(&app).await.unwrap();
    assert_eq!(requests(&server).await, 5, "healed");
}

#[tokio::test]
async fn bogus_correction_heals_after_one_retry() {
    // A proxy or GHES node reports GitHub 3000 s ahead once…
    let (app, github, server, _) = skewed(3000, created("ghs_x")).await;
    create(&app).await.unwrap();

    // …but the real GitHub agrees with our clock.
    github.offset.store(0, SeqCst);
    create(&app).await.unwrap();
    create(&app).await.unwrap();
    assert_eq!(requests(&server).await, 2 + 2 + 1);
}

#[tokio::test]
async fn expiry_in_flight_is_retried_with_a_fresh_credential() {
    let (app, github, server, clock) = skewed(0, created("ghs_x")).await;
    create(&app).await.unwrap();
    // Reuse the cached credential at age 299, then a 10-minute stall in
    // flight (e.g. retry middleware): GitHub sees it expired.
    clock.advance(299);
    github.delay_once.store(600, SeqCst);
    create(&app).await.unwrap();
    assert_eq!(requests(&server).await, 3);
}

#[tokio::test]
async fn unrelated_rejection_is_not_retried() {
    // A 401 for a valid-looking credential (e.g. wrong key) at a skew GitHub
    // tolerates: reported, not retried.
    let server = serve(vec![("POST", CREATE, ResponseTemplate::new(401))]).await;
    let mut app = app_for(&server.uri());
    assert_eq!(
        create(&app).await.unwrap_err(),
        Error::AppCredentialsRejected { clock_skew: None }
    );
    assert_eq!(requests(&server).await, 1);

    // Same, with GitHub 300 s ahead: reported as skew, still not retried.
    let (skewed_app, github, server, _) = skewed(300, ResponseTemplate::new(401)).await;
    let _ = github;
    app = skewed_app;
    assert_eq!(
        create(&app).await.unwrap_err(),
        Error::AppCredentialsRejected {
            clock_skew: Some(300)
        }
    );
    assert_eq!(requests(&server).await, 1);
}

#[tokio::test]
async fn unparseable_date_is_not_retried() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(CREATE))
        .respond_with(ResponseTemplate::new(401).insert_header("date", "yesterday"))
        .mount(&server)
        .await;
    assert_eq!(
        create(&app_for(&server.uri())).await.unwrap_err(),
        Error::AppCredentialsRejected { clock_skew: None }
    );
    assert_eq!(requests(&server).await, 1);
}

#[tokio::test]
async fn retry_outcomes_are_classified_normally() {
    for (accepted, expected) in [
        (
            ResponseTemplate::new(503),
            Error::Transient {
                kind: TransientKind::Server { status: 503 },
            },
        ),
        (ResponseTemplate::new(404), Error::InstallationGone),
    ] {
        let (app, _, server, _) = skewed(660, accepted).await;
        assert_eq!(create(&app).await.unwrap_err(), expected);
        assert_eq!(requests(&server).await, 2);
    }
}

#[tokio::test]
async fn concurrent_rejections_sign_once() {
    let clock = Manual::at(NOW);
    let github = Arc::new(GitHub {
        local: clock.clone(),
        offset: AtomicI64::new(660),
        delay_once: AtomicI64::new(0),
        accepted: created("ghs_x"),
    });
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(CREATE))
        .respond_with(SharedGitHub(github))
        .mount(&server)
        .await;
    let signatures = Arc::new(AtomicUsize::new(0));
    let app = App::builder(
        7,
        Counting(SigningKey::from_pem(PEM).unwrap(), signatures.clone()),
    )
    .base_url(server.uri())
    .clock(Arc::new(clock))
    .build()
    .unwrap();
    let results = futures_util::future::join_all((0..8).map(|_| create(&app))).await;
    assert!(results.iter().all(Result::is_ok));
    assert_eq!(requests(&server).await, 16);
    assert_eq!(signatures.load(SeqCst), 2, "one rejected, one corrected");
}

#[tokio::test]
async fn issued_at_needs_a_plausible_date() {
    for date in ["yesterday", "Fri, 01 Jan 2100 00:00:00 GMT"] {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path(CREATE))
            .respond_with(created("ghs_x").insert_header("date", date))
            .mount(&server)
            .await;
        let token = create(&app_for(&server.uri())).await.unwrap();
        assert_eq!(token.issued_at(), None, "{date}");
    }
}

#[tokio::test]
async fn jwt_failure_sends_nothing() {
    let server = MockServer::start().await;
    let app = builder_for(&server.uri())
        .clock(Arc::new(Fixed(0)))
        .build()
        .unwrap();
    let installation = app.installation(42);
    assert_eq!(
        installation.access_token(&permissions()).await.unwrap_err(),
        Error::Signing(SigningError::InvalidTime)
    );
    assert_eq!(
        installation.is_suspended().await,
        Err(Error::Signing(SigningError::InvalidTime))
    );
    assert_eq!(
        installation.get().await,
        Err(Error::Signing(SigningError::InvalidTime))
    );
    assert_eq!(
        app.credential().unwrap_err(),
        Error::Signing(SigningError::InvalidTime)
    );
    assert!(server.received_requests().await.unwrap().is_empty());
}

/// No token, JWT or upstream text may appear in any `Debug`/`Display` output.
#[tokio::test]
async fn credentials_never_leak_into_diagnostics() {
    let marker = format!("ghs_MARKER{}", "x".repeat(630));
    let mut rendered = Vec::new();

    let (token, seen) = create_against(vec![("POST", CREATE, created(&marker))]).await;
    let token = token.unwrap();
    let jwt = bearer(&seen[0]).to_owned();
    rendered.push(format!("{token:?}"));
    // The documented way to use the token keeps it out of reqwest's `Debug`.
    let request = reqwest::Client::new()
        .get("https://api.github.com/x")
        .bearer_auth(token.expose_secret())
        .build()
        .unwrap();
    rendered.push(format!("{request:?}"));

    let app = app_for("https://api.github.com");
    rendered.push(format!("{app:?} {:?}", app.installation(42)));
    rendered.push(format!("{:?}", app.credential().unwrap()));

    let body = serde_json::json!({ "message": "UPSTREAM_MARKER", "token": marker });
    for status in [201, 401, 403, 404, 422, 429, 500, 302] {
        let (result, _) = create_against(vec![
            (
                "POST",
                CREATE,
                ResponseTemplate::new(status).set_body_json(&body),
            ),
            ("GET", GET, ResponseTemplate::new(200).set_body_json(&body)),
        ])
        .await;
        let error = result.unwrap_err();
        rendered.push(format!("{error} {error:?}"));
    }

    for text in rendered {
        assert!(!text.contains("MARKER"), "leaked: {text:.80}");
        assert!(!text.contains(&jwt), "leaked JWT: {text:.80}");
        assert!(!text.contains("eyJ"), "leaked JWT: {text:.80}");
    }
}

/// A gateway in front of GitHub that reflects the request it proxied: the app
/// JWT we sent comes back in the body. Classification still works, and no
/// rendering of the error carries the credential or the body's text.
#[tokio::test]
async fn reflected_credentials_never_leak() {
    /// A body built from the reflected `Authorization` header.
    type Reflected = fn(&str) -> String;
    struct Reflect {
        status: u16,
        body: Reflected,
    }
    impl wiremock::Respond for Reflect {
        fn respond(&self, request: &wiremock::Request) -> ResponseTemplate {
            let authorization = request.headers["authorization"].to_str().unwrap();
            ResponseTemplate::new(self.status)
                .insert_header("date", http_date(NOW).as_str())
                .set_body_raw((self.body)(authorization), "application/json")
        }
    }
    let cases: [(u16, Reflected, Error); 5] = [
        (
            502,
            |auth| {
                format!(
                    "<html>502 Bad Gateway<pre>upstream request: Authorization: {auth}</pre></html>"
                )
            },
            Error::Transient {
                kind: TransientKind::Server { status: 502 },
            },
        ),
        (
            429,
            |auth| {
                serde_json::json!({"message": "You have exceeded a secondary rate limit", "seen": auth})
                    .to_string()
            },
            Error::RateLimited {
                scope: RateLimitScope::Secondary,
                retry_after: None,
            },
        ),
        (
            403,
            |auth| serde_json::json!({"message": format!("Denied for {auth}")}).to_string(),
            Error::Forbidden {
                reason: ForbiddenReason::Other,
            },
        ),
        (
            422,
            |auth| {
                serde_json::json!({"message": auth, "errors": [{"code": "invalid", "message": auth}]})
                    .to_string()
            },
            Error::PermissionsNotGranted,
        ),
        // Valid JSON with the wrong shape: a decoder that quoted the value it
        // rejected would quote the credential.
        (
            201,
            |auth| serde_json::json!({"token": auth, "expires_at": 4_102_444_800_u64}).to_string(),
            Error::Decode,
        ),
    ];
    for (status, body, expected) in cases {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path(CREATE))
            .respond_with(Reflect { status, body })
            .mount(&server)
            .await;
        // The 403's suspension check finds nothing.
        Mock::given(method("GET"))
            .and(path(GET))
            .respond_with(installed(active_body()).insert_header("date", http_date(NOW).as_str()))
            .mount(&server)
            .await;
        let error = app_for(&server.uri())
            .installation(42)
            .access_token(&permissions())
            .await
            .unwrap_err();
        let seen = server.received_requests().await.unwrap();
        let jwt = bearer(&seen[0]);
        let rendered = format!("{error} {error:?}");
        assert!(!rendered.contains(jwt), "leaked JWT: {rendered}");
        assert!(!rendered.contains("eyJ"), "leaked JWT: {rendered}");
        assert!(!rendered.contains("Bearer"), "leaked header: {rendered}");
        assert!(!rendered.contains("rate limit"), "leaked text: {rendered}");
        assert_eq!(error, expected, "{status}");
    }
}

async fn get_against(
    response: ResponseTemplate,
) -> (Result<InstallationInfo, Error>, Vec<wiremock::Request>) {
    let server = serve(vec![("GET", GET, response)]).await;
    let result = app_for(&server.uri()).installation(42).get().await;
    (result, server.received_requests().await.unwrap())
}

#[tokio::test]
async fn get_reads_the_typed_fields() {
    let (info, seen) = get_against(installed(installation_body())).await;
    let info = info.unwrap();
    assert_eq!(info.id, InstallationId::new(42));
    assert_eq!(info.app_id, AppId::new(7));
    assert_eq!(
        info.account,
        InstallationAccount {
            id: 1001,
            login: "acme".into(),
            kind: AccountKind::Organization,
        }
    );
    assert_eq!(info.permission("members"), Some("write"));
    assert_eq!(info.permission("metadata"), Some("read"));
    assert_eq!(
        info.permission("custom_properties_for_organizations"),
        Some("admin"),
        "unknown permissions are kept"
    );
    assert_eq!(info.permission("administration"), None);
    assert_eq!(info.permissions.len(), 3);
    assert_eq!(info.repository_selection, RepositorySelection::Selected);
    assert_eq!(
        info.suspended_at,
        Some(std::time::UNIX_EPOCH + Duration::from_secs(SUSPENDED_AT))
    );

    let [request] = seen.as_slice() else {
        panic!("one request")
    };
    assert_eq!(request.method.as_str(), "GET");
    assert!(request.body.is_empty());
    assert!(!request.headers.contains_key("content-type"));
    assert_eq!(header(request, "accept"), "application/vnd.github+json");
    assert_eq!(header(request, "x-github-api-version"), "2022-11-28");
    assert_eq!(jwt_claims(bearer(request))["iss"], "7");
}

#[tokio::test]
async fn get_reads_an_active_installation() {
    let mut body = active_body();
    body["repository_selection"] = "all".into();
    body["permissions"] = serde_json::json!({});
    let info = get_against(installed(body)).await.0.unwrap();
    assert_eq!(info.suspended_at, None);
    assert_eq!(info.repository_selection, RepositorySelection::All);
    assert!(info.permissions.is_empty());

    // Fractional seconds are accepted and dropped.
    let mut body = installation_body();
    body["suspended_at"] = "2026-10-01T00:00:00.750Z".into();
    let info = get_against(installed(body)).await.0.unwrap();
    assert_eq!(
        info.suspended_at,
        Some(std::time::UNIX_EPOCH + Duration::from_secs(SUSPENDED_AT))
    );
}

#[tokio::test]
async fn account_kinds_and_repository_selections() {
    let enterprise = serde_json::json!({
        "id": 9,
        "slug": "acme-corp",
        "name": "Acme Corp",
        "node_id": "E_kgDNAAk",
        "html_url": "https://github.com/enterprises/acme-corp",
        "website_url": null,
        "created_at": "2020-01-01T00:00:00Z"
    });
    let cases = [
        (
            serde_json::json!({"login": "octocat", "id": 1, "type": "User"}),
            "User",
            "octocat",
            AccountKind::User,
        ),
        (
            serde_json::json!({"login": "acme", "id": 2, "type": "Organization"}),
            "Organization",
            "acme",
            AccountKind::Organization,
        ),
        (
            serde_json::json!({"login": "robot", "id": 3, "type": "Bot"}),
            "Bot",
            "robot",
            AccountKind::Unknown,
        ),
        (
            serde_json::json!({"login": "x", "id": 4}),
            "Team",
            "x",
            AccountKind::Unknown,
        ),
        // Enterprise accounts have a slug and no `type`.
        (
            enterprise,
            "Enterprise",
            "acme-corp",
            AccountKind::Enterprise,
        ),
        (
            serde_json::json!({"login": "", "slug": "fallback", "id": 5, "type": "Enterprise"}),
            "Enterprise",
            "fallback",
            AccountKind::Enterprise,
        ),
    ];
    for (account, target_type, login, kind) in cases {
        let mut body = installation_body();
        body["account"] = account;
        body["target_type"] = target_type.into();
        let info = get_against(installed(body)).await.0.unwrap();
        assert_eq!(info.account.login, login);
        assert_eq!(info.account.kind, kind, "{login}");
    }

    let mut body = installation_body();
    body.as_object_mut().unwrap().remove("target_type");
    body["account"]["type"] = serde_json::Value::Null;
    let info = get_against(installed(body)).await.0.unwrap();
    assert_eq!(info.account.kind, AccountKind::Unknown);

    let mut body = installation_body();
    body["repository_selection"] = "subset".into();
    let info = get_against(installed(body)).await.0.unwrap();
    assert_eq!(info.repository_selection, RepositorySelection::Unknown);
}

#[tokio::test]
async fn answers_about_another_installation_are_decode_errors() {
    for (field, value) in [("id", 43), ("app_id", 8)] {
        let mut body = installation_body();
        body[field] = value.into();
        let (result, seen) = get_against(installed(body)).await;
        assert_eq!(result.unwrap_err(), Error::Decode, "{field}");
        assert_eq!(seen.len(), 1);
    }
}

#[tokio::test]
async fn malformed_installation_bodies_are_decode_errors() {
    type Edit = fn(&mut serde_json::Value);
    let edits: [(&str, Edit); 17] = [
        ("no id", |b| {
            b.as_object_mut().unwrap().remove("id");
        }),
        ("string id", |b| b["id"] = "42".into()),
        ("no app_id", |b| {
            b.as_object_mut().unwrap().remove("app_id");
        }),
        ("no account", |b| {
            b.as_object_mut().unwrap().remove("account");
        }),
        ("null account", |b| b["account"] = serde_json::Value::Null),
        ("account id 0", |b| b["account"]["id"] = 0.into()),
        ("negative account id", |b| b["account"]["id"] = (-1).into()),
        ("no account id", |b| {
            b["account"].as_object_mut().unwrap().remove("id");
        }),
        ("no login or slug", |b| {
            b["account"].as_object_mut().unwrap().remove("login");
        }),
        ("empty login", |b| b["account"]["login"] = "".into()),
        ("no permissions", |b| {
            b.as_object_mut().unwrap().remove("permissions");
        }),
        ("numeric access", |b| b["permissions"]["members"] = 2.into()),
        ("no repository_selection", |b| {
            b.as_object_mut().unwrap().remove("repository_selection");
        }),
        ("no suspended_at", |b| {
            b.as_object_mut().unwrap().remove("suspended_at");
        }),
        ("bad suspended_at", |b| {
            b["suspended_at"] = "yesterday".into()
        }),
        ("numeric suspended_at", |b| {
            b["suspended_at"] = 1_790_812_800.into()
        }),
        ("suspended before 1970", |b| {
            b["suspended_at"] = "1969-12-31T00:00:00Z".into()
        }),
    ];
    for (what, edit) in edits {
        let mut body = installation_body();
        edit(&mut body);
        let (result, _) = get_against(installed(body)).await;
        assert_eq!(result.unwrap_err(), Error::Decode, "{what}");
    }
    for response in [
        ResponseTemplate::new(200).set_body_raw("<html>OK</html>", "text/html"),
        ResponseTemplate::new(200),
        installed(serde_json::json!([installation_body()])),
        ResponseTemplate::new(200).set_body_bytes(vec![b' '; 10 * 1024 * 1024 + 1]),
    ] {
        assert_eq!(get_against(response).await.0.unwrap_err(), Error::Decode);
    }
}

#[tokio::test]
async fn get_statuses_are_classified() {
    let cases: Vec<(ResponseTemplate, Error)> = vec![
        (ResponseTemplate::new(404), Error::InstallationGone),
        (
            ResponseTemplate::new(403).set_body_json(
                serde_json::json!({"message": "Resource not accessible by integration"}),
            ),
            Error::Forbidden {
                reason: ForbiddenReason::Other,
            },
        ),
        (
            ResponseTemplate::new(403).insert_header("x-github-sso", "required; url=https://x"),
            Error::Forbidden {
                reason: ForbiddenReason::SsoRequired,
            },
        ),
        (
            ResponseTemplate::new(503),
            Error::Transient {
                kind: TransientKind::Server { status: 503 },
            },
        ),
        (
            ResponseTemplate::new(429).insert_header("retry-after", "7"),
            Error::RateLimited {
                scope: RateLimitScope::Secondary,
                retry_after: Some(Duration::from_secs(7)),
            },
        ),
        (
            ResponseTemplate::new(403).insert_header("x-ratelimit-remaining", "0"),
            Error::RateLimited {
                scope: RateLimitScope::Primary,
                retry_after: None,
            },
        ),
        (
            ResponseTemplate::new(401),
            Error::AppCredentialsRejected { clock_skew: None },
        ),
        (
            ResponseTemplate::new(201).set_body_json(installation_body()),
            Error::Unexpected { status: 201 },
        ),
        (
            ResponseTemplate::new(302).insert_header("location", "/elsewhere"),
            Error::Unexpected { status: 302 },
        ),
    ];
    for (response, expected) in cases {
        let (result, seen) = get_against(response).await;
        assert_eq!(result.unwrap_err(), expected);
        assert_eq!(seen.len(), 1, "{expected:?} must not trigger a follow-up");
    }
}

#[tokio::test]
async fn get_corrects_clock_skew_and_the_credential_follows() {
    let (app, _, server, _) = skewed_at("GET", GET, 660, installed(installation_body())).await;
    let before = app.credential().unwrap();
    assert_eq!(app.installation(42).get().await.unwrap().id.get(), 42);
    let seen = server.received_requests().await.unwrap();
    assert_eq!(seen.len(), 2, "one rejection, one retry");
    assert_eq!(bearer(&seen[0]), before.expose_secret());

    // The documented recovery: after `get`, the app lends the corrected
    // credential.
    let after = app.credential().unwrap();
    assert_eq!(after.expose_secret(), bearer(&seen[1]));
    assert_eq!(
        after.expires_at(),
        std::time::UNIX_EPOCH + Duration::from_secs((NOW + 660 + 540) as u64)
    );
}

#[tokio::test]
async fn app_credential_is_the_one_octoauth_sends() {
    let server = serve(vec![("GET", GET, installed(installation_body()))]).await;
    let clock = Manual::at(NOW);
    let app = builder_for(&server.uri())
        .clock(Arc::new(clock.clone()))
        .build()
        .unwrap();
    let credential = app.credential().unwrap();
    assert_eq!(credential.app_id(), AppId::new(7));
    assert_eq!(
        credential.expires_at(),
        std::time::UNIX_EPOCH + Duration::from_secs((NOW + 540) as u64)
    );
    let debug = format!("{credential:?}");
    assert_eq!(debug, "AppCredential { app_id: AppId(7), .. }");
    assert!(!debug.contains("eyJ"));

    app.installation(42).get().await.unwrap();
    let seen = server.received_requests().await.unwrap();
    assert_eq!(bearer(&seen[0]), credential.expose_secret());

    // Reused for 5 minutes, by clones too; then re-signed.
    clock.advance(299);
    assert_eq!(
        app.clone().credential().unwrap().expose_secret(),
        credential.expose_secret()
    );
    clock.advance(1);
    assert_ne!(
        app.credential().unwrap().expose_secret(),
        credential.expose_secret()
    );
}

#[test]
fn installation_is_owned() {
    fn assert_static<T: Send + Sync + 'static>(_: &T) {}
    let installation = App::new(7, SigningKey::from_pem(PEM).unwrap()).installation(42);
    assert_static(&installation);
    assert_eq!(installation.app().id(), AppId::new(7));
}

/// Feature `middleware`: creating tokens through a `reqwest_middleware` stack.
#[cfg(feature = "middleware")]
mod middleware {
    use super::*;
    use reqwest_middleware::{ClientBuilder, Middleware, Next};
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Counts requests and tags them, like a tracing or metrics middleware would.
    #[derive(Clone, Default)]
    struct Tag(Arc<AtomicUsize>);

    #[async_trait::async_trait]
    impl Middleware for Tag {
        async fn handle(
            &self,
            mut req: reqwest::Request,
            extensions: &mut http::Extensions,
            next: Next<'_>,
        ) -> reqwest_middleware::Result<reqwest::Response> {
            self.0.fetch_add(1, Ordering::SeqCst);
            req.headers_mut()
                .insert("x-middleware", "tagged".parse().unwrap());
            next.run(req, extensions).await
        }
    }

    /// Fails before dispatch, like a circuit breaker.
    struct Refuse;

    #[async_trait::async_trait]
    impl Middleware for Refuse {
        async fn handle(
            &self,
            _: reqwest::Request,
            _: &mut http::Extensions,
            _: Next<'_>,
        ) -> reqwest_middleware::Result<reqwest::Response> {
            Err(reqwest_middleware::Error::middleware(
                std::io::Error::other("circuit open"),
            ))
        }
    }

    fn client() -> reqwest::Client {
        reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .user_agent("middleware-test")
            .build()
            .unwrap()
    }

    #[tokio::test]
    async fn creates_through_middleware() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/app/installations/42/access_tokens"))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "token": "ghs_x", "expires_at": "2027-01-15T09:00:00Z"
            })))
            .mount(&server)
            .await;
        let tag = Tag::default();
        let app = App::builder(7, SigningKey::from_pem(PEM).unwrap())
            .base_url(server.uri())
            .middleware_client(ClientBuilder::new(client()).with(tag.clone()).build())
            .build()
            .unwrap();

        let token = app
            .installation(42)
            .access_token(&permissions())
            .await
            .unwrap();
        assert_eq!(token.expose_secret(), "ghs_x");
        assert_eq!(tag.0.load(Ordering::SeqCst), 1);
        let seen = server.received_requests().await.unwrap();
        assert_eq!(seen[0].headers["x-middleware"], "tagged");
        assert_eq!(seen[0].headers["user-agent"], "middleware-test");
    }

    #[tokio::test]
    async fn middleware_errors_are_ambiguous() {
        let app = App::builder(7, SigningKey::from_pem(PEM).unwrap())
            .middleware_client(ClientBuilder::new(client()).with(Refuse).build())
            .build()
            .unwrap();
        assert_eq!(
            app.installation(42)
                .access_token(&permissions())
                .await
                .unwrap_err(),
            Error::Transient {
                kind: TransientKind::NoResponse
            }
        );
    }
}

/// The `tracing` feature's events: names and fields, never a secret.
#[cfg(feature = "tracing")]
mod events {
    use super::*;
    use std::fmt::Write as _;
    use tracing::span::{Attributes, Id, Record};
    use tracing::{Event, Metadata, Subscriber, field};

    thread_local! {
        /// The events of this thread, while a test captures them.
        static LINES: std::cell::RefCell<Option<Vec<String>>> = const { std::cell::RefCell::new(None) };
    }

    /// Records octoauth's events as `LEVEL field=value…` into [`LINES`].
    /// Global, because callsite interest is cached process-wide and other
    /// tests run in parallel.
    struct Capture;

    impl Subscriber for Capture {
        fn enabled(&self, metadata: &Metadata<'_>) -> bool {
            metadata.target() == "octoauth"
        }
        fn new_span(&self, _: &Attributes<'_>) -> Id {
            Id::from_u64(1)
        }
        fn record(&self, _: &Id, _: &Record<'_>) {}
        fn record_follows_from(&self, _: &Id, _: &Id) {}
        fn event(&self, event: &Event<'_>) {
            struct Line(String);
            impl field::Visit for Line {
                fn record_debug(&mut self, field: &field::Field, value: &dyn std::fmt::Debug) {
                    write!(self.0, " {}={value:?}", field.name()).unwrap();
                }
            }
            let mut line = Line(event.metadata().level().to_string());
            event.record(&mut line);
            LINES.with_borrow_mut(|lines| lines.as_mut().map(|lines| lines.push(line.0)));
        }
        fn enter(&self, _: &Id) {}
        fn exit(&self, _: &Id) {}
    }

    #[tokio::test]
    async fn token_creation_and_clock_correction_are_reported() {
        static INSTALL: std::sync::Once = std::sync::Once::new();
        INSTALL.call_once(|| tracing::subscriber::set_global_default(Capture).unwrap());
        LINES.set(Some(Vec::new()));

        let (app, _, _server, _) = skewed(660, created("ghs_secret")).await;
        let token = create(&app).await.unwrap();
        let server = serve(vec![("POST", CREATE, ResponseTemplate::new(404))]).await;
        create(&app_for(&server.uri())).await.unwrap_err();

        let lines = LINES.take().unwrap();
        assert_eq!(
            lines,
            [
                r#"DEBUG event="app_credential_signed" app_id=7 correction=0"#.to_owned(),
                r#"WARN event="clock_skew_corrected" app_id=7 skew=660 correction=660"#.into(),
                r#"DEBUG event="app_credential_signed" app_id=7 correction=660"#.into(),
                format!(
                    r#"INFO event="installation_token_created" app_id=7 installation_id=42 token_id={}"#,
                    token.id()
                ),
                r#"DEBUG event="app_credential_signed" app_id=7 correction=0"#.into(),
                r#"WARN event="installation_token_failed" app_id=7 installation_id=42 code="gh:installation_gone""#.into(),
            ]
        );
        assert!(lines.iter().all(|line| !line.contains("ghs_")));
    }
}
