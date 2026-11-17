//! Real scheduled service/fanout with HTTP pagination and recording owners.
//!
//! GitHub is the octoemu emulator: 101 open invitations to acme, so the
//! organization's invitation list spans two pages of 100. A failing second
//! page is a fault rule; the walk is counted from the call log.
#![cfg(feature = "integration")]
use chrono::{Duration, Utc};
use ghinvite_core::{
    Account, AccountType, GithubInvitationId, InvitationLinkId, RequestId, SelectedRepos,
    org_delivery::*, org_observation::OrganizationObservation,
};
use ghinvite_e2e::{Call, Harness};
use ghinvite_workflows::{AppState, RepositoryGithub};
use octoemu::faults::{Fault, FaultAction};
use octoemu::seed::{OrgInvitation, User};
use restate_sdk::{
    context::{ContextReadState, ContextWriteState, ObjectContext, SharedObjectContext},
    errors::TerminalError,
    serde::Json,
};
use serde_json::{Value, json};
use std::sync::{
    Arc,
    atomic::{AtomicI64, Ordering},
};
/// The organization's open invitations: 101, from its owner.
const OPEN: u64 = 101;
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
    let world = (1..=OPEN).fold(ghinvite_e2e::github::seed(), |seed, n| {
        seed.user(User::new(3000 + n, format!("invitee-{n}")))
            .org_invitation(
                OrgInvitation::new(n, "acme", format!("invitee-{n}")).inviter("creator"),
            )
    });
    let h = Harness::start_with_emulator(ghinvite_e2e::github_emulator_with(world).await).await;
    let emulator = h.emulator();
    let second_page = h.github().rule(
        "second page fails",
        Fault::on(
            "GET",
            "/orgs/acme/invitations",
            FaultAction::respond_json(503, json!({"message":"Service Unavailable"})),
        )
        .query_contains("page=2"),
    );
    // The requests since the sweep started, as `path?query`.
    let requests =
        || -> Vec<String> { emulator.calls().into_iter().map(|call| call.path).collect() };
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
        requests().is_empty(),
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
            if requests()
                .iter()
                .any(|p| p.starts_with("/orgs/acme/invitations?") && p.contains("page=2"))
                && emulator.fault(second_page).unwrap().fired > 0
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
            requests()
        )
    });
    for key in &keys {
        assert_eq!(
            call(&h, "OrganizationDelivery", key, "observations", None).await["count"],
            0,
            "partial list supplies no evidence"
        );
    }
    assert!(emulator.remove_fault(second_page));
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
        assert_eq!(
            view["last"]["pending"].as_array().unwrap().len(),
            OPEN as usize
        );
        assert_eq!(view["last"]["installation_id"], 2);
    }
    let trace = requests();
    // Only the organization reads, with the organization App's read token
    // minted with its exact profile.
    for call in emulator.calls() {
        match call.path.split('?').next().unwrap_or_default() {
            "/app/installations/2/access_tokens" => assert_eq!(
                call.body_fields.as_ref().and_then(|f| f.get("permissions")),
                Some(&json!({"members":"read","metadata":"read"}))
            ),
            "/orgs/acme" | "/orgs/acme/invitations" | "/orgs/acme/failed_invitations" => {}
            other => panic!("unexpected {} {other}", call.method),
        }
    }
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
