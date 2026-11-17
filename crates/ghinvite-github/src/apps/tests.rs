use super::*;
use crate::{
    endpoints,
    mocks::{self, Expectation, FixedTokens, MockTransport},
    profiles::{ORG_READ, REPO_INVITE, REPO_READ},
    rest::{CredentialKind, PathTemplate},
    transport::Method,
};
use octoauth::TransientKind;
use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};

const USER_1: &str = "https://api.github.test/user/1";
const COLLABORATOR: &str = "https://api.github.test/repos/o/r/collaborators/u";

fn user(status: u16, bearer: String) -> Expectation {
    let mut expectation = Expectation::ok_json(
        Method::Get,
        USER_1,
        serde_json::json!({"id": 1, "login": "alice"}),
    )
    .require_header("authorization", bearer);
    expectation.response.status = status;
    expectation
}

fn repo_app(mock: &MockTransport, tokens: &FixedTokens) -> GithubApp {
    GithubApps::for_tests(mock, [(AppSlot::Repo, tokens.clone())])
        .app(AppSlot::Repo)
        .unwrap()
}

fn transient() -> TokenError {
    TokenError::GitHub(octoauth::Error::Transient {
        kind: TransientKind::Server { status: 502 },
    })
}

async fn get_user(app: &GithubApp, runs: &AtomicUsize) -> Result<u64, TokenCallError> {
    app.read(9, &REPO_READ, |handle| async move {
        runs.fetch_add(1, SeqCst);
        Ok(endpoints::get_user_by_id(&handle.rest(), 1).await?.id)
    })
    .await
}

async fn remove(app: &GithubApp, runs: &AtomicUsize) -> Result<(), TokenCallError> {
    app.write_once(9, &REPO_INVITE, |handle| async move {
        runs.fetch_add(1, SeqCst);
        let repo = endpoints::Repository {
            owner: "o",
            name: "r",
        };
        Ok(endpoints::remove_collaborator(&handle.rest(), repo, "u").await?)
    })
    .await
}

#[tokio::test]
async fn read_replaces_the_token_after_a_401_and_runs_once_more() {
    let tokens = FixedTokens::new(123);
    let mock = MockTransport::scripted(vec![
        user(401, tokens.bearer(9, 1)),
        user(200, tokens.bearer(9, 2)),
    ]);
    let app = repo_app(&mock, &tokens);
    let runs = AtomicUsize::new(0);
    assert_eq!(get_user(&app, &runs).await.unwrap(), 1);
    assert_eq!(runs.load(SeqCst), 2);
    assert_eq!((tokens.token_calls(), tokens.replace_calls()), (1, 1));
    assert_eq!(tokens.generation(9, &REPO_READ), 2);
    mock.assert_exhausted();
}

#[tokio::test]
async fn read_returns_a_second_401() {
    let tokens = FixedTokens::new(123);
    let mock = MockTransport::scripted(vec![
        user(401, tokens.bearer(9, 1)),
        user(401, tokens.bearer(9, 2)),
    ]);
    let app = repo_app(&mock, &tokens);
    let runs = AtomicUsize::new(0);
    let error = get_user(&app, &runs).await.unwrap_err();
    assert!(error.is_unauthorized(), "{error:?}");
    assert!(matches!(
        error,
        TokenCallError::Call(CallError::Api(ApiError::Unauthorized))
    ));
    assert_eq!(error.code(), "gh:api:unauthorized");
    assert_eq!(runs.load(SeqCst), 2);
    assert_eq!(tokens.replace_calls(), 1);
    mock.assert_exhausted();
}

#[tokio::test]
async fn read_does_not_retry_other_failures() {
    for (status, expected) in [
        (403, "gh:api:forbidden"),
        (404, "gh:api:not_found"),
        (502, "gh:api:server"),
    ] {
        let tokens = FixedTokens::new(123);
        let mock = MockTransport::scripted(vec![user(status, tokens.bearer(9, 1))]);
        let app = repo_app(&mock, &tokens);
        let runs = AtomicUsize::new(0);
        let error = get_user(&app, &runs).await.unwrap_err();
        assert_eq!(error.code(), expected);
        assert_eq!(runs.load(SeqCst), 1);
        assert_eq!(tokens.replace_calls(), 0);
        mock.assert_exhausted();
    }
    // The application's own error is not a 401 either.
    let tokens = FixedTokens::new(123);
    let mock = MockTransport::scripted(vec![]);
    let app = repo_app(&mock, &tokens);
    let runs = AtomicUsize::new(0);
    let result: Result<(), _> = app
        .read(9, &REPO_READ, |_| {
            runs.fetch_add(1, SeqCst);
            async { Err(CallError::App("mine")) }
        })
        .await;
    assert!(matches!(
        result,
        Err(TokenCallError::Call(CallError::App("mine")))
    ));
    assert_eq!((runs.load(SeqCst), tokens.replace_calls()), (1, 0));
}

#[tokio::test]
async fn write_once_never_replays_and_invalidates_the_rejected_token() {
    let tokens = FixedTokens::new(123);
    let mock = MockTransport::scripted(vec![
        Expectation::status(Method::Delete, COLLABORATOR, 401)
            .require_header("authorization", tokens.bearer(9, 1)),
        // The caller's own retry gets a new token.
        Expectation::status(Method::Delete, COLLABORATOR, 204)
            .require_header("authorization", tokens.bearer(9, 2)),
    ]);
    let app = repo_app(&mock, &tokens);
    let runs = AtomicUsize::new(0);
    let error = remove(&app, &runs).await.unwrap_err();
    assert!(error.is_unauthorized());
    assert_eq!(runs.load(SeqCst), 1);
    // Nothing is created until the caller (after settling its fence) asks.
    assert_eq!((tokens.token_calls(), tokens.replace_calls()), (1, 0));
    assert_eq!(tokens.generation(9, &REPO_INVITE), 1);
    assert_eq!(mock.remaining(), 1);
    app.ensure(9, &REPO_INVITE).await.unwrap();
    assert_eq!(tokens.generation(9, &REPO_INVITE), 2);
    remove(&app, &runs).await.unwrap();
    assert_eq!(runs.load(SeqCst), 2);
    assert_eq!(tokens.replace_calls(), 0);
    mock.assert_exhausted();
}

#[tokio::test]
async fn write_once_returns_other_failures_without_replacing() {
    let tokens = FixedTokens::new(123);
    let mock = MockTransport::scripted(vec![
        Expectation::status(Method::Delete, COLLABORATOR, 500)
            .require_header("authorization", tokens.bearer(9, 1)),
    ]);
    let app = repo_app(&mock, &tokens);
    let runs = AtomicUsize::new(0);
    let error = remove(&app, &runs).await.unwrap_err();
    assert!(matches!(
        error,
        TokenCallError::Call(CallError::Api(ApiError::Server { status: 500 }))
    ));
    assert_eq!((runs.load(SeqCst), tokens.replace_calls()), (1, 0));
    mock.assert_exhausted();
}

#[tokio::test]
async fn after_a_write_401_the_next_ensure_reports_a_failed_creation() {
    let tokens = FixedTokens::new(123);
    let mock = MockTransport::scripted(vec![
        Expectation::status(Method::Delete, COLLABORATOR, 401)
            .require_header("authorization", tokens.bearer(9, 1)),
    ]);
    let app = repo_app(&mock, &tokens);
    let error = remove(&app, &AtomicUsize::new(0)).await.unwrap_err();
    assert!(error.is_unauthorized());
    tokens.fail_next_token(transient());
    assert!(
        app.ensure(9, &REPO_INVITE)
            .await
            .unwrap_err()
            .is_transient()
    );
    // The rejected token is still not served.
    app.ensure(9, &REPO_INVITE).await.unwrap();
    assert_eq!(tokens.generation(9, &REPO_INVITE), 2);
    assert_eq!(tokens.replace_calls(), 0);
    mock.assert_exhausted();
}

#[tokio::test]
async fn ensure_and_refresh_send_no_api_request() {
    let tokens = FixedTokens::new(123);
    let mock = MockTransport::scripted(vec![]);
    let app = repo_app(&mock, &tokens);
    app.ensure(9, &REPO_READ).await.unwrap();
    app.ensure(9, &REPO_READ).await.unwrap();
    assert_eq!(tokens.generation(9, &REPO_READ), 1);
    assert_eq!((tokens.token_calls(), tokens.replace_calls()), (2, 0));

    app.ensure(10, &REPO_READ).await.unwrap();

    app.refresh(9, &REPO_READ).await.unwrap();
    assert_eq!(tokens.generation(9, &REPO_READ), 2);
    assert_eq!((tokens.token_calls(), tokens.replace_calls()), (4, 0));
    // Without a cached token: exactly one is created.
    app.refresh(9, &REPO_INVITE).await.unwrap();
    assert_eq!(tokens.generation(9, &REPO_INVITE), 1);
    // The installation's other scopes are invalidated, not created...
    assert_eq!(tokens.generation(9, &REPO_READ), 2);
    app.ensure(9, &REPO_READ).await.unwrap();
    assert_eq!(tokens.generation(9, &REPO_READ), 3);
    // ...and other installations keep theirs.
    app.ensure(10, &REPO_READ).await.unwrap();
    assert_eq!(tokens.generation(10, &REPO_READ), 1);
    assert_eq!(tokens.generation(9, &ORG_READ), 0);
}

#[tokio::test]
async fn token_errors_propagate_and_skip_the_call() {
    let tokens = FixedTokens::new(123);
    let mock = MockTransport::scripted(vec![user(401, tokens.bearer(9, 1))]);
    let app = repo_app(&mock, &tokens);
    let runs = AtomicUsize::new(0);

    tokens.fail_next_token(transient());
    let error = get_user(&app, &runs).await.unwrap_err();
    let TokenCallError::Token(token_error) = &error else {
        panic!("{error:?}");
    };
    assert!(token_error.is_transient());
    assert_eq!(error.code(), "gh:transient");
    assert_eq!(runs.load(SeqCst), 0);

    tokens.fail_next_token(TokenError::GitHub(octoauth::Error::InstallationGone));
    let error = remove(&app, &runs).await.unwrap_err();
    assert!(matches!(
        error,
        TokenCallError::Token(TokenError::GitHub(octoauth::Error::InstallationGone))
    ));
    tokens.fail_next_token(TokenError::GitHub(octoauth::Error::PermissionsNotGranted));
    assert_eq!(
        app.ensure(9, &REPO_READ).await.unwrap_err().code(),
        "gh:permissions_not_granted"
    );
    tokens.fail_next_token(transient());
    assert!(app.refresh(9, &REPO_READ).await.unwrap_err().is_transient());
    assert_eq!(runs.load(SeqCst), 0);

    // The replacement after a 401 fails: reported, no second run.
    app.ensure(9, &REPO_READ).await.unwrap();
    let before = tokens.token_calls();
    tokens.fail_next_replace(TokenError::GitHub(octoauth::Error::InstallationSuspended));
    let error = get_user(&app, &runs).await.unwrap_err();
    assert!(matches!(
        error,
        TokenCallError::Token(TokenError::GitHub(octoauth::Error::InstallationSuspended))
    ));
    assert_eq!(runs.load(SeqCst), 1);
    assert_eq!(tokens.token_calls(), before + 1);
    mock.assert_exhausted();
}

#[test]
fn rate_limit_and_github_error_accessors() {
    let limited = TokenError::GitHub(octoauth::Error::RateLimited {
        scope: RateLimitScope::Secondary,
        retry_after: Some(Duration::from_secs(7)),
    });
    assert!(limited.is_transient());
    assert_eq!(
        rate_limit(&limited),
        Some((RateLimitScope::Secondary, Some(Duration::from_secs(7))))
    );
    assert_eq!(rate_limit(&transient()), None);
    assert_eq!(rate_limit(&TokenError::ForeignToken), None);
    assert_eq!(
        github_error(&transient()),
        Some(&octoauth::Error::Transient {
            kind: TransientKind::Server { status: 502 }
        })
    );
    assert_eq!(github_error(&TokenError::ForeignToken), None);
}

#[tokio::test]
async fn app_rest_sends_the_app_credential() {
    let mock = MockTransport::scripted(vec![]);
    let apps = GithubApps::for_tests(&mock, [(AppSlot::Repo, FixedTokens::new(123))]);
    let app = apps.app(AppSlot::Repo).unwrap();
    let handle = app.app_rest().unwrap();
    // The handle exposes no secret; the app's cached credential is the one sent.
    let jwt = app.app.credential().unwrap().expose_secret().to_owned();
    assert_eq!(jwt.split('.').count(), 3, "a JWT");
    assert!(!jwt.starts_with("ghs_"));
    assert!(!format!("{handle:?}").contains(&jwt));

    let mock = MockTransport::scripted(vec![
        Expectation::ok_json(
            Method::Get,
            "https://api.github.test/app/installation-requests",
            serde_json::json!([]),
        )
        .require_header("authorization", format!("Bearer {jwt}"))
        .require_header("user-agent", "p1/repo"),
    ]);
    let tokens = FixedTokens::new(123);
    let env = mocks::env(&mock, "ghinvite-test");
    let installed = InstalledApp {
        slot: AppSlot::Repo,
        app: app.app.clone(),
        user_agent: "p1/repo".into(),
    };
    let apps = GithubApps::with_sources(
        env,
        [(installed, Arc::new(tokens.clone()) as Arc<dyn TokenSource>)],
    )
    .unwrap();
    let handle = apps.app(AppSlot::Repo).unwrap().app_rest().unwrap();
    let rest = handle.rest();
    assert_eq!(rest.credential_kind(), CredentialKind::AppJwt);
    let _: crate::rest::ApiResponse<serde_json::Value> = rest
        .call::<(), _>(
            Method::Get,
            PathTemplate::new("/app/installation-requests"),
            &[],
            None,
        )
        .await
        .unwrap();
    assert_eq!(tokens.token_calls(), 0);
    mock.assert_exhausted();
}

#[tokio::test]
async fn each_app_sends_its_user_agent_and_tokens() {
    let (repo, org) = (FixedTokens::new(123), FixedTokens::new(456));
    let mock = MockTransport::scripted(vec![
        user(200, repo.bearer(9, 1)).require_header("user-agent", "p1/repo"),
        user(200, org.bearer(9, 1)).require_header("user-agent", "p1/org"),
    ]);
    let apps = GithubApps::for_tests(
        &mock,
        [(AppSlot::Org, org.clone()), (AppSlot::Repo, repo.clone())],
    );
    assert_eq!(
        apps.slots().collect::<Vec<_>>(),
        [AppSlot::Repo, AppSlot::Org]
    );
    let runs = AtomicUsize::new(0);
    for slot in [AppSlot::Repo, AppSlot::Org] {
        let app = apps.app(slot).unwrap();
        assert_eq!(app.slot(), slot);
        get_user(&app, &runs).await.unwrap();
    }
    assert_eq!(apps.app(AppSlot::Org).unwrap().id(), AppId::new(456));
    mock.assert_exhausted();

    let repo_only = GithubApps::for_tests(&mock, [(AppSlot::Repo, repo)]);
    assert_eq!(
        repo_only.app(AppSlot::Org).unwrap_err(),
        AppUnavailable { slot: AppSlot::Org }
    );
}

#[tokio::test]
async fn closures_see_the_credential_identity_but_no_secret() {
    let tokens = FixedTokens::new(123);
    let mock = MockTransport::scripted(vec![]);
    let app = repo_app(&mock, &tokens);
    let seen = app
        .read(9, &REPO_READ, |handle| async move {
            let credential = handle.credential();
            assert!(!format!("{handle:?}").contains("ghs_fixed"));
            Ok::<_, CallError>((credential.app_id(), credential.installation_id()))
        })
        .await
        .unwrap();
    assert_eq!(seen, (AppId::new(123), InstallationId::new(9)));
}

#[test]
fn slots_round_trip_their_names() {
    for slot in [AppSlot::Repo, AppSlot::Org] {
        assert_eq!(AppSlot::from_name(slot.as_str()), Some(slot));
        assert_eq!(slot.to_string(), slot.as_str());
    }
    assert_eq!(AppSlot::from_name("signin"), None);
}

/// The futures run on a multi-threaded runtime's other threads, as inside
/// Restate's `ctx.run` (`futures_are_send` checks this for wasm32 too).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn futures_are_send_natively() {
    let tokens = FixedTokens::new(123);
    let mock = MockTransport::scripted(vec![user(200, tokens.bearer(9, 1))]);
    let app = repo_app(&mock, &tokens);
    let spawned = tokio::spawn(async move {
        app.ensure(9, &REPO_READ).await.unwrap();
        app.read(9, &REPO_READ, |handle| async move {
            Ok::<_, CallError>(endpoints::get_user_by_id(&handle.rest(), 1).await?.login)
        })
        .await
    });
    assert_eq!(spawned.await.unwrap().unwrap(), "alice");
    mock.assert_exhausted();
}
