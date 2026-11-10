use super::*;
use ghinvite_core::request_lifecycle::{DecideRequest, DecisionAction};

#[tokio::test]
async fn decline_public_message_is_explicit_bounded_csrf_protected_and_retry_stable() {
    let authority = FakeLinkAuthority::start().await;
    let link = octocat_link();
    authority.seed_link(&link);
    let request = pending_request(link.id, Utc::now() + Duration::days(7));
    authority.seed_request(request.clone());
    let (app, cookie) = octocat_app(&authority, false).await;
    let csrf = common::csrf_token(&app, &cookie).await;
    let operation = ghinvite_core::RequestId::new();
    let path = format!(
        "/console/accounts/octocat/requests/{}/decline",
        request.request_id
    );
    let post = |token: String, message: String| {
        let body = url::form_urlencoded::Serializer::new(String::new())
            .extend_pairs([
                ("csrf_token", token),
                ("link_id", link.id.to_string()),
                ("operation_id", operation.to_string()),
                ("requester_message", message),
                ("user_id", "666".into()),
            ])
            .finish();
        Request::builder()
            .method("POST")
            .uri(&path)
            .header("cookie", &cookie)
            .header("content-type", "application/x-www-form-urlencoded")
            .body(Body::from(body))
            .unwrap()
    };
    assert_eq!(
        app.clone()
            .oneshot(post("invalid".into(), "Hello".into()))
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        app.clone()
            .oneshot(post(csrf.clone(), "x".repeat(501)))
            .await
            .unwrap()
            .status(),
        StatusCode::BAD_REQUEST
    );
    assert!(!authority.calls().iter().any(|m| m == "decide"));
    authority.lose_acknowledgements("decide");
    let text = "  Please contact\r\nthe team.  ";
    let response = app
        .clone()
        .oneshot(post(csrf.clone(), text.into()))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    let html = response_html(response).await;
    assert!(html.contains("Retry original attempt"));
    let response = app.clone().oneshot(post(csrf, text.into())).await.unwrap();
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    let calls = authority.received::<DecideRequest>("decide");
    assert!(!calls.is_empty());
    for call in &calls {
        assert_eq!(call.admin.user_id, 42);
        assert_eq!(
            call.action,
            DecisionAction::Decline {
                reason: None,
                requester_message: Some("Please contact\nthe team.".into())
            }
        );
        assert_eq!(
            String::from(call.operation_id.clone()),
            operation.to_string()
        );
    }
    assert_eq!(
        authority
            .applied()
            .iter()
            .filter(|m| m.as_str() == "decide")
            .count(),
        1
    );
}
