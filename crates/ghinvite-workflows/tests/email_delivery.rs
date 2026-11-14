//! Actual Restate with standard opt-in composition, encrypted recovery and real budget.
#![cfg(feature = "integration")]
use axum::{
    extract::{Request, State},
    response::{IntoResponse, Response},
};
use chrono::Utc;
use ghinvite_core::{
    bulk::{BatchRef, BulkBatchId},
    email_delivery::{EmailCreateCommand, EmailRef},
    storage::{
        email_delivery::EmailDeliveryStorage,
        email_vault::{EmailVaultBinding, EmailVaultStorage},
    },
    targets::{MemberGrant, OrganizationTarget},
};
use ghinvite_e2e::{Call, Harness, journal::JournalAudit};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
#[path = "email_delivery/audit.rs"]
mod audit;
#[path = "email_delivery/bulk_cap.rs"]
mod bulk_cap;
#[path = "email_delivery/bulk_handoff.rs"]
mod bulk_handoff;
#[path = "email_delivery/journal.rs"]
mod journal;
#[path = "email_delivery/telemetry.rs"]
mod telemetry;
const MAIL: &str = "EMAIL_RUN_PRIVACY_MARKER+tag@example.com";
#[derive(Default)]
struct Github {
    trace: Mutex<Vec<(String, String, Value)>>,
    status: u16,
    account_login: Mutex<Option<String>>,
    batch_posts: Mutex<Vec<chrono::DateTime<Utc>>>,
}
async fn github(State(g): State<Arc<Github>>, r: Request) -> Response {
    let method = r.method().to_string();
    let path = r.uri().path().to_owned();
    let bytes = axum::body::to_bytes(r.into_body(), 65536).await.unwrap();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    g.trace
        .lock()
        .unwrap()
        .push((method.clone(), path.clone(), body.clone()));
    match path.as_str() {
  "/app/installations/2/access_tokens"=>{assert!(body["permissions"]==json!({"members":"read","metadata":"read"})||body["permissions"]==json!({"members":"write","metadata":"read"}));(axum::http::StatusCode::CREATED,axum::Json(json!({"token":"ghs_EMAIL_TOKEN_MARKER","expires_at":Utc::now()+chrono::Duration::hours(1)}))).into_response()},
  "/app/installations/2"=>axum::Json(json!({"id":2,"app_id":456,"account":{"id":100,"login":g.account_login.lock().unwrap().clone().unwrap_or_else(||"acme".into()),"type":"Organization"},"permissions":{"members":"write","metadata":"read"},"repository_selection":"all","suspended_at":null,"created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T00:00:00Z"})).into_response(),
  "/orgs/acme"|"/orgs/renamed" if method=="GET"=>axum::Json(json!({"id":100,"login":g.account_login.lock().unwrap().clone().unwrap_or_else(||"acme".into()),"created_at":"2020-01-01T00:00:00Z"})).into_response(),
  "/orgs/acme/invitations" if method=="POST"=>{let email=body["email"].as_str().unwrap(); let batch_row=email.strip_prefix("BULK60_EMAIL_PRIVACY_MARKER+").and_then(|s|s.strip_suffix("@example.com")).map(|s|s.parse::<u64>().unwrap()); assert!(email==MAIL || batch_row.is_some_and(|r|(1..=60).contains(&r))); assert_eq!(body,json!({"email":email,"role":"direct_member","team_ids":[]})); if batch_row.is_some(){g.batch_posts.lock().unwrap().push(Utc::now());}
 if g.status==201 {(axum::http::StatusCode::CREATED,axum::Json(json!({"id":batch_row.map_or(77,|r|1000+r),"created_at":Utc::now(),"email":email}))).into_response()}else{axum::http::StatusCode::from_u16(g.status).unwrap().into_response()}},
  "/orgs/acme/invitations"|"/orgs/acme/failed_invitations"|"/orgs/acme/teams"|"/orgs/renamed/invitations"|"/orgs/renamed/failed_invitations"|"/orgs/renamed/teams" if method=="GET"=>axum::Json(json!([])).into_response(),
  _=>panic!("forbidden or unexpected email delivery HTTP call {method} {path}")
 }
}
struct Fixture {
    h: Harness,
    g: Arc<Github>,
    storage: Arc<ghinvite_storage_sqlx::SqlxStorage>,
    vault: Arc<ghinvite_email_vault::EmailVault>,
}
impl Fixture {
    async fn new(status: u16) -> Self {
        Self::with_audit_fault(status, None).await
    }
    async fn with_audit_fault(
        status: u16,
        faults: Option<Arc<ghinvite_workflows::email_delivery::EmailAuditFaults>>,
    ) -> Self {
        let g = Arc::new(Github {
            status,
            ..Default::default()
        });
        let h = Harness::start_with_github_router(
            axum::Router::new().fallback(github).with_state(g.clone()),
        )
        .await;
        let storage = Arc::new(
            ghinvite_storage_sqlx::SqlxStorage::in_memory()
                .await
                .unwrap(),
        );
        let ring = Arc::new(
            kekring::Keyring::builder("p1-email")
                .generation(1, [7; 32])
                .unwrap()
                .current(1)
                .unwrap()
                .build()
                .unwrap(),
        );
        let vault = Arc::new(ghinvite_email_vault::EmailVault::new(storage.clone(), ring));
        let github_transport = ghinvite_e2e::github_transport(h.client.clone());
        let mut state = ghinvite_workflows::AppState::new(storage.clone())
            .with_email_deliveries(storage.clone(), vault.clone())
            .with_organization_delivery(storage.clone())
            .with_bulk_invites(storage.clone())
            .with_github_apps(Arc::new(
                ghinvite_workflows::RepositoryGithub::new(
                    github_transport,
                    &h.github_url,
                    ghinvite_e2e::installed_apps(&h.github_url, true),
                )
                .unwrap(),
            ));
        state.email_audit_faults = faults;
        h.restate
            .deploy(ghinvite_workflows::build_endpoint(state, storage.clone(), None).unwrap())
            .await;
        JournalAudit::retain(
            &h,
            &[
                "BulkInviteBatch",
                "BulkProjection",
                "EmailInviteDelivery",
                "EmailDeliveryProjection",
                "InviteBudget",
                "InviteBudgetProjection",
            ],
        )
        .await;
        let mut setup = ghinvite_e2e::setup_command(
            json!({"installation_id":2,"account_id":100,"account_login":"acme","account_type":"Organization","selected_repos":[],"installed_at":"2026-01-01T00:00:00Z"}),
        );
        setup["verified"]["app"] = json!("org");
        setup["verified"]["permissions"] = json!({"members":"write","metadata":"read"});
        let result = h
            .restate
            .invoke(
                &Call::object("Installation", "org:2", "onboard"),
                Some(&setup),
                None,
            )
            .await;
        assert_eq!(result.status, 200, "{result:?}");
        Self {
            h,
            g,
            storage,
            vault,
        }
    }
    async fn command(&self, active: bool) -> EmailCreateCommand {
        let command = EmailCreateCommand {
            batch: BatchRef {
                batch_id: BulkBatchId::new(),
                row: 1,
            },
            account_id: 100,
            installation_id: 2,
            organization: OrganizationTarget {
                org_id: 100,
                org_login: "acme".into(),
                grant: MemberGrant::Member,
            },
            teams: vec![],
            email_ref: EmailRef::from_bytes([8; 32]),
            approved_by: 7,
            approved_at: Utc::now(),
        };
        let b = EmailVaultBinding {
            email_ref: command.email_ref,
            batch: command.batch,
            account_id: 100,
            installation_id: 2,
        };
        self.vault
            .stage(
                &b,
                &ghinvite_email_vault::Address::parse(MAIL).unwrap(),
                command.approved_at.timestamp(),
            )
            .await
            .unwrap();
        if active {
            assert!(
                self.storage
                    .activate_email(&b, command.approved_at.timestamp())
                    .await
                    .unwrap()
            );
        }
        command
    }
    async fn create(&self, c: &EmailCreateCommand) -> Value {
        let r = self
            .h
            .restate
            .invoke(
                &Call::object("EmailInviteDelivery", &c.delivery_key(), "create"),
                Some(&serde_json::to_value(c).unwrap()),
                None,
            )
            .await;
        assert_eq!(r.status, 200, "{r:?}");
        let journal = self.h.restate.admin().journal(r.invocation_id()).await;
        assert!(!journal.is_empty());
        for entry in journal {
            assert!(!entry.raw_contains(MAIL));
            assert!(!entry.raw_contains("ghs_EMAIL_TOKEN_MARKER"));
        }
        r.body
    }
    fn posts(&self) -> usize {
        self.g
            .trace
            .lock()
            .unwrap()
            .iter()
            .filter(|(m, p, _)| m == "POST" && p == "/orgs/acme/invitations")
            .count()
    }
    async fn budget(&self) -> Value {
        let r = self
            .h
            .restate
            .invoke(
                &Call::object("InviteBudget", "org:100", "snapshot"),
                None::<&Value>,
                None,
            )
            .await;
        assert_eq!(r.status, 200);
        r.body
    }
}
#[tokio::test]
async fn confirmed_email_create_replays_one_post_without_requester_or_plaintext_journal() {
    let f = Fixture::new(201).await;
    let c = f.command(true).await;
    let first = f.create(&c).await;
    assert_eq!(first["outcome"]["kind"], "created");
    assert_eq!(first["outcome"]["upstream_id"], 77);
    for _ in 0..3 {
        assert_eq!(f.create(&c).await, first);
    }
    assert_eq!(f.posts(), 1);
    let mut changed = c.clone();
    changed.approved_by = 9;
    let conflict =
        f.h.restate
            .invoke(
                &Call::object("EmailInviteDelivery", &c.delivery_key(), "create"),
                Some(&serde_json::to_value(&changed).unwrap()),
                None,
            )
            .await;
    assert_eq!(conflict.status, 409);
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            if f.storage
                .email_delivery(&c.storage_id())
                .await
                .unwrap()
                .is_some_and(|s| {
                    matches!(
                        s.outcome,
                        ghinvite_core::email_delivery::EmailCreateOutcome::Created { .. }
                    )
                })
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(f.budget().await["content_used_hour"], 1);
    let rows = f
        .storage
        .batch_email_deliveries(100, c.batch.batch_id)
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert!(!serde_json::to_string(&rows).unwrap().contains(MAIL));
    let target = json!({"account":{"app":"org","account_id":100},"target_id":100,"deliveries":[c.storage_id()]});
    let observed =
        f.h.restate
            .invoke(
                &Call::service("OrganizationReconcile", "target"),
                Some(&target),
                None,
            )
            .await;
    assert_eq!(observed.status, 200, "{observed:?}");
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let status =
                f.h.restate
                    .invoke(
                        &Call::object("EmailInviteDelivery", &c.delivery_key(), "status"),
                        None::<&Value>,
                        None,
                    )
                    .await;
            if status.body["settlement"]["kind"] == "no_longer_pending" {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(f.posts(), 1);
    f.h.finish().await;
}
#[tokio::test]
async fn ambiguous_post_stays_unknown_charged_and_never_retries_even_after_list_absence() {
    let f = Fixture::new(503).await;
    let c = f.command(true).await;
    let first = f.create(&c).await;
    assert_eq!(first["outcome"]["kind"], "outcome_unknown");
    let unknown: chrono::DateTime<Utc> =
        serde_json::from_value(first["unknown_since"].clone()).unwrap();
    assert!(unknown >= c.approved_at && unknown <= Utc::now());
    assert!(first.get("last_observed_at").is_none());
    for _ in 0..3 {
        assert_eq!(f.create(&c).await, first);
    }
    assert_eq!(f.posts(), 1);
    assert_eq!(f.budget().await["content_used_hour"], 1);
    let at = Utc::now();
    let observation = ghinvite_core::org_observation::OrganizationObservation {
        account_id: 100,
        installation_id: 2,
        org_id: 100,
        org_login: "acme".into(),
        observation_started_at: at,
        observed_at: at,
        pending: vec![],
        failed: vec![],
    };
    let r =
        f.h.restate
            .invoke(
                &Call::object("EmailInviteDelivery", &c.delivery_key(), "reconcile"),
                Some(&serde_json::to_value(&observation).unwrap()),
                None,
            )
            .await;
    assert_eq!(r.status, 200);
    assert_eq!(f.create(&c).await, first);
    assert_eq!(f.posts(), 1);
    f.h.finish().await;
}
#[tokio::test]
async fn staged_address_without_activation_blocks_and_refunds_unused_slot() {
    let f = Fixture::new(201).await;
    let c = f.command(false).await;
    let first = f.create(&c).await;
    assert_eq!(
        first["outcome"],
        json!({"kind":"blocked","reason":"email_recovery_unavailable"})
    );
    assert_eq!(f.posts(), 0);
    assert_eq!(f.budget().await["content_used_hour"], 0);
    f.h.finish().await;
}

#[tokio::test]
async fn preinitialization_cancel_fences_later_create_and_does_not_reserve_or_post() {
    let f = Fixture::new(201).await;
    let c = f.command(true).await;
    let r =
        f.h.restate
            .invoke(
                &Call::object("EmailInviteDelivery", &c.delivery_key(), "cancel"),
                Some(&serde_json::to_value(&c).unwrap()),
                None,
            )
            .await;
    assert_eq!(r.status, 200, "{r:?}");
    assert_eq!(
        r.body["outcome"],
        json!({"kind":"aborted","reason":"batch_cancelled"})
    );
    assert_eq!(f.create(&c).await, r.body);
    assert_eq!(f.posts(), 0);
    assert_eq!(f.budget().await["content_used_hour"], 0);
    f.h.finish().await;
}
#[tokio::test]
async fn positive_rejection_releases_fence_and_unused_budget_without_repeating_failed_receipt() {
    let f = Fixture::new(422).await;
    let c = f.command(true).await;
    let r = f.create(&c).await;
    assert_eq!(r["outcome"]["kind"], "failed");
    assert_eq!(f.create(&c).await, r);
    assert_eq!(f.posts(), 1);
    assert_eq!(f.budget().await["content_used_hour"], 0);
    assert!(
        !f.storage
            .email_attempt_exists(&ghinvite_core::email_delivery::EmailAttempt { create: c })
            .await
            .unwrap()
    );
    f.h.finish().await;
}

#[path = "email_delivery/bulk_journal.rs"]
mod bulk_journal;
