use ghinvite_core::storage::{
    AppInstallationStorage, AuditStorage, ConsoleStorage, DeliveryStorage, RecordStorage,
};
use ghinvite_core::{
    audit::*,
    delivery::{DeliverySnapshot, Settlement},
    storage::AuditPosition,
    *,
};
use ghinvite_storage_sqlx::SqlxStorage;

async fn fixture() -> (SqlxStorage, DeliverySnapshot) {
    fixture_with(SqlxStorage::in_memory().await.unwrap()).await
}

async fn fixture_with(s: SqlxStorage) -> (SqlxStorage, DeliverySnapshot) {
    let at = "2026-09-17T12:00:00Z".parse().unwrap();
    s.put_app_installation(&ghinvite_core::account::Installation::repo(
        (Account {
            installation_id: 9,
            account_id: 100,
            account_login: "acme".into(),
            account_type: AccountType::Organization,
            installed_at: at,
            uninstalled_at: None,
            selected_repos: SelectedRepos::All,
        })
        .clone(),
    ))
    .await
    .unwrap();
    s.upsert_user(&User {
        user_id: 7,
        login: "alice".into(),
        avatar_url: None,
        github_created_at: None,
        last_seen_at: at,
    })
    .await
    .unwrap();
    let link = InvitationLink {
        batch_source: None,
        access_lifetime_seconds: None,
        email_domains: vec![],
        id: InvitationLinkId::new(),
        organization_installation_id: None,
        installation_id: 9,
        account_id: 100,
        created_by: 7,
        created_at: at,
        expires_at: None,
        max_uses: Some(1000),
        invitation_lifetime_seconds: 604800,
        abuse_threshold_per_hour: None,
        budget_share_pct: 50,
        uses_count: 0,

        approval_required: false,
        description: "Settlement".into(),
        internal_note: None,
        revoked_at: None,
        revoked_by: None,
        pause: None,
        targets: ghinvite_core::targets::Targets {
            repos: vec![ghinvite_core::targets::RepoTarget {
                repo_id: 10,
                repo_full_name: "acme/api".into(),
                grant: ghinvite_core::targets::RepoRole::Read,
            }],
            ..Default::default()
        },

        secret: ghinvite_core::LinkSecretState {
            secret_generation: 1,
            sha256: ghinvite_core::SecretHash::from_bytes([1; 32]),
        },
    };
    let request = InvitationRequest {
        batch: None,
        domain_evidence: None,
        id: RequestId::new(),
        invitation_link_id: Some(link.id),
        requester_id: 7,
        justification: None,
        state: RequestState::Approved,
        decided_by: None,
        decided_at: Some(at),
        cancellation_reason: None,
        decline_reason: None,
        requester_message: None,
        decision_deadline: None,
        created_at: at,
    };
    storage::projection::fixture::seed(&s, &link, std::slice::from_ref(&request), 1)
        .await
        .unwrap();
    let snapshot: DeliverySnapshot = ghinvite_core::delivery::CreateReceipt {
        audit_version: ghinvite_core::delivery_audit::DeliveryAuditVersion::CanonicalV2,
        command: ghinvite_core::delivery::CreateCommand {
            invitation_id: None,
            delivery_generation: 1,
            link_id: Some(link.id),
            batch: None,
            request_id: request.id,
            approval_id: "approval".into(),
            account_id: 100,
            installation_id: 9,
            requester_id: 7,
            repo_id: 10,
            repo_full_name: "acme/api".into(),
            permission: Permission::Pull,
            approved_at: at,
            budget_attribution: Default::default(),
            access_lifetime_seconds: None,
            budget_share_pct: 50,
        },
        outcome: ghinvite_core::delivery::CreateOutcome::Created { upstream_id: 9988 },
        revision: 1,
        confirmed_at: Some(at),
    }
    .into();
    s.project_delivery(&snapshot).await.unwrap();
    (s, snapshot)
}

fn transition(snapshot: &DeliverySnapshot, state: InvitationState) -> DeliverySnapshot {
    let confirmed_at = snapshot.create.confirmed_at.unwrap();
    let mut snapshot = snapshot.clone();
    snapshot.revision += 1;
    snapshot.settlement = Some(Settlement {
        state,
        event: AuditEvent {
            id: ghinvite_core::delivery_audit::repository_settlement_id(&snapshot.create).unwrap(),
            account_id: 100,
            occurred_at: confirmed_at + chrono::Duration::hours(1),
            event_type: if state == InvitationState::Accepted {
                EventType::InvitationAccepted
            } else {
                EventType::InvitationCancelled
            },
            actor_kind: ActorKind::System,
            actor_id: None,
            target_kind: TargetKind::RepositoryDelivery,
            target_id: snapshot.create.command.delivery_key(),
            metadata: serde_json::json!({"reconciled": true}),
            request_id: None,
        },
    });
    snapshot
}

#[tokio::test]
async fn accepted_settlement_survives_stale_sweep_and_acknowledgement_replay() {
    let (s, row) = fixture().await;
    let accepted = transition(&row, InvitationState::Accepted);
    s.project_delivery(&accepted).await.unwrap();
    assert!(
        s.project_delivery(&transition(&row, InvitationState::Cancelled))
            .await
            .is_err()
    );
    s.project_delivery(&row).await.unwrap();
    s.project_delivery(&accepted).await.unwrap();
    let result = s
        .get_delivery(&row.create.command.delivery_key())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        result.settlement.as_ref().unwrap().state,
        InvitationState::Accepted
    );
    assert_eq!(
        result.create.outcome,
        ghinvite_core::delivery::CreateOutcome::Created { upstream_id: 9988 }
    );
    let audit = s
        .list_audit_events(100, None, AuditPosition::Latest)
        .await
        .unwrap();
    assert_eq!(audit.events.len(), 2);
    assert!(audit.events.contains(&accepted.settlement.unwrap().event));
}

#[tokio::test]
async fn conflicting_audit_cannot_silently_omit_settlement_event() {
    let (s, row) = fixture().await;
    let accepted = transition(&row, InvitationState::Accepted);
    let mut conflict = accepted.settlement.as_ref().unwrap().event.clone();
    conflict.target_id = "different-invitation".into();
    s.audit(&conflict).await.unwrap();
    assert!(s.project_delivery(&accepted).await.is_err());
    assert_eq!(
        s.get_delivery(&row.create.command.delivery_key())
            .await
            .unwrap()
            .unwrap()
            .settlement,
        None
    );
}

#[tokio::test]
async fn late_writer_cannot_overwrite_committed_settlement() {
    let path = std::env::temp_dir().join(format!("settlement-fence-{}.sqlite", RequestId::new()));
    let (s, row) = fixture_with(SqlxStorage::at_path(&path).await.unwrap()).await;
    s.project_delivery(&transition(&row, InvitationState::Accepted))
        .await
        .unwrap();
    // A delayed write from an older invocation, outside the settlement API.
    let db =
        sqlx::SqlitePool::connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(&path))
            .await
            .unwrap();
    assert!(
        sqlx::query("UPDATE deliveries SET settlement = 'cancelled' WHERE id = ?1")
            .bind(row.create.command.delivery_key())
            .execute(&db)
            .await
            .is_err()
    );
    db.close().await;
    assert_eq!(
        s.get_delivery(&row.create.command.delivery_key())
            .await
            .unwrap()
            .unwrap()
            .settlement
            .unwrap()
            .state,
        InvitationState::Accepted
    );
    drop(s);
    std::fs::remove_file(&path).ok();
}

#[cfg(feature = "test-util")]
#[tokio::test]
async fn audit_failure_rolls_back_state_and_retry_publishes_once() {
    let (s, row) = fixture().await;
    let accepted = transition(&row, InvitationState::Accepted);
    s.debug_set_audit_failure(true).await.unwrap();
    assert!(s.project_delivery(&accepted).await.is_err());
    assert_eq!(
        s.get_delivery(&row.create.command.delivery_key())
            .await
            .unwrap()
            .unwrap(),
        row
    );
    s.debug_set_audit_failure(false).await.unwrap();
    s.project_delivery(&accepted).await.unwrap();
    s.project_delivery(&accepted).await.unwrap();
    assert!(
        s.list_audit_events(100, None, AuditPosition::Latest)
            .await
            .unwrap()
            .events
            .contains(&accepted.settlement.unwrap().event)
    );
}
