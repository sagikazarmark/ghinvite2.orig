use ghinvite_core::storage::{
    AuditStorage, ConsoleStorage, DeliveryStorage, InstallationStorage, RecordStorage,
};
use ghinvite_core::{
    audit::*,
    storage::{AuditPosition, projection::ProjectionStorage, settlement::Settlement},
    *,
};
use ghinvite_storage_sqlx::SqlxStorage;
use rand::SeedableRng;

async fn fixture() -> (SqlxStorage, GithubInvitation) {
    fixture_with(SqlxStorage::in_memory().await.unwrap()).await
}

async fn fixture_with(s: SqlxStorage) -> (SqlxStorage, GithubInvitation) {
    let at = "2026-09-17T12:00:00Z".parse().unwrap();
    s.insert_installation(&Account {
        installation_id: 9,
        account_id: 100,
        account_login: "acme".into(),
        account_type: AccountType::Organization,
        installed_at: at,
        uninstalled_at: None,
        selected_repos: SelectedRepos::All,
    })
    .await
    .unwrap();
    s.upsert_user(&User {
        user_id: 7,
        login: "alice".into(),
        avatar_url: None,
        last_seen_at: at,
    })
    .await
    .unwrap();
    let link = InvitationLink {
        id: InvitationLinkId::new(),
        slug: Slug::generate(&mut rand_chacha::ChaCha8Rng::seed_from_u64(1)),
        installation_id: 9,
        account_id: 100,
        created_by: 7,
        created_at: at,
        expires_at: None,
        max_uses: None,
        uses_count: 0,
        permission: Permission::Pull,
        approval_required: false,
        description: "Settlement".into(),
        internal_note: None,
        revoked_at: None,
        revoked_by: None,
        repos: vec![InvitationLinkRepo {
            repo_id: 10,
            repo_full_name: "acme/api".into(),
        }],
    };
    let request = InvitationRequest {
        id: RequestId::new(),
        invitation_link_id: link.id,
        requester_id: 7,
        justification: None,
        state: RequestState::Approved,
        decided_by: None,
        decided_at: Some(at),
        decline_reason: None,
        decision_deadline: None,
        created_at: at,
    };
    s.apply_transition(&storage::projection::fixture::envelope(
        &link,
        std::slice::from_ref(&request),
        1,
    ))
    .await
    .unwrap();
    let row = GithubInvitation {
        id: GithubInvitationId::new(),
        invitation_request_id: request.id,
        repo_id: 10,
        github_invitation_id: Some(9988),
        state: InvitationState::Sent,
        error_message: None,
        created_at: at,
        updated_at: at,
    };
    s.insert_github_invitation(&row).await.unwrap();
    (s, row)
}

fn transition(row: &GithubInvitation, state: InvitationState) -> Settlement {
    Settlement {
        expected: row.clone(),
        state,
        event: AuditEvent {
            id: AuditEventId::new(),
            account_id: 100,
            occurred_at: row.updated_at + chrono::Duration::hours(1),
            event_type: if state == InvitationState::Accepted {
                EventType::InvitationAccepted
            } else {
                EventType::InvitationCancelled
            },
            actor_kind: ActorKind::System,
            actor_id: None,
            target_kind: TargetKind::GithubInvitation,
            target_id: row.id.to_string(),
            metadata: serde_json::json!({"reconciled": true}),
            request_id: None,
        },
    }
}

#[tokio::test]
async fn accepted_settlement_survives_stale_sweep_and_acknowledgement_replay() {
    let (s, row) = fixture().await;
    let accepted = transition(&row, InvitationState::Accepted);
    s.settle_github_invitation(&accepted).await.unwrap();
    s.settle_github_invitation(&transition(&row, InvitationState::Cancelled))
        .await
        .unwrap();
    s.settle_github_invitation(&accepted).await.unwrap();
    let result = s.get_github_invitation(row.id).await.unwrap().unwrap();
    assert_eq!(result.state, InvitationState::Accepted);
    assert_eq!(result.github_invitation_id, Some(9988));
    let audit = s
        .list_audit_events(100, None, AuditPosition::Latest)
        .await
        .unwrap();
    assert_eq!(audit.events, vec![accepted.event]);
}

#[tokio::test]
async fn conflicting_audit_cannot_silently_omit_settlement_event() {
    let (s, row) = fixture().await;
    let accepted = transition(&row, InvitationState::Accepted);
    let mut conflict = accepted.event.clone();
    conflict.target_id = "different-invitation".into();
    s.audit(&conflict).await.unwrap();
    assert!(s.settle_github_invitation(&accepted).await.is_err());
    assert_eq!(
        s.get_github_invitation(row.id)
            .await
            .unwrap()
            .unwrap()
            .state,
        InvitationState::Sent
    );
}

#[tokio::test]
async fn late_writer_cannot_overwrite_committed_settlement() {
    let path = std::env::temp_dir().join(format!("settlement-fence-{}.sqlite", RequestId::new()));
    let (s, row) = fixture_with(SqlxStorage::at_path(&path).await.unwrap()).await;
    s.settle_github_invitation(&transition(&row, InvitationState::Accepted))
        .await
        .unwrap();
    // A delayed write from an older invocation, outside the settlement API.
    let db =
        sqlx::SqlitePool::connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(&path))
            .await
            .unwrap();
    assert!(
        sqlx::query("UPDATE github_invitations SET state = 'cancelled' WHERE id = ?1")
            .bind(row.id.to_string())
            .execute(&db)
            .await
            .is_err()
    );
    db.close().await;
    assert_eq!(
        s.get_github_invitation(row.id)
            .await
            .unwrap()
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
    assert!(s.settle_github_invitation(&accepted).await.is_err());
    assert_eq!(s.get_github_invitation(row.id).await.unwrap().unwrap(), row);
    s.debug_set_audit_failure(false).await.unwrap();
    s.settle_github_invitation(&accepted).await.unwrap();
    s.settle_github_invitation(&accepted).await.unwrap();
    assert_eq!(
        s.list_audit_events(100, None, AuditPosition::Latest)
            .await
            .unwrap()
            .events,
        vec![accepted.event]
    );
}
