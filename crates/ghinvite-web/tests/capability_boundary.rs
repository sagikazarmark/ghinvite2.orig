//! Browser capability boundaries, including anonymous CSRF and OAuth continuity.
use axum::{
    body::Body,
    http::{Request, StatusCode},
    response::Response,
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::Utc;
use ghinvite_core::invitation_link::{AccountAdmin, CreateLink, LinkSnapshot, RotateSecret};
use ghinvite_core::storage::LinkCapabilityStorage;
use ghinvite_core::{
    InvitationLinkId, SecretHash,
    targets::{RepoRole, RepoTarget, Targets},
};
use ghinvite_github::mocks::MockTransport;
use http_body_util::BodyExt;
use sha2::{Digest, Sha256};
use std::sync::Arc;
use tower::ServiceExt;
mod common;

async fn html(response: Response) -> String {
    String::from_utf8(
        response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec(),
    )
    .unwrap()
}
async fn get(app: &axum::Router, path: &str, cookie: Option<&str>) -> Response {
    let mut req = Request::get(path);
    if let Some(cookie) = cookie {
        req = req.header("cookie", cookie);
    }
    app.clone()
        .oneshot(req.body(Body::empty()).unwrap())
        .await
        .unwrap()
}
async fn post(app: &axum::Router, path: &str, cookie: &str, body: String) -> Response {
    app.clone()
        .oneshot(
            Request::post(path)
                .header("cookie", cookie)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn anonymous_code_exchange_requires_csrf_and_carries_only_hash_through_oauth() {
    let authority = common::authority_http_fixture::FakeLinkAuthority::start().await;
    let storage = Arc::new(
        ghinvite_storage_sqlx::SqlxStorage::in_memory()
            .await
            .unwrap(),
    );
    let id = InvitationLinkId::new();
    let secret = common::capability::state();
    let now = Utc::now();
    authority.seed(LinkSnapshot {
        metadata: None,
        link_id: id,
        secret: secret.clone(),
        created_at: now,
        revoked_at: None,
        revoked_by: None,
        pause: None,
        uses: 0,
        revision: 1,
        creation: CreateLink {
            batch_source: None,
            access_lifetime_seconds: None,
            email_domains: vec![],
            link_id: id,
            secret_sha256: secret.sha256,
            admin: AccountAdmin {
                account_id: 1,
                user_id: 1,
            },
            account_id: 1,
            organization_installation_id: None,
            installation_id: 77,
            description: "Private admin metadata".into(),
            internal_note: None,
            expires_at: None,
            max_uses: None,
            invitation_lifetime_seconds: 604800,
            abuse_threshold_per_hour: None,
            budget_share_pct: 50,
            approval_required: true,
            targets: Targets {
                repos: vec![RepoTarget {
                    repo_id: 10,
                    repo_full_name: "acme/api".into(),
                    grant: RepoRole::Read,
                }],
                ..Default::default()
            },
        },
    });
    // No projected link exists. The index is independently usable.
    storage
        .register_link_secret(&ghinvite_core::storage::link_secrets::LinkSecret {
            link_id: id,
            secret_sha256: secret.sha256,
            secret_generation: 1,
            created_at: now,
            retired_at: None,
        })
        .await
        .unwrap();
    let app = common::build_app(
        crate::common::sign_in::app_state(
            storage.clone(),
            Arc::new(MockTransport::scripted(
                common::sign_in::oauth_expectations(common::sign_in::OCTOCAT),
            )),
            authority.client(),
            ghinvite_web::WebConfig::for_local_dev_with_secret([9; 32]),
        ),
        tower_sessions::MemoryStore::default(),
    );
    let response = get(&app, "/code", None).await;
    let cookie = common::sign_in::session_cookie(&response, None);
    assert_eq!(response.headers()["cache-control"], "no-store");
    assert_eq!(response.headers()["referrer-policy"], "no-referrer");
    let page = html(response).await;
    let token = page
        .split("name=\"csrf_token\" value=\"")
        .nth(1)
        .unwrap()
        .split('"')
        .next()
        .unwrap();
    let raw = common::capability::secret();
    for body in [
        format!("secret={raw}"),
        format!("csrf_token=wrong&secret={raw}"),
        format!("csrf_token={token}&csrf_token={token}&secret={raw}"),
    ] {
        let response = post(&app, "/code", &cookie, body).await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert!(!html(response).await.contains(&raw));
    }
    for invalid in [
        id.to_string(),
        "A".repeat(43),
        format!("{raw}="),
        "x".repeat(43),
    ] {
        let response = post(
            &app,
            "/code",
            &cookie,
            format!("csrf_token={token}&secret={invalid}"),
        )
        .await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert!(!html(response).await.contains(&invalid));
    }
    assert!(authority.calls().is_empty());
    let response = post(
        &app,
        "/code",
        &cookie,
        format!("csrf_token={token}&secret={raw}"),
    )
    .await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(response.headers()["location"], "/join");
    let cookie = common::sign_in::session_cookie(&response, Some(cookie));
    let response = get(&app, "/join", Some(&cookie)).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(!format!("{:?}", response.headers()).contains(&raw));
    let offer = html(response).await;
    assert!(offer.contains("href=\"/login?return_to=%2Fjoin\""));
    assert!(offer.contains("Continue with GitHub"));
    assert!(offer.contains("acme/api"));
    assert!(!offer.contains(&raw));
    assert!(!offer.contains("Private admin metadata"));
    let cookie = common::sign_in::sign_in_with_cookie(&app, Some(cookie)).await;
    let response = get(&app, "/join", Some(&cookie)).await;
    assert_eq!(response.status(), StatusCode::OK);
    let page = html(response).await;
    assert!(page.contains("acme/api"));
    assert!(!page.contains(&raw));
    assert!(!page.contains("Private admin metadata"));
    let queries = authority.received::<serde_json::Value>("requester_page");
    assert_eq!(queries[0]["secret_sha256"], secret.sha256.as_hex());
    assert!(!serde_json::to_string(&queries).unwrap().contains(&raw));

    // Stale routing remains harmless while projection retirement is delayed.
    let rotated_bytes = [8u8; 32];
    let rotated_hash = SecretHash::from_bytes(Sha256::digest(rotated_bytes).into());
    let rotated = ghinvite_web::LinkAuthority::new(authority.client())
        .rotate_secret(RotateSecret {
            link_id: id,
            operation_id: ghinvite_core::RequestId::new()
                .to_string()
                .try_into()
                .unwrap(),
            admin: AccountAdmin {
                account_id: 1,
                user_id: 1,
            },
            new_sha256: rotated_hash,
        })
        .await
        .unwrap();
    assert_eq!(
        get(&app, "/join", Some(&cookie)).await.status(),
        StatusCode::NOT_FOUND
    );
    storage
        .register_link_secret(&ghinvite_core::storage::link_secrets::LinkSecret {
            link_id: id,
            secret_sha256: rotated_hash,
            secret_generation: rotated.secret_generation,
            created_at: now,
            retired_at: None,
        })
        .await
        .unwrap();
    let cookie = common::capability::select_secret(
        &app,
        Some(&cookie),
        &URL_SAFE_NO_PAD.encode(rotated_bytes),
    )
    .await;
    assert_eq!(
        get(&app, "/join", Some(&cookie)).await.status(),
        StatusCode::OK
    );
    for legacy in [format!("/i/{id}"), format!("/i/{raw}/request")] {
        let response = get(&app, &legacy, Some(&cookie)).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert!(response.headers().get("location").is_none());
        assert!(!html(response).await.contains(&raw));
    }
}
