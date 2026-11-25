//! The sign-in flow end to end against the GitHub emulator (`octoemu`,
//! GE-05) instead of wiremock: the authorize redirect (auto-consent for
//! `login=`), the callback's code and state, the PKCE exchange, `/user`,
//! `/user/emails` (verified only; more than one page is `Incomplete`),
//! revocation, and GitHub's error codes.
#![cfg(not(target_arch = "wasm32"))]

use octoemu::Emulator;
use octoemu::seed::{Access, App, Email, EmailVisibility, Seed, User};
use octologin::{
    ApiError, AuthorizeOptions, OAuthErrorCode, SignIn, SignInError, UserId, VerifiedEmail,
};
use std::collections::BTreeMap;
use std::time::{Duration, UNIX_EPOCH};

const PUBLIC_KEY: &str = include_str!("fixtures/rsa2048.pub.pem");
const CLIENT_ID: &str = "Iv23.signin";
const SECRET: &str = "signin-client-secret";
const CALLBACK: &str = "https://app.test/callback";

/// octocat (two verified addresses and an unverified one), many (101
/// addresses: more than a page), plain (no addresses); the sign-in App with
/// `email_addresses: read`, and one without it.
fn seed() -> Seed {
    let mut many = User::new(3, "many");
    for n in 0..101 {
        let email = Email::new(format!("a{n:03}@example.com"));
        many = many.email_address(if n == 0 {
            email.primary(EmailVisibility::Private)
        } else {
            email
        });
    }
    Seed::new()
        .start_time("2030-01-01T00:00:00Z".parse().unwrap())
        .user(
            User::new(1, "octocat")
                .created_at("2011-01-25T18:44:36Z".parse().unwrap())
                .email_address(Email::new("octocat@example.com").primary(EmailVisibility::Public))
                .email_address(Email::new("work@example.com"))
                .email_address(Email::new("stale@example.com").unverified()),
        )
        .user(User::new(2, "plain"))
        .user(many)
        .app(
            App::new(4, "signin", CLIENT_ID, SECRET)
                .public_key(PUBLIC_KEY)
                .callback_url(CALLBACK)
                .user_permission("email_addresses", Access::Read),
        )
        .app(
            App::new(5, "bare", "Iv23.bare", "bare-secret")
                .public_key(PUBLIC_KEY)
                .callback_url(CALLBACK),
        )
}

fn sign_in(emulator: &Emulator, client_id: &str, secret: &str) -> SignIn {
    SignIn::builder(client_id, secret, CALLBACK)
        .web_base_url(emulator.web_base())
        .api_base_url(emulator.api_base())
        .user_agent("octologin-emulator-test")
        .build()
        .unwrap()
}

/// Follows an authorize URL as a browser would, up to the redirect back to
/// the callback; that URL's query.
async fn browse(url: &str) -> BTreeMap<String, String> {
    let browser = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let response = browser.get(url).send().await.unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::FOUND);
    let location = response.headers()["location"].to_str().unwrap();
    assert!(location.starts_with(CALLBACK), "{location}");
    reqwest::Url::parse(location)
        .unwrap()
        .query_pairs()
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect()
}

#[tokio::test]
async fn signs_in_reads_the_user_and_revokes() {
    let emulator = Emulator::start(seed()).await.unwrap();
    let sign_in = sign_in(&emulator, CLIENT_ID, SECRET);
    let (url, pending) = sign_in
        .authorize(AuthorizeOptions::default().login("octocat"))
        .unwrap();
    let callback = browse(url.as_str()).await;
    let verifier = pending.verify(&callback["state"]).unwrap();
    let token = sign_in
        .exchange(&callback["code"], &verifier)
        .await
        .unwrap();
    assert!(token.expose_secret().starts_with("ghu_"));
    assert_eq!(token.client_id(), CLIENT_ID);

    let user = sign_in.user(&token).await.unwrap();
    assert_eq!(user.id, UserId::new(1));
    assert_eq!(user.login, "octocat");
    assert!(user.avatar_url.unwrap().ends_with("/avatars/u/1?v=4"));
    assert_eq!(
        user.created_at,
        Some(UNIX_EPOCH + Duration::from_secs(1_295_981_076))
    );
    assert_eq!(
        sign_in.emails(&token).await.unwrap(),
        [
            VerifiedEmail::new("octocat@example.com", true),
            VerifiedEmail::new("work@example.com", false),
        ]
    );

    // The code worked once.
    assert_eq!(
        sign_in.exchange(&callback["code"], &verifier).await.err(),
        Some(SignInError::Provider {
            code: OAuthErrorCode::BadVerificationCode
        })
    );

    let secret = token.expose_secret().to_owned();
    sign_in.revoke(token).await.unwrap();
    let dead = reqwest::Client::new()
        .get(format!("{}/user", emulator.api_base()))
        .bearer_auth(&secret)
        .header("user-agent", "test")
        .send()
        .await
        .unwrap();
    assert_eq!(dead.status(), reqwest::StatusCode::UNAUTHORIZED);
    // The emulator saw the App revoke it with its client credentials.
    let revoke = emulator
        .calls()
        .into_iter()
        .find(|c| c.route.as_deref() == Some("DELETE /applications/{client_id}/token"))
        .unwrap();
    assert_eq!(revoke.status, 204);
    assert_eq!(
        serde_json::to_value(&revoke.identity).unwrap(),
        serde_json::json!({"kind": "client", "app_id": 4})
    );
}

#[tokio::test]
async fn a_declined_authorization_comes_back_as_access_denied() {
    let emulator = Emulator::start(seed()).await.unwrap();
    emulator.deny_authorization(Some("octocat"));
    let sign_in = sign_in(&emulator, CLIENT_ID, SECRET);
    let (url, pending) = sign_in
        .authorize(AuthorizeOptions::default().login("octocat"))
        .unwrap();
    let callback = browse(url.as_str()).await;
    assert!(!callback.contains_key("code"));
    assert_eq!(
        OAuthErrorCode::from_github(&callback["error"]),
        OAuthErrorCode::AccessDenied
    );
    // The state still comes back, so the callback can be matched.
    assert!(pending.verify(&callback["state"]).is_ok());
}

#[tokio::test]
async fn more_than_one_page_of_emails_is_incomplete() {
    let emulator = Emulator::start(seed()).await.unwrap();
    let sign_in = sign_in(&emulator, CLIENT_ID, SECRET);
    let (url, pending) = sign_in
        .authorize(AuthorizeOptions::default().login("many"))
        .unwrap();
    let callback = browse(url.as_str()).await;
    let verifier = pending.verify(&callback["state"]).unwrap();
    let token = sign_in
        .exchange(&callback["code"], &verifier)
        .await
        .unwrap();
    assert_eq!(sign_in.emails(&token).await, Err(SignInError::Incomplete));
    // A user without addresses has none.
    let (url, pending) = sign_in
        .authorize(AuthorizeOptions::default().login("plain"))
        .unwrap();
    let callback = browse(url.as_str()).await;
    let verifier = pending.verify(&callback["state"]).unwrap();
    let token = sign_in
        .exchange(&callback["code"], &verifier)
        .await
        .unwrap();
    assert_eq!(sign_in.emails(&token).await, Ok(Vec::new()));
}

#[tokio::test]
async fn githubs_error_codes_reach_the_caller() {
    let emulator = Emulator::start(seed()).await.unwrap();
    let sign_in = sign_in(&emulator, CLIENT_ID, SECRET);
    let (url, pending) = sign_in
        .authorize(AuthorizeOptions::default().login("octocat"))
        .unwrap();
    let callback = browse(url.as_str()).await;
    assert!(pending.verify(&callback["state"]).is_ok());

    // Another verifier than the challenge's.
    assert_eq!(
        sign_in
            .exchange(&callback["code"], &"x".repeat(43))
            .await
            .err(),
        Some(SignInError::Provider {
            code: OAuthErrorCode::BadVerificationCode
        })
    );
    // The wrong secret.
    let (url, pending) = sign_in
        .authorize(AuthorizeOptions::default().login("octocat"))
        .unwrap();
    let callback = browse(url.as_str()).await;
    let wrong = self::sign_in(&emulator, CLIENT_ID, "not-the-secret");
    assert_eq!(
        wrong
            .exchange(
                &callback["code"],
                &pending.verify(&callback["state"]).unwrap()
            )
            .await
            .err(),
        Some(SignInError::Provider {
            code: OAuthErrorCode::IncorrectClientCredentials
        })
    );

    // An App without email_addresses: read cannot list addresses.
    let bare = self::sign_in(&emulator, "Iv23.bare", "bare-secret");
    let (url, pending) = bare
        .authorize(AuthorizeOptions::default().login("octocat"))
        .unwrap();
    let callback = browse(url.as_str()).await;
    let token = bare
        .exchange(
            &callback["code"],
            &pending.verify(&callback["state"]).unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(bare.user(&token).await.unwrap().login, "octocat");
    assert!(matches!(
        bare.emails(&token).await,
        Err(SignInError::Api(ApiError::Forbidden { .. }))
    ));
    // Only the App that obtained a token may revoke it.
    assert_eq!(
        sign_in.revoke(token).await.err(),
        Some(SignInError::ForeignToken)
    );
}
