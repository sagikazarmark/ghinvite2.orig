//! #83: one installation transition leaves one audit event behind, even when
//! the projection is retried after its audit write has already committed.
//!
//! `InstallationProjection::apply` retains the audit event in a `ctx.run` step
//! before it projects anything, so a retry replays the identity the first
//! attempt minted. Nothing but a real invocation retry can show that: if the
//! identity were minted outside the journal, the retry would mint a second one
//! and the account would gain a duplicate "installation created" entry.
#![cfg(feature = "integration")]

use chrono::{DateTime, Utc};
use ghinvite_core::audit::{AuditEvent, EventType};
use ghinvite_core::storage::*;
use ghinvite_core::*;
use restate_sdk::http_server::HttpServer;
use serde_json::json;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

/// Wraps the real storage so one installation audit write can commit and still
/// report failure. Every other call goes straight through.
struct LostAuditAcknowledgement {
    inner: Arc<ghinvite_storage_sqlx::SqlxStorage>,
    swallowed: AtomicUsize,
}

#[async_trait::async_trait]
impl Storage for LostAuditAcknowledgement {
    async fn insert_installation(&self, account: &Account) -> Result<()> {
        self.inner.insert_installation(account).await
    }
    async fn mark_installation_uninstalled(
        &self,
        installation_id: u64,
        when: DateTime<Utc>,
    ) -> Result<()> {
        self.inner
            .mark_installation_uninstalled(installation_id, when)
            .await
    }
    async fn update_installation_repos(
        &self,
        installation_id: u64,
        selected: &SelectedRepos,
    ) -> Result<()> {
        self.inner
            .update_installation_repos(installation_id, selected)
            .await
    }
    async fn get_installation(&self, installation_id: u64) -> Result<Option<Account>> {
        self.inner.get_installation(installation_id).await
    }
    async fn get_active_installation_by_account_id(
        &self,
        account_id: u64,
    ) -> Result<Option<Account>> {
        self.inner
            .get_active_installation_by_account_id(account_id)
            .await
    }
    async fn get_active_installation_by_login(&self, login: &str) -> Result<Option<Account>> {
        self.inner.get_active_installation_by_login(login).await
    }
    async fn get_latest_installation_by_login(&self, login: &str) -> Result<Option<Account>> {
        self.inner.get_latest_installation_by_login(login).await
    }
    async fn list_active_installations(&self) -> Result<Vec<Account>> {
        self.inner.list_active_installations().await
    }
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
    async fn invitation_link_belongs_to_account(
        &self,
        account_id: u64,
        id: InvitationLinkId,
    ) -> Result<bool> {
        self.inner
            .invitation_link_belongs_to_account(account_id, id)
            .await
    }
    async fn list_invitation_links_for_account(
        &self,
        account_id: u64,
    ) -> Result<Vec<InvitationLink>> {
        self.inner
            .list_invitation_links_for_account(account_id)
            .await
    }
    async fn get_invitation_request(&self, id: RequestId) -> Result<Option<InvitationRequest>> {
        self.inner.get_invitation_request(id).await
    }
    async fn count_pending_requests_for_account(&self, account_id: u64) -> Result<u64> {
        self.inner
            .count_pending_requests_for_account(account_id)
            .await
    }
    async fn insert_github_invitation(&self, invitation: &GithubInvitation) -> Result<()> {
        self.inner.insert_github_invitation(invitation).await
    }
    async fn get_github_invitation(
        &self,
        id: GithubInvitationId,
    ) -> Result<Option<GithubInvitation>> {
        self.inner.get_github_invitation(id).await
    }
    async fn get_github_invitation_by_github_id(
        &self,
        github_id: u64,
    ) -> Result<Option<GithubInvitation>> {
        self.inner
            .get_github_invitation_by_github_id(github_id)
            .await
    }
    async fn list_audit_events(
        &self,
        account_id: u64,
        event: Option<ghinvite_core::audit::EventType>,
        position: AuditPosition,
    ) -> Result<AuditPage> {
        self.inner
            .list_audit_events(account_id, event, position)
            .await
    }
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

#[tokio::test]
async fn a_retried_projection_leaves_one_installation_audit_event() {
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
                        json!({"id":9,"account":{"id":100,"login":"acme","type":"Organization"},"suspended_at":null}),
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
    let state = ghinvite_workflows::AppState::new(
        acknowledgements.clone(),
        Arc::new(
            ghinvite_github::InstallationClient::new(
                Arc::new(ghinvite_github::transport::ReqwestTransport::with_client(
                    client.clone(),
                )),
                ghinvite_github::jwt::AppJwtSigner::from_pem(
                    123,
                    include_str!("../../ghinvite-github/src/jwt_test_key.pem"),
                )
                .unwrap(),
            )
            .with_base(base),
        ),
    );
    // Projection ownership is not what this test varies; only the audit write is.
    let endpoint = ghinvite_workflows::build_endpoint(state, storage.clone(), None).unwrap();
    let listener = tokio::net::TcpListener::bind("0.0.0.0:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(async move {
        HttpServer::new(endpoint)
            .serve_with_cancel(listener, std::future::pending::<()>())
            .await;
    });

    let admin = std::env::var("RESTATE_ADMIN_URL")
        .expect("run scripts/test-restate.sh installation_audit_replay");
    let ingress = std::env::var("RESTATE_INGRESS_URL").unwrap();
    let host = std::env::var("RESTATE_ENDPOINT_HOST").unwrap();
    for _ in 0..100 {
        if client.get(format!("{admin}/health")).send().await.is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let response = client
        .post(format!("{admin}/deployments"))
        .json(&json!({"uri":format!("http://{host}:{port}")}))
        .send()
        .await
        .unwrap();
    assert!(
        response.status().is_success(),
        "{}",
        response.text().await.unwrap()
    );

    let response = client
        .post(format!("{ingress}/Installation/9/onboard"))
        .json(&json!({
            "installation_id": 9,
            "actor_user_id": 7,
            "account_id": 100,
            "account_login": "acme",
            "account_type": "Organization",
            "selected_repos": "all",
            "installed_at": "2026-05-04T12:00:00Z"
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
                && storage.get_installation(9).await.unwrap().is_some()
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

    server.abort();
    stub_server.abort();
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
