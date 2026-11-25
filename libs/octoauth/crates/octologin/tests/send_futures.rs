//! Natively every request future is `Send`, so multi-threaded hosts (axum,
//! `tokio::spawn`) can await them directly. On wasm32 they are not.
#![cfg(not(target_arch = "wasm32"))]

use octologin::{SignIn, UserToken};

fn assert_send<T: Send>(_: T) {}

/// Compiles only if every future is `Send`; never called.
#[allow(dead_code)]
fn request_futures_are_send(sign_in: &SignIn, token: UserToken) {
    assert_send(sign_in.exchange("code", "verifier"));
    assert_send(sign_in.user(&token));
    assert_send(sign_in.emails(&token));
    assert_send(sign_in.revoke(token));
    assert_send(
        sign_in.with_user_token("code", "verifier", async |token| sign_in.user(token).await),
    );
}

#[test]
fn request_futures_are_send_natively() {}
