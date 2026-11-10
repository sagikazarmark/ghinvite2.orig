use super::*;
use ghinvite_core::{
    bulk::{
        BatchRef, BulkBatchId, Entry, Manifest, Resolution, Review, RowEvidence, RowRequest,
        Submission,
    },
    bulk_cancel::{CancelUnsentResult, OrganizationCancelUnsent},
    invitation_link::AccountAdmin,
    storage::{
        bulk::BulkStorage,
        projection::{ProjectionStorage, RequestProjectionEnvelope},
    },
    targets::{MemberGrant, OrganizationTarget, Targets, TeamTarget},
};
pub(super) async fn command(f: &Fixture) -> OrganizationCreateCommand {
    command_and_input(f).await.0
}
pub(super) async fn command_and_input(
    f: &Fixture,
) -> (
    OrganizationCreateCommand,
    ghinvite_core::request_lifecycle::InitializeRequest,
) {
    command_and_input_with_teams(f, &[5]).await
}
pub(super) async fn command_and_input_with_teams(
    f: &Fixture,
    team_ids: &[u64],
) -> (
    OrganizationCreateCommand,
    ghinvite_core::request_lifecycle::InitializeRequest,
) {
    let now = Utc::now();
    let batch_id = BulkBatchId::new();
    let request_id = RequestId::new();
    let mut review = Review::new(
        Manifest {
            batch_id,
            admin: AccountAdmin {
                account_id: 100,
                user_id: 7,
            },
            installation_id: 2,
            organization_installation_id: None,
            targets: Targets {
                organization: Some(OrganizationTarget {
                    org_id: 100,
                    org_login: "acme".into(),
                    grant: MemberGrant::Member,
                }),
                teams: team_ids
                    .iter()
                    .map(|id| TeamTarget {
                        team_id: *id,
                        team_slug: format!("team-{id}"),
                        grant: MemberGrant::Member,
                    })
                    .collect(),
                repos: vec![],
            },
            access_lifetime_seconds: None,
            entries: vec![Entry::Handle {
                login: "alice".into(),
            }],
        },
        vec![RowEvidence {
            previews: vec![],
            row: 1,
            resolution: Resolution::Found {
                user_id: 8,
                login: "alice".into(),
            },
            all_targets_satisfied: false,
            observed_at: now,
            next_budget_slot: None,
        }],
        now,
    )
    .unwrap();
    let row = RowRequest {
        source: BatchRef { batch_id, row: 1 },
        request_id,
        requester_id: 8,
        requester_login: "alice".into(),
    };
    review
        .submit(
            1,
            Submission {
                submitted_by: 7,
                requests: vec![row.clone()],
                emails: vec![],
                personal_links: vec![],
                submitted_at: now,
            },
        )
        .unwrap();
    f.storage.project_bulk_review(&review).await.unwrap();
    let init = ghinvite_core::bulk_commands::initialization(&review, &row).unwrap();
    f.storage
        .apply_request(&RequestProjectionEnvelope {
            request: init.request.clone(),
            events: vec![],
            approval: None,
        })
        .await
        .unwrap();
    let command = OrganizationCreateCommand {
        invitation_id: Some(GithubInvitationId::new()),
        link_id: None,
        batch: Some(row.source),
        budget_attribution: Default::default(),
        access_lifetime_seconds: None,
        request_id,
        approval_id: format!("request.approved/{request_id}"),
        account_id: 100,
        installation_id: 2,
        requester_id: 8,
        organization: init.targets.organization.clone().unwrap(),
        teams: init.targets.teams.clone(),
        delivery_generation: 1,
        approved_at: now,
        budget_share_pct: 50,
    };
    (command, init)
}
fn input(c: &OrganizationCreateCommand) -> OrganizationCancelUnsent {
    OrganizationCancelUnsent {
        operation_id: RequestId::new().to_string().try_into().unwrap(),
        admin: AccountAdmin {
            account_id: 100,
            user_id: 7,
        },
        command: c.clone(),
    }
}
async fn stop(f: &Fixture, i: &OrganizationCancelUnsent) -> CancelUnsentResult {
    serde_json::from_value(f.call(&i.command, "cancel_unsent", json!(i)).await).unwrap()
}
async fn active(f: &Fixture, c: &OrganizationCreateCommand) {
    f.call(c,"control",json!({"envelope":{"identity":{"link_id":null,"batch":c.batch,"request_id":c.request_id,"kind":"organization","target_id":100,"delivery_generation":1},"operation_id":RequestId::new(),"revision":1,"hold_epoch":0,"at":c.approved_at,"control":{"kind":"active"}},"admitted_at":c.approved_at,"invitation_lifetime_seconds":604800})).await;
}
#[tokio::test]
async fn cancel_before_create_materializes_batch_aborts_and_suppresses_late_dispatch() {
    let f = Fixture::new(Default::default()).await;
    let c = command(&f).await;
    let query = input(&c);
    assert_eq!(stop(&f, &query).await, CancelUnsentResult::Aborted);
    let stopped = f.create(&c).await;
    assert!(matches!(
        stopped.create.outcome,
        OrganizationCreateOutcome::Aborted {
            reason: ghinvite_core::delivery::AbortReason::BatchCancelled
        }
    ));
    assert!(matches!(
        stopped.teams[&5].create.outcome,
        TeamCreateOutcome::Aborted {
            reason: ghinvite_core::delivery::AbortReason::BatchCancelled
        }
    ));
    assert_eq!(stop(&f, &query).await, CancelUnsentResult::Aborted);
    assert_eq!(f.create(&c).await, stopped);
    assert!(f.writes("POST").is_empty());
    assert!(f.writes("PUT").is_empty());
    assert!(f.writes("DELETE").is_empty());
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            if f.storage
                .get_organization_delivery(&c.storage_id())
                .await
                .unwrap()
                == Some(stopped.clone())
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let mut changed = query;
    changed.admin.user_id = 8;
    let response =
        f.h.restate
            .invoke(
                &Call::object("OrganizationDelivery", &c.delivery_key(), "cancel_unsent"),
                Some(&json!(changed)),
                None,
            )
            .await;
    assert_eq!(response.status, 409);
}
#[tokio::test]
async fn uncertain_org_post_protects_attached_team_and_blocks_future_writes_without_refund() {
    let f = Fixture::new(Scenario {
        writes: VecDeque::from([502]),
        ..Default::default()
    })
    .await;
    let c = command(&f).await;
    active(&f, &c).await;
    let original = f.create(&c).await;
    assert_eq!(
        original.create.outcome,
        OrganizationCreateOutcome::OutcomeUnknown
    );
    let query = input(&c);
    assert_eq!(stop(&f, &query).await, CancelUnsentResult::MayHaveWritten);
    for _ in 0..3 {
        assert_eq!(stop(&f, &query).await, CancelUnsentResult::MayHaveWritten);
        let current = f.create(&c).await;
        assert_eq!(
            current.create.outcome,
            OrganizationCreateOutcome::OutcomeUnknown
        );
        assert_eq!(
            current.teams[&5].create.outcome,
            TeamCreateOutcome::OutcomeUnknown
        );
    }
    assert_eq!(f.writes("POST").len(), 1);
    assert!(f.writes("PUT").is_empty());
    assert!(f.writes("DELETE").is_empty());
    assert_eq!(
        standard_call(&f, "InviteBudget", "org:100", "snapshot", None).await["content_used_hour"],
        1
    );
}
#[tokio::test]
async fn satisfied_org_and_team_preserve_receipts_without_claiming_owned_write() {
    let f = Fixture::new(Scenario {
        org: Some(("admin", "active")),
        teams: BTreeMap::from([(5, ("maintainer", "active"))]),
        ..Default::default()
    })
    .await;
    let c = command(&f).await;
    active(&f, &c).await;
    let original = f.create(&c).await;
    assert_eq!(
        stop(&f, &input(&c)).await,
        CancelUnsentResult::AlreadyCompleted
    );
    assert_eq!(f.create(&c).await, original);
    assert!(f.writes("POST").is_empty());
    assert!(f.writes("PUT").is_empty());
    assert!(f.writes("DELETE").is_empty());
}

#[tokio::test]
async fn org_only_current_owner_cancels_batch_through_real_request_before_dispatch() {
    let f = Fixture::start(Default::default(), true).await;
    use ghinvite_core::storage::AppInstallationReadStorage;
    assert!(
        f.storage
            .active_app_installation(ghinvite_core::account::App::Repo, 100)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        f.storage
            .active_app_installation(ghinvite_core::account::App::Org, 100)
            .await
            .unwrap()
            .is_some()
    );
    let (command, init) = command_and_input(&f).await;
    let cancellation = ghinvite_core::bulk_cancel::CancelRequest {
        operation_id: RequestId::new().to_string().try_into().unwrap(),
        admin: AccountAdmin {
            account_id: 100,
            user_id: 7,
        },
        input: init.clone(),
    };
    let request = command.request_id.to_string();
    let receipt = standard_call(
        &f,
        "InvitationRequest",
        &request,
        "cancel_batch",
        Some(json!(cancellation)),
    )
    .await;
    assert_eq!(receipt["stopped"], true);
    assert_eq!(receipt["aborted"], 1);
    assert_eq!(receipt["uncertain"], 0);
    assert_eq!(receipt["already_written"], 0);
    assert_eq!(
        standard_call(
            &f,
            "InvitationRequest",
            &request,
            "cancel_batch",
            Some(json!(cancellation))
        )
        .await,
        receipt
    );
    // A delayed initialization is the same approved row, but its target owner
    // already retained the no-write stop before the request dispatched it.
    standard_call(
        &f,
        "InvitationRequest",
        &request,
        "initialize_batch",
        Some(json!(init)),
    )
    .await;
    let key = command.delivery_key();
    let snapshot: OrganizationDeliverySnapshot = serde_json::from_value(
        standard_call(&f, "OrganizationDelivery", &key, "status", None).await,
    )
    .unwrap();
    assert!(matches!(
        snapshot.create.outcome,
        OrganizationCreateOutcome::Aborted {
            reason: ghinvite_core::delivery::AbortReason::BatchCancelled
        }
    ));
    assert!(matches!(
        snapshot.teams[&5].create.outcome,
        TeamCreateOutcome::Aborted {
            reason: ghinvite_core::delivery::AbortReason::BatchCancelled
        }
    ));
    let trace = f.github.0.lock().unwrap().trace.clone();
    assert!(
        trace
            .iter()
            .any(|(m, p, _)| m == "GET" && p == "/orgs/acme/memberships/owner")
    );
    assert!(!trace.iter().any(|(m, p, _)| m == "DELETE"
        || m == "PUT"
        || (m == "POST" && p == "/orgs/acme/invitations")));
}
