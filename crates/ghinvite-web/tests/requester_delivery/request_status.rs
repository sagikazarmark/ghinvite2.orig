use super::*;

pub(super) async fn owned_fixture() -> (DeliveryFixture, String, RequestId) {
    let fixture = fixture().await;
    settle(&fixture).await;
    let id = fixture.deliveries[0].create.command.request_id;
    for snapshot in fixture.storage.list_delivery_for_request(id).await.unwrap() {
        fixture
            .authority
            .set_repository_delivery(snapshot.create.command.delivery_key(), Some(snapshot));
    }
    // Deliberately do not select any invitation capability in this browser.
    let cookie = oauth_sign_in(&fixture.app).await;
    (fixture, cookie, id)
}
async fn get(fixture: &DeliveryFixture, cookie: &str, id: RequestId) -> axum::response::Response {
    fixture
        .app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/requests/{id}"))
                .header("cookie", cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
}
async fn refresh(
    fixture: &DeliveryFixture,
    cookie: &str,
    id: RequestId,
    target: &str,
    csrf: &str,
) -> axum::response::Response {
    fixture
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/requests/{id}/refresh"))
                .header("cookie", cookie)
                .header("accept", "application/json")
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from(format!("target={target}&csrf_token={csrf}")))
                .unwrap(),
        )
        .await
        .unwrap()
}
#[tokio::test]
async fn request_id_status_uses_owner_without_selected_capability_and_never_exposes_admin_context()
{
    let (fixture, cookie, id) = owned_fixture().await;
    let response = get(&fixture, &cookie, id).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        response.headers()["cache-control"]
            .to_str()
            .unwrap()
            .contains("no-store")
    );
    assert_eq!(response.headers()["referrer-policy"], "no-referrer");
    let html = body_text(response).await;
    assert!(html.contains("Request status"));
    assert!(html.contains("Refresh repository delivery"));
    assert!(!html.contains("private admin note"));
    assert!(!html.contains("private internal justification"));
    assert!(!html.contains("private upstream diagnostic"));
    assert!(!html.contains("Submit request"));
    assert!(
        !fixture
            .authority
            .calls()
            .iter()
            .any(|m| m == "requester_page" || m == "admit")
    );
    assert_eq!(
        get(&fixture, &cookie, RequestId::new()).await.status(),
        StatusCode::NOT_FOUND
    );
    let mut foreign = fixture.authority.request(id).unwrap();
    foreign.requester_id += 1;
    fixture.authority.seed_request(foreign);
    assert_eq!(
        get(&fixture, &cookie, id).await.status(),
        StatusCode::NOT_FOUND
    );
}
#[tokio::test]
async fn requester_refresh_is_csrf_protected_uses_current_generation_and_renders_throttle_receipt()
{
    let (fixture, cookie, id) = owned_fixture().await;
    let csrf = common::csrf_token(&fixture.app, &cookie).await;
    assert_eq!(
        refresh(&fixture, &cookie, id, "repo:5", "invalid")
            .await
            .status(),
        StatusCode::FORBIDDEN
    );
    assert!(!fixture.authority.calls().iter().any(|m| m == "refresh"));
    let response = refresh(&fixture, &cookie, id, "repo:5", &csrf).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&body_text(response).await).unwrap()["kind"],
        "requested"
    );
    fixture.authority.set_repository_refresh(
        ghinvite_core::delivery::RepositoryRefresh::Throttled {
            retry_at: "2099-01-01T00:00:00Z".parse().unwrap(),
        },
    );
    let response = refresh(&fixture, &cookie, id, "repo:5", &csrf).await;
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    let json: serde_json::Value = serde_json::from_str(&body_text(response).await).unwrap();
    assert_eq!(json["kind"], "throttled");
    assert_eq!(json["retry_at"], "2099-01-01T00:00:00Z");
    let before = fixture
        .authority
        .calls()
        .iter()
        .filter(|m| m.as_str() == "refresh")
        .count();
    // A settled delivery and a target outside the manifest cannot schedule reads.
    assert_eq!(
        refresh(&fixture, &cookie, id, "repo:1", &csrf)
            .await
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        refresh(&fixture, &cookie, id, "repo:999", &csrf)
            .await
            .status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        fixture
            .authority
            .calls()
            .iter()
            .filter(|m| m.as_str() == "refresh")
            .count(),
        before
    );
    let mut current = fixture
        .deliveries
        .iter()
        .find(|s| s.create.command.repo_id == 5)
        .unwrap()
        .clone();
    current.create.command.delivery_generation = 2;
    fixture.authority.set_delivery_progress(
        id,
        vec![RepositoryProgress {
            repo_id: 5,
            delivery_generation: 2,
            stage: DispatchStage::Submitted,
        }],
    );
    fixture
        .authority
        .set_repository_delivery(current.create.command.delivery_key(), Some(current));
    assert_eq!(
        refresh(&fixture, &cookie, id, "repo:5", &csrf)
            .await
            .status(),
        StatusCode::TOO_MANY_REQUESTS
    );
}
#[tokio::test]
async fn requester_refresh_rejects_missing_or_misbound_owner_and_never_falls_back_to_projection() {
    let (fixture, cookie, id) = owned_fixture().await;
    let csrf = common::csrf_token(&fixture.app, &cookie).await;
    let snapshot = fixture
        .deliveries
        .iter()
        .find(|s| s.create.command.repo_id == 5)
        .unwrap()
        .clone();
    let key = snapshot.create.command.delivery_key();
    for wrong in [
        "request",
        "link",
        "account",
        "requester",
        "grant",
        "generation",
        "batch",
    ] {
        let mut other = snapshot.clone();
        match wrong {
            "request" => other.create.command.request_id = RequestId::new(),
            "link" => other.create.command.link_id = Some(InvitationLinkId::new()),
            "account" => other.create.command.account_id += 1,
            "requester" => other.create.command.requester_id += 1,
            "grant" => other.create.command.permission = Permission::Push,
            "generation" => other.create.command.delivery_generation += 1,
            "batch" => {
                other.create.command.batch = Some(ghinvite_core::bulk::BatchRef {
                    batch_id: ghinvite_core::bulk::BulkBatchId::new(),
                    row: 1,
                })
            }
            _ => unreachable!(),
        }
        fixture
            .authority
            .set_repository_delivery(key.clone(), Some(other));
        assert_eq!(
            refresh(&fixture, &cookie, id, "repo:5", &csrf)
                .await
                .status(),
            StatusCode::SERVICE_UNAVAILABLE,
            "{wrong}"
        );
    }
    fixture.authority.set_repository_delivery(key, None);
    let response = refresh(&fixture, &cookie, id, "repo:5", &csrf).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(body_text(response).await.contains("not_tracked"));
    assert!(!fixture.authority.calls().iter().any(|m| m == "refresh"));
}
