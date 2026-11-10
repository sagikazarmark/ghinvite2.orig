//! Operator-only telemetry through protected sessions and actual SQLite reads.
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use chrono::{Duration, Utc};
use ghinvite_core::{InvitationLinkId, RequestId};
use ghinvite_github::mocks::MockTransport;
use ghinvite_web::{WebConfig, session, session_store::ProtectedStore};
use ghinvite_web_server::SqliteBackend;
use http_body_util::BodyExt;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use std::sync::Arc;
use tower::ServiceExt;
use tower_sessions::session::Id;

struct Fixture {
    app: axum::Router,
    pool: sqlx::SqlitePool,
    path: std::path::PathBuf,
    operator: Id,
    member: Id,
}
impl Fixture {
    async fn new() -> Self {
        let path = std::env::temp_dir().join(format!("p1-operations-{}.sqlite", RequestId::new()));
        let storage = ghinvite_storage_sqlx::SqlxStorage::at_path(&path)
            .await
            .unwrap();
        // Controlled projected rows for read-model edge cases. Route behavior,
        // SQL aggregation and encrypted session authentication are all real.
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(&path)
                    .foreign_keys(false),
            )
            .await
            .unwrap();
        let mut config = WebConfig::for_local_dev_with_secret([17; 32]);
        config.operator_ids.insert(42);
        let backend =
            SqliteBackend::new(sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap());
        backend.migrate().await.unwrap();
        let store = ProtectedStore::new(backend, config.keyring.clone());
        let operator = identity(&store, 42).await;
        let member = identity(&store, 77).await;
        let state = crate::common::sign_in::app_state(
            Arc::new(storage),
            Arc::new(MockTransport::scripted(vec![])),
            Arc::new(ghinvite_web::RestateClient::new("http://127.0.0.1:9").unwrap()),
            config,
        );
        Self {
            app: common::build_app(state, store),
            pool,
            path,
            operator,
            member,
        }
    }
    async fn seed(&self) {
        let now = Utc::now();
        for (account, rows) in [
            (
                1001,
                vec![
                    (
                        "outcome_unknown",
                        None,
                        Some(now - Duration::seconds(3700)),
                        None,
                        None,
                    ),
                    ("outcome_unknown", None, None, None, None),
                    (
                        "outcome_unknown",
                        None,
                        Some(now + Duration::minutes(5)),
                        None,
                        None,
                    ),
                    (
                        "created",
                        None,
                        None,
                        Some(now - Duration::seconds(7300)),
                        None,
                    ),
                    (
                        "already_pending",
                        None,
                        None,
                        Some(now - Duration::hours(4)),
                        Some(now - Duration::seconds(10)),
                    ),
                    (
                        "blocked",
                        Some("installation_permissions"),
                        None,
                        None,
                        None,
                    ),
                ],
            ),
            (
                2002,
                vec![
                    (
                        "blocked",
                        Some("outside_collaborators_restricted"),
                        None,
                        None,
                        None,
                    ),
                    ("blocked", Some("ip_allow_list"), None, None, None),
                ],
            ),
        ] {
            let link = InvitationLinkId::new().to_string();
            let request = RequestId::new().to_string();
            sqlx::query("INSERT INTO invitation_links (id,installation_id,account_id,created_by,created_at,approval_required,secret_generation,secret_sha256,projection_revision,projection_content) VALUES (?1,1,?2,42,?3,1,1,?4,1,'{}')")
                .bind(&link).bind(account).bind((now-Duration::hours(5)).to_rfc3339()).bind("17".repeat(32)).execute(&self.pool).await.unwrap();
            sqlx::query("INSERT INTO invitation_requests (id,invitation_link_id,requester_id,state,created_at,projection_revision) VALUES (?1,?2,77,'approved',?3,1)")
                .bind(&request).bind(&link).bind((now-Duration::hours(4)).to_rfc3339()).execute(&self.pool).await.unwrap();
            for (index, (outcome, reason, unknown, sent, observed)) in rows.into_iter().enumerate()
            {
                let target = index as i64 + 1;
                sqlx::query("INSERT INTO deliveries (id,request_id,target_id,delivery_generation,create_outcome,outcome_reason,unknown_since,sent_at,last_observed_at,created_at,updated_at) VALUES (?1,?2,?3,1,?4,?5,?6,?7,?8,?9,?9)")
                    .bind(format!("{request}:{target}:1")).bind(&request).bind(target)
                    .bind(outcome).bind(reason).bind(unknown.map(|date|date.to_rfc3339())).bind(sent.map(|date|date.to_rfc3339())).bind(observed.map(|date|date.to_rfc3339()))
                    .bind((now-Duration::hours(4)).to_rfc3339()).execute(&self.pool).await.unwrap();
            }
        }
    }
    async fn get(&self, path: &str, identity: Option<Id>) -> (StatusCode, String) {
        let mut request = Request::builder().uri(path);
        if let Some(id) = identity {
            request = request.header("cookie", format!("id={id}"));
        }
        let response = self
            .app
            .clone()
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.headers()["cache-control"], "private, no-store");
        let status = response.status();
        let body = String::from_utf8(
            response
                .into_body()
                .collect()
                .await
                .unwrap()
                .to_bytes()
                .to_vec(),
        )
        .unwrap();
        (status, body)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}
async fn identity(store: &ProtectedStore<SqliteBackend>, user_id: u64) -> Id {
    let tower = tower_sessions::Session::new(None, Arc::new(store.clone()), None);
    ghinvite_web::session_store::establish_authenticated_lifetime(&tower)
        .await
        .unwrap();
    session::save(
        &tower,
        &session::Session {
            user_id,
            login: format!("user-{user_id}"),
            csrf_token: Some("csrf".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    tower.save().await.unwrap();
    tower.id().unwrap()
}

#[tokio::test]
async fn operator_reads_actionable_counts_and_incomplete_evidence_without_mutating() {
    let fixture = Fixture::new().await;
    fixture.seed().await;
    let (status, body) = fixture
        .get("/console/operations", Some(fixture.operator))
        .await;
    assert_eq!(status, StatusCode::OK);
    for text in [
        "Delivery operations",
        "Incomplete observation",
        "Account 1001",
        "Account 2002",
        "more than 1 hour",
        "more than 2 hours",
        "Missing timestamps",
        "Future timestamps",
        "Threshold exceeded",
        "App permissions",
        "Organization policy restricts outside collaborators",
        "IP allow list",
        "do not repeat an unconfirmed write",
    ] {
        assert!(body.contains(text), "missing {text}");
    }
    let (status, body) = fixture
        .get("/console/operations.json", Some(fixture.operator))
        .await;
    assert_eq!(status, StatusCode::OK);
    let report: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(report["repository_deliveries"], 8);
    assert_eq!(report["incomplete"], true);
    assert_eq!(report["unknown"]["total"], 3);
    assert_eq!(report["unknown"]["over_threshold"], 1);
    assert_eq!(report["unknown"]["missing_timestamp"], 1);
    assert_eq!(report["unknown"]["future_timestamp"], 1);
    assert_eq!(report["pending"]["total"], 2);
    assert_eq!(report["pending"]["over_threshold"], 1);
    assert_eq!(report["by_account"][0]["account_id"], 1001);
    assert_eq!(
        report["by_account"][1]["blocked"].as_array().unwrap().len(),
        2
    );
    assert!(
        report["observed_at"]
            .as_str()
            .unwrap()
            .parse::<chrono::DateTime<Utc>>()
            .is_ok()
    );
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM deliveries")
        .fetch_one(&fixture.pool)
        .await
        .unwrap();
    assert_eq!(count, 8);
}

#[tokio::test]
async fn anonymous_and_nonoperator_are_concealed_before_the_storage_read() {
    let fixture = Fixture::new().await;
    fixture.seed().await;
    for identity in [None, Some(fixture.member)] {
        for path in ["/console/operations", "/console/operations.json"] {
            let (status, body) = fixture.get(path, identity).await;
            assert_eq!(status, StatusCode::NOT_FOUND);
            for private in [
                "1001",
                "2002",
                "unknown",
                "outside_collaborators",
                "observed_at",
            ] {
                assert!(!body.contains(private));
            }
        }
    }
    sqlx::query("DROP TABLE deliveries")
        .execute(&fixture.pool)
        .await
        .unwrap();
    assert_eq!(
        fixture
            .get("/console/operations", Some(fixture.member))
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    for path in ["/console/operations", "/console/operations.json"] {
        let (status, body) = fixture.get(path, Some(fixture.operator)).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert!(body.contains("Health cannot be determined"));
        assert!(!body.contains("no such table"));
        assert!(!body.contains("1001"));
    }
}

#[tokio::test]
async fn an_empty_projection_does_not_claim_healthy_delivery() {
    let fixture = Fixture::new().await;
    let (status, body) = fixture
        .get("/console/operations", Some(fixture.operator))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("No projected delivery steps"));
    assert!(body.contains("no delivery health evidence"));
    assert!(!body.contains("All healthy"));
}
mod common;

#[tokio::test]
async fn operator_report_includes_linkless_bulk_and_email_without_private_address_refs() {
    let storage = ghinvite_storage_sqlx::SqlxStorage::in_memory()
        .await
        .unwrap();
    // Run the rollback-heavy shared fixture on its single SQLite connection,
    // then exercise the real report using a protected operator session.
    ghinvite_core::storage::test_suite::scenario_bulk_review(storage.clone()).await;
    let mut config = WebConfig::for_local_dev_with_secret([17; 32]);
    config.operator_ids.insert(42);
    let backend = SqliteBackend::new(sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap());
    backend.migrate().await.unwrap();
    let store = ProtectedStore::new(backend, config.keyring.clone());
    let operator = identity(&store, 42).await;
    let member = identity(&store, 77).await;
    let app = common::build_app(
        crate::common::sign_in::app_state(
            Arc::new(storage),
            Arc::new(MockTransport::scripted(vec![])),
            Arc::new(ghinvite_web::RestateClient::new("http://127.0.0.1:9").unwrap()),
            config,
        ),
        store,
    );
    async fn get(app: &axum::Router, path: &str, id: Id) -> (StatusCode, String) {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(path)
                    .header("cookie", format!("id={id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.headers()["cache-control"], "private, no-store");
        let status = response.status();
        let body = String::from_utf8(
            response
                .into_body()
                .collect()
                .await
                .unwrap()
                .to_bytes()
                .to_vec(),
        )
        .unwrap();
        (status, body)
    }
    let (status, body) = get(&app, "/console/operations.json", operator).await;
    assert_eq!(status, StatusCode::OK);
    let report: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(report["repository_deliveries"], 3);
    assert_eq!(report["organization_deliveries"], 1);
    assert_eq!(report["team_steps"], 1);
    assert_eq!(report["email_deliveries"], 1);
    let steps = report["steps"].as_array().unwrap();
    assert_eq!(steps.len(), 6);
    for step in steps {
        assert!(step["link_id"].is_null());
        assert!(step["batch_id"].is_string());
        assert_eq!(step["request_id"].is_null(), step["kind"] == "email");
    }
    assert!(!body.contains("email_ref"));
    assert!(!body.contains(&"ed".repeat(32)));
    let (status, html) = get(&app, "/console/operations", operator).await;
    assert_eq!(status, StatusCode::OK);
    assert!(html.contains("Email invitation"));
    assert!(html.contains("email invitations: 1"));
    assert!(html.contains("Batch "));
    assert!(!html.contains("Request None"));
    assert!(!html.contains(&"ed".repeat(32)));
    let (status, body) = get(&app, "/console/operations.json", member).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(!body.contains("batch_id"));
    assert!(!body.contains("email_ref"));
}

#[tokio::test]
async fn organization_and_team_steps_have_explicit_identity_and_concealed_scope() {
    let fixture = Fixture::new().await;
    fixture.seed().await;
    let request:String=sqlx::query_scalar("SELECT r.id FROM invitation_requests r JOIN invitation_links l ON l.id=r.invitation_link_id WHERE l.account_id=1001 LIMIT 1").fetch_one(&fixture.pool).await.unwrap();
    let id = format!("org:{request}:1001:1");
    let at = (Utc::now() - Duration::hours(3)).to_rfc3339();
    sqlx::query("INSERT INTO deliveries(id,request_id,kind,target_id,delivery_generation,create_outcome,outcome_reason,created_at,updated_at) VALUES(?1,?2,'org',1001,1,'blocked','no_seat',?3,?3)").bind(&id).bind(&request).bind(&at).execute(&fixture.pool).await.unwrap();
    let receipt=serde_json::json!({"create":{"team_id":99,"outcome":{"kind":"outcome_unknown"},"revision":2,"confirmed_at":null},"revision":2,"tracking_stopped":null,"settlement":null}).to_string();
    sqlx::query("INSERT INTO delivery_steps(delivery_id,step,receipt,outcome,reason,updated_at,projection_revision) VALUES(?1,'team:99',?2,'outcome_unknown',NULL,?3,2)").bind(&id).bind(receipt).bind(&at).execute(&fixture.pool).await.unwrap();
    let (status, body) = fixture
        .get("/console/operations.json", Some(fixture.operator))
        .await;
    assert_eq!(status, StatusCode::OK);
    let report: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(report["organization_deliveries"], 1);
    assert_eq!(report["team_steps"], 1);
    assert_eq!(report["unknown"]["missing_timestamp"], 2);
    assert_eq!(report["incomplete"], true);
    let step = report["steps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["step"] == "team:99")
        .unwrap();
    assert_eq!(step["id"], id);
    assert_eq!(step["kind"], "team");
    assert_eq!(step["target_id"], 99);
    let (_, html) = fixture
        .get("/console/operations", Some(fixture.operator))
        .await;
    assert!(html.contains("No organization seat available"));
    assert!(html.contains("Team 99"));
    assert!(html.contains("team:99"));
    for who in [None, Some(fixture.member)] {
        let (status, body) = fixture.get("/console/operations.json", who).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(!body.contains(&id));
        assert!(!body.contains("no_seat"));
    }
}
