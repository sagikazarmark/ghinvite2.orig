use super::*;
use crate::{RestateClient, link_authority::LinkAuthority};
use ghinvite_core::admission::{Admit, Rejection};
use std::{
    io::Write,
    sync::{Arc, Mutex},
    time::Duration,
};
use wiremock::{Mock, MockServer, ResponseTemplate, matchers::method};

#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);
impl Write for Capture {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
fn command() -> Admit {
    Admit {
        link_id: "01ARZ3NDEKTSV4RRFFQ69G5FAV".parse().unwrap(),
        operation_id: "01ARZ3NDEKTSV4RRFFQ69G5FAW".to_owned().try_into().unwrap(),
        requester_id: 91919191,
        justification: Some("PRIVATE_JUSTIFICATION".into()),
        secret_sha256: [42; 32].into(),
        secret_generation: 7,
        domain_evidence: None,
        scope_evidence: vec![],
    }
}
fn captured(capture: &Capture) -> String {
    String::from_utf8(capture.0.lock().unwrap().clone()).unwrap()
}
fn check_privacy(text: &str) {
    for secret in [
        "PRIVATE_",
        "01ARZ3NDEKTSV4RRFFQ69G5FAV",
        "01ARZ3NDEKTSV4RRFFQ69G5FAW",
        "91919191",
        "2a2a2a",
        "secret_sha256",
        "/InvitationLink/",
    ] {
        assert!(
            !text.contains(secret),
            "private marker appeared in telemetry"
        );
    }
}

#[tokio::test]
async fn actual_rpc_delays_outcomes_and_replays_emit_one_safe_observation_per_attempt() {
    let capture = Capture::default();
    let writer = capture.clone();
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_ansi(false)
        .with_max_level(tracing::Level::INFO)
        .with_writer(move || writer.clone())
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);
    let receipt = AdmissionReceipt {
        decided_at: chrono::Utc::now(),
        result: AdmissionResult::Rejected {
            reason: Rejection::Paused,
        },
    };
    let cases = [
        (
            ResponseTemplate::new(200)
                .set_body_json(&receipt)
                .set_delay(Duration::from_millis(50)),
            "refused",
        ),
        (
            ResponseTemplate::new(400).set_body_string("PRIVATE_ERROR"),
            "invalid",
        ),
        (
            ResponseTemplate::new(404).set_body_string("PRIVATE_ERROR"),
            "missing",
        ),
        (
            ResponseTemplate::new(409).set_body_string("PRIVATE_ERROR"),
            "conflict",
        ),
        (
            ResponseTemplate::new(503).set_body_string("PRIVATE_ERROR"),
            "unknown",
        ),
        (
            ResponseTemplate::new(200).set_body_string("PRIVATE_BAD_JSON"),
            "unknown",
        ),
    ];
    for (response, expected) in cases {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(response)
            .expect(2)
            .mount(&server)
            .await;
        let authority = LinkAuthority::new(Arc::new(RestateClient::new(server.uri()).unwrap()));
        let before = captured(&capture).len();
        // Exact operation replay is another ingress observation, not another approval.
        for _ in 0..2 {
            let _ = authority.admit(command()).await;
        }
        let all = captured(&capture);
        let text = &all[before..];
        assert_eq!(text.matches("event=\"admission_rpc_completed\"").count(), 2);
        assert_eq!(text.matches(&format!("outcome=\"{expected}\"")).count(), 2);
        assert_eq!(text.matches("clock_valid=true").count(), 2);
        if expected == "refused" {
            for line in text
                .lines()
                .filter(|line| line.contains("admission_rpc_completed"))
            {
                let elapsed: u64 = line
                    .split("elapsed_ms=")
                    .nth(1)
                    .unwrap()
                    .split_whitespace()
                    .next()
                    .unwrap()
                    .parse()
                    .unwrap();
                assert!(elapsed >= 40, "actual ingress delay was not measured");
            }
        }
        check_privacy(text);
        eprint!("{text}");
        server.verify().await;
    }
}

#[tokio::test]
async fn cancelled_rpc_emits_interrupted_once_without_claiming_no_effect() {
    let capture = Capture::default();
    let writer = capture.clone();
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_ansi(false)
        .with_max_level(tracing::Level::INFO)
        .with_writer(move || writer.clone())
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(2)))
        .expect(1)
        .mount(&server)
        .await;
    let authority = LinkAuthority::new(Arc::new(RestateClient::new(server.uri()).unwrap()));
    assert!(
        tokio::time::timeout(Duration::from_millis(100), authority.admit(command()))
            .await
            .is_err()
    );
    let text = captured(&capture);
    assert_eq!(text.matches("event=\"admission_rpc_completed\"").count(), 1);
    assert!(text.contains("outcome=\"interrupted\""));
    assert!(!text.contains("outcome=\"accepted\""));
    check_privacy(&text);
    server.verify().await;
}

#[tokio::test]
async fn accepted_receipt_is_preserved_and_named_without_identity_fields() {
    let capture = Capture::default();
    let writer = capture.clone();
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_ansi(false)
        .with_max_level(tracing::Level::INFO)
        .with_writer(move || writer.clone())
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);
    let receipt = AdmissionReceipt {
        decided_at: chrono::Utc::now(),
        result: AdmissionResult::Accepted {
            request_id: "01ARZ3NDEKTSV4RRFFQ69G5FAV".parse().unwrap(),
            state: ghinvite_core::RequestState::Pending,
            decision_deadline: None,
        },
    };
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(&receipt))
        .expect(1)
        .mount(&server)
        .await;
    let authority = LinkAuthority::new(Arc::new(RestateClient::new(server.uri()).unwrap()));
    assert_eq!(authority.admit(command()).await.unwrap(), receipt);
    let text = captured(&capture);
    assert_eq!(text.matches("event=\"admission_rpc_completed\"").count(), 1);
    assert!(text.contains("outcome=\"accepted\""));
    check_privacy(&text);
}

#[tokio::test]
async fn actual_ingress_deadline_emits_one_unknown_observation() {
    let capture = Capture::default();
    let writer = capture.clone();
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_ansi(false)
        .with_max_level(tracing::Level::INFO)
        .with_writer(move || writer.clone())
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(30)))
        .expect(1)
        .mount(&server)
        .await;
    let authority = LinkAuthority::new(Arc::new(RestateClient::new(server.uri()).unwrap()));
    assert!(matches!(
        authority.admit(command()).await,
        Err(AuthorityError::Unknown(_))
    ));
    let text = captured(&capture);
    assert_eq!(text.matches("event=\"admission_rpc_completed\"").count(), 1);
    assert!(text.contains("outcome=\"unknown\""));
    let line = text
        .lines()
        .find(|line| line.contains("admission_rpc_completed"))
        .unwrap();
    let elapsed: u64 = line
        .split("elapsed_ms=")
        .nth(1)
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap()
        .parse()
        .unwrap();
    assert!(
        (14_000..=20_000).contains(&elapsed),
        "actual fixed ingress deadline was not observed"
    );
    check_privacy(&text);
    eprint!("{text}");
    server.verify().await;
}
