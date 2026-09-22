//! Public ingress boundary: credentials stay on server-to-Restate requests.
use ghinvite_web::restate_client::RestateAuth;
use ghinvite_web::{LinkAuthority, RestateClient, WebError};
use serde_json::{Value, json};
use std::sync::Arc;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const API_KEY: &str = "test-ingress-key-do-not-expose";
const CODE: &str = "abcdefgh12345678";

fn authority(client: &RestateClient) -> LinkAuthority {
    LinkAuthority::new(Arc::new(client.clone()))
}

#[test]
fn bearer_credentials_require_https_except_on_loopback() {
    let auth = RestateAuth::from_config(None, Some(API_KEY)).unwrap();
    for url in [
        "http://ingress.example",
        "http://192.0.2.1",
        "http://localhost.example",
    ] {
        let error = RestateClient::with_auth(url, auth.clone()).unwrap_err();
        assert!(!format!("{error} {error:?}").contains(API_KEY));
    }
    for url in [
        "https://ingress.example",
        "http://127.0.0.1:8080",
        "http://[::1]:8080",
        "http://localhost:8080",
    ] {
        RestateClient::with_auth(url, auth.clone()).unwrap();
    }
}

#[test]
fn ingress_credentials_cannot_be_embedded_in_urls() {
    for url in [
        format!("https://user:{API_KEY}@ingress.example"),
        format!("https://ingress.example?api_key={API_KEY}"),
        format!("https://ingress.example#{API_KEY}"),
        format!("not a url {API_KEY}"),
    ] {
        let error = RestateClient::new(url).unwrap_err();
        assert!(!format!("{error} {error:?}").contains(API_KEY));
    }
}

/// A rejected fire-and-forget send is not proof the command did not happen:
/// the ingress may have persisted the invocation before the response went
/// wrong. A refused read carries no such doubt.
#[tokio::test]
async fn a_rejected_send_stays_outcome_unknown_while_a_refused_read_does_not() {
    let ingress = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(503).set_body_string("upstream capacity exceeded"))
        .mount(&ingress)
        .await;
    let client = RestateClient::new(ingress.uri()).unwrap();

    let sent = client.send("Service", "", "write", &()).await.unwrap_err();
    let read = client
        .call::<_, Value>("Service", "", "read", &())
        .await
        .unwrap_err();

    assert!(sent.to_string().contains("outcome unknown"), "{sent}");
    assert!(!read.to_string().contains("outcome unknown"), "{read}");
    // A route that changes state through `call` can still upgrade the refusal
    // to outcome-unknown when it cannot re-read the result.
    let WebError::Restate(failure) = &read else {
        panic!("expected an ingress failure, got {read:?}")
    };
    assert!(
        failure
            .clone()
            .into_outcome_unknown()
            .to_string()
            .contains("outcome unknown")
    );
    // Either way the ingress's own words stay out of the error, and the status
    // stays available for logs.
    for error in [&sent, &read] {
        assert!(!format!("{error} {error:?}").contains("capacity exceeded"));
        assert_eq!(error.upstream_status(), Some(503));
    }
}

#[tokio::test]
async fn decoding_and_transport_failures_are_sanitized() {
    let ingress = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!(API_KEY)))
        .mount(&ingress)
        .await;
    let client = RestateClient::with_auth(
        ingress.uri(),
        RestateAuth::from_config(None, Some(API_KEY)).unwrap(),
    )
    .unwrap();
    for error in [
        client
            .call::<_, u64>("Service", "", "read", &())
            .await
            .unwrap_err(),
        authority(&client)
            .resolve(CODE)
            .await
            .map_err(WebError::from)
            .unwrap_err(),
    ] {
        assert!(!format!("{error} {error:?}").contains(API_KEY));
    }
    // Reserve then close a port to cause a real connection failure.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    drop(listener);
    let client =
        RestateClient::with_auth(&url, RestateAuth::from_config(None, Some(API_KEY)).unwrap())
            .unwrap();
    for error in [
        client
            .call::<_, Value>("Service", "", "read", &())
            .await
            .unwrap_err(),
        authority(&client)
            .resolve(CODE)
            .await
            .map_err(WebError::from)
            .unwrap_err(),
        client.send("Service", "", "write", &()).await.unwrap_err(),
    ] {
        let diagnostic = format!("{error} {error:?}");
        assert!(!diagnostic.contains(API_KEY));
        assert!(!diagnostic.contains(&url));
    }
}

#[tokio::test]
async fn rejected_credentials_and_upstream_errors_never_expose_response_content() {
    for status in [401, 403, 500] {
        let ingress = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(status).set_body_string(format!(
                "Authorization: Bearer {API_KEY}; private upstream diagnostics"
            )))
            .mount(&ingress)
            .await;
        for auth in [
            RestateAuth::local_unauthenticated(),
            RestateAuth::from_config(None, Some("invalid-test-key")).unwrap(),
        ] {
            let client = RestateClient::with_auth(ingress.uri(), auth).unwrap();
            let errors = [
                client
                    .call::<_, Value>("Installation", "42", "sync", &())
                    .await
                    .unwrap_err(),
                authority(&client)
                    .resolve(CODE)
                    .await
                    .map_err(WebError::from)
                    .unwrap_err(),
                client
                    .send("Installation", "42", "sync", &())
                    .await
                    .unwrap_err(),
            ];
            for error in errors {
                assert!(matches!(error, ghinvite_web::WebError::Restate(_)));
                let diagnostic = format!("{error} {error:?}");
                assert!(!diagnostic.contains(API_KEY), "{diagnostic}");
                assert!(!diagnostic.contains("private upstream diagnostics"));
            }
        }
    }
}

#[tokio::test]
async fn configuration_requires_credentials_unless_local_mode_is_explicit() {
    for (mode, key) in [
        (None, None),
        (Some("bearer"), None),
        (None, Some("")),
        (None, Some("   ")),
        (None, Some("key\r\nInjected: value")),
        (Some("typo"), Some(API_KEY)),
        (Some("local-unauthenticated"), Some(API_KEY)),
    ] {
        let error = RestateAuth::from_config(mode, key).unwrap_err();
        assert!(!format!("{error} {error:?}").contains(API_KEY));
    }
    let auth = RestateAuth::from_config(Some("bearer"), Some(API_KEY)).unwrap();
    let client = RestateClient::with_auth("https://ingress.example", auth.clone()).unwrap();
    assert!(!format!("{auth:?} {client:?}").contains(API_KEY));

    let ingress = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(|request: &wiremock::Request| {
            assert!(!request.headers.contains_key("authorization"));
            ResponseTemplate::new(200).set_body_json(json!(null))
        })
        .expect(3)
        .mount(&ingress)
        .await;
    let auth = RestateAuth::from_config(Some("local-unauthenticated"), None).unwrap();
    let client = RestateClient::with_auth(ingress.uri(), auth).unwrap();
    client
        .call::<_, ()>("Service", "", "read", &())
        .await
        .unwrap();
    let decision = serde_json::from_value(json!({
        "link_id": ghinvite_core::InvitationLinkId::new(),
        "request_id": ghinvite_core::RequestId::new(),
        "operation_id": ghinvite_core::RequestId::new(),
        "admin": {"account_id": 1, "user_id": 2}, "action": {"kind": "approve"}}))
    .unwrap();
    authority(&client).decision_status(decision).await.unwrap();
    client.send("Service", "", "write", &()).await.unwrap();
}

#[tokio::test]
async fn authenticated_ingress_accepts_calls_link_authority_calls_and_sends() {
    let ingress = MockServer::start().await;
    for endpoint in [
        "/Installation/42/sync",
        "/Installation/42/sync/send",
        "/Projection/apply",
        "/Projection/send/apply",
    ] {
        Mock::given(method("POST"))
            .and(path(endpoint))
            .and(header("authorization", format!("Bearer {API_KEY}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ok": true})))
            .expect(1)
            .mount(&ingress)
            .await;
    }
    let link = ghinvite_core::InvitationLinkId::new();
    Mock::given(method("POST"))
        .and(path(format!("/InvitationCode/{CODE}/resolve")))
        .and(header("authorization", format!("Bearer {API_KEY}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(link))
        .expect(1)
        .mount(&ingress)
        .await;
    let auth = RestateAuth::from_config(None, Some(API_KEY)).unwrap();
    let client = RestateClient::with_auth(ingress.uri(), auth).unwrap();
    let result: Value = client
        .call("Installation", "42", "sync", &())
        .await
        .unwrap();
    assert_eq!(result, json!({"ok": true}));
    assert_eq!(authority(&client).resolve(CODE).await.unwrap(), link);
    client
        .send("Installation", "42", "sync", &())
        .await
        .unwrap();
    let result: Value = client.call("Projection", "", "apply", &()).await.unwrap();
    assert_eq!(result, json!({"ok": true}));
    client.send("Projection", "", "apply", &()).await.unwrap();
}
