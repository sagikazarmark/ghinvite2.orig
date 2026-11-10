use super::*;
use std::sync::Mutex;

struct PreviewTransport {
    inner: MockTransport,
    calls: Mutex<Vec<String>>,
}
impl ghinvite_github::HttpTransport for PreviewTransport {
    async fn send(
        &self,
        request: ghinvite_github::Request,
    ) -> std::result::Result<Response, ghinvite_github::TransportError> {
        self.calls.lock().unwrap().push(request.url.clone());
        self.inner.send(request).await
    }
}
async fn fixture(
    permission_user: u64,
    unavailable: bool,
) -> (
    axum::Router,
    String,
    Arc<PreviewTransport>,
    ghinvite_core::RequestId,
) {
    let (_, _, storage, envelope, authority) = deadline_queue_app_with_authority(None).await;
    let mut script = oauth_expectations(OCTOCAT);
    script.extend([
        Expectation::ok_json(
            Method::Get,
            "https://api.github.com/users/requester",
            serde_json::json!({"id":99,"login":"requester"}),
        )
        .require_header("authorization", "Bearer ghs_fixed_123_77_1"),
        repo_identity(10, "octocat/api", 42),
    ]);
    let mut permission = Expectation::ok_json(
        Method::Get,
        "https://api.github.com/repos/octocat/api/collaborators/requester/permission",
        serde_json::json!({"user":{"id":permission_user,"login":"requester"},"role_name":"write"}),
    );
    if unavailable {
        permission.response.status = 503;
        permission.response.body = br#"{"message":"UPSTREAM_PREVIEW_PRIVATE_MARKER"}"#.to_vec();
    }
    script.push(permission);
    let transport = Arc::new(PreviewTransport {
        inner: MockTransport::scripted(script),
        calls: Mutex::new(vec![]),
    });
    let app = build_app(
        crate::common::sign_in::app_state(
            storage,
            transport.clone(),
            authority.client(),
            console_config(),
        ),
        tower_sessions::MemoryStore::default(),
    );
    let cookie = sign_in(&app).await;
    transport.calls.lock().unwrap().clear();
    (app, cookie, transport, envelope.requests[0].request_id)
}

#[tokio::test]
async fn approval_preview_is_explicit_scoped_cached_and_read_only() {
    let (app, cookie, transport, id) = fixture(99, false).await;
    let path = format!("/console/accounts/octocat/requests/{id}");
    let response = identity_request(&app, &cookie, "GET", &path).await;
    assert_eq!(response.status(), StatusCode::OK);
    let html = response_html(response).await;
    assert!(html.contains("Effective access preview"));
    assert!(html.contains("Not checked yet"));
    assert!(html.contains(&format!("{path}?preview=1")));
    assert!(transport.calls.lock().unwrap().is_empty());
    let queue = response_html(
        identity_request(&app, &cookie, "GET", "/console/accounts/octocat/requests").await,
    )
    .await;
    assert!(queue.contains(&format!("{path}?preview=1")));
    assert!(transport.calls.lock().unwrap().is_empty());
    for _ in 0..2 {
        let response = identity_request(&app, &cookie, "GET", &format!("{path}?preview=1")).await;
        assert_eq!(response.status(), StatusCode::OK);
        let html = response_html(response).await;
        assert!(html.contains("Repository role: write"));
        assert!(html.contains("Oldest evidence checked:"));
        assert!(html.contains("each delivery checks current access again before any write"));
        assert!(!html.contains("ghs_fixed_"));
    }
    let calls = transport.calls.lock().unwrap().clone();
    assert_eq!(calls.len(), 3); // user, repo identity and effective role
    assert_eq!(
        calls.iter().filter(|p| p.contains("/permission")).count(),
        1
    );
    for path in [
        format!(
            "/console/accounts/octocat/requests/{}?preview=1",
            ghinvite_core::RequestId::new()
        ),
        format!("/console/accounts/other/requests/{id}?preview=1"),
    ] {
        let response = identity_request(&app, &cookie, "GET", &path).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert!(
            !response_html(response)
                .await
                .contains("Repository role: write")
        );
    }
    assert_eq!(*transport.calls.lock().unwrap(), calls);
}

#[tokio::test]
async fn approval_preview_missing_or_mismatched_evidence_stays_unavailable_without_disclosing_errors()
 {
    for (user, unavailable) in [(999, false), (99, true)] {
        let (app, cookie, _, id) = fixture(user, unavailable).await;
        let response = identity_request(
            &app,
            &cookie,
            "GET",
            &format!("/console/accounts/octocat/requests/{id}?preview=1"),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let html = response_html(response).await;
        assert!(html.contains("octocat/api"));
        assert!(html.contains("Unavailable"));
        assert!(html.contains("Check effective access again"));
        assert!(!html.contains("Repository role: write"));
        assert!(!html.contains("No existing access observed"));
        assert!(!html.contains("UPSTREAM_PREVIEW_PRIVATE_MARKER"));
    }
}

#[tokio::test]
async fn approval_preview_requires_authority_over_the_entire_request_before_requester_reads() {
    use ghinvite_core::{account::AccountSettings, storage::AppInstallationStorage};
    let (_, _, _, envelope, _) = deadline_queue_app_with_authority(None).await;
    let storage = Arc::new(
        ghinvite_storage_sqlx::SqlxStorage::in_memory()
            .await
            .unwrap(),
    );
    storage
        .seed_console_installation(&acme_installation())
        .await
        .unwrap();
    storage
        .put_account_settings(&AccountSettings {
            account_id: 9001,
            revision: 1,
            repo_admin_delegation: true,
            changed_by: 42,
            changed_at: Utc::now(),
        })
        .await
        .unwrap();
    for (user_id, login) in [(42, "octocat"), (99, "requester")] {
        storage
            .upsert_user(&ghinvite_core::User {
                user_id,
                login: login.into(),
                avatar_url: None,
                github_created_at: None,
                last_seen_at: Utc::now(),
            })
            .await
            .unwrap();
    }
    let mut link = list_link(1);
    link.targets.repos.push(ghinvite_core::targets::RepoTarget {
        repo_id: 11,
        repo_full_name: "acme/web".into(),
        grant: ghinvite_core::targets::RepoRole::Read,
    });
    storage.seed_link(&link).await.unwrap();
    let mut request = envelope.requests[0].as_request();
    request.invitation_link_id = Some(link.id);
    storage.seed_request(&request).await.unwrap();
    let mut script = oauth_expectations(GithubUser::new("delegate", 43));
    script.extend([
        Expectation::ok_json(Method::Get,"https://api.github.com/installation/repositories?per_page=100",serde_json::json!({"total_count":2,"repositories":[{"id":10,"full_name":"acme/api","owner":{"id":9001,"login":"acme"}},{"id":11,"full_name":"acme/web","owner":{"id":9001,"login":"acme"}}]})),
        repo_identity(10,"acme/api",9001),repo_admin("acme/api",43,"delegate"),repo_identity(11,"acme/web",9001),
        Expectation::ok_json(Method::Get,"https://api.github.com/repos/acme/web/collaborators/delegate/permission",serde_json::json!({"user":{"id":43,"login":"delegate"},"role_name":"read"})),
    ]);
    let transport = Arc::new(PreviewTransport {
        inner: MockTransport::scripted(script),
        calls: Mutex::new(vec![]),
    });
    let app = build_app(
        crate::common::sign_in::app_state(
            storage,
            transport.clone(),
            unreachable_restate(),
            console_config(),
        ),
        tower_sessions::MemoryStore::default(),
    );
    let cookie = sign_in(&app).await;
    transport.calls.lock().unwrap().clear();
    let response = identity_request(
        &app,
        &cookie,
        "GET",
        &format!("/console/accounts/acme/requests/{}?preview=1", request.id),
    )
    .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let html = response_html(response).await;
    assert!(!html.contains("Effective access preview"));
    let calls = transport.calls.lock().unwrap();
    assert!(
        calls
            .iter()
            .any(|url| url.ends_with("/collaborators/delegate/permission"))
    );
    assert!(
        calls
            .iter()
            .all(|url| !url.contains("/users/") && !url.contains("/collaborators/requester/"))
    );
}
