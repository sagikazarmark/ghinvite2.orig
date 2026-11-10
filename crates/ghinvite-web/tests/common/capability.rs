//! Public-flow fixture uses a real CSRF-protected capability exchange.
#![allow(dead_code)]
use axum::{body::Body, http::Request};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use ghinvite_core::storage::LinkCapabilityStorage;
use ghinvite_core::{InvitationLink, InvitationLinkId, LinkSecretState, SecretHash};
use http_body_util::BodyExt;
use sha2::{Digest, Sha256};
use tower::ServiceExt;

pub fn secret() -> String {
    URL_SAFE_NO_PAD.encode([7u8; 32])
}
pub fn state() -> LinkSecretState {
    LinkSecretState {
        sha256: SecretHash::from_bytes(Sha256::digest([7u8; 32]).into()),
        secret_generation: 1,
    }
}
pub fn pending(link_id: InvitationLinkId) -> ghinvite_web::session::PendingLink {
    let state = state();
    ghinvite_web::session::PendingLink {
        link_id,
        secret_sha256: state.sha256,
        secret_generation: state.secret_generation,
    }
}
pub async fn register(storage: &impl LinkCapabilityStorage, link: &InvitationLink) {
    storage
        .register_link_secret(&ghinvite_core::storage::link_secrets::LinkSecret {
            secret_sha256: link.secret.sha256,
            link_id: link.id,
            secret_generation: link.secret.secret_generation,
            created_at: link.created_at,
            retired_at: None,
        })
        .await
        .unwrap();
}
pub fn secret_for(id: InvitationLinkId) -> String {
    URL_SAFE_NO_PAD.encode(
        Sha256::new()
            .chain_update(b"ghinvite-test-link-secret:")
            .chain_update(id.to_string().as_bytes())
            .finalize(),
    )
}
pub fn state_for(id: InvitationLinkId) -> LinkSecretState {
    let bytes = URL_SAFE_NO_PAD.decode(secret_for(id)).unwrap();
    LinkSecretState {
        sha256: SecretHash::from_bytes(Sha256::digest(bytes).into()),
        secret_generation: 1,
    }
}
pub async fn select(app: &axum::Router, cookie: Option<&str>) -> String {
    select_secret(app, cookie, &secret()).await
}
pub async fn select_secret(app: &axum::Router, cookie: Option<&str>, supplied: &str) -> String {
    let mut request = Request::builder().uri("/i");
    if let Some(cookie) = cookie {
        request = request.header("cookie", cookie);
    }
    let response = app
        .clone()
        .oneshot(request.body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let cookie = super::sign_in::session_cookie(&response, cookie.map(str::to_owned));
    let html = String::from_utf8(
        response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec(),
    )
    .unwrap();
    let csrf = html
        .split("name=\"csrf_token\" value=\"")
        .nth(1)
        .unwrap()
        .split('"')
        .next()
        .unwrap();
    let response = app
        .clone()
        .oneshot(
            Request::post("/i")
                .header("cookie", &cookie)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from(format!("csrf_token={csrf}&secret={supplied}")))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), 303);
    assert_eq!(response.headers()["location"], "/join");
    super::sign_in::session_cookie(&response, Some(cookie))
}
