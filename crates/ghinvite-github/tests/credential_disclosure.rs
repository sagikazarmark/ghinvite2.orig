//! Credentials must not survive a GitHub failure.
//!
//! The token endpoints answer with credentials, and anything between us and
//! `api.github.com` can echo the `Authorization` header we sent into its own
//! error page. This exercises those paths with sensitive-looking payloads and
//! asserts the values reach neither the error value nor the logs.

use ghinvite_github::installation::InstallationClient;
use ghinvite_github::jwt::AppJwtSigner;
use ghinvite_github::mocks::{Expectation, MockTransport};
use ghinvite_github::oauth::{OAuthConfig, exchange_code};
use ghinvite_github::transport::{Method, Response};
use std::collections::BTreeMap;
use std::io::Write;
use std::sync::{Arc, Mutex};

/// Same static key the unit tests use — signing a fresh one costs seconds.
const TEST_KEY_PEM: &str = include_str!("../src/jwt_test_key.pem");

const INSTALLATION_TOKEN: &str = "ghs_16C7e42F292c6912E7710c838347Ae178B4a";
const USER_TOKEN: &str = "gho_16C7e42F292c6912E7710c838347Ae178B4a";
const CLIENT_SECRET: &str = "client-secret-do-not-expose";
const APP_JWT: &str = "eyJhbGciOiJSUzI1NiJ9.eyJpc3MiOiIxMjM0NSJ9.c2lnbmF0dXJlLXZhbHVl";

/// A `tracing` sink that keeps everything written to it, so a test can assert
/// on what an operator would actually see.
#[derive(Clone, Default)]
struct CapturedLogs(Arc<Mutex<Vec<u8>>>);

impl CapturedLogs {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
    }
}

impl Write for CapturedLogs {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for CapturedLogs {
    type Writer = Self;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

/// Capture every event this thread emits for as long as the guard lives.
/// `#[tokio::test]` runs on a current-thread runtime, so the awaited work stays
/// on the thread the guard was installed on.
fn capture_logs() -> (CapturedLogs, tracing::subscriber::DefaultGuard) {
    let logs = CapturedLogs::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(logs.clone())
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .finish();
    (logs.clone(), tracing::subscriber::set_default(subscriber))
}

fn signer() -> AppJwtSigner {
    AppJwtSigner::from_pem(123, TEST_KEY_PEM).unwrap()
}

fn oauth_config() -> OAuthConfig {
    OAuthConfig {
        client_id: "Iv1.test".into(),
        client_secret: CLIENT_SECRET.into(),
        redirect_uri: "https://app.test/oauth/callback".into(),
    }
}

fn responds(url: &str, status: u16, body: Vec<u8>) -> MockTransport {
    MockTransport::scripted(vec![Expectation {
        method: Method::Post,
        url: url.into(),
        required_headers: BTreeMap::new(),
        expected_body: None,
        response: Response {
            status,
            headers: BTreeMap::new(),
            body,
        },
    }])
}

const MINT_URL: &str = "https://api.github.test/app/installations/55/access_tokens";
const TOKEN_URL: &str = "https://github.com/login/oauth/access_token";

fn assert_absent(haystack: &str, needles: &[&str], what: &str) {
    for needle in needles {
        assert!(
            !haystack.contains(needle),
            "{what} disclosed {needle:?}:\n{haystack}"
        );
    }
}

/// A token response that decodes cleanly as JSON but not as our payload type:
/// serde names the value it rejected, which here is the credential itself.
#[tokio::test]
async fn malformed_installation_token_response_discloses_nothing() {
    let (logs, _guard) = capture_logs();
    let body = format!(r#"{{"token":"{INSTALLATION_TOKEN}","expires_at":4102444800}}"#);
    let mock = responds(MINT_URL, 201, body.clone().into_bytes());
    let client = InstallationClient::new(Arc::new(mock.clone()), signer())
        .with_base("https://api.github.test");

    let error = client.mint_installation_token(55).await.unwrap_err();

    let rendered = format!("{error} {error:?}");
    assert_absent(&rendered, &[INSTALLATION_TOKEN], "error formatting");
    assert_absent(&logs.text(), &[INSTALLATION_TOKEN], "logs");
    // The failure is still classified as a decode failure, which callers treat
    // as transient rather than as a terminal answer.
    assert!(
        matches!(error, ghinvite_github::Error::Decode(_)),
        "{rendered}"
    );
    mock.assert_exhausted();
}

/// A gateway in front of GitHub that reflects the request it proxied. The App
/// JWT we sent comes back inside a body we never asked for.
#[tokio::test]
async fn gateway_error_reflecting_our_credentials_discloses_nothing() {
    let (logs, _guard) = capture_logs();
    let body = format!(
        "<html><h1>502 Bad Gateway</h1><pre>upstream request was:\n\
         Authorization: Bearer {APP_JWT}\n</pre></html>"
    );
    let mock = responds(MINT_URL, 502, body.into_bytes());
    let client = InstallationClient::new(Arc::new(mock.clone()), signer())
        .with_base("https://api.github.test");

    let error = client.mint_installation_token(55).await.unwrap_err();

    let rendered = format!("{error} {error:?}");
    assert_absent(&rendered, &[APP_JWT, "Authorization"], "error formatting");
    assert_absent(&logs.text(), &[APP_JWT, "Authorization"], "logs");
    // 502 stays visible: the retry classification in ghinvite-workflows reads it.
    assert_eq!(error.status(), Some(502));
    assert!(logs.text().contains("502"), "{}", logs.text());
    mock.assert_exhausted();
}

/// GitHub's documented 422 envelope. The sub-code is the whole reason callers
/// look at a 422 at all, so sanitizing must not take it away.
#[tokio::test]
async fn documented_validation_sub_codes_survive_sanitizing() {
    let body = br#"{"message":"Validation Failed","errors":[{"resource":"RepositoryInvitation","code":"already_exists","field":"invitee_id"}]}"#;
    let mock = responds(MINT_URL, 422, body.to_vec());
    let client = InstallationClient::new(Arc::new(mock.clone()), signer())
        .with_base("https://api.github.test");

    let error = client.mint_installation_token(55).await.unwrap_err();

    assert_eq!(error.status(), Some(422));
    assert!(error.to_string().contains("already_exists"), "{error}");
    mock.assert_exhausted();
}

/// The OAuth token endpoint, reached with the client secret in the request
/// body. A failure there must not carry the response payload either.
#[tokio::test]
async fn malformed_oauth_token_response_discloses_nothing() {
    let (logs, _guard) = capture_logs();
    let body = format!(r#"{{"access_token":"{USER_TOKEN}","scope":42}}"#);
    let mock = responds(TOKEN_URL, 200, body.into_bytes());

    let error = exchange_code(&mock, &oauth_config(), "the-code")
        .await
        .unwrap_err();

    let rendered = format!("{error} {error:?}");
    assert_absent(
        &rendered,
        &[USER_TOKEN, CLIENT_SECRET, "the-code"],
        "error formatting",
    );
    assert_absent(
        &logs.text(),
        &[USER_TOKEN, CLIENT_SECRET, "the-code"],
        "logs",
    );
    mock.assert_exhausted();
}

/// GitHub answers a failed exchange with 200 plus an error payload. The
/// documented code is kept; the prose beside it is not.
#[tokio::test]
async fn oauth_error_payload_keeps_the_code_and_drops_the_prose() {
    let (logs, _guard) = capture_logs();
    let body = format!(
        r#"{{"error":"bad_verification_code","error_description":"expired; retry with {USER_TOKEN}"}}"#
    );
    let mock = responds(TOKEN_URL, 200, body.into_bytes());

    let error = exchange_code(&mock, &oauth_config(), "the-code")
        .await
        .unwrap_err();

    let rendered = format!("{error} {error:?}");
    assert_absent(
        &rendered,
        &[USER_TOKEN, "expired; retry"],
        "error formatting",
    );
    assert_absent(&logs.text(), &[USER_TOKEN, "expired; retry"], "logs");
    assert!(rendered.contains("bad_verification_code"), "{rendered}");
    assert!(
        logs.text().contains("bad_verification_code"),
        "{}",
        logs.text()
    );
    mock.assert_exhausted();
}

/// A non-2xx from the OAuth token endpoint, with a body that reflects the
/// client secret we posted.
#[tokio::test]
async fn oauth_gateway_failure_discloses_nothing() {
    let (logs, _guard) = capture_logs();
    let body = format!(r#"{{"message":"rejected request with client_secret={CLIENT_SECRET}"}}"#);
    let mock = responds(TOKEN_URL, 503, body.into_bytes());

    let error = exchange_code(&mock, &oauth_config(), "the-code")
        .await
        .unwrap_err();

    let rendered = format!("{error} {error:?}");
    assert_absent(&rendered, &[CLIENT_SECRET], "error formatting");
    assert_absent(&logs.text(), &[CLIENT_SECRET], "logs");
    // 503 stays visible so the caller can still decide to retry.
    assert_eq!(error.status(), Some(503));
    mock.assert_exhausted();
}
