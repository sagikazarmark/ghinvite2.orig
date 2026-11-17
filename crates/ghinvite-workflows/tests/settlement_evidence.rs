//! Real Restate owner transitions driven by typed GitHub HTTP observations.
//!
//! GitHub is the octoemu emulator ([`ghinvite_e2e::github`]): alice's
//! invitation, acceptance, role and expiry are GitHub's own state, changed
//! through the admin API; only a repository read that fails (404) and a lost
//! write response are fault rules.
#![cfg(feature = "integration")]
use chrono::Utc;
use ghinvite_core::{
    GithubInvitationId, InvitationLinkId, RequestId,
    delivery::{CreateCommand, CreateOutcome, DeliverySnapshot},
    storage::{ConsoleStorage, RecordStorage},
};
use ghinvite_e2e::github::Outcome;
use ghinvite_e2e::{Call, Harness};
use ghinvite_workflows::settlement_policy::{Evidence, Observation};
use octoemu::faults::{Fault, FaultAction};
use octoemu::seed::{RepositoryInvitation, Role, Seed};
use serde_json::{Value, json};
use std::sync::Arc;

struct Runtime {
    harness: Harness,
    storage: Arc<ghinvite_storage_sqlx::SqlxStorage>,
}
impl Runtime {
    async fn start() -> Self {
        let harness = Harness::start().await;
        let storage = Arc::new(
            ghinvite_storage_sqlx::SqlxStorage::in_memory()
                .await
                .unwrap(),
        );
        for (user_id, login) in [(7, "creator"), (8, "alice")] {
            storage
                .upsert_user(&ghinvite_core::User {
                    user_id,
                    login: login.into(),
                    avatar_url: None,
                    github_created_at: None,
                    last_seen_at: Utc::now(),
                })
                .await
                .unwrap();
        }
        let github_transport = ghinvite_e2e::github_transport(harness.client.clone());
        let apps = Arc::new(
            ghinvite_workflows::RepositoryGithub::new(
                github_transport,
                &harness.github_url,
                ghinvite_e2e::repository_apps(&harness.github_url),
            )
            .unwrap(),
        );
        let state = ghinvite_workflows::AppState::new(storage.clone()).with_github_apps(apps);
        let endpoint = ghinvite_workflows::build_endpoint(state, storage.clone(), None).unwrap();
        harness.restate.deploy(endpoint).await;
        let reply=harness.restate.invoke(&Call::object("AccountInstallation", "repo:100","onboard"),Some(&ghinvite_e2e::setup_command(json!({"app":"repo","installation_id":1,"actor_user_id":7,"account_id":100,"account_login":"acme","account_type":"Organization","selected_repos":"all","installed_at":Utc::now()}))),None).await;
        assert_eq!(reply.status, 200, "{reply:?}");
        Self { harness, storage }
    }
    async fn command(&self) -> CreateCommand {
        let link:ghinvite_core::InvitationLink=serde_json::from_value(json!({"secret":{"secret_generation":1,"sha256":"2a".repeat(32)},
            "id":InvitationLinkId::new(),"installation_id":1,"account_id":100,"created_by":7,"created_at":Utc::now(),"expires_at":null,"max_uses":null,"uses_count":1,"approval_required":true,"description":"Settlement evidence","internal_note":null,"revoked_at":null,"revoked_by":null,"targets":{"organization":null,"teams":[],"repos":[{"repo_id":10,"repo_full_name":"acme/api","grant":"write"}]}
        })).unwrap();
        let request:ghinvite_core::InvitationRequest=serde_json::from_value(json!({"id":RequestId::new(),"invitation_link_id":link.id,"requester_id":8,"justification":"Docs","state":"approved","decided_by":7,"decided_at":Utc::now(),"decline_reason":null,"created_at":Utc::now(),"decision_deadline":null})).unwrap();
        ghinvite_core::storage::projection::fixture::seed(
            self.storage.as_ref(),
            &link,
            std::slice::from_ref(&request),
            1,
        )
        .await
        .unwrap();
        CreateCommand {
            batch: None,
            budget_attribution: Default::default(),
            access_lifetime_seconds: None,
            budget_share_pct: 50,
            invitation_id: Some(GithubInvitationId::new()),
            delivery_generation: 1,
            link_id: Some(link.id),
            request_id: request.id,
            approval_id: format!("fixture/{}", request.id),
            account_id: 100,
            installation_id: 1,
            requester_id: 8,
            repo_id: 10,
            repo_full_name: "acme/api".into(),
            permission: ghinvite_core::Permission::Push,
            approved_at: request.decided_at.unwrap(),
        }
    }
    async fn owner(&self, command: &CreateCommand, handler: &str, input: &Value) -> Value {
        let reply = self
            .harness
            .restate
            .invoke(
                &Call::object("RepositoryDelivery", &command.delivery_key(), handler),
                (!input.is_null()).then_some(input),
                None,
            )
            .await;
        assert_eq!(reply.status, 200, "{handler}: {reply:?}");
        reply.body
    }
    async fn snapshot(&self, command: &CreateCommand) -> DeliverySnapshot {
        serde_json::from_value(self.owner(command, "status", &Value::Null).await).unwrap()
    }
    async fn observe(&self, command: &CreateCommand) {
        let snapshot = self.snapshot(command).await;
        let reply = self
            .harness
            .restate
            .invoke(
                &Call::service("DeliveryObservation", "observe"),
                Some(&json!(snapshot)),
                None,
            )
            .await;
        assert_eq!(reply.status, 200, "{reply:?}");
    }
    async fn projected(&self, expected: &DeliverySnapshot) {
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                let rows = self
                    .storage
                    .list_delivery_for_request(expected.create.command.request_id)
                    .await
                    .unwrap();
                if rows.iter().any(|row| row == expected) {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(25)).await;
            }
        })
        .await
        .expect("retained settlement projection must converge");
    }
    /// A fresh GitHub side: no invitations, collaborators or staged
    /// answers, and an empty call log.
    async fn reset(&self) {
        self.harness.github().reset().await;
    }
    /// alice's open invitation to `acme/api`.
    fn invitation(&self) -> Option<u64> {
        self.harness
            .github()
            .invitations("alice")
            .into_iter()
            .find(|(_, repo)| repo == "acme/api")
            .map(|(id, _)| id)
    }
    /// `(method, path)` of every call since the log was last emptied.
    fn calls(&self) -> Vec<(String, String)> {
        self.harness
            .emulator()
            .calls()
            .into_iter()
            .map(|call| (call.method, call.path))
            .collect()
    }
}

#[tokio::test]
async fn real_owner_applies_only_complete_current_evidence() {
    let runtime = Runtime::start().await;
    // AC-ACC-2: base/effective read does not satisfy requested write.
    let command = runtime.command().await;
    runtime.owner(&command, "create", &json!(command)).await;
    let created = runtime.snapshot(&command).await;
    let invitation = runtime.invitation().expect("alice is invited");
    assert_eq!(
        created.create.outcome,
        CreateOutcome::Created {
            upstream_id: invitation
        },
        "calls={:?}",
        runtime.calls()
    );
    // alice declines, and reads the repository anyway (a direct read
    // collaborator).
    let github = runtime.harness.github();
    github.decline("alice", invitation).await;
    github.collaborate("alice", Role::Pull).await;
    runtime.observe(&command).await;
    let declined = runtime.snapshot(&command).await;
    assert_eq!(
        declined.settlement.as_ref().unwrap().state,
        ghinvite_core::InvitationState::Declined
    );
    assert_eq!(declined.create, created.create);
    assert_eq!(
        declined.settlement.as_ref().unwrap().event.metadata["evidence"]["role_name"],
        "read"
    );
    // A duplicate or stale stronger observation cannot replace the winner.
    let stale = Observation::new(
        &created,
        Utc::now(),
        Evidence::Absent {
            availability_verified: true,
            role_name: Some(ghinvite_core::targets::EffectiveRole::Admin),
            direct_collaborator: Some(true),
            membership: None,
        },
    );
    runtime.owner(&command, "reconcile", &json!(stale)).await;
    assert_eq!(runtime.snapshot(&command).await, declined);
    runtime.projected(&declined).await;
    let events = runtime
        .storage
        .list_audit_events(
            100,
            Some(ghinvite_core::audit::EventType::InvitationDeclined),
            ghinvite_core::storage::AuditPosition::Latest,
        )
        .await
        .unwrap()
        .events;
    assert_eq!(
        events
            .iter()
            .filter(|event| event.target_id
                == command
                    .invitation_id
                    .expect("retained V1 fixture identity")
                    .to_string())
            .count(),
        1
    );

    // AC-ACC-4: full empty invitation list followed by repository 404 never settles.
    runtime.reset().await;
    let command = runtime.command().await;
    runtime.owner(&command, "create", &json!(command)).await;
    let created = runtime.snapshot(&command).await;
    // alice accepts (a write collaborator; the list is empty), but the
    // repository read answers 404 (a fault rule).
    github
        .accept("alice", runtime.invitation().expect("alice is invited"))
        .await;
    let unavailable = github.rule(
        "repository unavailable",
        Fault::on(
            "GET",
            "/repos/acme/api",
            FaultAction::respond_json(404, json!({"message":"Not Found"})),
        ),
    );
    runtime.harness.emulator().clear_calls();
    runtime.observe(&command).await;
    assert_eq!(runtime.snapshot(&command).await, created);
    {
        let calls = runtime.calls();
        let list = calls
            .iter()
            .position(|(_, p)| p.split('?').next().unwrap().ends_with("/invitations"))
            .unwrap();
        let verify = calls
            .iter()
            .position(|(_, p)| p == "/repos/acme/api")
            .unwrap();
        assert!(list < verify);
    }
    assert!(runtime.harness.emulator().fault(unavailable).unwrap().fired > 0);
    runtime.harness.emulator().remove_fault(unavailable);
    runtime.observe(&command).await;
    assert_eq!(
        runtime.snapshot(&command).await.settlement.unwrap().state,
        ghinvite_core::InvitationState::Accepted
    );

    // Existing pending invitations are foreign. Revocation and own expiry stop
    // local tracking, leave upstream pending, and never invent an upstream fact.
    for (handler, id) in [("cancel", 9001), ("tick_expire", 9002)] {
        runtime.reset().await;
        // The organization's owner already invited alice to write.
        runtime
            .harness
            .emulator()
            .seed(Seed::new().repository_invitation(
                RepositoryInvitation::new(id, "acme/api", "alice", Role::Push).inviter("creator"),
            ))
            .unwrap();
        let command = runtime.command().await;
        runtime.owner(&command, "create", &json!(command)).await;
        let foreign = runtime.snapshot(&command).await;
        assert!(foreign.create.outcome.tracked_foreign());
        runtime.harness.emulator().clear_calls();
        runtime.owner(&command,handler,&json!({"invitation_id":command.invitation_id,"installation_id":1,"at":Utc::now(),"by_user":7})).await;
        let stopped = runtime.snapshot(&command).await;
        assert!(stopped.tracking_stopped.is_some());
        assert!(stopped.settlement.is_none());
        assert_eq!(stopped.create, foreign.create);
        runtime.observe(&command).await;
        assert!(runtime.calls().is_empty());
        assert_eq!(runtime.invitation(), Some(id));
        runtime.projected(&stopped).await;
    }

    // The seven-day tick must not mistake an unexpired pending invitation for
    // GitHub expiry; explicit expired=true is required and preserves create history.
    runtime.reset().await;
    let command = runtime.command().await;
    runtime.owner(&command, "create", &json!(command)).await;
    let created = runtime.snapshot(&command).await;
    let tick = json!({"invitation_id":command.invitation_id,"installation_id":1,"at":Utc::now()});
    runtime.owner(&command, "tick_expire", &tick).await;
    assert_eq!(runtime.snapshot(&command).await, created);
    github
        .expire(runtime.invitation().expect("alice is invited"))
        .await;
    runtime.owner(&command, "tick_expire", &tick).await;
    let expired = runtime.snapshot(&command).await;
    assert_eq!(
        expired.settlement.as_ref().unwrap().state,
        ghinvite_core::InvitationState::Expired
    );
    assert_eq!(expired.create, created.create);
    runtime.projected(&expired).await;
    assert!(!runtime.calls().iter().any(|(method, _)| method == "DELETE"));

    // Ambiguous creation: absence, even with strong role, cannot resolve it.
    // The write is applied but its response is lost; alice accepts and
    // becomes an admin, so her invitation is gone.
    runtime.reset().await;
    github
        .outcome("acme/api", "alice", Outcome::CreatedResponseLost)
        .await;
    let command = runtime.command().await;
    runtime.owner(&command, "create", &json!(command)).await;
    let unknown = runtime.snapshot(&command).await;
    assert_eq!(unknown.create.outcome, CreateOutcome::OutcomeUnknown);
    github
        .accept(
            "alice",
            runtime.invitation().expect("the lost write applied"),
        )
        .await;
    github.collaborate("alice", Role::Admin).await;
    runtime.observe(&command).await;
    assert_eq!(runtime.snapshot(&command).await, unknown);
    assert_eq!(github.writes(), 1);
    // Presence resolves it: the same lost write, the invitation still open.
    runtime.reset().await;
    github
        .outcome("acme/api", "alice", Outcome::CreatedResponseLost)
        .await;
    let command = runtime.command().await;
    runtime.owner(&command, "create", &json!(command)).await;
    let unknown = runtime.snapshot(&command).await;
    assert_eq!(unknown.create.outcome, CreateOutcome::OutcomeUnknown);
    let invitation = runtime.invitation().expect("the lost write applied");
    runtime.observe(&command).await;
    let resolved = runtime.snapshot(&command).await;
    assert_eq!(
        resolved.create.outcome,
        CreateOutcome::Created {
            upstream_id: invitation
        }
    );
    assert!(resolved.settlement.is_none());
    assert_eq!(github.writes(), 1);
    runtime.harness.finish().await;
}
