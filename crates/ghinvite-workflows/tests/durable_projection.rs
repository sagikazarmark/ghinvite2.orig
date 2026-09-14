//! #54: production command -> durable projector -> public SQLx read boundary.
#![cfg(feature = "integration")]

use ghinvite_core::storage::projection::{ProjectionEnvelope, ProjectionStorage};
use ghinvite_core::storage::{AuditPosition, Storage};
use ghinvite_core::{Account, AccountType, InvitationLinkId, SelectedRepos, User};
use ghinvite_storage_sqlx::SqlxStorage;
use ghinvite_workflows::{admission_v1, projection_v1};
use restate_sdk::endpoint::Endpoint;
use serde_json::{Value, json};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tokio::time::{sleep, timeout};

// Fail once AFTER the real adapter commit and before the Restate run result is
// acknowledged. Recovery must reapply the actual database writes safely.
struct LostAcknowledgement {
    storage: Arc<SqlxStorage>,
    committed_attempts: AtomicUsize,
}
#[async_trait::async_trait]
impl ProjectionStorage for LostAcknowledgement {
    async fn get_projected_request(
        &self,
        id: ghinvite_core::RequestId,
    ) -> ghinvite_core::storage::Result<Option<ghinvite_core::storage::projection::RequestSnapshot>>
    {
        self.storage.get_projected_request(id).await
    }
    async fn apply_transition(
        &self,
        envelope: &ProjectionEnvelope,
    ) -> ghinvite_core::storage::Result<()> {
        self.storage.apply_transition(envelope).await?;
        if self.committed_attempts.fetch_add(1, Ordering::SeqCst) == 0 {
            return Err(ghinvite_core::storage::Error::Database(
                "fixture: committed but acknowledgement lost".into(),
            ));
        }
        Ok(())
    }
}

// #55 owns lifecycle execution; only its startup contract is needed here.
struct RequestSink;
impl admission_v1::InvitationRequestV1 for RequestSink {
    async fn run(
        &self,
        _: restate_sdk::context::WorkflowContext<'_>,
        _: restate_sdk::serde::Json<admission_v1::WorkflowEnvelope>,
    ) -> Result<(), restate_sdk::errors::TerminalError> {
        Ok(())
    }
}

#[tokio::test]
async fn commands_continue_during_sql_outage_and_projection_recovers() {
    timeout(Duration::from_secs(90), async {
        let admin = std::env::var("RESTATE_ADMIN_URL").expect("run scripts/test-restate.sh durable_projection");
        let ingress = std::env::var("RESTATE_INGRESS_URL").unwrap();
        let host = std::env::var("RESTATE_ENDPOINT_HOST").unwrap();
        let storage = Arc::new(SqlxStorage::in_memory().await.unwrap());
        let client = reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(5)).build().unwrap();
        loop {
            if client.get(format!("{admin}/health")).send().await.is_ok_and(|r| r.status().is_success()) { break; }
            sleep(Duration::from_millis(100)).await;
        }
        let acknowledgements = Arc::new(LostAcknowledgement { storage: storage.clone(), committed_attempts: AtomicUsize::new(0) });
        let endpoint = projection_v1::bind(admission_v1::bind(Endpoint::builder()), acknowledgements.clone())
            .bind(admission_v1::InvitationRequestV1::serve(RequestSink)).build();
        let listener = tokio::net::TcpListener::bind("0.0.0.0:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let app = axum::Router::new().fallback(move |request: axum::extract::Request| {
            let endpoint = endpoint.clone();
            async move {
                let response = endpoint.handle(request);
                let (parts, body) = response.into_parts();
                axum::response::Response::from_parts(parts, axum::body::Body::new(body))
            }
        });
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap(); });
        struct Stop(tokio::task::JoinHandle<()>);
        impl Drop for Stop { fn drop(&mut self) { self.0.abort(); } }
        let _server = Stop(server);
        let response = client.post(format!("{admin}/deployments"))
            .json(&json!({"uri": format!("http://{host}:{port}")})).send().await.unwrap();
        assert!(response.status().is_success(), "{}", response.text().await.unwrap());
        let link_id = InvitationLinkId::new();
        let command = |handler: &'static str, input: Value| {
            let client = client.clone();
            let url = format!("{ingress}/InvitationLinkV1/{link_id}/{handler}");
            async move {
                let response = client.post(url).json(&input).send().await.unwrap();
                assert!(response.status().is_success(), "{}", response.text().await.unwrap());
                response.json::<Value>().await.unwrap()
            }
        };
        // Parents are absent. Creation succeeds and its separate projection waits.
        command("create", json!({"version": 1, "link_id": link_id,
            "admin": {"account_id": 100, "user_id": 7}, "account_id": 100, "installation_id": 1,
            "description": "Workshop", "internal_note": null, "expires_at": null, "max_uses": 1,
            "permission": "pull", "approval_required": true,
            "repos": [{"repo_id": 10, "repo_full_name": "acme/api"}]})).await;
        // Observe a durable dependency failure before restoring its parents.
        loop {
            let response = client.post(format!("{admin}/query")).header("accept", "application/json")
                .json(&json!({"query": "SELECT id, last_failure FROM sys_invocation WHERE target_service_name = 'InvitationProjectionV1'"}))
                .send().await.unwrap();
            let body: Value = response.json().await.unwrap();
            if body["rows"].as_array().is_some_and(|rows| rows.iter().any(|row| row["last_failure"].as_str().is_some_and(|s| s.contains("dependency")))) { break; }
            sleep(Duration::from_millis(100)).await;
        }
        assert!(storage.get_invitation_link_by_id(link_id).await.unwrap().is_none());
        let now = chrono::Utc::now();
        storage.insert_installation(&Account { installation_id: 1, account_id: 100,
            account_login: "acme".into(), account_type: AccountType::Organization,
            installed_at: now, uninstalled_at: None, selected_repos: SelectedRepos::All }).await.unwrap();
        for user_id in [7, 8] {
            storage.upsert_user(&User { user_id, login: format!("user{user_id}"),
                avatar_url: None, last_seen_at: now }).await.unwrap();
        }
        // Real SQLite write failure late in the transaction, not a fake consumer.
        storage.debug_set_audit_failure(true).await.unwrap();
        let accepted = command("admit", json!({"version": 1, "link_id": link_id,
            "operation_id": ghinvite_core::RequestId::new(), "requester_id": 8,
            "justification": "Access please"})).await;
        assert_eq!(accepted["result"]["kind"], "accepted");
        let revoked = command("revoke", json!({"link_id": link_id,
            "admin": {"account_id": 100, "user_id": 7}})).await;
        assert!(revoked["revoked_at"].is_string());
        loop {
            let response = client.post(format!("{admin}/query")).header("accept", "application/json")
                .json(&json!({"query": "SELECT id, last_failure FROM sys_invocation WHERE target_service_name = 'InvitationProjectionV1'"}))
                .send().await.unwrap();
            let body: Value = response.json().await.unwrap();
            if body["rows"].as_array().is_some_and(|rows| rows.iter().any(|row| row["last_failure"].as_str().is_some_and(|s| s.contains("injected audit failure")))) { break; }
            sleep(Duration::from_millis(100)).await;
        }
        assert!(storage.list_requests_for_link(link_id).await.unwrap().is_empty());
        assert!(storage.list_audit_events(100, None, AuditPosition::Latest).await.unwrap().events.is_empty());
        storage.debug_set_projection_invariant_failure(true).await.unwrap();
        storage.debug_set_audit_failure(false).await.unwrap();
        // Invariants stay inspectable/retryable rather than terminally dropping
        // accepted work. Repair the SQL rule and let the same invocation redrive.
        loop {
            let response = client.post(format!("{admin}/query")).header("accept", "application/json")
                .json(&json!({"query": "SELECT id, last_failure FROM sys_invocation WHERE target_service_name = 'InvitationProjectionV1'"}))
                .send().await.unwrap();
            let body: Value = response.json().await.unwrap();
            if body["rows"].as_array().is_some_and(|rows| rows.iter().any(|row| row["last_failure"].as_str().is_some_and(|s| s.contains("projection invariant")))) { break; }
            sleep(Duration::from_millis(100)).await;
        }
        assert!(storage.list_requests_for_link(link_id).await.unwrap().is_empty());
        storage.debug_set_projection_invariant_failure(false).await.unwrap();
        loop {
            let link = storage.get_invitation_link_by_id(link_id).await.unwrap();
            let requests = storage.list_requests_for_link(link_id).await.unwrap();
            let events = storage.list_audit_events(100, None, AuditPosition::Latest).await.unwrap();
            if link.as_ref().is_some_and(|l| l.revoked_at.is_some()) && requests.len() == 1 && events.events.len() == 4
                && acknowledgements.committed_attempts.load(Ordering::SeqCst) >= 4 {
                assert_eq!(link.unwrap().uses_count, 1);
                assert_eq!(requests[0].id.to_string(), accepted["result"]["request_id"]);
                let projected = storage.get_projected_request(requests[0].id).await.unwrap().unwrap();
                assert_eq!(serde_json::to_value(projected.decision_deadline).unwrap(), accepted["result"]["decision_deadline"]);
                break;
            }
            sleep(Duration::from_millis(100)).await;
        }
    }).await.expect("durable projection recovery deadline");
}
