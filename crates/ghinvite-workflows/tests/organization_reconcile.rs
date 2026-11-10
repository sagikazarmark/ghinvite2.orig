//! Real scheduled service/fanout with HTTP pagination and recording owners.
#![cfg(feature = "integration")]
use axum::{
    extract::{Request, State},
    response::{IntoResponse, Response},
};
use chrono::{Duration, Utc};
use ghinvite_core::{
    Account, AccountType, GithubInvitationId, InvitationLinkId, RequestId, SelectedRepos,
    org_delivery::*, org_observation::OrganizationObservation,
};
use ghinvite_e2e::{Call, Harness};
use ghinvite_workflows::{AppState, RepositoryGithub};
use restate_sdk::{
    context::{ContextReadState, ContextWriteState, ObjectContext, SharedObjectContext},
    errors::TerminalError,
    serde::Json,
};
use serde_json::{Value, json};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicI64, Ordering},
};
#[derive(Default)]
struct Github {
    base: String,
    fail_second: bool,
    requests: Vec<String>,
}
async fn github(State(state): State<Arc<Mutex<Github>>>, request: Request) -> Response {
    let path = request.uri().path().to_owned();
    let query = request.uri().query().unwrap_or("").to_owned();
    let body = axum::body::to_bytes(request.into_body(), 8192)
        .await
        .unwrap();
    let mut s = state.lock().unwrap();
    s.requests.push(format!("{path}?{query}"));
    if path == "/app/installations/2/access_tokens" {
        assert_eq!(
            serde_json::from_slice::<Value>(&body).unwrap(),
            json!({"permissions":{"members":"read","metadata":"read"}})
        );
        return (
            axum::http::StatusCode::CREATED,
            axum::Json(json!({"token":"ghs_org_sweep","expires_at":"2099-01-01T00:00:00Z"})),
        )
            .into_response();
    }
    if path == "/orgs/acme" {
        return axum::Json(json!({"id":100,"login":"acme","created_at":"2020-01-01T00:00:00Z"}))
            .into_response();
    }
    let invitation = |id| json!({"id":id,"login":"alice","email":null,"role":"direct_member","created_at":"2026-01-01T00:00:00Z","inviter":{"id":7,"login":"owner"},"team_count":0});
    if path == "/orgs/acme/invitations" {
        if query.contains("page=2") {
            if s.fail_second {
                return axum::http::StatusCode::SERVICE_UNAVAILABLE.into_response();
            }
            return axum::Json(json!([invitation(100)])).into_response();
        }
        return (
            [(
                "link",
                format!("<{}/orgs/acme/invitations?page=2>; rel=\"next\"", s.base),
            )],
            axum::Json(json!([invitation(99)])),
        )
            .into_response();
    }
    if path == "/orgs/acme/failed_invitations" {
        return axum::Json(json!([])).into_response();
    }
    panic!("unexpected {path}")
}
struct AccountInstallation;
fn account() -> Account {
    Account {
        installation_id: 2,
        account_id: 100,
        account_login: "acme".into(),
        account_type: AccountType::Organization,
        selected_repos: SelectedRepos::All,
        installed_at: Utc::now() - Duration::days(1),
        uninstalled_at: None,
    }
}
#[restate_sdk::object]
impl AccountInstallation {
    #[handler]
    async fn current_installation(
        &self,
        ctx: SharedObjectContext<'_>,
    ) -> Result<Json<Option<Account>>, TerminalError> {
        assert_eq!(ctx.key(), "org:100");
        Ok(Json(Some(account())))
    }
    #[handler]
    async fn recheck(&self, _ctx: ObjectContext<'_>) -> Result<(), TerminalError> {
        Ok(())
    }
}
struct OrganizationDelivery;
#[restate_sdk::object]
impl OrganizationDelivery {
    #[handler]
    async fn seed(
        &self,
        ctx: ObjectContext<'_>,
        Json(snapshot): Json<OrganizationDeliverySnapshot>,
    ) -> Result<(), TerminalError> {
        assert_eq!(ctx.key(), snapshot.create.command.delivery_key());
        ctx.set("snapshot", Json(snapshot));
        Ok(())
    }
    #[handler]
    async fn status(
        &self,
        ctx: SharedObjectContext<'_>,
    ) -> Result<Json<Option<OrganizationDeliverySnapshot>>, TerminalError> {
        Ok(Json(
            ctx.get::<Json<OrganizationDeliverySnapshot>>("snapshot")
                .await?
                .map(|j| j.0),
        ))
    }
    #[handler]
    async fn observe_listed(
        &self,
        ctx: ObjectContext<'_>,
        Json(observation): Json<OrganizationObservation>,
    ) -> Result<(), TerminalError> {
        observation.validate().unwrap();
        ctx.set("observation", Json(observation));
        ctx.set("count", ctx.get::<u64>("count").await?.unwrap_or(0) + 1);
        Ok(())
    }
    #[handler]
    async fn observations(
        &self,
        ctx: SharedObjectContext<'_>,
    ) -> Result<Json<Value>, TerminalError> {
        Ok(Json(
            json!({"count":ctx.get::<u64>("count").await?.unwrap_or(0),"last":ctx.get::<Json<OrganizationObservation>>("observation").await?.map(|j|j.0)}),
        ))
    }
}
async fn call(
    h: &Harness,
    service: &str,
    key: &str,
    handler: &str,
    input: Option<&Value>,
) -> Value {
    let response = h
        .restate
        .invoke(&Call::object(service, key, handler), input, None)
        .await;
    assert_eq!(response.status, 200, "{handler}: {}", response.body);
    response.body
}
#[tokio::test]
async fn organization_schedule_discards_partial_walk_and_reuses_complete_lists_for_all_deliveries()
{
    let github_state = Arc::new(Mutex::new(Github {
        fail_second: true,
        ..Default::default()
    }));
    let h = Harness::start_with_github_router(
        axum::Router::new()
            .fallback(github)
            .with_state(github_state.clone()),
    )
    .await;
    github_state.lock().unwrap().base = h.github_url.clone();
    let github_transport = ghinvite_e2e::github_transport(h.client.clone());
    let storage = Arc::new(
        ghinvite_storage_sqlx::SqlxStorage::in_memory()
            .await
            .unwrap(),
    );
    let state = AppState::new(storage.clone())
        .with_github_apps(Arc::new(
            RepositoryGithub::new(
                github_transport,
                &h.github_url,
                ghinvite_e2e::installed_apps(&h.github_url, true),
            )
            .unwrap(),
        ))
        .with_organization_delivery(storage);
    let clock = Arc::new(AtomicI64::new(Utc::now().timestamp_millis()));
    let endpoint = restate_sdk::endpoint::Endpoint::builder()
        .bind(AccountInstallation)
        .bind(OrganizationDelivery)
        .bind(ghinvite_workflows::sweep_schedule::SweepSchedule {
            organization_enabled: true,
            clock: Some(clock.clone()),
        })
        .bind(ghinvite_workflows::reconcile::Reconcile {
            state: state.clone(),
        })
        .bind(ghinvite_workflows::org_reconcile::OrganizationReconcile { state })
        .build();
    h.restate.deploy(endpoint).await;
    call(&h, "SweepSchedule", "org:100", "ensure_scheduled", None).await;
    let idle = call(&h, "SweepSchedule", "org:100", "status", None).await;
    assert_eq!(idle["cadence_seconds"], 86400);
    let mut keys = vec![];
    for user in [8, 9] {
        let command = OrganizationCreateCommand {
            batch: None,
            budget_attribution: Default::default(),
            access_lifetime_seconds: None,
            invitation_id: Some(GithubInvitationId::new()),
            link_id: Some(InvitationLinkId::new()),
            request_id: RequestId::new(),
            approval_id: "approval".into(),
            account_id: 100,
            installation_id: 2,
            requester_id: user,
            organization: ghinvite_core::targets::OrganizationTarget {
                org_id: 100,
                org_login: "acme".into(),
                grant: ghinvite_core::targets::MemberGrant::Member,
            },
            teams: vec![],
            delivery_generation: 1,
            approved_at: Utc::now(),
            budget_share_pct: 50,
        };
        let key = command.delivery_key();
        let mut snapshot = OrganizationDeliverySnapshot::planned(command).unwrap();
        snapshot.create.outcome = OrganizationCreateOutcome::OutcomeUnknown;
        call(
            &h,
            "OrganizationDelivery",
            &key,
            "seed",
            Some(&json!(snapshot)),
        )
        .await;
        call(
            &h,
            "SweepSchedule",
            "org:100",
            "track",
            Some(&json!({"target_id":100,"delivery_key":key})),
        )
        .await;
        keys.push(key);
    }
    let active = call(&h, "SweepSchedule", "org:100", "status", None).await;
    assert_eq!(active["cadence_seconds"], 3600);
    assert_eq!(active["tracked"]["100"].as_array().unwrap().len(), 2);
    assert_ne!(active["token"], idle["token"]);
    clock.fetch_add(3601000, Ordering::SeqCst);
    call(&h, "SweepSchedule", "org:100", "run", Some(&idle["token"])).await;
    assert!(
        github_state.lock().unwrap().requests.is_empty(),
        "stale token cannot start another sweep chain"
    );
    call(
        &h,
        "SweepSchedule",
        "org:100",
        "run",
        Some(&active["token"]),
    )
    .await;
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if github_state
                .lock()
                .unwrap()
                .requests
                .iter()
                .any(|p| p.contains("invitations?page=2"))
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|error| {
        panic!(
            "second page was not requested: {error}; trace={:?}",
            github_state.lock().unwrap().requests
        )
    });
    for key in &keys {
        assert_eq!(
            call(&h, "OrganizationDelivery", key, "observations", None).await["count"],
            0,
            "partial list supplies no evidence"
        );
    }
    github_state.lock().unwrap().fail_second = false;
    let next = call(&h, "SweepSchedule", "org:100", "status", None).await;
    clock.fetch_add(3601000, Ordering::SeqCst);
    call(&h, "SweepSchedule", "org:100", "run", Some(&next["token"])).await;
    for key in &keys {
        let view = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let v = call(&h, "OrganizationDelivery", key, "observations", None).await;
                if v["count"] == 1 {
                    break v;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(view["last"]["pending"].as_array().unwrap().len(), 2);
        assert_eq!(view["last"]["installation_id"], 2);
    }
    let trace = github_state.lock().unwrap().requests.clone();
    assert_eq!(
        trace
            .iter()
            .filter(|p| p.starts_with("/orgs/acme/invitations?"))
            .count(),
        4,
        "one failed and one complete two-page walk, shared across both owners"
    );
    assert_eq!(
        trace
            .iter()
            .filter(|p| p.starts_with("/orgs/acme/failed_invitations?"))
            .count(),
        1
    );
    h.finish().await;
}
