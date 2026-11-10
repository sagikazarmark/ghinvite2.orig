use super::*;
use std::sync::Mutex;

struct AvatarTransport {
    rest: MockTransport,
    calls: Mutex<Vec<ghinvite_github::Request>>,
    image: Mutex<Response>,
}
impl ghinvite_github::HttpTransport for AvatarTransport {
    async fn send(
        &self,
        request: ghinvite_github::Request,
    ) -> Result<Response, ghinvite_github::TransportError> {
        if request
            .url
            .starts_with("https://avatars.githubusercontent.com/")
        {
            self.calls.lock().unwrap().push(request);
            return Ok(self.image.lock().unwrap().clone());
        }
        ghinvite_github::HttpTransport::send(&self.rest, request).await
    }
}

#[tokio::test]
async fn approval_profiles_use_stored_github_facts_and_only_scoped_public_avatar_proxy() {
    let transport = Arc::new(AvatarTransport {
        rest: MockTransport::scripted(oauth_expectations(OCTOCAT)),
        calls: Mutex::new(vec![]),
        image: Mutex::new(Response {
            status: 200,
            headers: [("content-type".into(), "image/png".into())].into(),
            body: b"\x89PNG\r\n\x1a\n".to_vec(),
        }),
    });
    let (app, cookie, storage, envelope, _) =
        deadline_queue_app_with_transport(None, Some(transport.clone())).await;
    let request = envelope.requests[0].request_id;
    let profile_path = format!("/console/accounts/octocat/requests/{request}/avatar");
    storage
        .upsert_user(&ghinvite_core::User {
            user_id: 99,
            login: "requester".into(),
            // This value must never become a browser source or outbound destination.
            avatar_url: Some("https://internal.invalid/AVATAR_HOST_MARKER".into()),
            github_created_at: Some("2010-01-02T03:04:05Z".parse().unwrap()),
            last_seen_at: Utc::now(),
        })
        .await
        .unwrap();
    for path in [
        "/console/accounts/octocat/requests".to_owned(),
        format!("/console/accounts/octocat/requests/{request}"),
        format!(
            "/console/accounts/octocat/links/{}/requests",
            envelope.link.link_id
        ),
    ] {
        let response = identity_request(&app, &cookie, "GET", &path).await;
        assert_eq!(response.status(), StatusCode::OK, "{path}");
        assert!(
            response.headers()["content-security-policy"]
                .to_str()
                .unwrap()
                .contains("img-src 'self' data:;")
        );
        let html = response_html(response).await;
        assert!(html.contains("2010-01-02"), "{path}: {html}");
        assert!(html.contains(&profile_path));
        assert!(!html.contains("AVATAR_HOST_MARKER"));
    }
    assert!(
        transport.calls.lock().unwrap().is_empty(),
        "page render performs no avatar/profile fanout"
    );
    let response = identity_request(&app, &cookie, "GET", &profile_path).await;
    assert_eq!(response.status(), StatusCode::OK);
    for (header, value) in [
        ("content-type", "image/png"),
        ("cache-control", "private, no-store"),
        ("x-content-type-options", "nosniff"),
        ("referrer-policy", "no-referrer"),
    ] {
        assert_eq!(response.headers()[header], value);
    }
    assert_eq!(
        response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .as_ref(),
        b"\x89PNG\r\n\x1a\n"
    );
    {
        let calls = transport.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(
            calls[0].url,
            "https://avatars.githubusercontent.com/u/99?s=80&v=4"
        );
        assert!(!calls[0].headers.contains_key("authorization"));
        assert!(!calls[0].headers.contains_key("cookie"));
    }
    for path in [
        format!(
            "/console/accounts/octocat/requests/{}/avatar",
            ghinvite_core::RequestId::new()
        ),
        "/console/accounts/octocat/requests/not-an-id/avatar".into(),
        format!("/console/accounts/foreign/requests/{request}/avatar"),
    ] {
        assert_eq!(
            identity_request(&app, &cookie, "GET", &path).await.status(),
            StatusCode::NOT_FOUND
        );
    }
    assert_eq!(
        identity_request(&app, "", "GET", &profile_path)
            .await
            .status(),
        StatusCode::SEE_OTHER
    );
    assert_eq!(
        transport.calls.lock().unwrap().len(),
        1,
        "unauthorized/unknown routes must not call CDN"
    );
    *transport.image.lock().unwrap() = Response {
        status: 302,
        headers: [("location".into(), "https://internal.invalid".into())].into(),
        body: vec![],
    };
    assert_eq!(
        identity_request(&app, &cookie, "GET", &profile_path)
            .await
            .status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        transport.calls.lock().unwrap().len(),
        2,
        "no redirect request"
    );
}

pub(super) async fn browser_fixture() -> (
    axum::Router,
    String,
    Arc<ghinvite_storage_sqlx::SqlxStorage>,
    QueueFixture,
) {
    use base64::Engine;
    let image = base64::engine::general_purpose::STANDARD.decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aL1cAAAAASUVORK5CYII=").unwrap();
    let transport = Arc::new(AvatarTransport {
        rest: MockTransport::scripted(oauth_expectations(OCTOCAT)),
        calls: Mutex::new(vec![]),
        image: Mutex::new(Response {
            status: 200,
            headers: [("content-type".into(), "image/png".into())].into(),
            body: image,
        }),
    });
    let (app, cookie, storage, envelope, _) =
        deadline_queue_app_with_transport(None, Some(transport)).await;
    storage
        .upsert_user(&ghinvite_core::User {
            user_id: 99,
            login: "requester".into(),
            avatar_url: Some("https://avatars.githubusercontent.com/u/99".into()),
            github_created_at: Some("2010-01-02T03:04:05Z".parse().unwrap()),
            last_seen_at: Utc::now(),
        })
        .await
        .unwrap();
    (app, cookie, storage, envelope)
}
