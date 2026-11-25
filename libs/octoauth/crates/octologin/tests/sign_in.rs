//! The sign-in flow against a local GitHub stand-in (wiremock). Ported from
//! gh-toolkit's `tests/signin_contract.rs` (sign-in only).

use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use octologin::{
    ApiError, AuthorizeOptions, OAuthErrorCode, PendingAuthorization, SignIn, SignInError,
    TransientKind, UserId, VerifiedEmail,
};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, UNIX_EPOCH};
use wiremock::matchers::{body_string_contains, header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const CLIENT_ID: &str = "Iv1.signin";
const SECRET: &str = "CLIENT_SECRET_MARKER";
const CALLBACK: &str = "https://app.test/callback";

fn token() -> String {
    format!("ghu_MARKER_{}", "x".repeat(640))
}

fn sign_in(server: &MockServer) -> SignIn {
    SignIn::builder(CLIENT_ID, SECRET, CALLBACK)
        .web_base_url(server.uri())
        .api_base_url(server.uri())
        .user_agent("octologin-test")
        .build()
        .unwrap()
}

fn token_answer() -> serde_json::Value {
    serde_json::json!({
        "access_token": token(),
        "token_type": "bearer",
        "refresh_token": "ghr_SECRET_MUST_DISCARD",
        "expires_in": 28800,
    })
}

async fn mount_exchange(server: &MockServer, answer: ResponseTemplate) {
    Mock::given(method("POST"))
        .and(path("/login/oauth/access_token"))
        .respond_with(answer)
        .mount(server)
        .await;
}

fn verifier() -> String {
    "v".repeat(43)
}

/// Everything that could leak, rendered.
fn assert_no_secrets(rendered: &str) {
    for secret in [SECRET, "CODE_MARKER", "ghu_MARKER", "ghr_SECRET", "PRIVATE"] {
        assert!(
            !rendered.contains(secret),
            "leaked {secret}: {rendered:.120}"
        );
    }
}

#[tokio::test]
async fn authorize_uses_state_pkce_and_no_scope() {
    let server = MockServer::start().await;
    let sign_in = sign_in(&server);
    let mut states = BTreeSet::new();
    for _ in 0..64 {
        let (url, pending) = sign_in.authorize(AuthorizeOptions::default()).unwrap();
        assert!(states.insert(pending.state.clone()), "states repeat");
        assert_eq!(URL_SAFE_NO_PAD.decode(&pending.state).unwrap().len(), 32);
        assert_eq!(
            URL_SAFE_NO_PAD
                .decode(pending.pkce_verifier.as_str())
                .unwrap()
                .len(),
            32
        );
        let url = reqwest::Url::parse(url.as_str()).unwrap();
        assert_eq!(url.path(), "/login/oauth/authorize");
        let pairs: BTreeMap<_, _> = url.query_pairs().collect();
        assert_eq!(pairs["client_id"], CLIENT_ID);
        assert_eq!(pairs["redirect_uri"], CALLBACK);
        assert_eq!(pairs["state"], pending.state);
        assert_eq!(pairs["code_challenge_method"], "S256");
        assert_eq!(
            pairs["code_challenge"],
            URL_SAFE_NO_PAD.encode(Sha256::digest(pending.pkce_verifier.as_bytes()))
        );
        assert!(!pairs.contains_key("scope"));
        assert_eq!(pairs.len(), 5);
        let state = pending.state.clone();
        assert_eq!(pending.verify(&state).unwrap().len(), 43);
    }

    let (url, pending) = sign_in
        .authorize(
            AuthorizeOptions::default()
                .allow_signup(false)
                .login("octocat"),
        )
        .unwrap();
    let pairs: BTreeMap<_, _> = reqwest::Url::parse(url.as_str())
        .unwrap()
        .query_pairs()
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    assert_eq!(pairs["allow_signup"], "false");
    assert_eq!(pairs["login"], "octocat");
    assert_eq!(
        format!("{url:?} {pending:?}"),
        "AuthorizeUrl(..) PendingAuthorization(..)"
    );
}

#[test]
fn state_is_checked_exactly() {
    let pending = || PendingAuthorization {
        state: "s".repeat(43),
        pkce_verifier: zeroize::Zeroizing::new(verifier()),
    };
    assert!(pending().verify(&"s".repeat(43)).is_ok());
    for received in [
        "".to_owned(),
        "s".repeat(42),
        "s".repeat(44),
        "x".repeat(43),
    ] {
        assert_eq!(
            pending().verify(&received).unwrap_err(),
            SignInError::StateMismatch
        );
    }
    // A stored state of the wrong length never matches, not even itself.
    let short = PendingAuthorization {
        state: String::new(),
        pkce_verifier: zeroize::Zeroizing::new(verifier()),
    };
    assert_eq!(short.verify("").unwrap_err(), SignInError::StateMismatch);
}

#[test]
fn configuration_is_validated() {
    let build = |client_id: &str, secret: &str, redirect: &str| {
        SignIn::builder(client_id, secret, redirect)
            .build()
            .map(|_| ())
            .map_err(|e| (e.field(), e.what()))
    };
    assert_eq!(build(CLIENT_ID, SECRET, CALLBACK), Ok(()));
    assert_eq!(build(CLIENT_ID, SECRET, "http://127.0.0.1:3000/cb"), Ok(()));
    assert_eq!(build(CLIENT_ID, SECRET, "http://localhost/cb?x=1"), Ok(()));
    for bad in [
        "https://user:secret@app.test/cb",
        "https://app.test/cb#fragment",
        "http://remote.test/cb",
        "javascript:alert(1)",
        "not a url",
    ] {
        assert_eq!(
            build(CLIENT_ID, SECRET, bad),
            Err(("redirect_uri", "url")),
            "{bad}"
        );
    }
    assert_eq!(
        build("", SECRET, CALLBACK),
        Err(("client_id", "identifier"))
    );
    assert_eq!(
        build("a:b", SECRET, CALLBACK),
        Err(("client_id", "identifier"))
    );
    assert_eq!(
        build(CLIENT_ID, "", CALLBACK),
        Err(("client_secret", "printable_ascii"))
    );
    assert_eq!(
        build(CLIENT_ID, "a b", CALLBACK),
        Err(("client_secret", "printable_ascii"))
    );
    let base = |url: &str| {
        SignIn::builder(CLIENT_ID, SECRET, CALLBACK)
            .web_base_url(url)
            .build()
            .map(|_| ())
            .map_err(|e| (e.field(), e.what()))
    };
    assert_eq!(base("https://ghe.example"), Ok(()));
    assert_eq!(
        base("https://ghe.example/?x=1"),
        Err(("web_base_url", "url"))
    );
    assert_eq!(base("http://ghe.example"), Err(("web_base_url", "url")));

    let sign_in = SignIn::builder(CLIENT_ID, SECRET, CALLBACK)
        .build()
        .unwrap();
    assert_no_secrets(&format!("{sign_in:?}"));
    assert_no_secrets(&format!(
        "{:?}",
        SignIn::builder(CLIENT_ID, SECRET, CALLBACK)
    ));
}

#[tokio::test]
async fn the_whole_flow() {
    let server = MockServer::start().await;
    mount_exchange(
        &server,
        ResponseTemplate::new(200).set_body_json(token_answer()),
    )
    .await;
    let bearer = format!("Bearer {}", token());
    Mock::given(method("GET"))
        .and(path("/user"))
        .and(header("authorization", bearer.as_str()))
        .and(header("x-github-api-version", "2022-11-28"))
        .and(header("accept", "application/vnd.github+json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": 42, "login": "octocat", "avatar_url": null,
            "created_at": "2011-01-25T18:44:36Z", "name": "The Octocat"
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/user/emails"))
        .and(query_param("per_page", "100"))
        .and(header("authorization", bearer.as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            {"email": "PRIVATE@example.test", "primary": true, "verified": true, "visibility": "private"},
            {"email": "unverified@example.test", "primary": false, "verified": false, "visibility": null},
        ])))
        .mount(&server)
        .await;
    let basic = format!("Basic {}", STANDARD.encode(format!("{CLIENT_ID}:{SECRET}")));
    Mock::given(method("DELETE"))
        .and(path("/applications/Iv1.signin/token"))
        .and(header("authorization", basic.as_str()))
        .and(header("content-type", "application/json"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;

    let sign_in = sign_in(&server);
    let token = sign_in.exchange("CODE_MARKER", &verifier()).await.unwrap();
    assert_eq!(token.client_id(), CLIENT_ID);
    assert_eq!(token.expose_secret(), self::token());
    assert_no_secrets(&format!("{token:?}"));

    let user = sign_in.user(&token).await.unwrap();
    assert_eq!(user.id, UserId::new(42));
    assert_eq!(user.login, "octocat");
    assert_eq!(user.avatar_url, None);
    assert_eq!(
        user.created_at,
        Some(UNIX_EPOCH + Duration::from_secs(1_295_981_076))
    );

    // Only verified addresses come back.
    let emails = sign_in.emails(&token).await.unwrap();
    assert_eq!(emails, [VerifiedEmail::new("PRIVATE@example.test", true)]);
    assert_no_secrets(&format!("{emails:?}"));

    sign_in.revoke(token).await.unwrap();

    let requests = server.received_requests().await.unwrap();
    let exchange = &requests[0];
    let headers: BTreeSet<_> = exchange
        .headers
        .keys()
        .map(|k| k.as_str().to_owned())
        .collect();
    for expected in ["accept", "content-type", "user-agent"] {
        assert!(headers.contains(expected), "{expected}");
    }
    assert!(!headers.contains("authorization"));
    assert_eq!(exchange.headers["accept"], "application/json");
    assert_eq!(
        exchange.headers["content-type"],
        "application/x-www-form-urlencoded"
    );
    assert_eq!(exchange.headers["user-agent"], "octologin-test");
    let form: BTreeMap<_, _> = url::form_urlencoded::parse(&exchange.body)
        .into_owned()
        .collect();
    assert_eq!(
        form,
        BTreeMap::from([
            ("client_id".to_owned(), CLIENT_ID.to_owned()),
            ("client_secret".to_owned(), SECRET.to_owned()),
            ("code".to_owned(), "CODE_MARKER".to_owned()),
            ("code_verifier".to_owned(), verifier()),
            ("redirect_uri".to_owned(), CALLBACK.to_owned()),
        ])
    );
    let revoke: serde_json::Value = serde_json::from_slice(&requests[3].body).unwrap();
    assert_eq!(revoke, serde_json::json!({"access_token": self::token()}));
}

#[tokio::test]
async fn provider_errors_keep_only_the_code() {
    for status in [200, 400, 401, 500] {
        let server = MockServer::start().await;
        mount_exchange(
            &server,
            ResponseTemplate::new(status).set_body_json(serde_json::json!({
                "error": "bad_verification_code",
                "error_description": SECRET,
            })),
        )
        .await;
        let error = sign_in(&server)
            .exchange("CODE_MARKER", &verifier())
            .await
            .unwrap_err();
        assert_eq!(
            error,
            SignInError::Provider {
                code: OAuthErrorCode::BadVerificationCode
            }
        );
        assert_no_secrets(&format!("{error:?} {error}"));
    }

    // Unknown codes are not kept; escaped ones are read as JSON.
    for (body, code) in [
        (
            r#"{"error":"ghu_MARKER_unbounded_secret","error_description":"PRIVATE"}"#,
            OAuthErrorCode::Unrecognized,
        ),
        (
            r#"{"error":"bad_\u0076erification_code","error_description":"CLIENT_SECRET_MARKER"}"#,
            OAuthErrorCode::BadVerificationCode,
        ),
    ] {
        let server = MockServer::start().await;
        mount_exchange(
            &server,
            ResponseTemplate::new(400).set_body_raw(body, "application/json"),
        )
        .await;
        let error = sign_in(&server)
            .exchange("CODE_MARKER", &verifier())
            .await
            .unwrap_err();
        assert_eq!(error, SignInError::Provider { code });
        assert_no_secrets(&format!("{error:?} {error}"));
    }
    assert_eq!(
        OAuthErrorCode::from_github("access_denied"),
        OAuthErrorCode::AccessDenied
    );
    assert_eq!(OAuthErrorCode::AccessDenied.code(), "access_denied");
}

#[tokio::test]
async fn malformed_answers_are_decode_errors_without_secrets() {
    for answer in [
        serde_json::json!({"token_type": "bearer"}),
        serde_json::json!({"access_token": "", "token_type": "bearer"}),
        serde_json::json!({"access_token": "ghu_MARKER_\n", "token_type": "bearer"}),
        serde_json::json!({"access_token": "ghu_MARKER_token", "token_type": SECRET}),
        serde_json::json!({"access_token": "ghu_MARKER_token"}),
        serde_json::json!({"access_token": 7, "token_type": "bearer", "PRIVATE": token()}),
        serde_json::json!(["ghu_MARKER_token"]),
    ] {
        let server = MockServer::start().await;
        mount_exchange(&server, ResponseTemplate::new(200).set_body_json(&answer)).await;
        let error = sign_in(&server)
            .exchange("CODE_MARKER", &verifier())
            .await
            .unwrap_err();
        assert_eq!(error, SignInError::Decode, "{answer}");
        assert_no_secrets(&format!("{error:?} {error}"));
    }

    // An error status without an OAuth error is classified, without its text.
    let server = MockServer::start().await;
    mount_exchange(
        &server,
        ResponseTemplate::new(502).set_body_raw(
            format!("<html>Authorization: Bearer {}</html>", token()),
            "text/html",
        ),
    )
    .await;
    let error = sign_in(&server)
        .exchange("CODE_MARKER", &verifier())
        .await
        .unwrap_err();
    assert_eq!(error, SignInError::Api(ApiError::Server { status: 502 }));
    assert_no_secrets(&format!("{error:?} {error}"));
}

#[tokio::test]
async fn invalid_input_and_foreign_tokens_send_nothing() {
    let server = MockServer::start().await;
    mount_exchange(
        &server,
        ResponseTemplate::new(200).set_body_json(token_answer()),
    )
    .await;
    let sign_in = sign_in(&server);
    for (code, verifier) in [
        ("", verifier()),
        ("code with space", verifier()),
        (&*"c".repeat(1025), verifier()),
        ("code", "short".to_owned()),
        ("code", "v".repeat(129)),
        ("code", format!("{}+", "v".repeat(43))),
    ] {
        assert_eq!(
            sign_in.exchange(code, &verifier).await.unwrap_err(),
            SignInError::InvalidInput
        );
    }
    assert!(server.received_requests().await.unwrap().is_empty());

    // A token obtained through another app is not revoked through this one.
    let token = sign_in.exchange("code", &verifier()).await.unwrap();
    let other = SignIn::builder("other-client", "other-secret", CALLBACK)
        .api_base_url(server.uri())
        .build()
        .unwrap();
    assert_eq!(
        other.revoke(token).await.unwrap_err(),
        SignInError::ForeignToken
    );
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
}

#[tokio::test]
async fn more_than_one_page_of_emails_is_refused() {
    let server = MockServer::start().await;
    mount_exchange(
        &server,
        ResponseTemplate::new(200).set_body_json(token_answer()),
    )
    .await;
    for (link, incomplete) in [
        (
            r#"<https://api.github.com/user/emails?page=2>; rel="next", <https://api.github.com/user/emails?page=3>; rel="last""#,
            true,
        ),
        (
            r#"<https://api.github.com/user/emails?page=2>; rel="last next""#,
            true,
        ),
        (
            r#"<https://api.github.com/user/emails?page=1>; rel="prev""#,
            false,
        ),
    ] {
        server.reset().await;
        mount_exchange(
            &server,
            ResponseTemplate::new(200).set_body_json(token_answer()),
        )
        .await;
        Mock::given(method("GET"))
            .and(path("/user/emails"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("link", link)
                    .set_body_json(serde_json::json!([])),
            )
            .mount(&server)
            .await;
        let sign_in = sign_in(&server);
        let token = sign_in.exchange("code", &verifier()).await.unwrap();
        let result = sign_in.emails(&token).await;
        if incomplete {
            assert_eq!(result.unwrap_err(), SignInError::Incomplete, "{link}");
        } else {
            assert_eq!(result.unwrap(), vec![], "{link}");
        }
    }
}

#[tokio::test]
async fn api_errors_are_classified_and_redirects_not_followed() {
    let server = MockServer::start().await;
    mount_exchange(
        &server,
        ResponseTemplate::new(200).set_body_json(token_answer()),
    )
    .await;
    Mock::given(method("GET"))
        .and(path("/user"))
        .respond_with(
            ResponseTemplate::new(403)
                .insert_header("retry-after", "30")
                .set_body_json(serde_json::json!({"message": "secondary rate limit PRIVATE"})),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/user/emails"))
        .respond_with(ResponseTemplate::new(302).insert_header("location", "/elsewhere"))
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/applications/Iv1.signin/token"))
        .and(body_string_contains("ghu_MARKER"))
        .respond_with(ResponseTemplate::new(422).set_body_json(
            serde_json::json!({"message": token(), "errors": [{"code": "invalid"}]}),
        ))
        .mount(&server)
        .await;
    let sign_in = sign_in(&server);
    let token = sign_in.exchange("code", &verifier()).await.unwrap();

    let error = sign_in.user(&token).await.unwrap_err();
    assert_eq!(error.code(), "gh:api:rate_limited:secondary");
    assert_no_secrets(&format!("{error:?} {error}"));

    assert_eq!(
        sign_in.emails(&token).await.unwrap_err(),
        SignInError::Api(ApiError::Status { status: 302 })
    );

    let error = sign_in.revoke(token).await.unwrap_err();
    assert!(matches!(
        error,
        SignInError::Api(ApiError::Validation { .. })
    ));
    assert_no_secrets(&format!("{error:?} {error}"));
}

#[tokio::test]
async fn redirects_are_refused() {
    let server = MockServer::start().await;
    mount_exchange(
        &server,
        ResponseTemplate::new(307).insert_header("location", "/elsewhere"),
    )
    .await;
    Mock::given(method("POST"))
        .and(path("/elsewhere"))
        .respond_with(ResponseTemplate::new(200).set_body_json(token_answer()))
        .mount(&server)
        .await;

    // The default client does not follow.
    let error = sign_in(&server)
        .exchange("code", &verifier())
        .await
        .unwrap_err();
    assert_eq!(error, SignInError::Api(ApiError::Status { status: 307 }));
    assert_eq!(server.received_requests().await.unwrap().len(), 1);

    // A client that follows is caught, and the planted answer discarded.
    let following = SignIn::builder(CLIENT_ID, SECRET, CALLBACK)
        .web_base_url(server.uri())
        .client(reqwest::Client::new())
        .build()
        .unwrap();
    assert_eq!(
        following.exchange("code", &verifier()).await.unwrap_err(),
        SignInError::Redirected
    );
}

#[tokio::test]
async fn transport_failures_are_transient() {
    // Nothing listens here.
    let sign_in = SignIn::builder(CLIENT_ID, SECRET, CALLBACK)
        .web_base_url("http://127.0.0.1:9")
        .build()
        .unwrap();
    assert_eq!(
        sign_in.exchange("code", &verifier()).await.unwrap_err(),
        SignInError::Transient {
            kind: TransientKind::NotSent
        }
    );
}

#[tokio::test]
async fn identities_must_be_real() {
    for body in [
        serde_json::json!({"id": 0, "login": "octocat"}),
        serde_json::json!({"id": 42, "login": ""}),
        serde_json::json!({"login": "octocat"}),
    ] {
        let server = MockServer::start().await;
        mount_exchange(
            &server,
            ResponseTemplate::new(200).set_body_json(token_answer()),
        )
        .await;
        Mock::given(method("GET"))
            .and(path("/user"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&body))
            .mount(&server)
            .await;
        let sign_in = sign_in(&server);
        let token = sign_in.exchange("code", &verifier()).await.unwrap();
        assert_eq!(
            sign_in.user(&token).await.unwrap_err(),
            SignInError::Decode,
            "{body}"
        );
    }
}

/// An exchange that yields `token()`, `GET /user`, and a revocation
/// answering `revoke_status`.
async fn with_user_token_server(revoke_status: u16) -> MockServer {
    let server = MockServer::start().await;
    mount_exchange(
        &server,
        ResponseTemplate::new(200).set_body_json(token_answer()),
    )
    .await;
    Mock::given(method("GET"))
        .and(path("/user"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": 42, "login": "octocat"
        })))
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/applications/Iv1.signin/token"))
        .respond_with(ResponseTemplate::new(revoke_status))
        .mount(&server)
        .await;
    server
}

async fn revocations(server: &MockServer) -> usize {
    server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.method.as_str() == "DELETE")
        .count()
}

#[tokio::test]
async fn with_user_token_reads_then_revokes() {
    let server = with_user_token_server(204).await;
    let sign_in = sign_in(&server);
    let signed_in = sign_in
        .with_user_token("CODE_MARKER", &verifier(), async |token| {
            assert_eq!(token.expose_secret(), self::token());
            sign_in.user(token).await
        })
        .await
        .unwrap();
    assert_eq!(signed_in.value.id, UserId::new(42));
    assert_eq!(signed_in.revocation, Ok(()));
    let requests = server.received_requests().await.unwrap();
    let paths: Vec<_> = requests.iter().map(|r| r.url.path()).collect();
    assert_eq!(
        paths,
        [
            "/login/oauth/access_token",
            "/user",
            "/applications/Iv1.signin/token"
        ]
    );
}

#[tokio::test]
async fn with_user_token_revokes_after_a_failed_read() {
    let server = with_user_token_server(204).await;
    let sign_in = sign_in(&server);
    let error = sign_in
        .with_user_token("CODE_MARKER", &verifier(), async |_| {
            Err::<(), _>("read failed")
        })
        .await
        .unwrap_err();
    assert_eq!(error, octologin::WithUserTokenError::Call("read failed"));
    assert_eq!(revocations(&server).await, 1);
}

#[tokio::test]
async fn with_user_token_reports_a_failed_revocation() {
    let server = with_user_token_server(500).await;
    let sign_in = sign_in(&server);
    let signed_in = sign_in
        .with_user_token("CODE_MARKER", &verifier(), async |token| {
            sign_in.user(token).await
        })
        .await
        .unwrap();
    assert_eq!(signed_in.value.login, "octocat");
    let error = signed_in.revocation.unwrap_err();
    assert!(error.to_string().starts_with("gh:"), "{error}");
    assert_eq!(revocations(&server).await, 1);
}

#[tokio::test]
async fn with_user_token_without_a_token_neither_reads_nor_revokes() {
    let server = MockServer::start().await;
    mount_exchange(
        &server,
        ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "error": "bad_verification_code"
        })),
    )
    .await;
    let sign_in = sign_in(&server);
    let error = sign_in
        .with_user_token(
            "CODE_MARKER",
            &verifier(),
            async |_| -> Result<(), SignInError> { panic!("read ran without a token") },
        )
        .await
        .unwrap_err();
    assert!(
        matches!(
            error,
            octologin::WithUserTokenError::SignIn(SignInError::Provider {
                code: OAuthErrorCode::BadVerificationCode,
                ..
            })
        ),
        "{error:?}"
    );
    // Flattened into the caller's error.
    assert!(matches!(error.into_error(), SignInError::Provider { .. }));
    assert_eq!(revocations(&server).await, 0);
}
