//! Real Restate, real budget, SQLite fences/projection and HTTP organization stages.
#![cfg(feature = "integration")]
#[path = "organization_delivery/access_grants.rs"]
mod access_grants;
#[path = "organization_delivery/audit.rs"]
mod audit;
#[path = "organization_delivery/budget_observations.rs"]
mod budget_observations;
#[path = "organization_delivery/cancel_unsent.rs"]
mod cancel_unsent;
#[path = "organization_delivery/journal.rs"]
mod journal;
#[path = "organization_delivery/policy.rs"]
mod policy;
#[path = "organization_delivery/reissue.rs"]
mod reissue;
#[path = "organization_delivery/reissue_request.rs"]
mod reissue_request;
#[path = "organization_delivery/requester_notice.rs"]
mod requester_notice;
#[path = "organization_delivery/telemetry.rs"]
mod telemetry;
use axum::{
    body::to_bytes,
    extract::{Request, State},
    response::{IntoResponse, Response},
};
use chrono::Utc;
use ghinvite_core::{
    Account, AccountType, GithubInvitationId, InvitationLinkId, RequestId, SelectedRepos,
    org_delivery::*,
    storage::{AppInstallationStorage, RecordStorage, org_delivery::OrgDeliveryStorage},
};
use ghinvite_e2e::{Call, Harness, journal::JournalAudit};
use restate_sdk::{
    context::{
        ContextClient, ContextReadState, ContextWriteState, ObjectContext, SharedObjectContext,
    },
    errors::TerminalError,
    serde::Json,
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, VecDeque},
    sync::{Arc, Mutex, atomic::Ordering},
};
#[derive(Clone, Copy)]
enum PolicyAfterRejection {
    NoSeat,
    ManagedTeam(u64),
    WrongOrganization,
    Unavailable,
}
#[derive(Default)]
struct Scenario {
    sequence_property: bool,
    sequence_replies: VecDeque<sequence_property::WireReply>,
    sequence_consumed: Vec<sequence_property::WireReply>,
    policy_after_rejection: Option<PolicyAfterRejection>,
    policy_read_unavailable: bool,
    org: Option<(&'static str, &'static str)>,
    teams: BTreeMap<u64, (&'static str, &'static str)>,
    pending: bool,
    hide_pending: bool,
    delay_post: bool,
    deny_team_reads: bool,
    managed_teams: Vec<u64>,
    seats_full: bool,
    spammed_writes: bool,
    two_factor_required: Option<bool>,
    suspended: bool,
    seats_fill_after_first_read: bool,
    org_reads: usize,
    org_id_override: Option<u64>,
    attached: Vec<u64>,
    created_at: Option<chrono::DateTime<Utc>>,
    writes: VecDeque<u16>,
    deletes: VecDeque<u16>,
    trace: Vec<(String, String, Value)>,
}
#[derive(Default)]
struct Github(Mutex<Scenario>);
fn invitation(s: &Scenario) -> Value {
    json!({"id":99,"login":"alice","email":null,"role":if s.org==Some(("admin","pending")){"admin"}else{"direct_member"},"created_at":s.created_at.unwrap_or_else(||Utc::now()-chrono::Duration::minutes(5)),"inviter":{"id":7,"login":"owner"},"team_count":s.attached.len()})
}
async fn github(State(state): State<Arc<Github>>, req: Request) -> Response {
    let method = req.method().to_string();
    let path = req.uri().path().to_owned();
    // `p1/org`: the org app through `GithubApps` (its REST calls and token
    // creation), access grants' org parts included.
    let user_agent = req.headers().get("user-agent").unwrap();
    assert_eq!(user_agent, "p1/org");
    let bytes = to_bytes(req.into_body(), 65536).await.unwrap();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    if let Some((reply, delay)) = sequence_property::scripted_write(&state, &method, &path, &body) {
        if delay {
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        }
        return reply;
    }
    if method == "POST" && path == "/orgs/acme/invitations" && state.0.lock().unwrap().delay_post {
        tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    }
    let mut s = state.0.lock().unwrap();
    s.trace.push((method.clone(), path.clone(), body.clone()));
    if path == "/app/installations/2/access_tokens" {
        assert!(
            body["permissions"]["members"] == "read" || body["permissions"]["members"] == "write"
        );
        return (axum::http::StatusCode::CREATED,axum::Json(json!({"token":"ghs_ORG_RUNTIME_MARKER","expires_at":Utc::now()+chrono::Duration::hours(1)}))).into_response();
    }
    if path == "/app/installations/2" {
        return axum::Json(json!({"id":2,"app_id":456,"account":{"id":100,"login":"acme","type":"Organization"},"permissions":{"members":"write","metadata":"read"},"repository_selection":"all","suspended_at":s.suspended.then(Utc::now),"created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T00:00:00Z"})).into_response();
    }
    if path == "/orgs/acme" {
        if s.policy_read_unavailable {
            return axum::http::StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
        s.org_reads += 1;
        if s.seats_fill_after_first_read && s.org_reads > 1 {
            s.seats_full = true;
        }
        return axum::Json(json!({"id":s.org_id_override.unwrap_or(100),"login":"acme","created_at":"2020-01-01T00:00:00Z","two_factor_requirement_enabled":s.two_factor_required,"plan":if s.seats_full{json!({"name":"enterprise","seats":10,"filled_seats":10})}else{Value::Null}}))
            .into_response();
    }
    if method == "GET" && (path == "/user/7" || path == "/users/owner") {
        return axum::Json(json!({"id":7,"login":"owner"})).into_response();
    }
    if method == "GET" && path == "/orgs/acme/memberships/owner" {
        return axum::Json(json!({"role":"admin","state":"active","user":{"id":7,"login":"owner"},"organization":{"id":100,"login":"acme"}})).into_response();
    }
    if path == "/user/8" || path == "/users/alice" {
        return axum::Json(json!({"id":8,"login":"alice"})).into_response();
    }
    if path == "/orgs/acme/memberships/alice" && method == "DELETE" {
        let status = s.deletes.pop_front().unwrap_or(204);
        if status == 204 || status == 502 {
            s.org = None;
            s.teams.clear();
        }
        return axum::http::StatusCode::from_u16(status)
            .unwrap()
            .into_response();
    }
    if path == "/orgs/acme/memberships/alice" {
        if s.hide_pending {
            return axum::http::StatusCode::NOT_FOUND.into_response();
        }
        return match s.org{Some((role,state))=>axum::Json(json!({"role":role,"state":state,"user":{"id":8,"login":"alice"},"organization":{"id":100,"login":"acme"}})).into_response(),None=>axum::http::StatusCode::NOT_FOUND.into_response()};
    }
    if method == "GET"
        && s.deny_team_reads
        && (path.ends_with("/teams") || path.contains("/teams/"))
    {
        return axum::http::StatusCode::FORBIDDEN.into_response();
    }
    if path == "/orgs/acme/invitations/99" && method == "DELETE" {
        let status = s.deletes.pop_front().unwrap_or(204);
        if status == 204 || status == 502 {
            s.pending = false;
            s.org = None;
        }
        if status == 422 {
            return (axum::http::StatusCode::UNPROCESSABLE_ENTITY, axum::Json(json!({"message":"Validation Failed","errors":[{"resource":"OrganizationInvitation","code":"spammed"}]}))).into_response();
        }
        return axum::http::StatusCode::from_u16(status)
            .unwrap()
            .into_response();
    }
    if path == "/orgs/acme/invitations/99/teams" {
        return axum::Json(json!(
            s.attached
                .iter()
                .map(|id| json!({"id":id,"slug":format!("team-{id}"),"name":format!("Team {id}")}))
                .collect::<Vec<_>>()
        ))
        .into_response();
    }
    if path == "/orgs/acme/failed_invitations" {
        return axum::Json(json!([])).into_response();
    }
    if path == "/orgs/acme/invitations" {
        if method == "GET" {
            return axum::Json(if s.pending {
                json!([invitation(&s)])
            } else {
                json!([])
            })
            .into_response();
        }
        assert_eq!(method, "POST");
        assert_eq!(body["invitee_id"], 8);
        assert_eq!(body["role"], "direct_member");
        assert!(body.get("email").is_none());
        let status = s.writes.pop_front().unwrap_or(201);
        if status == 422 && s.spammed_writes {
            return (
                axum::http::StatusCode::UNPROCESSABLE_ENTITY,
                axum::Json(json!({"errors":[{"code":"spammed"}]})),
            )
                .into_response();
        }
        if status != 201 {
            apply_rejection_policy(&mut s);
            return (
                axum::http::StatusCode::from_u16(status).unwrap(),
                axum::Json(json!({"message":"fixture rejected"})),
            )
                .into_response();
        }
        s.pending = true;
        s.org = Some(("member", "pending"));
        s.created_at.get_or_insert_with(Utc::now);
        s.attached = serde_json::from_value(body["team_ids"].clone()).unwrap();
        return (axum::http::StatusCode::CREATED, axum::Json(invitation(&s))).into_response();
    }
    if let Some(tail) = path.strip_prefix("/orgs/acme/teams/team-") {
        let id = tail.split('/').next().unwrap().parse::<u64>().unwrap();
        if !tail.contains('/') {
            return axum::Json(
                json!({"id":id,"slug":format!("team-{id}"),"name":format!("Team {id}"),"ldap_dn":if s.managed_teams.contains(&id){Some("LDAP_PRIVATE_MARKER")}else{None}}),
            )
            .into_response();
        }
        assert!(tail.ends_with("/memberships/alice"));
        if method == "GET" {
            return match s.teams.get(&id) {
                Some((role, state)) => {
                    axum::Json(json!({"role":role,"state":state})).into_response()
                }
                None => axum::http::StatusCode::NOT_FOUND.into_response(),
            };
        }
        if method == "DELETE" {
            let status = s.deletes.pop_front().unwrap_or(204);
            if status == 204 || status == 502 {
                s.teams.remove(&id);
            }
            return axum::http::StatusCode::from_u16(status)
                .unwrap()
                .into_response();
        }
        assert_eq!(method, "PUT");
        assert_eq!(body["role"], "member");
        let status = s.writes.pop_front().unwrap_or(200);
        if status != 200 {
            apply_rejection_policy(&mut s);
            return (
                axum::http::StatusCode::from_u16(status).unwrap(),
                axum::Json(json!({"message":"fixture rejected"})),
            )
                .into_response();
        }
        let membership = if s.org.is_some_and(|(_, state)| state == "active") {
            "active"
        } else {
            "pending"
        };
        s.teams.insert(id, ("member", membership));
        return axum::Json(json!({"role":"member","state":membership})).into_response();
    }
    panic!("unexpected {method} {path}")
}
fn apply_rejection_policy(s: &mut Scenario) {
    match s.policy_after_rejection.take() {
        Some(PolicyAfterRejection::NoSeat) => s.seats_full = true,
        Some(PolicyAfterRejection::ManagedTeam(id)) => s.managed_teams.push(id),
        Some(PolicyAfterRejection::WrongOrganization) => {
            s.seats_full = true;
            s.org_id_override = Some(999);
        }
        Some(PolicyAfterRejection::Unavailable) => s.policy_read_unavailable = true,
        None => {}
    }
}

fn account() -> Account {
    Account {
        installation_id: 2,
        account_id: 100,
        account_login: "acme".into(),
        account_type: AccountType::Organization,
        installed_at: Utc::now() - chrono::Duration::days(1),
        uninstalled_at: None,
        selected_repos: SelectedRepos::All,
    }
}
// Isolated availability source; S1/S4 still verify live numeric app/account/user
// identity with real broker HTTP. Installation lifecycle has its own real suite.
// The isolated owner fixture records identity-only wakes and reads the shared
// status immediately. A notice cannot serve as a replacement for durable facts.
struct InvitationRequest;
#[restate_sdk::object]
impl InvitationRequest {
    #[handler]
    async fn on_delivery_settled(
        &self,
        ctx: ObjectContext<'_>,
        Json(notice): Json<ghinvite_core::request_completion::DeliveryNotice>,
    ) -> Result<(), TerminalError> {
        assert_eq!(
            notice.kind,
            ghinvite_core::delivery_control::DeliveryKind::Organization
        );
        let snapshot = ctx
            .object_client::<ghinvite_workflows::org_delivery::OrganizationDeliveryClient>(format!(
                "{}:{}:{}",
                ctx.key(),
                notice.target_id,
                notice.delivery_generation
            ))
            .status()
            .call()
            .await?
            .0
            .expect("notice follows durable snapshot");
        snapshot.validate().unwrap();
        assert_eq!(snapshot.create.command.request_id.to_string(), ctx.key());
        let results = ghinvite_core::request_completion::organization(&snapshot);
        assert!(!results.is_empty(), "pending-only snapshot must not notify");
        ctx.set(
            "observed",
            Json(json!({"notice":notice,"snapshot":snapshot,"results":results})),
        );
        Ok(())
    }
    #[handler]
    async fn observed(
        &self,
        ctx: SharedObjectContext<'_>,
    ) -> Result<Json<Option<Value>>, TerminalError> {
        Ok(Json(ctx.get::<Json<Value>>("observed").await?.map(|j| j.0)))
    }
}
struct AccountInstallation;
struct Installation {
    rechecks: Arc<std::sync::atomic::AtomicUsize>,
}
#[restate_sdk::object]
impl Installation {
    #[handler]
    async fn recheck_permissions(
        &self,
        ctx: restate_sdk::context::ObjectContext<'_>,
    ) -> Result<(), TerminalError> {
        assert_eq!(ctx.key(), "org:2");
        self.rechecks.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

#[restate_sdk::object]
impl AccountInstallation {
    #[handler]
    async fn eligibility_installation(
        &self,
        ctx: SharedObjectContext<'_>,
    ) -> Result<Json<Option<ghinvite_core::account::Installation>>, TerminalError> {
        assert_eq!(ctx.key(), "org:100");
        let mut value = ghinvite_core::account::Installation::repo(account());
        value.app = ghinvite_core::account::App::Org;
        Ok(Json(Some(value)))
    }
    #[handler]
    async fn current_installation(
        &self,
        ctx: SharedObjectContext<'_>,
    ) -> Result<Json<Option<Account>>, TerminalError> {
        assert_eq!(ctx.key(), "org:100");
        Ok(Json(Some(account())))
    }
}
struct Fixture {
    h: Harness,
    github: Arc<Github>,
    storage: Arc<ghinvite_storage_sqlx::SqlxStorage>,
    faults: Arc<ghinvite_workflows::org_delivery::OrganizationFaults>,
    rechecks: Arc<std::sync::atomic::AtomicUsize>,
    lifetime: u32,
}
impl Fixture {
    async fn new(scenario: Scenario) -> Self {
        Self::start(scenario, false).await
    }
    async fn start(scenario: Scenario, standard: bool) -> Self {
        Self::start_options(scenario, standard, false, 604800).await
    }
    async fn cancellation(scenario: Scenario, lifetime: u32) -> Self {
        Self::start_options(scenario, false, true, lifetime).await
    }
    async fn start_options(
        scenario: Scenario,
        standard: bool,
        cancellation: bool,
        lifetime: u32,
    ) -> Self {
        Self::start_all(scenario, standard, cancellation, lifetime, false).await
    }
    async fn access(scenario: Scenario) -> Self {
        Self::start_all(scenario, false, false, 604800, true).await
    }
    async fn start_all(
        scenario: Scenario,
        standard: bool,
        cancellation: bool,
        lifetime: u32,
        access: bool,
    ) -> Self {
        Self::start_features(scenario, standard, cancellation, lifetime, access, false).await
    }
    async fn start_reissue() -> Self {
        Self::start_features(Default::default(), true, true, 604800, false, true).await
    }
    async fn start_features(
        scenario: Scenario,
        standard: bool,
        cancellation: bool,
        lifetime: u32,
        access: bool,
        reissue: bool,
    ) -> Self {
        let sequence_property = scenario.sequence_property;
        let github = Arc::new(Github(Mutex::new(scenario)));
        let h = Harness::start_with_github_router(
            axum::Router::new()
                .fallback(github_handler())
                .with_state(github.clone()),
        )
        .await;
        let storage = Arc::new(
            ghinvite_storage_sqlx::SqlxStorage::in_memory()
                .await
                .unwrap(),
        );
        if !standard {
            storage
                .put_app_installation(&ghinvite_core::account::Installation::repo(
                    (account()).clone(),
                ))
                .await
                .unwrap();
            let mut install = ghinvite_core::account::Installation::repo(account());
            install.app = ghinvite_core::account::App::Org;
            install.permissions.insert(
                "members".into(),
                ghinvite_core::account::InstallationPermission::Write,
            );
            storage.put_app_installation(&install).await.unwrap();
        }
        for (id, login) in [(7, "owner"), (8, "alice")] {
            storage
                .upsert_user(&ghinvite_core::User {
                    user_id: id,
                    login: login.into(),
                    avatar_url: None,
                    github_created_at: None,
                    last_seen_at: Utc::now(),
                })
                .await
                .unwrap();
        }
        let github_client = if sequence_property {
            reqwest::Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .read_timeout(std::time::Duration::from_millis(500))
                .build()
                .unwrap()
        } else {
            h.client.clone()
        };
        let github_transport = ghinvite_e2e::github_transport(github_client);
        let state = ghinvite_workflows::AppState::new(storage.clone())
            .with_invite_budget(storage.clone())
            .with_github_apps(Arc::new(
                ghinvite_workflows::RepositoryGithub::new(
                    github_transport,
                    &h.github_url,
                    ghinvite_e2e::installed_apps(&h.github_url, true),
                )
                .unwrap(),
            ));
        let state = if cancellation {
            state.with_pre_activation_cancellation()
        } else {
            state
        };
        let state = if access {
            state.with_access_grants(storage.clone())
        } else {
            state
        };
        let state = if reissue {
            state.with_reissues(storage.clone())
        } else {
            state
        };
        let faults = Arc::new(ghinvite_workflows::org_delivery::OrganizationFaults::default());
        let rechecks = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        if standard {
            h.restate
                .deploy(
                    ghinvite_workflows::build_endpoint(
                        state
                            .with_organization_delivery(storage.clone())
                            .with_bulk_invites(storage.clone()),
                        storage.clone(),
                        None,
                    )
                    .unwrap(),
                )
                .await;
            let mut setup = ghinvite_e2e::setup_command(
                json!({"installation_id":2,"actor_user_id":7,"account_id":100,"account_login":"acme","account_type":"Organization","selected_repos":[],"installed_at":"2026-01-01T00:00:00Z"}),
            );
            setup["verified"]["app"] = json!("org");
            setup["verified"]["permissions"] = json!({"members":"write","metadata":"read"});
            let response = h
                .restate
                .invoke(
                    &Call::object("Installation", "org:2", "onboard"),
                    Some(&setup),
                    None,
                )
                .await;
            assert_eq!(response.status, 200, "{response:?}");
            use ghinvite_core::storage::AppInstallationReadStorage;
            tokio::time::timeout(std::time::Duration::from_secs(10), async {
                loop {
                    if storage
                        .active_app_installation(ghinvite_core::account::App::Org, 100)
                        .await
                        .unwrap()
                        .is_some()
                    {
                        break;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                }
            })
            .await
            .unwrap();
            tokio::time::timeout(std::time::Duration::from_secs(10), async {
                loop {
                    let schedule = h
                        .restate
                        .invoke(
                            &Call::object("SweepSchedule", "org:100", "status"),
                            None::<&Value>,
                            None,
                        )
                        .await;
                    let value = schedule.body;
                    if value["cadence_seconds"] == 86400 && value["next_run"].is_string() {
                        assert_eq!(value["tracked"], json!({}));
                        break;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                }
            })
            .await
            .unwrap();
        } else {
            let builder = ghinvite_workflows::invite_budget::bind_pre_activation(
                restate_sdk::endpoint::Endpoint::builder()
                    .bind(AccountInstallation)
                    .bind(InvitationRequest)
                    .bind(ghinvite_workflows::sweep_schedule::SweepSchedule {
                        organization_enabled: true,
                        ..Default::default()
                    })
                    .bind(
                        ghinvite_workflows::org_eligibility::OrganizationEligibility {
                            state: state.clone(),
                        },
                    )
                    .bind(ghinvite_workflows::org_reconcile::OrganizationReconcile {
                        state: state.clone(),
                    })
                    .bind(Installation {
                        rechecks: rechecks.clone(),
                    }),
                storage.clone(),
                Default::default(),
            );
            let builder = if access {
                ghinvite_workflows::access_grant::bind(builder, state.clone(), storage.clone())
            } else {
                builder
            };
            h.restate
                .deploy(
                    ghinvite_workflows::org_delivery::bind_with_faults(
                        builder,
                        state,
                        storage.clone(),
                        faults.clone(),
                    )
                    .build(),
                )
                .await;
        }
        JournalAudit::retain(&h, &["OrganizationDelivery", "InviteBudget"]).await;
        Self {
            h,
            github,
            storage,
            faults,
            rechecks,
            lifetime,
        }
    }
    async fn command(&self, teams: &[u64]) -> OrganizationCreateCommand {
        self.command_at(teams, Utc::now()).await
    }
    async fn command_at(
        &self,
        teams: &[u64],
        now: chrono::DateTime<Utc>,
    ) -> OrganizationCreateCommand {
        let organization = json!({"org_id":100,"org_login":"acme","grant":"member"});
        let teams = teams
            .iter()
            .map(|id| json!({"team_id":id,"team_slug":format!("team-{id}"),"grant":"member"}))
            .collect::<Vec<_>>();
        let link:ghinvite_core::InvitationLink=serde_json::from_value(json!({"secret":{"secret_generation":1,"sha256":"2a".repeat(32)},"id":InvitationLinkId::new(),"installation_id":2,"account_id":100,"created_by":7,"created_at":now,"expires_at":null,"max_uses":null,"uses_count":1,"approval_required":true,"description":"Organization protocol","internal_note":null,"revoked_at":null,"revoked_by":null,"targets":{"organization":organization,"teams":teams,"repos":[]}})).unwrap();
        let request:ghinvite_core::InvitationRequest=serde_json::from_value(json!({"id":RequestId::new(),"invitation_link_id":link.id,"requester_id":8,"justification":null,"state":"approved","decided_by":7,"decided_at":now,"decline_reason":null,"created_at":now,"decision_deadline":null})).unwrap();
        ghinvite_core::storage::projection::fixture::seed(
            self.storage.as_ref(),
            &link,
            std::slice::from_ref(&request),
            1,
        )
        .await
        .unwrap();
        let command = OrganizationCreateCommand {
            batch: None,
            budget_attribution: Default::default(),
            access_lifetime_seconds: None,
            invitation_id: Some(GithubInvitationId::new()),
            link_id: Some(link.id),
            request_id: request.id,
            approval_id: format!("fixture/{}", request.id),
            account_id: 100,
            installation_id: 2,
            requester_id: 8,
            organization: link.targets.organization.unwrap(),
            teams: link.targets.teams,
            delivery_generation: 1,
            approved_at: now,
            budget_share_pct: 50,
        };
        self.control(
            &command,
            1,
            ghinvite_core::delivery_control::LinkControl::Active,
        )
        .await;
        command
    }
    async fn call(&self, c: &OrganizationCreateCommand, method: &str, body: Value) -> Value {
        let response = self
            .h
            .restate
            .invoke(
                &Call::object("OrganizationDelivery", &c.delivery_key(), method),
                Some(&body),
                None,
            )
            .await;
        assert_eq!(response.status, 200, "{response:?}");
        let journal = self
            .h
            .restate
            .admin()
            .journal(response.invocation_id())
            .await;
        assert!(!journal.is_empty());
        assert!(
            journal
                .iter()
                .all(|e| !e.raw_contains("ghs_ORG_RUNTIME_MARKER")
                    && !e.raw_contains("LDAP_PRIVATE_MARKER"))
        );
        response.body
    }
    async fn control(
        &self,
        c: &OrganizationCreateCommand,
        revision: u64,
        control: ghinvite_core::delivery_control::LinkControl,
    ) {
        self.call(c,"control",json!({"envelope":{"identity":{"link_id":c.link_id,"request_id":c.request_id,"kind":"organization","target_id":100,"delivery_generation":1},"operation_id":ghinvite_core::link_lifecycle::control_operation_id(c.link_id.expect("linked fixture"),revision),"revision":revision,"hold_epoch":if revision>1{2}else{0},"at":Utc::now(),"control":control},"admitted_at":c.approved_at,"invitation_lifetime_seconds":self.lifetime})).await;
    }
    async fn create(&self, c: &OrganizationCreateCommand) -> OrganizationDeliverySnapshot {
        serde_json::from_value(self.call(c, "create", json!(c)).await).unwrap()
    }
    fn writes(&self, method: &str) -> Vec<(String, String, Value)> {
        self.github
            .0
            .lock()
            .unwrap()
            .trace
            .iter()
            .filter(|t| t.0 == method && !t.1.ends_with("access_tokens"))
            .cloned()
            .collect()
    }
}
fn github_handler() -> axum::routing::MethodRouter<Arc<Github>> {
    axum::routing::any(github)
}

#[tokio::test]
async fn active_admin_and_team_maintainer_are_not_downgraded() {
    let f = Fixture::new(Scenario {
        org: Some(("admin", "active")),
        teams: BTreeMap::from([(5, ("maintainer", "active"))]),
        ..Default::default()
    })
    .await;
    let c = f.command(&[5]).await;
    let result = f.create(&c).await;
    assert!(matches!(
        result.create.outcome,
        OrganizationCreateOutcome::AlreadySatisfied {
            role: OrganizationRole::Admin
        }
    ));
    assert!(matches!(
        result.teams[&5].create.outcome,
        TeamCreateOutcome::AlreadySatisfied {
            role: TeamRole::Maintainer,
            ..
        }
    ));
    assert!(f.writes("POST").is_empty());
    assert!(f.writes("PUT").is_empty());
    assert_eq!(f.create(&c).await, result);
}
#[tokio::test]
async fn newcomer_posts_all_team_ids_once_and_replay_preserves_receipts() {
    let f = Fixture::new(Default::default()).await;
    let c = f.command(&[5, 6]).await;
    let result = f.create(&c).await;
    result.validate().unwrap();
    assert!(matches!(
        result.create.outcome,
        OrganizationCreateOutcome::Created {
            upstream_id: 99,
            ..
        }
    ));
    for t in result.teams.values() {
        assert!(matches!(
            t.create.outcome,
            TeamCreateOutcome::Created {
                via: TeamCreation::OrganizationInvitation { upstream_id: 99 }
            }
        ));
    }
    assert_eq!(f.create(&c).await, result);
    assert_eq!(f.writes("POST").len(), 1);
    assert_eq!(f.writes("POST")[0].2["team_ids"], json!([5, 6]));
    assert!(f.writes("PUT").is_empty());
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            if f.storage
                .get_organization_delivery(&c.storage_id())
                .await
                .unwrap()
                == Some(result.clone())
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
}
#[tokio::test]
async fn pending_admin_has_no_org_post_and_team_pending_has_no_fake_invitation_id() {
    let f = Fixture::new(Scenario {
        org: Some(("admin", "pending")),
        pending: true,
        ..Default::default()
    })
    .await;
    let c = f.command(&[5]).await;
    let result = f.create(&c).await;
    assert!(matches!(
        result.create.outcome,
        OrganizationCreateOutcome::AlreadyPending {
            upstream_id: 99,
            role: OrganizationInvitationRole::Admin
        }
    ));
    assert!(matches!(
        result.teams[&5].create.outcome,
        TeamCreateOutcome::Created {
            via: TeamCreation::PendingMembership
        }
    ));
    assert!(f.writes("POST").is_empty());
    assert_eq!(f.writes("PUT").len(), 1);
    assert_eq!(f.create(&c).await, result);
}
#[tokio::test]
async fn three_actual_run_ack_losses_recover_without_a_second_post() {
    let f = Fixture::new(Default::default()).await;
    let c = f.command(&[5]).await;
    f.faults.lose_results.store(3, Ordering::SeqCst);
    let first = f.create(&c).await;
    assert!(matches!(
        first.create.outcome,
        OrganizationCreateOutcome::OutcomeUnknown
    ));
    assert_eq!(f.faults.lost_results.load(Ordering::SeqCst), 3);
    assert_eq!(f.writes("POST").len(), 1);
    let recovered = f.create(&c).await;
    assert!(matches!(
        recovered.create.outcome,
        OrganizationCreateOutcome::Created {
            upstream_id: 99,
            ..
        }
    ));
    assert_eq!(f.writes("POST").len(), 1);
    assert_eq!(f.create(&c).await, recovered);
}

#[tokio::test]
async fn submitted_org_post_keeps_all_attached_teams_uncertain_through_revoke() {
    use ghinvite_core::delivery_control::{HoldWindow, LinkControl};
    let f = Fixture::new(Scenario {
        hide_pending: true,
        ..Default::default()
    })
    .await;
    let c = f.command(&[5, 6]).await;
    f.faults.lose_results.store(3, Ordering::SeqCst);
    let uncertain = f.create(&c).await;
    assert!(matches!(
        uncertain.create.outcome,
        OrganizationCreateOutcome::OutcomeUnknown
    ));
    assert!(
        uncertain
            .teams
            .values()
            .all(|t| matches!(t.create.outcome, TeamCreateOutcome::OutcomeUnknown))
    );
    let window = HoldWindow {
        epoch: 2,
        window_start: c.approved_at - chrono::Duration::seconds(1),
        paused_at: Utc::now(),
    };
    f.control(
        &c,
        2,
        LinkControl::AutoPause {
            window: window.clone(),
        },
    )
    .await;
    f.control(
        &c,
        3,
        LinkControl::Revoked {
            cancel_pending_invitations: true,
            decline_pending_requests: false,
            abort_hold: Some(window),
        },
    )
    .await;
    let still_unknown = f.create(&c).await;
    assert!(
        still_unknown
            .teams
            .values()
            .all(|t| !t.create.outcome.confirmed()),
        "a shared POST may have attached every team"
    );
    f.github.0.lock().unwrap().hide_pending = false;
    let recovered = f.create(&c).await;
    assert!(matches!(
        recovered.create.outcome,
        OrganizationCreateOutcome::Created {
            upstream_id: 99,
            ..
        }
    ));
    assert!(recovered.teams.values().all(|t| matches!(
        t.create.outcome,
        TeamCreateOutcome::Created {
            via: TeamCreation::OrganizationInvitation { upstream_id: 99 }
        }
    )));
    assert_eq!(f.writes("POST").len(), 1);
    assert!(
        f.writes("DELETE").is_empty(),
        "cancellation remains a separate fenced executor"
    );
}
#[tokio::test]
async fn delayed_success_uses_post_io_control_time() {
    let f = Fixture::new(Scenario {
        delay_post: true,
        ..Default::default()
    })
    .await;
    let c = f.command(&[5]).await;
    let result = f.create(&c).await;
    result.validate().unwrap();
    let OrganizationCreateOutcome::Created { created_at, .. } = result.create.outcome else {
        panic!("{result:?}")
    };
    assert!(created_at > c.approved_at + chrono::Duration::seconds(1));
    assert!(result.create.confirmed_at.unwrap() >= created_at);
    assert_eq!(f.create(&c).await, result);
    assert_eq!(f.writes("POST").len(), 1);
}

async fn standard_call(
    f: &Fixture,
    service: &str,
    key: &str,
    method: &str,
    body: Option<Value>,
) -> Value {
    let response =
        f.h.restate
            .invoke(&Call::object(service, key, method), body.as_ref(), None)
            .await;
    assert_eq!(
        response.status, 200,
        "{service}/{key}/{method}: {response:?}"
    );
    response.body
}
#[tokio::test]
async fn standard_endpoint_admission_dispatches_real_organization_owner_and_projectors() {
    let f = Fixture::start(Default::default(), true).await;
    let link = InvitationLinkId::new();
    let create = json!({"secret_sha256":"2a".repeat(32),"link_id":link,"account_id":100,"installation_id":2,"budget_share_pct":17,"admin":{"account_id":100,"user_id":7},"description":"Standard org delivery","max_uses":10,"approval_required":false,"targets":{"organization":{"org_id":100,"org_login":"acme","grant":"member"},"teams":[{"team_id":5,"team_slug":"team-5","grant":"member"}],"repos":[]}});
    assert!(
        standard_call(
            &f,
            "InvitationLink",
            &link.to_string(),
            "create",
            Some(create)
        )
        .await
        .get("Ok")
        .is_some()
    );
    let admission = json!({"scope_evidence":[],"secret_sha256":"2a".repeat(32),"secret_generation":1,"link_id":link,"operation_id":RequestId::new(),"requester_id":8,"justification":null});
    let admitted = standard_call(
        &f,
        "InvitationLink",
        &link.to_string(),
        "admit",
        Some(admission.clone()),
    )
    .await;
    assert_eq!(admitted["result"]["kind"], "accepted");
    let request = admitted["result"]["request_id"].as_str().unwrap();
    let key = format!("{request}:100:1");
    let snapshot: OrganizationDeliverySnapshot =
        tokio::time::timeout(std::time::Duration::from_secs(15), async {
            loop {
                let result = standard_call(&f, "OrganizationDelivery", &key, "status", None).await;
                if result["create"]["outcome"]["kind"] == "created" {
                    break serde_json::from_value(result).unwrap();
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
    assert_eq!(snapshot.create.command.budget_share_pct, 17);
    assert_eq!(snapshot.create.command.installation_id, 2);
    assert!(matches!(
        snapshot.teams[&5].create.outcome,
        TeamCreateOutcome::Created {
            via: TeamCreation::OrganizationInvitation { upstream_id: 99 }
        }
    ));
    assert_eq!(
        standard_call(
            &f,
            "InvitationLink",
            &link.to_string(),
            "admit",
            Some(admission)
        )
        .await,
        admitted
    );
    let query = json!({"link_id":link,"request_id":request,"requester_id":8});
    let recovered = standard_call(&f, "InvitationRequest", request, "recover", Some(query)).await;
    assert_eq!(
        recovered["submitted_organization"]["command"],
        json!(snapshot.create.command)
    );
    assert!(recovered["plan"]["commands"].as_array().unwrap().is_empty());
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            if f.storage
                .get_organization_delivery(&snapshot.create.command.storage_id())
                .await
                .unwrap()
                == Some(snapshot.clone())
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(f.writes("POST").len(), 1);
    assert!(f.writes("PUT").is_empty());
}

#[tokio::test]
async fn bounded_401_retry_releases_only_rejected_generations_and_refunds_double_rejection() {
    let f = Fixture::new(Scenario {
        writes: VecDeque::from([401, 401]),
        ..Default::default()
    })
    .await;
    let c = f.command(&[]).await;
    let result = f.create(&c).await;
    assert!(matches!(
        result.create.outcome,
        OrganizationCreateOutcome::Blocked {
            reason: OrganizationBlockedReason::InstallationUnavailable
        }
    ));
    assert_eq!(f.writes("POST").len(), 2);
    let input = OrganizationAttempt {
        create: c.clone(),
        action: OrganizationWrite::Invite,
    };
    assert!(!f.storage.organization_attempt_exists(&input).await.unwrap());
    assert_eq!(
        f.storage.claim_organization_attempt(&input).await.unwrap(),
        Some(3),
        "both positive401 generations were released"
    );
    let budget = standard_call(&f, "InviteBudget", "org:100", "snapshot", None).await;
    assert_eq!(budget["content_used_hour"], 0);
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while f.rechecks.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(f.rechecks.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn uncertain_team_step_does_not_replay_or_prevent_another_team_from_completing() {
    let f = Fixture::new(Scenario {
        org: Some(("member", "active")),
        writes: VecDeque::from([503, 200]),
        ..Default::default()
    })
    .await;
    let c = f.command(&[5, 6]).await;
    let first = f.create(&c).await;
    assert!(matches!(
        first.create.outcome,
        OrganizationCreateOutcome::AlreadySatisfied { .. }
    ));
    assert!(matches!(
        first.teams[&5].create.outcome,
        TeamCreateOutcome::OutcomeUnknown
    ));
    tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
    let second = f.create(&c).await;
    assert_eq!(first.create, second.create);
    assert!(matches!(
        second.teams[&5].create.outcome,
        TeamCreateOutcome::OutcomeUnknown
    ));
    assert!(matches!(
        second.teams[&6].create.outcome,
        TeamCreateOutcome::Upgraded
    ));
    let writes = f.writes("PUT");
    assert_eq!(writes.iter().filter(|t| t.1.contains("team-5/")).count(), 1);
    assert_eq!(writes.iter().filter(|t| t.1.contains("team-6/")).count(), 1);
    assert!(f.writes("POST").is_empty());
    assert_eq!(f.create(&c).await, second);
}
#[tokio::test]
async fn hold_and_resume_only_unfinished_team_preserves_org_and_other_team_receipts() {
    use ghinvite_core::delivery_control::{HoldWindow, LinkControl};
    let f = Fixture::new(Scenario {
        org: Some(("member", "active")),
        teams: BTreeMap::from([(5, ("maintainer", "active"))]),
        writes: VecDeque::from([403, 200]),
        ..Default::default()
    })
    .await;
    let c = f.command(&[5, 6]).await;
    let first = f.create(&c).await;
    assert!(matches!(
        first.teams[&6].create.outcome,
        TeamCreateOutcome::Blocked { .. }
    ));
    let window = HoldWindow {
        epoch: 2,
        window_start: c.approved_at - chrono::Duration::seconds(1),
        paused_at: Utc::now(),
    };
    f.control(&c, 2, LinkControl::AutoPause { window }).await;
    let held = f.create(&c).await;
    assert!(matches!(
        held.teams[&6].create.outcome,
        TeamCreateOutcome::Held { .. }
    ));
    assert_eq!(held.create, first.create);
    assert_eq!(held.teams[&5], first.teams[&5]);
    assert_eq!(f.writes("PUT").len(), 1);
    f.control(&c, 3, LinkControl::Active).await;
    let resumed = f.create(&c).await;
    assert!(matches!(
        resumed.teams[&6].create.outcome,
        TeamCreateOutcome::Upgraded
    ));
    assert_eq!(resumed.create, first.create);
    assert_eq!(resumed.teams[&5], first.teams[&5]);
    assert_eq!(f.writes("PUT").len(), 2);
}

async fn terminal(f: &Fixture, c: &OrganizationCreateCommand) -> OrganizationDeliverySnapshot {
    tokio::time::timeout(std::time::Duration::from_secs(12), async {
        loop {
            let snapshot: OrganizationDeliverySnapshot = serde_json::from_value(
                standard_call(f, "OrganizationDelivery", &c.delivery_key(), "status", None).await,
            )
            .unwrap();
            if snapshot.settlement.is_some() {
                break snapshot;
            }
            tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        }
    })
    .await
    .unwrap()
}
fn revoke() -> ghinvite_core::delivery_control::LinkControl {
    ghinvite_core::delivery_control::LinkControl::Revoked {
        cancel_pending_invitations: true,
        decline_pending_requests: false,
        abort_hold: None,
    }
}
#[tokio::test]
async fn owned_cancel_has_independent_fence_budget_and_immutable_team_receipts() {
    let f = Fixture::cancellation(Default::default(), 604800).await;
    let c = f.command(&[5]).await;
    let created = f.create(&c).await;
    f.control(&c, 2, revoke()).await;
    let cancelled = terminal(&f, &c).await;
    assert_eq!(cancelled.create, created.create);
    assert_eq!(cancelled.teams[&5].create, created.teams[&5].create);
    assert_eq!(
        cancelled.settlement.as_ref().unwrap().state,
        ghinvite_core::InvitationState::Cancelled
    );
    assert_eq!(
        cancelled.settlement.as_ref().unwrap().event.metadata["reason"],
        "link_revoked"
    );
    assert_eq!(
        cancelled.teams[&5].settlement.as_ref().unwrap().state,
        ghinvite_core::InvitationState::Cancelled
    );
    let input = OrganizationAttempt {
        create: c.clone(),
        action: OrganizationWrite::Cancel {
            upstream_id: 99,
            reason: ghinvite_core::delivery::SettlementReason::LinkRevoked,
        },
    };
    assert!(f.storage.organization_attempt_exists(&input).await.unwrap());
    assert!(
        f.storage
            .organization_attempt_exists(&OrganizationAttempt {
                create: c.clone(),
                action: OrganizationWrite::Invite
            })
            .await
            .unwrap()
    );
    let budget = standard_call(&f, "InviteBudget", "org:100", "snapshot", None).await;
    assert_eq!(budget["content_used_hour"], 2);
    for _ in 0..3 {
        standard_call(
            &f,
            "OrganizationDelivery",
            &c.delivery_key(),
            "drive_cancellation",
            None,
        )
        .await;
    }
    assert_eq!(f.writes("DELETE").len(), 1);
    assert_eq!(f.create(&c).await, cancelled);
}
#[tokio::test]
async fn foreign_org_and_pending_team_stop_tracking_without_delete_or_charge() {
    let f = Fixture::cancellation(
        Scenario {
            org: Some(("admin", "pending")),
            pending: true,
            ..Default::default()
        },
        604800,
    )
    .await;
    let c = f.command(&[5]).await;
    let created = f.create(&c).await;
    let before = standard_call(&f, "InviteBudget", "org:100", "snapshot", None).await;
    f.control(&c, 2, revoke()).await;
    let stopped = f.create(&c).await;
    assert_eq!(stopped.create, created.create);
    assert_eq!(
        stopped.tracking_stopped,
        Some(ghinvite_core::delivery::SettlementReason::LinkRevoked)
    );
    assert_eq!(
        stopped.teams[&5].tracking_stopped,
        Some(ghinvite_core::delivery::SettlementReason::LinkRevoked)
    );
    assert!(stopped.settlement.is_none());
    assert!(f.writes("DELETE").is_empty());
    let after = standard_call(&f, "InviteBudget", "org:100", "snapshot", None).await;
    assert_eq!(before["content_used_hour"], after["content_used_hour"]);
}
#[tokio::test]
async fn lost_delete_ack_remains_charged_and_never_repeats_even_when_absent() {
    let f = Fixture::cancellation(
        Scenario {
            deletes: VecDeque::from([502]),
            ..Default::default()
        },
        604800,
    )
    .await;
    let c = f.command(&[]).await;
    let created = f.create(&c).await;
    f.control(&c, 2, revoke()).await;
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while f.writes("DELETE").is_empty() {
            tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        }
    })
    .await
    .unwrap();
    for _ in 0..3 {
        standard_call(
            &f,
            "OrganizationDelivery",
            &c.delivery_key(),
            "drive_cancellation",
            None,
        )
        .await;
    }
    let current = f.create(&c).await;
    assert_eq!(current.create, created.create);
    assert!(current.settlement.is_none());
    assert_eq!(f.writes("DELETE").len(), 1);
    assert_eq!(
        standard_call(&f, "InviteBudget", "org:100", "snapshot", None).await["content_used_hour"],
        2
    );
}
#[tokio::test]
async fn actual_delayed_own_expiry_rechecks_and_cancels_once() {
    let f = Fixture::cancellation(Default::default(), 3600).await;
    // Start the short remaining timer after Restate startup, which can exceed
    // the remaining interval when another runtime starts concurrently.
    let upstream_created = Utc::now() - chrono::Duration::seconds(3594);
    f.github.0.lock().unwrap().created_at = Some(upstream_created);
    let c = f
        .command_at(&[5], upstream_created - chrono::Duration::seconds(1))
        .await;
    let created = f.create(&c).await;
    standard_call(
        &f,
        "OrganizationDelivery",
        &c.delivery_key(),
        "tick_own_expiry",
        Some(json!({"generation":2,"upstream_id":99})),
    )
    .await;
    assert!(f.writes("DELETE").is_empty());
    let expired = terminal(&f, &c).await;
    assert!(Utc::now() >= upstream_created + chrono::Duration::seconds(3600));
    assert_eq!(expired.create, created.create);
    assert_eq!(
        expired.settlement.as_ref().unwrap().event.metadata["reason"],
        "own_expiry"
    );
    assert_eq!(
        expired.teams[&5].settlement.as_ref().unwrap().reason,
        Some(ghinvite_core::delivery::SettlementReason::OwnExpiry)
    );
    let cancellation_events = audit::wait_facts(
        &f,
        &c,
        ghinvite_core::audit::EventType::InvitationCancelled,
        2,
    )
    .await;
    assert!(
        cancellation_events
            .iter()
            .all(|event| event.metadata["reason"] == "own_expiry")
    );
    assert_eq!(f.writes("DELETE").len(), 1);
    let notice = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let value = standard_call(
                &f,
                "InvitationRequest",
                &c.request_id.to_string(),
                "observed",
                None,
            )
            .await;
            if !value.is_null() {
                break value;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        notice["notice"],
        json!({"kind":"organization","target_id":100,"delivery_generation":1})
    );
    assert_eq!(notice["snapshot"]["create"], json!(created.create));
    assert_eq!(notice["results"].as_array().unwrap().len(), 2);
    assert!(
        notice["results"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["result"] == "lapsed_invite")
    );
}

#[tokio::test]
async fn owned_cancel_does_not_depend_on_attached_team_visibility() {
    let f = Fixture::cancellation(Default::default(), 604800).await;
    let c = f.command(&[5]).await;
    let created = f.create(&c).await;
    let trace_start = {
        let mut github = f.github.0.lock().unwrap();
        github.deny_team_reads = true;
        github.trace.len()
    };
    f.control(&c, 2, revoke()).await;
    let cancelled = terminal(&f, &c).await;
    assert_eq!(cancelled.create, created.create);
    assert_eq!(
        cancelled.settlement.unwrap().state,
        ghinvite_core::InvitationState::Cancelled
    );
    assert_eq!(f.writes("DELETE").len(), 1);
    assert!(
        f.github.0.lock().unwrap().trace[trace_start..]
            .iter()
            .all(|(method, path, _)| method != "GET"
                || (!path.ends_with("/teams") && !path.contains("/teams/")))
    );
}

#[tokio::test]
async fn spam_rejection_refunds_cancel_but_closes_content_for_other_writes() {
    let f = Fixture::cancellation(
        Scenario {
            deletes: VecDeque::from([422]),
            ..Default::default()
        },
        604800,
    )
    .await;
    let c = f.command(&[]).await;
    let created = f.create(&c).await;
    f.control(&c, 2, revoke()).await;
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while f.writes("DELETE").is_empty() {
            tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        }
    })
    .await
    .unwrap();
    // Queue behind the cancellation owner so its durable completion, closure,
    // exact fence release and unused-slot refund have all been acknowledged.
    let current = f.create(&c).await;
    let counts = standard_call(&f, "InviteBudget", "org:100", "snapshot", None).await;
    assert_eq!(counts["limit_counts"]["spammed"], 1);
    assert_eq!(counts["limit_counts"]["rate_limited"], 0);
    assert_eq!(current.create, created.create);
    assert!(current.settlement.is_none());
    let reserve = standard_call(&f, "InviteBudget", "org:100", "reserve", Some(json!({"request_key":"independent-after-spam","kind":{"kind":"other_write"},"link_id":null}))).await;
    assert_eq!(reserve["kind"], "retry_at");
    assert_eq!(reserve["why"], "closed");
    let until: chrono::DateTime<Utc> = serde_json::from_value(reserve["at"].clone()).unwrap();
    assert!(until > Utc::now() + chrono::Duration::hours(23));
    assert_eq!(
        standard_call(&f, "InviteBudget", "org:100", "snapshot", None).await["content_used_hour"],
        1
    );
    let attempt = OrganizationAttempt {
        create: c.clone(),
        action: OrganizationWrite::Cancel {
            upstream_id: 99,
            reason: ghinvite_core::delivery::SettlementReason::LinkRevoked,
        },
    };
    assert_eq!(
        f.storage
            .claim_organization_attempt(&attempt)
            .await
            .unwrap(),
        Some(2)
    );
    assert_eq!(f.writes("DELETE").len(), 1);
}

#[tokio::test]
async fn three_actual_cancel_run_losses_keep_one_delete_and_the_original_charge() {
    let f = Fixture::cancellation(Default::default(), 604800).await;
    let c = f.command(&[]).await;
    let created = f.create(&c).await;
    f.faults.lose_cancel_results.store(3, Ordering::SeqCst);
    f.control(&c, 2, revoke()).await;
    tokio::time::timeout(std::time::Duration::from_secs(15), async {
        while f.faults.lost_cancel_results.load(Ordering::SeqCst) != 3 {
            tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        }
    })
    .await
    .unwrap();
    for _ in 0..3 {
        standard_call(
            &f,
            "OrganizationDelivery",
            &c.delivery_key(),
            "drive_cancellation",
            None,
        )
        .await;
    }
    let current = f.create(&c).await;
    assert_eq!(current.create, created.create);
    assert!(current.settlement.is_none());
    assert_eq!(f.writes("DELETE").len(), 1);
    assert_eq!(f.faults.lost_cancel_results.load(Ordering::SeqCst), 3);
    assert_eq!(
        standard_call(&f, "InviteBudget", "org:100", "snapshot", None).await["content_used_hour"],
        2
    );
}

fn observed(c: &OrganizationCreateCommand, at: chrono::DateTime<Utc>, pending: bool) -> Value {
    json!({"account_id":100,"installation_id":2,"org_id":100,"org_login":"acme","observation_started_at":at,"observed_at":at,
        "pending":if pending {vec![json!({"upstream_id":99,"login":"alice","role":"direct_member","created_at":c.approved_at,"team_count":c.teams.len()})]}else{vec![]},"failed":[]})
}
#[tokio::test]
async fn listed_presence_fences_stale_absence_and_fresh_absence_settles_immutable_receipts() {
    let f = Fixture::new(Default::default()).await;
    let c = f.command(&[5]).await;
    let created = f.create(&c).await;
    // A complete list still containing the owned invitation takes precedence
    // over a contradictory membership read from a later instant.
    f.github.0.lock().unwrap().org = Some(("member", "active"));
    let first = observed(&c, Utc::now(), true);
    standard_call(
        &f,
        "OrganizationDelivery",
        &c.delivery_key(),
        "observe_listed",
        Some(first.clone()),
    )
    .await;
    let still_listed: OrganizationDeliverySnapshot = serde_json::from_value(
        standard_call(
            &f,
            "OrganizationDelivery",
            &c.delivery_key(),
            "status",
            None,
        )
        .await,
    )
    .unwrap();
    assert!(still_listed.settlement.is_none());
    {
        let mut gh = f.github.0.lock().unwrap();
        gh.org = None;
        gh.pending = false;
    }
    let mut stale = first;
    stale["pending"] = json!([]);
    standard_call(
        &f,
        "OrganizationDelivery",
        &c.delivery_key(),
        "observe_listed",
        Some(stale),
    )
    .await;
    let retained: OrganizationDeliverySnapshot = serde_json::from_value(
        standard_call(
            &f,
            "OrganizationDelivery",
            &c.delivery_key(),
            "status",
            None,
        )
        .await,
    )
    .unwrap();
    assert!(retained.settlement.is_none());
    standard_call(
        &f,
        "OrganizationDelivery",
        &c.delivery_key(),
        "observe_listed",
        Some(observed(&c, Utc::now(), false)),
    )
    .await;
    let settled = terminal(&f, &c).await;
    assert_eq!(settled.create, created.create);
    assert_eq!(
        settled.settlement.unwrap().state,
        ghinvite_core::InvitationState::Declined
    );
    assert_eq!(
        settled.teams[&5].settlement.as_ref().unwrap().state,
        ghinvite_core::InvitationState::Declined
    );
    assert_eq!(f.writes("POST").len(), 1);
}
#[tokio::test]
async fn failed_list_without_verified_expiry_reason_stays_pending_then_positive_acceptance_wins() {
    let f = Fixture::new(Default::default()).await;
    let c = f.command(&[5]).await;
    let created = f.create(&c).await;
    {
        let mut gh = f.github.0.lock().unwrap();
        gh.org = None;
        gh.pending = false;
    }
    let mut evidence = observed(&c, Utc::now(), false);
    evidence["failed"] = json!([{"invitation":{"upstream_id":99,"login":"alice","role":"direct_member","created_at":c.approved_at,"team_count":1},"failed_at":Utc::now()}]);
    standard_call(
        &f,
        "OrganizationDelivery",
        &c.delivery_key(),
        "observe_listed",
        Some(evidence),
    )
    .await;
    let retained: OrganizationDeliverySnapshot = serde_json::from_value(
        standard_call(
            &f,
            "OrganizationDelivery",
            &c.delivery_key(),
            "status",
            None,
        )
        .await,
    )
    .unwrap();
    assert!(retained.settlement.is_none());
    f.github.0.lock().unwrap().org = Some(("member", "active"));
    standard_call(
        &f,
        "OrganizationDelivery",
        &c.delivery_key(),
        "observe_listed",
        Some(observed(&c, Utc::now(), false)),
    )
    .await;
    let settled = terminal(&f, &c).await;
    assert_eq!(settled.create, created.create);
    assert_eq!(
        settled.settlement.unwrap().state,
        ghinvite_core::InvitationState::Accepted
    );
    assert_eq!(
        settled.teams[&5].settlement.as_ref().unwrap().state,
        ghinvite_core::InvitationState::Accepted
    );
}
#[tokio::test]
async fn existing_pending_team_membership_settles_independently_of_org_acceptance() {
    let f = Fixture::new(Scenario {
        org: Some(("member", "pending")),
        pending: true,
        ..Default::default()
    })
    .await;
    let c = f.command(&[5]).await;
    let created = f.create(&c).await;
    {
        let mut gh = f.github.0.lock().unwrap();
        gh.org = Some(("member", "active"));
        gh.pending = false;
    }
    standard_call(
        &f,
        "OrganizationDelivery",
        &c.delivery_key(),
        "observe_listed",
        Some(observed(&c, Utc::now(), false)),
    )
    .await;
    let settled = terminal(&f, &c).await;
    assert!(settled.teams[&5].settlement.is_none());
    assert!(settled.needs_tracking());
    f.github
        .0
        .lock()
        .unwrap()
        .teams
        .insert(5, ("maintainer", "active"));
    standard_call(
        &f,
        "OrganizationDelivery",
        &c.delivery_key(),
        "observe_listed",
        Some(observed(&c, Utc::now(), false)),
    )
    .await;
    let done: OrganizationDeliverySnapshot = serde_json::from_value(
        standard_call(
            &f,
            "OrganizationDelivery",
            &c.delivery_key(),
            "status",
            None,
        )
        .await,
    )
    .unwrap();
    assert_eq!(done.teams[&5].create, created.teams[&5].create);
    assert_eq!(
        done.teams[&5].settlement.as_ref().unwrap().state,
        ghinvite_core::InvitationState::Accepted
    );
    assert!(!done.needs_tracking());
}
#[tokio::test]
async fn webhook_wake_checks_generation_and_only_fresh_reads_establish_acceptance() {
    let f = Fixture::new(Default::default()).await;
    let c = f.command(&[]).await;
    let created = f.create(&c).await;
    let mut wake = json!({"identity":{"kind":"organization","link_id":c.link_id,"request_id":c.request_id,"target_id":100,"delivery_generation":2},"account_id":100,"installation_id":2,"requester_id":8,"team_id":null,"source":"organization_member_added","payload_sha256":vec![7u8;32],"received_at":Utc::now()});
    let refused =
        f.h.restate
            .invoke(
                &Call::object("OrganizationDelivery", &c.delivery_key(), "on_webhook"),
                Some(&wake),
                None,
            )
            .await;
    assert_eq!(refused.status, 409);
    wake["identity"]["delivery_generation"] = json!(1);
    standard_call(
        &f,
        "OrganizationDelivery",
        &c.delivery_key(),
        "on_webhook",
        Some(wake.clone()),
    )
    .await;
    // The authenticated event is only a hint: a still-pending live invitation
    // remains pending despite the event's member_added source.
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    let pending: OrganizationDeliverySnapshot = serde_json::from_value(
        standard_call(
            &f,
            "OrganizationDelivery",
            &c.delivery_key(),
            "status",
            None,
        )
        .await,
    )
    .unwrap();
    assert!(pending.settlement.is_none());
    {
        let mut gh = f.github.0.lock().unwrap();
        gh.org = Some(("admin", "active"));
        gh.pending = false;
    }
    standard_call(
        &f,
        "OrganizationDelivery",
        &c.delivery_key(),
        "on_webhook",
        Some(wake),
    )
    .await;
    let settled = terminal(&f, &c).await;
    assert_eq!(settled.create, created.create);
    assert_eq!(
        settled.settlement.unwrap().state,
        ghinvite_core::InvitationState::Accepted
    );
}

#[tokio::test]
async fn unknown_org_requires_positive_invitation_presence_not_active_membership_or_absence() {
    let f = Fixture::new(Default::default()).await;
    let c = f.command(&[]).await;
    f.faults.lose_results.store(3, Ordering::SeqCst);
    let unknown = f.create(&c).await;
    assert!(matches!(
        unknown.create.outcome,
        OrganizationCreateOutcome::OutcomeUnknown
    ));
    {
        let mut gh = f.github.0.lock().unwrap();
        gh.org = Some(("member", "active"));
        gh.pending = false;
    }
    standard_call(
        &f,
        "OrganizationDelivery",
        &c.delivery_key(),
        "observe_listed",
        Some(observed(&c, Utc::now(), false)),
    )
    .await;
    assert_eq!(f.create(&c).await.create, unknown.create);
    {
        let mut gh = f.github.0.lock().unwrap();
        gh.org = Some(("member", "pending"));
        gh.pending = true;
    }
    let mut present = observed(&c, Utc::now(), true);
    present["pending"][0]["created_at"] = json!(f.github.0.lock().unwrap().created_at.unwrap());
    standard_call(
        &f,
        "OrganizationDelivery",
        &c.delivery_key(),
        "observe_listed",
        Some(present),
    )
    .await;
    let recovered: OrganizationDeliverySnapshot = serde_json::from_value(
        standard_call(
            &f,
            "OrganizationDelivery",
            &c.delivery_key(),
            "status",
            None,
        )
        .await,
    )
    .unwrap();
    assert!(matches!(
        recovered.create.outcome,
        OrganizationCreateOutcome::Created {
            upstream_id: 99,
            ..
        }
    ));
    assert_eq!(f.writes("POST").len(), 1);
}

#[tokio::test]
async fn requester_refresh_is_durable_throttled_and_never_writes() {
    let f = Fixture::new(Default::default()).await;
    let c = f.command(&[]).await;
    f.create(&c).await;
    let first = standard_call(
        &f,
        "OrganizationDelivery",
        &c.delivery_key(),
        "refresh",
        None,
    )
    .await;
    assert_eq!(first["kind"], "requested");
    for _ in 0..3 {
        let repeated = standard_call(
            &f,
            "OrganizationDelivery",
            &c.delivery_key(),
            "refresh",
            None,
        )
        .await;
        assert_eq!(repeated["kind"], "throttled");
        let at: chrono::DateTime<Utc> =
            serde_json::from_value(repeated["retry_at"].clone()).unwrap();
        assert!(at > Utc::now());
    }
    assert_eq!(f.writes("POST").len(), 1);
    assert!(f.writes("DELETE").is_empty());
}

#[tokio::test]
async fn positive_no_seat_uses_daily_retry_and_early_wake_or_replay_cannot_bypass_it() {
    let f = Fixture::new(Scenario {
        seats_full: true,
        ..Default::default()
    })
    .await;
    let c = f.command(&[5]).await;
    let blocked = f.create(&c).await;
    assert!(matches!(
        blocked.create.outcome,
        OrganizationCreateOutcome::Blocked {
            reason: OrganizationBlockedReason::NoSeat
        }
    ));
    assert!(f.writes("POST").is_empty());
    let reads = f.github.0.lock().unwrap().trace.len();
    f.github.0.lock().unwrap().seats_full = false;
    for _ in 0..3 {
        assert_eq!(f.create(&c).await.create, blocked.create);
    }
    standard_call(
        &f,
        "OrganizationDelivery",
        &c.delivery_key(),
        "recheck",
        Some(json!({"generation":1})),
    )
    .await;
    assert_eq!(f.github.0.lock().unwrap().trace.len(), reads);
    let rows=f.h.restate.admin().sql_or_panic(&format!("SELECT CAST(value AS VARCHAR) AS decoded FROM state WHERE service_name='OrganizationDelivery' AND service_key='{}' AND key='wake_due'",c.delivery_key())).await;
    assert_eq!(rows.len(), 1);
    let due: chrono::DateTime<Utc> =
        serde_json::from_str(rows[0]["decoded"].as_str().unwrap()).unwrap();
    assert!(due > Utc::now() + chrono::Duration::hours(23));
    assert!(due <= Utc::now() + chrono::Duration::hours(24));
    assert_eq!(
        standard_call(&f, "InviteBudget", "org:100", "snapshot", None).await["content_used_hour"],
        0
    );
}

#[tokio::test]
async fn seat_capacity_lost_at_s4_refunds_reservation_without_claiming_fence() {
    let f = Fixture::new(Scenario {
        seats_fill_after_first_read: true,
        ..Default::default()
    })
    .await;
    let c = f.command(&[5]).await;
    let blocked = f.create(&c).await;
    assert!(matches!(
        blocked.create.outcome,
        OrganizationCreateOutcome::Blocked {
            reason: OrganizationBlockedReason::NoSeat
        }
    ));
    {
        let github = f.github.0.lock().unwrap();
        assert_eq!(github.org_reads, 2);
        assert!(github.trace.iter().any(|(_, path, body)| path
            == "/app/installations/2/access_tokens"
            && body["permissions"]["members"] == "write"));
    }
    assert!(f.writes("POST").is_empty());
    assert!(
        !f.storage
            .organization_attempt_exists(&OrganizationAttempt {
                create: c.clone(),
                action: OrganizationWrite::Invite
            })
            .await
            .unwrap()
    );
    assert_eq!(
        standard_call(&f, "InviteBudget", "org:100", "snapshot", None).await["content_used_hour"],
        0
    );
    let before = f.github.0.lock().unwrap().trace.len();
    assert_eq!(f.create(&c).await.create, blocked.create);
    assert_eq!(f.github.0.lock().unwrap().trace.len(), before);
}

#[tokio::test]
async fn standard_foreign_invitation_disappearance_notifies_and_lapses_request() {
    let f = Fixture::start(
        Scenario {
            org: Some(("member", "pending")),
            pending: true,
            ..Default::default()
        },
        true,
    )
    .await;
    let link = InvitationLinkId::new();
    let create = json!({"secret_sha256":"2a".repeat(32),"link_id":link,"account_id":100,"installation_id":2,"admin":{"account_id":100,"user_id":7},"description":"Foreign pending completion","max_uses":10,"approval_required":false,"targets":{"organization":{"org_id":100,"org_login":"acme","grant":"member"},"teams":[],"repos":[]}});
    assert!(
        standard_call(
            &f,
            "InvitationLink",
            &link.to_string(),
            "create",
            Some(create)
        )
        .await
        .get("Ok")
        .is_some()
    );
    let input = json!({"scope_evidence":[],"secret_sha256":"2a".repeat(32),"secret_generation":1,"link_id":link,"operation_id":RequestId::new(),"requester_id":8,"justification":null});
    let admitted = standard_call(
        &f,
        "InvitationLink",
        &link.to_string(),
        "admit",
        Some(input.clone()),
    )
    .await;
    let request = admitted["result"]["request_id"].as_str().unwrap();
    let key = format!("{request}:100:1");
    let initial: OrganizationDeliverySnapshot =
        tokio::time::timeout(std::time::Duration::from_secs(15), async {
            loop {
                let value = standard_call(&f, "OrganizationDelivery", &key, "status", None).await;
                if value["create"]["outcome"]["kind"] == "already_pending" {
                    break serde_json::from_value(value).unwrap();
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
    let query = json!({"link_id":link,"request_id":request,"requester_id":8});
    let approved = standard_call(
        &f,
        "InvitationRequest",
        request,
        "request_status",
        Some(query.clone()),
    )
    .await;
    assert_eq!(approved["state"], "approved");
    let plan = standard_call(
        &f,
        "InvitationRequest",
        request,
        "approved_plan",
        Some(query.clone()),
    )
    .await;
    {
        let mut upstream = f.github.0.lock().unwrap();
        upstream.pending = false;
        upstream.org = None;
    }
    standard_call(
        &f,
        "OrganizationDelivery",
        &key,
        "observe_listed",
        Some(observed(&initial.create.command, Utc::now(), false)),
    )
    .await;
    let lapsed = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let value = standard_call(
                &f,
                "InvitationRequest",
                request,
                "request_status",
                Some(query.clone()),
            )
            .await;
            if value["state"] == "lapsed" {
                break value;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(lapsed["decision"], approved["decision"]);
    assert_eq!(lapsed["targets"], approved["targets"]);
    assert_eq!(
        standard_call(
            &f,
            "InvitationRequest",
            request,
            "approved_plan",
            Some(query.clone())
        )
        .await,
        plan
    );
    let terminal = terminal(&f, &initial.create.command).await;
    assert_eq!(terminal.create, initial.create);
    assert_eq!(
        terminal.settlement.unwrap().state,
        ghinvite_core::InvitationState::Declined
    );
    // Duplicate identity-only wakes re-read retained facts; original admission
    // and dispatch receipts cannot be rewritten by a final notification.
    for _ in 0..3 {
        standard_call(
            &f,
            "InvitationRequest",
            request,
            "on_delivery_settled",
            Some(json!({"kind":"organization","target_id":100,"delivery_generation":1})),
        )
        .await;
    }
    assert_eq!(
        standard_call(
            &f,
            "InvitationRequest",
            request,
            "request_status",
            Some(query)
        )
        .await,
        lapsed
    );
    assert_eq!(
        standard_call(
            &f,
            "InvitationLink",
            &link.to_string(),
            "admit",
            Some(input)
        )
        .await,
        admitted
    );
    assert!(f.writes("POST").is_empty());
    assert!(f.writes("PUT").is_empty());
    assert!(f.writes("DELETE").is_empty());
}

#[tokio::test]
async fn eligibility_cache_is_normalized_and_installation_bound() {
    let f = Fixture::new(Scenario {
        seats_full: true,
        ..Default::default()
    })
    .await;
    let first = standard_call(&f, "OrganizationEligibility", "org:100", "facts", None).await;
    assert_eq!(first["seats"], json!({"capacity":10,"used":10}));
    assert!(first["managed_enterprise"].is_null());
    assert!(first["two_factor_required"].is_null());
    assert_eq!(
        standard_call(&f, "OrganizationEligibility", "org:100", "facts", None).await,
        first
    );
    assert_eq!(f.github.0.lock().unwrap().org_reads, 1);
    f.github.0.lock().unwrap().seats_full = false;
    standard_call(
        &f,
        "OrganizationEligibility",
        "org:100",
        "refresh",
        Some(json!({"account_id":100,"installation_id":3})),
    )
    .await;
    assert_eq!(
        standard_call(&f, "OrganizationEligibility", "org:100", "facts", None).await,
        first
    );
    standard_call(
        &f,
        "OrganizationEligibility",
        "org:100",
        "refresh",
        Some(json!({"account_id":100,"installation_id":2})),
    )
    .await;
    let refreshed = standard_call(&f, "OrganizationEligibility", "org:100", "facts", None).await;
    assert!(refreshed["seats"].is_null());
    assert_eq!(f.github.0.lock().unwrap().org_reads, 2);
}

#[tokio::test]
async fn member_removed_wakes_only_no_seat_and_retains_exact_event_receipt() {
    let f = Fixture::new(Scenario {
        seats_full: true,
        ..Default::default()
    })
    .await;
    let c = f.command(&[5]).await;
    let blocked = f.create(&c).await;
    assert!(matches!(
        blocked.create.outcome,
        OrganizationCreateOutcome::Blocked {
            reason: OrganizationBlockedReason::NoSeat
        }
    ));
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let schedule = standard_call(&f, "SweepSchedule", "org:100", "status", None).await;
            if schedule["tracked"]["100"]
                .as_array()
                .is_some_and(|keys| keys.contains(&json!(c.delivery_key())))
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    f.github.0.lock().unwrap().seats_full = false;
    let unrelated = json!({"account_id":100,"installation_id":3,"removed_user_id":999,"payload_sha256":vec![43u8;32]});
    let reads_before = f.github.0.lock().unwrap().trace.len();
    standard_call(
        &f,
        "OrganizationEligibility",
        "org:100",
        "member_removed",
        Some(unrelated.clone()),
    )
    .await;
    standard_call(
        &f,
        "OrganizationEligibility",
        "org:100",
        "member_removed",
        Some(unrelated.clone()),
    )
    .await;
    assert_eq!(f.github.0.lock().unwrap().trace.len(), reads_before);
    let mut retarget = unrelated;
    retarget["installation_id"] = json!(2);
    let refused =
        f.h.restate
            .invoke(
                &Call::object("OrganizationEligibility", "org:100", "member_removed"),
                Some(&retarget),
                None,
            )
            .await;
    assert_eq!(refused.status, 409, "{refused:?}");
    let hint = json!({"account_id":100,"installation_id":2,"removed_user_id":999,"payload_sha256":vec![42u8;32]});
    standard_call(
        &f,
        "OrganizationEligibility",
        "org:100",
        "member_removed",
        Some(hint.clone()),
    )
    .await;
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let snapshot = standard_call(
                &f,
                "OrganizationDelivery",
                &c.delivery_key(),
                "status",
                None,
            )
            .await;
            if snapshot["create"]["outcome"]["kind"] == "created" {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let reads = f.github.0.lock().unwrap().trace.len();
    standard_call(
        &f,
        "OrganizationEligibility",
        "org:100",
        "member_removed",
        Some(hint),
    )
    .await;
    assert_eq!(f.github.0.lock().unwrap().trace.len(), reads);
    assert_eq!(f.writes("POST").len(), 1);
}

#[tokio::test]
async fn positive_managed_team_blocks_missing_membership_without_guessing_existing_access() {
    for existing in [false, true] {
        let f = Fixture::new(Scenario {
            org: Some(("member", "active")),
            managed_teams: vec![5],
            teams: if existing {
                BTreeMap::from([(5, ("member", "active"))])
            } else {
                BTreeMap::new()
            },
            ..Default::default()
        })
        .await;
        let c = f.command(&[5]).await;
        let snapshot = f.create(&c).await;
        let encoded = serde_json::to_value(&snapshot).unwrap();
        assert!(!encoded.to_string().contains("LDAP_PRIVATE_MARKER"));
        assert!(f.writes("PUT").is_empty());
        if existing {
            assert!(encoded.to_string().contains("already_satisfied"));
        } else {
            assert!(encoded.to_string().contains("idp_synced_team"));
        }
    }
    let f = Fixture::new(Scenario {
        managed_teams: vec![5],
        ..Default::default()
    })
    .await;
    let c = f.command(&[5]).await;
    let snapshot = f.create(&c).await;
    assert!(
        serde_json::to_string(&snapshot)
            .unwrap()
            .contains("idp_synced_team")
    );
    assert!(f.writes("POST").is_empty());
}

#[tokio::test]
async fn authenticated_installation_event_refreshes_existing_advisory_cache() {
    let f = Fixture::start(
        Scenario {
            seats_full: true,
            ..Default::default()
        },
        true,
    )
    .await;
    assert!(
        standard_call(&f, "OrganizationEligibility", "org:100", "facts", None).await["seats"]
            .is_object()
    );
    f.github.0.lock().unwrap().seats_full = false;
    let event = json!({"delivery_id":"ba".repeat(32),"action":"new_permissions_accepted","verified":{"app":"org","account":account(),"suspended_at":null,"permissions":{"members":"write","metadata":"read"}},"sender_id":7,"sender_login":"owner","updated_at":Utc::now()});
    standard_call(&f, "Installation", "org:2", "onboard_webhook", Some(event)).await;
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let facts =
                standard_call(&f, "OrganizationEligibility", "org:100", "facts", None).await;
            if facts.is_object() && facts["seats"].is_null() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        f.github.0.lock().unwrap().org_reads,
        4,
        "onboarding and webhook each verify availability; the facts cache performs its own initial read and event refresh"
    );
}

#[tokio::test]
async fn eligibility_unverified_identity_is_unknown_and_never_cached_as_healthy() {
    let f = Fixture::new(Scenario {
        org_id_override: Some(101),
        seats_full: true,
        ..Default::default()
    })
    .await;
    assert!(
        standard_call(&f, "OrganizationEligibility", "org:100", "facts", None)
            .await
            .is_null()
    );
    assert!(
        standard_call(&f, "OrganizationEligibility", "org:100", "facts", None)
            .await
            .is_null()
    );
    assert_eq!(
        f.github.0.lock().unwrap().org_reads,
        1,
        "bounded failed-fact cache suppresses immediate retry"
    );
    f.github.0.lock().unwrap().org_id_override = None;
    standard_call(
        &f,
        "OrganizationEligibility",
        "org:100",
        "refresh",
        Some(json!({"account_id":100,"installation_id":2})),
    )
    .await;
    assert_eq!(
        standard_call(&f, "OrganizationEligibility", "org:100", "facts", None).await["account_id"],
        100
    );
    assert_eq!(f.github.0.lock().unwrap().org_reads, 2);
}

#[tokio::test]
async fn same_installation_suspension_suppresses_cached_facts_before_refresh_delivery() {
    let f = Fixture::start(
        Scenario {
            seats_full: true,
            ..Default::default()
        },
        true,
    )
    .await;
    assert!(
        standard_call(&f, "OrganizationEligibility", "org:100", "facts", None).await["seats"]
            .is_object()
    );
    let at = Utc::now();
    f.github.0.lock().unwrap().suspended = true;
    // Apply only the canonical account owner update: the Installation object's
    // asynchronous cache-refresh message has intentionally not been delivered.
    let event = json!({"delivery_id":"bc".repeat(32),"action":"suspend","verified":{"app":"org","account":account(),"suspended_at":at,"permissions":{"members":"write","metadata":"read"}},"sender_id":7,"sender_login":"owner","updated_at":at});
    standard_call(
        &f,
        "AccountInstallation",
        "org:100",
        "onboard_webhook",
        Some(event),
    )
    .await;
    let reads = f.github.0.lock().unwrap().trace.len();
    assert!(
        standard_call(&f, "OrganizationEligibility", "org:100", "facts", None)
            .await
            .is_null()
    );
    assert_eq!(
        f.github.0.lock().unwrap().trace.len(),
        reads,
        "suspension is retained evidence, not another API read"
    );
    assert_eq!(
        standard_call(
            &f,
            "AccountInstallation",
            "org:100",
            "current_installation",
            None
        )
        .await["installation_id"],
        2,
        "legacy delivery account view is unchanged"
    );
}

#[path = "organization_delivery/invitation_timing.rs"]
mod invitation_timing;

#[path = "organization_delivery/sequence_property.rs"]
mod sequence_property;
