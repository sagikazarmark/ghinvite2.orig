//! Test-only fixture builders. Behind `#[cfg(test)]` (declared in `lib.rs`).

use crate::state::AppState;
use chrono::{DateTime, Utc};
use std::sync::Arc;

/// Build an in-memory `SqlxStorage`.
pub(crate) async fn fixture_storage() -> Arc<dyn crate::WorkflowStorage> {
    Arc::new(
        ghinvite_storage_sqlx::SqlxStorage::in_memory()
            .await
            .unwrap(),
    )
}

/// State without GitHub I/O; HTTP tests explicitly provide typed apps.
pub(crate) async fn fixture_state() -> AppState {
    AppState::new(fixture_storage().await)
}

/// Convenience: an `AppState` plus concrete storage for tests that need
/// debug-only audit reads.
pub(crate) async fn fixture_state_with_storage()
-> (AppState, Arc<ghinvite_storage_sqlx::SqlxStorage>) {
    let storage = Arc::new(
        ghinvite_storage_sqlx::SqlxStorage::in_memory()
            .await
            .unwrap(),
    );
    let storage_for_state: Arc<dyn crate::WorkflowStorage> = storage.clone();
    let state = AppState::new(storage_for_state);

    (state, storage)
}

/// `GithubApps` for unit tests: `repo` (App 123) and `org` (App 456) with
/// `FixedTokens`, REST calls through `mock` to `mocks::API_BASE`.
pub(crate) fn github_apps(
    mock: &ghinvite_github::mocks::MockTransport,
) -> ghinvite_github::GithubApps {
    use ghinvite_github::{AppSlot, mocks::FixedTokens};
    ghinvite_github::GithubApps::for_tests(
        mock,
        [
            (AppSlot::Repo, FixedTokens::new(123)),
            (AppSlot::Org, FixedTokens::new(456)),
        ],
    )
}

/// Build a `DateTime<Utc>` from RFC-3339; same pattern used in storage tests.
pub(crate) fn dt(s: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
}

/// The `{"message": ...}` response GitHub returns for a refusal, carrying
/// whatever evidence `headers` gives it. Every throttling test varies the
/// evidence and nothing else, so the shape lives here rather than in each one.
pub(crate) fn refusal(
    status: u16,
    headers: &[(&str, &str)],
    message: &str,
) -> ghinvite_github::Response {
    ghinvite_github::Response {
        status,
        headers: headers
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect(),
        body: serde_json::json!({ "message": message }).to_string().into(),
    }
}

/// The [`ApiError`](ghinvite_github::ApiError) `ghinvite_github`'s REST
/// layer classifies [`refusal`]'s response as, for one installation read.
pub(crate) async fn refused(
    status: u16,
    headers: &[(&str, &str)],
    message: &str,
) -> ghinvite_github::ApiError {
    use ghinvite_github::{
        Method,
        mocks::{self, Expectation, MockTransport},
        rest::{PathTemplate, Rest},
    };
    let mock = MockTransport::scripted(vec![Expectation {
        response: refusal(status, headers, message),
        ..Expectation::status(Method::Get, format!("{}/refusal", mocks::API_BASE), status)
    }]);
    let env = mocks::env(&mock, "ghinvite-workflows-test");
    let credential = mocks::installation_credential(123, 1, "ghs_refusal");
    let error = Rest::new(&env, &credential)
        .call::<(), serde_json::Value>(Method::Get, PathTemplate::new("/refusal"), &[], None)
        .await
        .expect_err("a refusal");
    mock.assert_exhausted();
    error
}

/// Golden fixtures for persisted serde shapes (plan GM-00, PD-9). Fixtures live
/// in `golden/` and are only compared; `GOLDEN_BLESS=1` rewrites them.
pub(crate) mod golden {
    use serde::{Serialize, de::DeserializeOwned};

    fn path(name: &str) -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("golden")
            .join(name)
    }

    /// Compare `actual` with the fixture text, returning the fixture.
    pub(crate) fn text(name: &str, actual: &str) -> String {
        let path = path(name);
        if std::env::var_os("GOLDEN_BLESS").is_some_and(|value| value == "1") {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, format!("{actual}\n")).unwrap();
        }
        let expected = std::fs::read_to_string(&path).unwrap_or_else(|error| {
            panic!(
                "missing golden {} ({error}); bless with GOLDEN_BLESS=1",
                path.display()
            )
        });
        let expected = expected.strip_suffix('\n').unwrap_or(&expected).to_owned();
        assert_eq!(
            actual,
            expected,
            "persisted shape changed (PD-9): {}",
            path.display()
        );
        expected
    }

    /// Production writes these values with `serde_json` (Restate's `Json`,
    /// `serde_json::to_vec`). The fixture is the same serializer's pretty form,
    /// so field order and value encodings are compared exactly. The fixture
    /// must also decode and re-encode to itself, as a replay would.
    pub(crate) fn json<T: Serialize + DeserializeOwned>(name: &str, value: &T) -> T {
        let compact: serde_json::Value =
            serde_json::from_slice(&serde_json::to_vec(value).unwrap()).unwrap();
        let fixture = text(name, &serde_json::to_string_pretty(value).unwrap());
        assert_eq!(
            compact,
            serde_json::from_str::<serde_json::Value>(&fixture).unwrap()
        );
        let decoded: T = serde_json::from_str(&fixture).unwrap();
        assert_eq!(
            serde_json::to_string_pretty(&decoded).unwrap(),
            fixture,
            "fixture does not round-trip: {name}"
        );
        decoded
    }
}
