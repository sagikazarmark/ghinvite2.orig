use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use ghinvite_github::Method;
use ghinvite_github::mocks::{Expectation, MockTransport};
use ghinvite_web::WebConfig;
use std::sync::Arc;
use tower::ServiceExt;
mod common;
use common::{
    restate_recorder::RestateRecorder,
    sign_in::{OCTOCAT, oauth_expectations, sign_in},
};

struct AssertAppJwt(MockTransport);
impl ghinvite_github::HttpTransport for AssertAppJwt {
    async fn send(
        &self,
        request: ghinvite_github::Request,
    ) -> Result<ghinvite_github::Response, ghinvite_github::TransportError> {
        if !request.url.contains("/app/installations/") {
            return self.0.send(request).await;
        }
        use base64::Engine as _;
        assert!(request.url.contains("/app/installations/"));
        let credential = request.headers["authorization"]
            .strip_prefix("Bearer ")
            .expect("App JWT bearer");
        let segments: Vec<_> = credential.split('.').collect();
        assert_eq!(segments.len(), 3);
        let claims: serde_json::Value = serde_json::from_slice(
            &base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(segments[1])
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            claims["iss"],
            format!(
                "Iv.fixture-{}",
                request.headers["user-agent"].strip_prefix("p1/").unwrap()
            )
        );
        self.0.send(request).await
    }
}

async fn app(mock: MockTransport) -> (axum::Router, RestateRecorder) {
    let storage = Arc::new(
        ghinvite_storage_sqlx::SqlxStorage::in_memory()
            .await
            .unwrap(),
    );
    let restate = RestateRecorder::start().await;
    let config = WebConfig::for_local_dev_with_secret([7; 32]);
    (
        common::build_app(
            crate::common::sign_in::app_state(
                storage,
                Arc::new(AssertAppJwt(mock)),
                restate.client(),
                config,
            ),
            tower_sessions::MemoryStore::default(),
        ),
        restate,
    )
}

#[tokio::test]
async fn unauthenticated_setup_preserves_app_return_path_and_unknown_apps_are_404() {
    let (app, restate) = app(MockTransport::scripted(vec![])).await;
    for name in ["github", "signin", "other"] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/setup/{name}?installation_id=77"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }
    let response = app
        .oneshot(
            Request::builder()
                .uri("/setup/repo?installation_id=77&setup_action=update")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        response.headers()["location"],
        "/login?return_to=%2Fsetup%2Frepo%3Finstallation_id%3D77%26setup_action%3Dupdate"
    );
    assert!(restate.calls().is_empty());
}

#[tokio::test]
async fn setup_verifies_selected_app_and_never_passes_viewer_as_installer() {
    for (name, app_id, account_id, login, kind, action) in [
        ("repo", 123, 9001, "acme", "Organization", "install"),
        ("repo", 123, 42, "octocat", "User", "update"),
        ("org", 456, 9001, "acme", "Organization", "install"),
    ] {
        let mut expectations = oauth_expectations(OCTOCAT);
        expectations.push(
            Expectation::ok_json(
                Method::Get,
                "https://api.github.com/app/installations/77",
                common::apps::installation(app_id, 77, account_id, login, kind),
            )
            .require_header("user-agent", format!("p1/{name}")),
        );
        let mock = MockTransport::scripted(expectations);
        let (app, restate) = app(mock.clone()).await;
        let cookie = sign_in(&app).await;
        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/setup/{name}?installation_id=77&setup_action={action}&sender_id=42"
                    ))
                    .header("cookie", cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert_eq!(
            response.headers()["location"],
            format!("/console/accounts/{login}")
        );
        mock.assert_exhausted();
        let calls = restate.calls();
        assert_eq!(calls.len(), 1);
        let call = &calls[0];
        assert_eq!(
            (
                call.service.as_str(),
                call.key.as_str(),
                call.method.as_str(),
                call.send
            ),
            (
                "Installation",
                format!("{name}:77").as_str(),
                "onboard",
                false
            )
        );
        assert!(call.body["operation_id"].is_string());
        assert_eq!(call.body["verified"]["app"], name);
        assert_eq!(call.body["verified"]["account"]["account_id"], account_id);
        assert_eq!(
            call.body["verified"]["account"]["selected_repos"],
            serde_json::json!([])
        );
        assert_eq!(
            call.body["verified"]["permissions"]["administration"],
            "write"
        );
        for forbidden in [
            "actor_user_id",
            "sender_id",
            "installed_by_user_id",
            "installed_by_login",
        ] {
            assert!(!call.body.to_string().contains(forbidden));
        }
    }
}

#[tokio::test]
async fn setup_rejects_mismatched_app_or_installation_and_upstream_failure() {
    for (app_id, installation_id, status) in [
        (456, 77, 200),
        (123, 999, 200),
        (123, 77, 404),
        (123, 77, 500),
    ] {
        let mut expectations = oauth_expectations(OCTOCAT);
        expectations.push(if status == 200 {
            Expectation::ok_json(
                Method::Get,
                "https://api.github.com/app/installations/77",
                common::apps::installation(app_id, installation_id, 9001, "acme", "Organization"),
            )
        } else {
            Expectation::status(
                Method::Get,
                "https://api.github.com/app/installations/77",
                status,
            )
        });
        let (app, restate) = app(MockTransport::scripted(expectations)).await;
        let cookie = sign_in(&app).await;
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/setup/repo?installation_id=77")
                    .header("cookie", cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            if status == 500 {
                StatusCode::SERVICE_UNAVAILABLE
            } else {
                StatusCode::NOT_FOUND
            }
        );
        assert!(restate.calls().is_empty());
    }
}
