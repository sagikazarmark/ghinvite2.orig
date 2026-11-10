//! Whole request epoch proof through the real org-only composition.
use super::*;
use ghinvite_core::{
    InvitationState, RequestState,
    invitation_link::AccountAdmin,
    reissue::{BudgetAttribution, ReissueCommand, ReissueTarget},
    request_lifecycle::{AdminRequestStatus, ApprovedDispatch, RequestSnapshot},
};

async fn dispatch(f: &Fixture, query: &AdminRequestStatus) -> ApprovedDispatch {
    serde_json::from_value(
        standard_call(
            f,
            "InvitationRequest",
            &query.request_id.to_string(),
            "admin_dispatch",
            Some(json!(query)),
        )
        .await,
    )
    .unwrap()
}
async fn request(f: &Fixture, query: &AdminRequestStatus) -> RequestSnapshot {
    serde_json::from_value(
        standard_call(
            f,
            "InvitationRequest",
            &query.request_id.to_string(),
            "admin_status",
            Some(json!(query)),
        )
        .await,
    )
    .unwrap()
}
async fn created(f: &Fixture, c: &OrganizationCreateCommand) -> OrganizationDeliverySnapshot {
    let mut last = Value::Null;
    let result = tokio::time::timeout(std::time::Duration::from_secs(15), async {
        loop {
            let value =
                standard_call(f, "OrganizationDelivery", &c.delivery_key(), "status", None).await;
            if let Ok(snapshot) =
                serde_json::from_value::<OrganizationDeliverySnapshot>(value.clone())
                && matches!(
                    snapshot.create.outcome,
                    OrganizationCreateOutcome::Created { .. }
                )
                && snapshot
                    .teams
                    .values()
                    .all(|t| matches!(t.create.outcome, TeamCreateOutcome::Created { .. }))
            {
                return snapshot;
            }
            last = value;
            tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        }
    })
    .await;
    result.unwrap_or_else(|error| {
        panic!(
            "current generation {} creates all attached teams: {error}; last={last}; trace={:?}",
            c.delivery_generation,
            f.github.0.lock().unwrap().trace
        )
    })
}

#[tokio::test]
async fn org_only_request_reissue_advances_attached_teams_and_preserves_original_approval() {
    let f = Fixture::start_reissue().await;
    let (_, init) = cancel_unsent::command_and_input_with_teams(&f, &[5, 6]).await;
    let query = AdminRequestStatus {
        request_id: init.request.request_id,
        admin: AccountAdmin {
            account_id: 100,
            user_id: 7,
        },
    };
    let key = query.request_id.to_string();
    standard_call(
        &f,
        "InvitationRequest",
        &key,
        "initialize_batch",
        Some(json!(init)),
    )
    .await;
    let first_plan = dispatch(&f, &query).await;
    let first = first_plan.organization_command.as_ref().unwrap();
    let original = created(&f, first).await;
    assert_eq!(first.delivery_generation, 1);
    assert_eq!(first.teams.len(), 2);
    {
        let mut upstream = f.github.0.lock().unwrap();
        upstream.pending = false;
        upstream.org = None;
    }
    standard_call(
        &f,
        "OrganizationDelivery",
        &first.delivery_key(),
        "observe_listed",
        Some(observed(first, Utc::now(), false)),
    )
    .await;
    let ended = terminal(&f, first).await;
    assert_eq!(
        ended.settlement.as_ref().unwrap().state,
        InvitationState::Declined
    );
    assert_eq!(
        ended.teams[&5].settlement.as_ref().unwrap().state,
        InvitationState::Declined
    );
    assert_eq!(ended.create, original.create);
    // An owned decline is a closed outcome, not LapsedInvite; its original
    // approval remains retained while this explicit operation advances scope.
    let before = request(&f, &query).await;
    assert_eq!(before.state, RequestState::Approved);
    let input = ReissueCommand {
        operation_id: RequestId::new().to_string().try_into().unwrap(),
        request_id: query.request_id,
        admin: query.admin.clone(),
        target: ReissueTarget::Organization { org_id: 100 },
    };
    let receipt = standard_call(&f, "InvitationRequest", &key, "reissue", Some(json!(input))).await;
    assert_eq!(receipt, json!({"Ok":{"delivery_generation":2}}));
    assert_eq!(
        standard_call(&f, "InvitationRequest", &key, "reissue", Some(json!(input))).await,
        receipt
    );
    let next_plan = dispatch(&f, &query).await;
    let next = next_plan.organization_command.as_ref().unwrap();
    assert_eq!(next.delivery_generation, 2);
    assert_eq!(next.teams, first.teams);
    assert_eq!(next.organization, first.organization);
    assert_eq!(next.batch, first.batch);
    assert_eq!(next.link_id, first.link_id);
    assert_eq!(next.approval_id, first.approval_id);
    assert_eq!(next.approved_at, first.approved_at);
    assert_eq!(
        next.budget_attribution,
        BudgetAttribution::AdministratorReissue
    );
    assert!(first.invitation_id.is_none());
    assert!(next.invitation_id.is_none());
    assert_ne!(next.delivery_key(), first.delivery_key());
    assert_ne!(next.storage_id(), first.storage_id());
    let second = created(&f, next).await;
    assert_eq!(second.teams.keys().copied().collect::<Vec<_>>(), vec![5, 6]);
    assert!(second.teams.values().all(|team| team.settlement.is_none()));
    standard_call(
        &f,
        "InvitationRequest",
        &key,
        "on_delivery_settled",
        Some(json!({"kind":"organization","target_id":100,"delivery_generation":1})),
    )
    .await;
    let after = request(&f, &query).await;
    assert_eq!(after.state, RequestState::Approved);
    assert_eq!(after.decision, before.decision);
    assert_eq!(after.decision, init.request.decision);
    let retained: OrganizationDeliverySnapshot = serde_json::from_value(
        standard_call(
            &f,
            "OrganizationDelivery",
            &first.delivery_key(),
            "status",
            None,
        )
        .await,
    )
    .unwrap();
    assert_eq!(retained, ended);
    standard_call(
        &f,
        "InvitationRequest",
        &key,
        "initialize_batch",
        Some(json!(init)),
    )
    .await;
    assert_eq!(dispatch(&f, &query).await, next_plan);
    assert_eq!(f.writes("POST").len(), 2);
    assert!(f.writes("PUT").is_empty());
    assert!(f.writes("DELETE").is_empty());
    use ghinvite_core::storage::AppInstallationReadStorage;
    assert!(
        f.storage
            .active_app_installation(ghinvite_core::account::App::Repo, 100)
            .await
            .unwrap()
            .is_none()
    );
}
