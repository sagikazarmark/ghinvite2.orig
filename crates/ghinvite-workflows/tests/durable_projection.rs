//! #54: production command -> durable projector -> public SQLx read boundary.
#![cfg(feature = "integration")]

#[path = "common/installation_context.rs"]
mod installation_context;
use ghinvite_core::storage::projection::{ProjectionEnvelope, ProjectionStorage};
use ghinvite_core::storage::{
    AppInstallationStorage, AuditPosition, ConsoleStorage, RecordStorage,
};
use ghinvite_core::{Account, AccountType, InvitationLinkId, SelectedRepos, User};
use ghinvite_storage_sqlx::SqlxStorage;
use ghinvite_workflows::{admission, projection};
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
    observed_failures: AtomicUsize,
    // Link revisions in commit order. The per-link projector must never commit
    // a later transition before an earlier one that is still retrying.
    committed_revisions: std::sync::Mutex<Vec<u64>>,
}
#[async_trait::async_trait]
impl ProjectionStorage for LostAcknowledgement {
    async fn apply_reissue(
        &self,
        envelope: &ghinvite_core::reissue_projection::RequestReissueProjection,
    ) -> ghinvite_core::storage::Result<()> {
        self.storage.apply_reissue(envelope).await
    }

    async fn apply_revocation(
        &self,
        envelope: &ghinvite_core::revocation::RevocationProjection,
    ) -> ghinvite_core::storage::Result<()> {
        self.storage.apply_revocation(envelope).await?;
        self.committed_revisions
            .lock()
            .unwrap()
            .push(envelope.revision);
        self.committed_attempts.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    async fn apply_request(
        &self,
        envelope: &ghinvite_core::storage::projection::RequestProjectionEnvelope,
    ) -> ghinvite_core::storage::Result<()> {
        self.storage.apply_request(envelope).await
    }
    async fn apply_transition(
        &self,
        envelope: &ProjectionEnvelope,
    ) -> ghinvite_core::storage::Result<()> {
        if let Err(error) = self.storage.apply_transition(envelope).await {
            // Observe the actual adapter refusal. Restate's transient
            // last_failure column is cleared when a retry becomes ready.
            let stage = match &error {
                ghinvite_core::storage::Error::ProjectionDependency => 1,
                ghinvite_core::storage::Error::Database(message)
                    if message.contains("injected audit failure") =>
                {
                    2
                }
                ghinvite_core::storage::Error::ProjectionInvariant(_) => 4,
                _ => 0,
            };
            self.observed_failures.fetch_or(stage, Ordering::SeqCst);
            return Err(error);
        }
        self.committed_revisions
            .lock()
            .unwrap()
            .push(envelope.link.revision);
        if self.committed_attempts.fetch_add(1, Ordering::SeqCst) == 0 {
            return Err(ghinvite_core::storage::Error::Database(
                "fixture: committed but acknowledgement lost".into(),
            ));
        }
        Ok(())
    }
}

#[tokio::test]
async fn commands_continue_during_sql_outage_and_projection_recovers() {
    timeout(Duration::from_secs(90), async {
        let harness = ghinvite_e2e::Harness::start().await;
        let admin = harness.restate.admin_url().to_owned();
        let ingress = harness.restate.ingress_url().to_owned();
        let host = std::env::var("RESTATE_ENDPOINT_HOST").unwrap_or_else(|_| "127.0.0.1".into());
        let storage = Arc::new(SqlxStorage::in_memory().await.unwrap());
        let client = reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(5)).build().unwrap();
        loop {
            if client.get(format!("{admin}/health")).send().await.is_ok_and(|r| r.status().is_success()) { break; }
            sleep(Duration::from_millis(100)).await;
        }
        let acknowledgements = Arc::new(LostAcknowledgement { storage: storage.clone(), committed_attempts: AtomicUsize::new(0), observed_failures: AtomicUsize::new(0), committed_revisions: Default::default() });
        let context = installation_context::InstallationContext::start(storage.clone(),client.clone()).await;
        let endpoint = context.bind(ghinvite_workflows::request_owner::bind(projection::bind(admission::bind_protocol_fixture(Endpoint::builder()), acknowledgements.clone()))).build();
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
            let url = if handler == "decide" { format!("{ingress}/InvitationRequest/{}/decide", input["request_id"].as_str().unwrap()) }
                else { format!("{ingress}/InvitationLink/{link_id}/{handler}") };
            async move {
                let response = client.post(url).json(&input).send().await.unwrap();
                assert!(response.status().is_success(), "{}", response.text().await.unwrap());
                response.json::<Value>().await.unwrap()
            }
        };
        // Parents are absent. Creation succeeds and its separate projection waits.
        command("create", json!({"secret_sha256":"2a".repeat(32),"link_id": link_id,
            "admin": {"account_id": 100, "user_id": 7}, "account_id": 100, "installation_id": 1,
            "description": "Workshop", "internal_note": null, "expires_at": null, "max_uses": 1,
             "approval_required": true,
            "targets":{"organization":null,"teams":[],"repos":[{"repo_id": 10, "repo_full_name": "acme/api","grant":"read"}]}})).await;
        // Observe a durable dependency failure before restoring its parents.
        while acknowledgements.observed_failures.load(Ordering::SeqCst) & 1 == 0 {
            sleep(Duration::from_millis(20)).await;
        }
        assert!(storage.get_invitation_link_by_id(link_id).await.unwrap().is_none());
        let now = chrono::Utc::now();
        let mut installation = ghinvite_core::account::Installation::repo(Account { installation_id: 1, account_id: 100,
            account_login: "acme".into(), account_type: AccountType::Organization,
            installed_at: "2026-01-01T00:00:00Z".parse().unwrap(), uninstalled_at: None, selected_repos: SelectedRepos::Subset(vec![]) });
        // Match the owner's initial revision before its separate repository refresh.
        installation.permissions = std::collections::BTreeMap::from([
            ("administration".into(), ghinvite_core::account::InstallationPermission::Write),
            ("metadata".into(), ghinvite_core::account::InstallationPermission::Read),
        ]);
        storage.put_app_installation(&installation).await.unwrap();
        for user_id in [7, 8] {
            storage.upsert_user(&User { user_id, login: format!("user{user_id}"),
                avatar_url: None, github_created_at: None, last_seen_at: now }).await.unwrap();
        }
        // Real SQLite write failure late in the transaction, not a fake consumer.
        storage.debug_set_audit_failure(true).await.unwrap();
        let accepted = command("admit", json!({"scope_evidence":[],"secret_sha256":"2a".repeat(32),"secret_generation":1,"link_id": link_id,
            "operation_id": ghinvite_core::RequestId::new(), "requester_id": 8,
            "justification": "Access please"})).await;
        assert_eq!(accepted["result"]["kind"], "accepted");
        context.seed(&client,&ingress).await;
        let revoke_operation = ghinvite_core::RequestId::new();
        let revoked = command("revoke", json!({"link_id": link_id, "operation_id":revoke_operation,"cascade":{"decline_pending_requests":false,"cancel_pending_invitations":false,"remove_granted_access":false},
            "admin": {"account_id": 100, "user_id": 7}})).await;
        assert_eq!(revoked, json!({"Ok":{"addressed_requests":0,"addressed_deliveries":0}}));
        let decision = command("decide", json!({"link_id": link_id,
            "request_id": accepted["result"]["request_id"], "operation_id": ghinvite_core::RequestId::new(),
            "admin": {"account_id": 100, "user_id": 7}, "action": {"kind": "decline", "reason": "Admin-only context"}})).await;
        assert_eq!(decision["outcome"], "applied");
        let browser = ghinvite_web::LinkAuthority::new(Arc::new(ghinvite_web::RestateClient::new(&ingress).unwrap()));
        let metadata = browser.update_metadata(ghinvite_core::admission::UpdateMetadata {
            link_id, admin: ghinvite_core::invitation_link::AccountAdmin { account_id: 100, user_id: 7 },
            description: "Updated workshop".into(), internal_note: Some("Private updated note".into()),
        }).await.unwrap();
        assert_eq!(metadata.creation.description, "Workshop");
        while acknowledgements.observed_failures.load(Ordering::SeqCst) & 2 == 0 {
            sleep(Duration::from_millis(20)).await;
        }
        assert!(storage.request_history(100, link_id, None).await.unwrap().requests.is_empty());
        assert!(storage.list_audit_events(100, None, AuditPosition::Latest).await.unwrap().events.is_empty());
        storage.debug_set_projection_invariant_failure(true).await.unwrap();
        storage.debug_set_audit_failure(false).await.unwrap();
        // Invariants stay inspectable/retryable rather than terminally dropping
        // accepted work. Repair the SQL rule and let the same invocation redrive.
        while acknowledgements.observed_failures.load(Ordering::SeqCst) & 4 == 0 {
            sleep(Duration::from_millis(20)).await;
        }
        assert!(storage.request_history(100, link_id, None).await.unwrap().requests.is_empty());
        storage.debug_set_projection_invariant_failure(false).await.unwrap();
        loop {
            let link = storage.get_invitation_link_by_id(link_id).await.unwrap();
            let requests = storage.request_history(100, link_id, None).await.unwrap().requests;
            let events = storage.list_audit_events(100, None, AuditPosition::Latest).await.unwrap();
            if link.as_ref().is_some_and(|l| l.revoked_at.is_some() && l.description == "Updated workshop") && requests.len() == 1 && events.events.iter().filter(|event| event.target_kind != ghinvite_core::audit::TargetKind::Installation).count() == 6
                && acknowledgements.committed_attempts.load(Ordering::SeqCst) >= 5
                && events.events.iter().any(|event| event.event_type == ghinvite_core::audit::EventType::InstallationOwnerRecorded) {
                // The original six link/request audit events remain exact; the
                // independently required installation lifecycle also records its owner.
                assert_eq!(events.events.iter().filter(|event| event.event_type == ghinvite_core::audit::EventType::InstallationOwnerRecorded).count(),1);
                let link = link.unwrap();
                assert_eq!(link.uses_count, 1);
                assert_eq!(link.internal_note.as_deref(), Some("Private updated note"));
                assert_eq!(requests[0].id.to_string(), accepted["result"]["request_id"]);
                assert_eq!(requests[0].state, ghinvite_core::RequestState::Declined);
                assert_eq!(requests[0].decided_by, Some(7));
                assert_eq!(requests[0].decline_reason.as_deref(), Some("Admin-only context"));
                assert_eq!(serde_json::to_value(requests[0].decided_at).unwrap(), decision["request"]["decision"]["effective_at"]);
                assert_eq!(serde_json::to_value(requests[0].decision_deadline).unwrap(), accepted["result"]["decision_deadline"]);
                let revisions = acknowledgements.committed_revisions.lock().unwrap().clone();
                assert!(revisions.is_sorted(), "link transitions committed out of order: {revisions:?}");
                break;
            }
            sleep(Duration::from_millis(100)).await;
        }
        harness.finish().await;
    }).await.expect("durable projection recovery deadline");
}
