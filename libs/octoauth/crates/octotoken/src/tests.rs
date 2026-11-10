//! `AppTokenProvider` against a GitHub stand-in that creates numbered tokens.

use crate::clock::Clock;
use crate::{AppTokenProvider, InstallationTokenProvider, TokenError, WithTokenError, with_token};
use octoauth::{
    Access, App, AppId, Error, InstallationId, InstallationToken, OrganizationPermission,
    Permissions, SigningKey, TokenParts, TokenScope, TransientKind,
};
use std::sync::atomic::Ordering::SeqCst;
use std::sync::atomic::{AtomicI64, AtomicU64, AtomicUsize};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use wiremock::matchers::{method, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

const PEM: &str = include_str!("../tests/fixtures/rsa2048.pem");

fn real_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

/// A clock tests move by hand, starting at real time.
#[derive(Clone)]
struct Manual(Arc<AtomicI64>);

impl Manual {
    fn new() -> Self {
        Self(Arc::new(AtomicI64::new(real_now())))
    }

    fn advance(&self, seconds: i64) {
        self.0.fetch_add(seconds, SeqCst);
    }
}

impl Clock for Manual {
    fn now(&self) -> i64 {
        self.0.load(SeqCst)
    }
}

/// Creates `ghs_<n>` tokens living `lifetime` seconds, with `Date` set to real
/// time plus `offset` (GitHub's clock), after `delay_ms`; or answers with
/// `fail` once if set.
struct GitHub {
    created: AtomicUsize,
    lifetime: AtomicI64,
    offset: AtomicI64,
    delay_ms: AtomicU64,
    date: Mutex<Option<String>>,
    fail: Mutex<Option<ResponseTemplate>>,
}

fn http_date(seconds: i64) -> String {
    chrono::DateTime::from_timestamp(seconds, 0)
        .unwrap()
        .format("%a, %d %b %Y %H:%M:%S GMT")
        .to_string()
}

impl wiremock::Respond for Shared {
    fn respond(&self, _: &wiremock::Request) -> ResponseTemplate {
        let github = &self.0;
        let n = github.created.fetch_add(1, SeqCst) + 1;
        let now = real_now() + github.offset.load(SeqCst);
        let date = github
            .date
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_else(|| http_date(now));
        let delay = Duration::from_millis(github.delay_ms.load(SeqCst));
        let response = github.fail.lock().unwrap().take().unwrap_or_else(|| {
            let expires_at =
                chrono::DateTime::from_timestamp(now + github.lifetime.load(SeqCst), 0)
                    .unwrap()
                    .to_rfc3339();
            ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "token": format!("ghs_{n}"),
                "expires_at": expires_at,
            }))
        });
        response
            .insert_header("date", date.as_str())
            .set_delay(delay)
    }
}

struct Shared(Arc<GitHub>);

struct Fixture {
    provider: AppTokenProvider,
    github: Arc<GitHub>,
    clock: Manual,
    _server: MockServer,
}

impl Fixture {
    async fn new() -> Self {
        let github = Arc::new(GitHub {
            created: AtomicUsize::new(0),
            lifetime: AtomicI64::new(3600),
            offset: AtomicI64::new(0),
            delay_ms: AtomicU64::new(0),
            date: Mutex::new(None),
            fail: Mutex::new(None),
        });
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path_regex(r"^/app/installations/\d+/access_tokens$"))
            .respond_with(Shared(github.clone()))
            .mount(&server)
            .await;
        let app = App::builder(7, SigningKey::from_pem(PEM).unwrap())
            .base_url(server.uri())
            .build()
            .unwrap();
        let clock = Manual::new();
        let provider = AppTokenProvider::builder(app)
            .clock(Arc::new(clock.clone()))
            .build()
            .unwrap();
        Self {
            provider,
            github,
            clock,
            _server: server,
        }
    }

    fn created(&self) -> usize {
        self.github.created.load(SeqCst)
    }

    async fn token(&self) -> Result<InstallationToken, TokenError> {
        self.provider.token(InstallationId::new(42), &scope()).await
    }
}

fn scope() -> TokenScope {
    Permissions::new([(OrganizationPermission::Members, Access::Read)])
        .unwrap()
        .into()
}

#[tokio::test]
async fn cache_hits_create_nothing() {
    let f = Fixture::new().await;
    let first = f.token().await.unwrap();
    assert_eq!(first.expose_secret(), "ghs_1");
    let again = f
        .provider
        .clone()
        .token(InstallationId::new(42), &scope())
        .await;
    assert_eq!(again.unwrap().id(), first.id(), "clones share the cache");
    assert_eq!(f.created(), 1);

    // Another installation or scope is another token.
    f.provider
        .token(InstallationId::new(43), &scope())
        .await
        .unwrap();
    let write = Permissions::new([(OrganizationPermission::Members, Access::Write)]).unwrap();
    f.provider
        .token(InstallationId::new(42), &write.into())
        .await
        .unwrap();
    assert_eq!(f.created(), 3);
}

#[tokio::test]
async fn tokens_are_returned_with_min_remaining_left() {
    let f = Fixture::new().await;
    f.token().await.unwrap();
    f.clock.advance(3600 - 300);
    f.token().await.unwrap();
    assert_eq!(f.created(), 1, "exactly 5 minutes left");
    f.clock.advance(1);
    assert_eq!(f.token().await.unwrap().expose_secret(), "ghs_2");
}

#[tokio::test]
async fn freshness_ignores_host_clock_skew() {
    // GitHub's clock two hours ahead or behind: the lifetime still counts.
    for offset in [7200, -7200] {
        let f = Fixture::new().await;
        f.github.offset.store(offset, SeqCst);
        f.token().await.unwrap();
        f.clock.advance(3000);
        f.token().await.unwrap();
        assert_eq!(f.created(), 1, "offset {offset}");
    }
}

#[tokio::test]
async fn without_issued_at_expiry_is_read_on_the_local_clock() {
    let f = Fixture::new().await;
    *f.github.date.lock().unwrap() = Some("not a date".into());
    let token = f.token().await.unwrap();
    assert_eq!(token.issued_at(), None);
    f.clock.advance(3000);
    f.token().await.unwrap();
    assert_eq!(f.created(), 1);
    f.clock.advance(400);
    f.token().await.unwrap();
    assert_eq!(f.created(), 2);
}

#[tokio::test]
async fn short_lived_tokens_are_still_returned() {
    // Shorter than the minimum: each call creates and returns, never loops.
    let f = Fixture::new().await;
    f.github.lifetime.store(120, SeqCst);
    f.token().await.unwrap();
    f.token().await.unwrap();
    assert_eq!(f.created(), 2);
}

#[tokio::test]
async fn concurrent_requests_create_once() {
    let f = Fixture::new().await;
    f.github.delay_ms.store(200, SeqCst);
    let tokens = futures_util::future::join_all((0..8).map(|_| f.token())).await;
    let first = tokens[0].as_ref().unwrap().id();
    assert!(tokens.iter().all(|t| t.as_ref().unwrap().id() == first));
    assert_eq!(f.created(), 1);
}

#[tokio::test]
async fn waiters_share_a_failed_creation() {
    let f = Fixture::new().await;
    f.github.delay_ms.store(200, SeqCst);
    *f.github.fail.lock().unwrap() = Some(ResponseTemplate::new(503));
    let results = futures_util::future::join_all((0..8).map(|_| f.token())).await;
    let expected = TokenError::GitHub(Error::Transient {
        kind: TransientKind::Server { status: 503 },
    });
    assert!(results.iter().all(|r| r.as_ref().unwrap_err() == &expected));
    assert_eq!(f.created(), 1);

    // Not cached: the next request creates again.
    f.token().await.unwrap();
    assert_eq!(f.created(), 2);
}

#[tokio::test]
async fn a_cancelled_creation_does_not_block_others() {
    let f = Fixture::new().await;
    f.github.delay_ms.store(1000, SeqCst);
    let cancelled = tokio::time::timeout(Duration::from_millis(50), f.token()).await;
    assert!(cancelled.is_err());
    f.github.delay_ms.store(0, SeqCst);
    f.token().await.unwrap();
}

#[tokio::test]
async fn replace_returns_a_newer_token() {
    let f = Fixture::new().await;
    let first = f.token().await.unwrap();
    // Created in the same second as `first` would share its expiry: move on.
    tokio::time::sleep(Duration::from_millis(1100)).await;

    let second = f.provider.replace(&first).await.unwrap();
    assert_ne!(second.id(), first.id());
    assert_eq!(f.created(), 2);
    assert_eq!(f.token().await.unwrap().id(), second.id(), "cached");

    // Another holder of `first` gets `second` without creating one.
    let again = f.provider.replace(&first).await.unwrap();
    assert_eq!(again.id(), second.id());
    assert_eq!(f.created(), 2);

    // Replacing `second` creates one.
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let third = f.provider.replace(&second).await.unwrap();
    assert_ne!(third.id(), second.id());
    assert_eq!(f.created(), 3);
}

#[tokio::test]
async fn concurrent_replacements_create_once() {
    let f = Fixture::new().await;
    let first = f.token().await.unwrap();
    tokio::time::sleep(Duration::from_millis(1100)).await;
    f.github.delay_ms.store(200, SeqCst);
    let replacements =
        futures_util::future::join_all((0..8).map(|_| f.provider.replace(&first))).await;
    let id = replacements[0].as_ref().unwrap().id();
    assert_ne!(id, first.id());
    assert!(replacements.iter().all(|r| r.as_ref().unwrap().id() == id));
    assert_eq!(f.created(), 2);
}

#[tokio::test]
async fn a_rejected_token_is_never_served_again() {
    let f = Fixture::new().await;
    let first = f.token().await.unwrap();
    *f.github.fail.lock().unwrap() = Some(ResponseTemplate::new(503));
    assert!(f.provider.replace(&first).await.is_err());
    let next = f.token().await.unwrap();
    assert_ne!(next.id(), first.id());
    assert_eq!(f.created(), 3);
}

#[tokio::test]
async fn foreign_tokens_are_refused() {
    let f = Fixture::new().await;
    let foreign = InstallationToken::from_parts(TokenParts {
        secret: "ghs_other".into(),
        app_id: AppId::new(8),
        installation_id: InstallationId::new(42),
        scope: scope(),
        expires_at: SystemTime::now() + Duration::from_secs(3600),
        issued_at: None,
    })
    .unwrap();
    assert_eq!(
        f.provider.replace(&foreign).await.unwrap_err(),
        TokenError::ForeignToken
    );
    assert_eq!(f.created(), 0);
}

#[tokio::test]
async fn expired_slots_are_swept() {
    let f = Fixture::new().await;
    for installation in 1..=3 {
        f.provider
            .token(InstallationId::new(installation), &scope())
            .await
            .unwrap();
    }
    assert_eq!(f.provider.slot_count(), 3);
    f.clock.advance(3601);
    f.provider
        .token(InstallationId::new(4), &scope())
        .await
        .unwrap();
    assert_eq!(f.provider.slot_count(), 1);
}

#[tokio::test]
async fn min_remaining_is_validated() {
    let app = App::new(7, SigningKey::from_pem(PEM).unwrap());
    let build = |seconds| {
        AppTokenProvider::builder(app.clone())
            .min_remaining(Duration::from_secs(seconds))
            .build()
            .map(|_| ())
            .map_err(|e| (e.field(), e.what()))
    };
    assert_eq!(build(30), Ok(()));
    assert_eq!(build(1800), Ok(()));
    for seconds in [29, 1801] {
        assert_eq!(build(seconds), Err(("min_remaining", "range")));
    }
}

#[test]
fn errors_delegate() {
    let limited = TokenError::GitHub(Error::RateLimited {
        scope: octoauth::RateLimitScope::Secondary,
        retry_after: Some(Duration::from_secs(7)),
    });
    assert_eq!(limited.code(), "gh:rate_limited:secondary");
    assert!(limited.is_transient());
    assert_eq!(limited.suggested_wait(), Some(Duration::from_secs(7)));
    assert_eq!(TokenError::ForeignToken.code(), "gh:provider:foreign_token");
    assert!(!TokenError::ForeignToken.is_transient());
    assert_eq!(
        TokenError::ForeignToken.to_string(),
        "gh:provider:foreign_token"
    );
}

/// The closure's error in `with_token` tests: a GitHub answer.
#[derive(Debug, PartialEq)]
enum Answer {
    Unauthorized,
    NotFound,
}

fn unauthorized(answer: &Answer) -> bool {
    *answer == Answer::Unauthorized
}

/// Runs `with_token` against a GitHub API stand-in that answers 401 to the
/// tokens in `rejected`, 404 to any other if `not_found`, and success
/// otherwise; returns the outcome and the tokens the API saw.
async fn call_with(
    f: &Fixture,
    rejected: &[&str],
    not_found: bool,
) -> (Result<String, WithTokenError<Answer>>, Vec<String>) {
    let seen = Mutex::new(Vec::new());
    let result = with_token(
        &f.provider,
        InstallationId::new(42),
        &scope(),
        unauthorized,
        |token| {
            let secret = token.expose_secret().to_owned();
            seen.lock().unwrap().push(secret.clone());
            let answer = if rejected.contains(&secret.as_str()) {
                Err(Answer::Unauthorized)
            } else if not_found {
                Err(Answer::NotFound)
            } else {
                Ok(format!("ok with {secret}"))
            };
            async move { answer }
        },
    )
    .await;
    (result, seen.into_inner().unwrap())
}

#[tokio::test]
async fn with_token_calls_once_when_accepted() {
    let f = Fixture::new().await;
    let (result, seen) = call_with(&f, &[], false).await;
    assert_eq!(result.unwrap(), "ok with ghs_1");
    assert_eq!(seen, ["ghs_1"]);
    assert_eq!(f.created(), 1);
}

#[tokio::test]
async fn with_token_replaces_once_after_a_401() {
    let f = Fixture::new().await;
    let (result, seen) = call_with(&f, &["ghs_1"], false).await;
    assert_eq!(result.unwrap(), "ok with ghs_2");
    assert_eq!(seen, ["ghs_1", "ghs_2"]);
    assert_eq!(f.created(), 2);
    // The replacement is what the provider hands out from now on.
    assert_eq!(f.token().await.unwrap().expose_secret(), "ghs_2");
}

#[tokio::test]
async fn with_token_returns_a_second_401() {
    let f = Fixture::new().await;
    let (result, seen) = call_with(&f, &["ghs_1", "ghs_2"], false).await;
    assert_eq!(
        result.unwrap_err(),
        WithTokenError::Call(Answer::Unauthorized)
    );
    assert_eq!(seen, ["ghs_1", "ghs_2"]);
    assert_eq!(f.created(), 2);
}

#[tokio::test]
async fn with_token_never_retries_other_errors() {
    let f = Fixture::new().await;
    let (result, seen) = call_with(&f, &[], true).await;
    assert_eq!(result.unwrap_err(), WithTokenError::Call(Answer::NotFound));
    assert_eq!(seen, ["ghs_1"]);
    assert_eq!(f.created(), 1);
}

#[tokio::test]
async fn with_token_reports_token_failures() {
    let unavailable = TokenError::GitHub(Error::Transient {
        kind: TransientKind::Server { status: 503 },
    });

    // No token: the call never runs.
    let f = Fixture::new().await;
    *f.github.fail.lock().unwrap() = Some(ResponseTemplate::new(503));
    let (result, seen) = call_with(&f, &[], false).await;
    assert_eq!(
        result.unwrap_err(),
        WithTokenError::Token(unavailable.clone())
    );
    assert!(seen.is_empty());

    // No replacement: the call does not run again.
    let f = Fixture::new().await;
    f.token().await.unwrap();
    *f.github.fail.lock().unwrap() = Some(ResponseTemplate::new(503));
    let (result, seen) = call_with(&f, &["ghs_1"], false).await;
    assert_eq!(result.unwrap_err(), WithTokenError::Token(unavailable));
    assert_eq!(seen, ["ghs_1"]);
}

#[tokio::test]
async fn concurrent_401s_share_one_replacement() {
    let f = Fixture::new().await;
    f.github.delay_ms.store(200, SeqCst);
    let scope = scope();
    let calls = (0..8).map(|_| {
        with_token(
            &f.provider,
            InstallationId::new(42),
            &scope,
            unauthorized,
            |token| async move {
                if token.expose_secret() == "ghs_1" {
                    // A replacement must be newer: tokens expire per second.
                    tokio::time::sleep(Duration::from_millis(1100)).await;
                    Err(Answer::Unauthorized)
                } else {
                    Ok(token.expose_secret().to_owned())
                }
            },
        )
    });
    let results = futures_util::future::join_all(calls).await;
    assert!(
        results.iter().all(|r| r.as_ref().unwrap() == "ghs_2"),
        "{results:?}"
    );
    assert_eq!(f.created(), 2);
}

#[test]
fn with_token_errors_render_the_inner_error() {
    let error: WithTokenError<TokenError> = WithTokenError::Token(TokenError::ForeignToken);
    assert_eq!(error.to_string(), "gh:provider:foreign_token");
    let error = WithTokenError::Call(octoauth::ApiError::NotFound);
    assert_eq!(error.to_string(), "gh:api:not_found");
}
