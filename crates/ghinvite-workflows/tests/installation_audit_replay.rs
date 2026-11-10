//! #83: one installation transition leaves one audit event behind, even when
//! the projection is retried after its audit write has already committed.
//!
//! `AccountInstallation` retains the audit event in a `ctx.run` step
//! before sending the projection, so a retry replays the identity the first
//! attempt minted. Nothing but a real invocation retry can show that: if the
//! identity were minted outside the journal, the retry would mint a second one
//! and the account would gain a duplicate "installation created" entry.
#![cfg(feature = "integration")]

use chrono::Utc;
use ghinvite_core::audit::{AuditEvent, EventType};
use ghinvite_core::storage::*;
use ghinvite_core::*;
use serde_json::json;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

/// Wraps the real storage so one installation audit write can commit and still
/// report failure. Every other workflow storage call goes straight through.
struct LostAuditAcknowledgement {
    inner: Arc<ghinvite_storage_sqlx::SqlxStorage>,
    swallowed: AtomicUsize,
}

#[async_trait::async_trait]
impl AuditStorage for LostAuditAcknowledgement {
    /// Commit the event, then lose the acknowledgement once. Restate cannot tell
    /// this from a crash between the write and its journal entry, so it retries
    /// the invocation — which is the only way to observe whether the event
    /// identity was retained or minted afresh.
    async fn audit(&self, event: &AuditEvent) -> Result<()> {
        self.inner.audit(event).await?;
        if event.event_type == EventType::InstallationCreated
            && self.swallowed.fetch_add(1, Ordering::SeqCst) == 0
        {
            return Err(ghinvite_core::storage::Error::Database(
                "fixture: committed but acknowledgement lost".into(),
            ));
        }
        Ok(())
    }
}

#[async_trait::async_trait]
impl RecordStorage for LostAuditAcknowledgement {
    async fn upsert_user(&self, user: &User) -> Result<()> {
        self.inner.upsert_user(user).await
    }
    async fn get_user(&self, user_id: u64) -> Result<Option<User>> {
        self.inner.get_user(user_id).await
    }
    async fn get_invitation_link_by_id(
        &self,
        id: InvitationLinkId,
    ) -> Result<Option<InvitationLink>> {
        self.inner.get_invitation_link_by_id(id).await
    }
    async fn get_invitation_request(&self, id: RequestId) -> Result<Option<InvitationRequest>> {
        self.inner.get_invitation_request(id).await
    }
}

#[async_trait::async_trait]
impl AppInstallationReadStorage for LostAuditAcknowledgement {
    async fn app_installation(
        &self,
        app: account::App,
        id: u64,
    ) -> Result<Option<account::Installation>> {
        self.inner.app_installation(app, id).await
    }
    async fn active_app_installation(
        &self,
        app: account::App,
        id: u64,
    ) -> Result<Option<account::Installation>> {
        self.inner.active_app_installation(app, id).await
    }
    async fn active_app_installation_by_login(
        &self,
        app: account::App,
        login: &str,
    ) -> Result<Option<account::Installation>> {
        self.inner
            .active_app_installation_by_login(app, login)
            .await
    }
    async fn latest_app_installation_by_login(
        &self,
        app: account::App,
        login: &str,
    ) -> Result<Option<account::Installation>> {
        self.inner
            .latest_app_installation_by_login(app, login)
            .await
    }
    async fn list_active_app_installations(
        &self,
        app: account::App,
    ) -> Result<Vec<account::Installation>> {
        self.inner.list_active_app_installations(app).await
    }
    async fn account_settings(&self, id: u64) -> Result<Option<account::AccountSettings>> {
        self.inner.account_settings(id).await
    }
}
#[async_trait::async_trait]
impl AppInstallationStorage for LostAuditAcknowledgement {
    async fn project_installation_change(
        &self,
        owner: account::AccountInstallationRef,
        change: &ghinvite_core::installation_audit::InstallationChange,
    ) -> Result<()> {
        self.inner
            .project_installation_change(owner, change)
            .await?;
        if change
            .event
            .as_ref()
            .is_some_and(|event| event.event_type == EventType::InstallationCreated)
            && self.swallowed.fetch_add(1, Ordering::SeqCst) == 0
        {
            return Err(Error::Database(
                "fixture: committed but acknowledgement lost".into(),
            ));
        }
        Ok(())
    }

    async fn put_app_installation(&self, value: &account::Installation) -> Result<()> {
        self.inner.put_app_installation(value).await
    }
    async fn put_account_settings(&self, value: &account::AccountSettings) -> Result<()> {
        self.inner.put_account_settings(value).await
    }
}

/// Installation transitions deliver nothing.
#[async_trait::async_trait]
impl DeliveryStorage for LostAuditAcknowledgement {
    async fn cancellation_attempt_exists(
        &self,
        command: &ghinvite_core::storage::delivery_attempts::CancelCommand,
    ) -> Result<bool> {
        self.inner.cancellation_attempt_exists(command).await
    }

    async fn claim_cancellation_attempt(
        &self,
        command: &ghinvite_core::storage::delivery_attempts::CancelCommand,
    ) -> Result<Option<u64>> {
        self.inner.claim_cancellation_attempt(command).await
    }
    async fn reject_cancellation_attempt(
        &self,
        command: &ghinvite_core::storage::delivery_attempts::CancelCommand,
        generation: u64,
    ) -> Result<()> {
        self.inner
            .reject_cancellation_attempt(command, generation)
            .await
    }

    async fn claim_delivery_attempt(
        &self,
        _: &ghinvite_core::delivery::CreateCommand,
    ) -> Result<Option<u64>> {
        unreachable!()
    }
    async fn reject_delivery_attempt(
        &self,
        _: &ghinvite_core::delivery::CreateCommand,
        _: u64,
    ) -> Result<()> {
        unreachable!()
    }
    async fn delivery_attempt_exists(
        &self,
        _: &ghinvite_core::delivery::CreateCommand,
    ) -> Result<bool> {
        unreachable!()
    }
    async fn project_delivery_with_events(
        &self,
        _: &ghinvite_core::delivery_audit::RepositoryDeliveryProjection,
    ) -> Result<()> {
        unreachable!()
    }
    async fn project_repository_telemetry(
        &self,
        value: &ghinvite_core::storage::operational::RepositoryTelemetry,
    ) -> Result<()> {
        self.inner.project_repository_telemetry(value).await
    }
}

#[tokio::test]
async fn a_retried_projection_leaves_one_installation_audit_event() {
    let harness = ghinvite_e2e::Harness::start().await;
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(20))
        .build()
        .unwrap();
    let storage = Arc::new(
        ghinvite_storage_sqlx::SqlxStorage::in_memory()
            .await
            .unwrap(),
    );
    storage.run_migrations().await.unwrap();
    storage
        .upsert_user(&User {
            user_id: 7,
            login: "installer".into(),
            avatar_url: None,
            github_created_at: None,
            last_seen_at: Utc::now(),
        })
        .await
        .unwrap();

    // GitHub confirms installation 9 belongs to account 100 and shares one repo.
    let stub = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", stub.local_addr().unwrap());
    let stub_server = tokio::spawn(async move {
        axum::serve(
            stub,
            axum::Router::new().fallback(|request: axum::extract::Request| async move {
                let path = request.uri().path();
                let (status, body) = if path.ends_with("/access_tokens") {
                    (
                        201,
                        json!({"token":"fixture","expires_at":"2099-01-01T00:00:00Z"}),
                    )
                } else if path == "/installation/repositories" {
                    (
                        200,
                        json!({"total_count":1,"repositories":[{"id":10,"full_name":"acme/api","private":true}]}),
                    )
                } else if path == "/app/installations/9" {
                    (
                        200,
                        json!({"id":9,"account":{"id":100,"login":"acme","type":"Organization"},"app_id":123,"created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T00:00:00Z","repository_selection":"all","permissions":{"administration":"write","metadata":"read"},"suspended_at":null}),
                    )
                } else {
                    (404, json!({}))
                };
                (
                    axum::http::StatusCode::from_u16(status).unwrap(),
                    axum::Json(body),
                )
            }),
        )
        .await
        .unwrap();
    });

    let acknowledgements = Arc::new(LostAuditAcknowledgement {
        inner: storage.clone(),
        swallowed: AtomicUsize::new(0),
    });
    let state =
        ghinvite_workflows::AppState::new(acknowledgements.clone()).with_github_apps(Arc::new(
            ghinvite_workflows::RepositoryGithub::new(
                ghinvite_e2e::github_transport(client.clone()),
                &base,
                ghinvite_e2e::repository_apps(&base),
            )
            .unwrap(),
        ));
    // Projection ownership is not what this test varies; only the audit write is.
    let endpoint = ghinvite_workflows::build_endpoint(state, storage.clone(), None).unwrap();
    harness.restate.deploy(endpoint).await;
    let ingress = harness.restate.ingress_url();

    let response = client
        .post(format!("{ingress}/Installation/repo:9/onboard"))
        .json(&json!({
            "operation_id": RequestId::new().to_string(),
            "verified": {
                "app": "repo",
                "account": {
                    "installation_id": 9,
                    "account_id": 100,
                    "account_login": "acme",
                    "account_type": "Organization",
                    "selected_repos": "all",
                    "installed_at": "2026-01-01T00:00:00Z",
                    "uninstalled_at": null
                },
                "permissions": {"administration":"write","metadata":"read"},
                "suspended_at": null
            }
        }))
        .send()
        .await
        .unwrap();
    assert!(
        response.status().is_success(),
        "{}",
        response.text().await.unwrap()
    );

    // The projection is a durable send, and its first audit write is refused
    // after committing, so the row only settles once the retry has run.
    let created = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if acknowledgements.swallowed.load(Ordering::SeqCst) > 1
                && storage
                    .app_installation(account::App::Repo, 9)
                    .await
                    .unwrap()
                    .is_some()
            {
                return installation_created_events(&storage).await;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("the retried projection should settle");

    assert_eq!(
        created, 1,
        "a retry must replay the retained audit identity, not mint a second one"
    );

    stub_server.abort();
    harness.finish().await;
}

async fn installation_created_events(storage: &ghinvite_storage_sqlx::SqlxStorage) -> usize {
    storage
        .list_audit_events(100, None, AuditPosition::Latest)
        .await
        .unwrap()
        .events
        .iter()
        .filter(|event| event.event_type == EventType::InstallationCreated)
        .count()
}
