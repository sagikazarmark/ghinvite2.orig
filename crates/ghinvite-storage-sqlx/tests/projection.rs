use ghinvite_core::storage::projection::{ProjectionEnvelope, ProjectionStorage};
use ghinvite_core::storage::{
    AuditPosition, ConsoleStorage, DeliveryStorage, InstallationStorage, RecordStorage,
};
use ghinvite_core::{Account, AccountType, SelectedRepos, User};
use ghinvite_storage_sqlx::SqlxStorage;
use serde_json::json;

fn envelope() -> ProjectionEnvelope {
    serde_json::from_value(json!({
        "transition_id": "link/01ARZ3NDEKTSV4RRFFQ69G5FAV/2",
        "link": {
            "link_id": "01ARZ3NDEKTSV4RRFFQ69G5FAV", "revision": 2, "uses": 1,
            "invitation_code": "abcdefghijklmnop", "created_at": "2026-09-14T00:00:00Z",
            "revoked_at": null, "revoked_by": null,
            "creation": {
                "link_id": "01ARZ3NDEKTSV4RRFFQ69G5FAV",
                "admin": {"account_id": 100, "user_id": 7}, "account_id": 100,
                "installation_id": 1, "description": "Workshop", "internal_note": null,
                "expires_at": null, "max_uses": 1, "permission": "pull",
                "approval_required": true,
                "repos": [{"repo_id": 10, "repo_full_name": "acme/api"}]
            }
        },
        "requests": [{
            "request_id": "01ARZ3NDEKTSV4RRFFQ69G5FAW", "link_id": "01ARZ3NDEKTSV4RRFFQ69G5FAV",
            "account_id": 100, "requester_id": 8, "justification": "Access please",
            "state": "pending", "admitted_at": "2026-09-14T01:00:00Z",
            "decision_deadline": "2026-09-21T01:00:00Z", "revision": 1
        }],
        "events": [{"event_id": "request.created/01ARZ3NDEKTSV4RRFFQ69G5FAW",
            "kind": "request.created", "actor_id": 8, "target_id": "01ARZ3NDEKTSV4RRFFQ69G5FAW",
            "effective_at": "2026-09-14T01:00:00Z", "evaluated_at": "2026-09-14T01:00:00Z"}]
    }))
    .unwrap()
}

async fn parents(storage: &SqlxStorage) {
    let now = "2026-09-14T00:00:00Z".parse().unwrap();
    storage
        .insert_installation(&Account {
            installation_id: 1,
            account_id: 100,
            account_login: "acme".into(),
            account_type: AccountType::Organization,
            installed_at: now,
            uninstalled_at: None,
            selected_repos: SelectedRepos::All,
        })
        .await
        .unwrap();
    for user_id in [7, 8] {
        storage
            .upsert_user(&User {
                user_id,
                login: format!("user{user_id}"),
                avatar_url: None,
                last_seen_at: now,
            })
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn create_audit_failure_rolls_back_and_retry_preserves_each_confirmed_outcome() {
    let path = std::env::temp_dir().join(format!(
        "delivery-audit-{}.db",
        ghinvite_core::RequestId::new()
    ));
    let storage = SqlxStorage::at_path(&path).await.unwrap();
    parents(&storage).await;
    let envelope = envelope();
    storage.apply_transition(&envelope).await.unwrap();
    let fault = sqlx::SqlitePool::connect(&format!("sqlite://{}", path.display()))
        .await
        .unwrap();
    for (outcome, kind, actor) in [
        (
            json!({"kind":"created","upstream_id":9876}),
            "invitation.sent",
            "system",
        ),
        (
            json!({"kind":"already_collaborator"}),
            "invitation.accepted",
            "github",
        ),
        (
            json!({"kind":"failed","status":422}),
            "invitation.send_failed",
            "system",
        ),
    ] {
        let id = ghinvite_core::GithubInvitationId::new();
        let receipt: ghinvite_core::delivery::CreateReceipt = serde_json::from_value(json!({
            "command":{"invitation_id":id,"link_id":envelope.link.link_id,"request_id":envelope.requests[0].request_id,
                "approval_id":"approval","account_id":100,"installation_id":1,"requester_id":8,"repo_id":10,
                "repo_full_name":"acme/api","permission":"pull","approved_at":"2026-09-14T01:00:00Z"},
            "revision":1,"outcome":outcome,"confirmed_at":"2026-09-14T02:00:00Z"
        })).unwrap();
        // An earlier unconfirmed observation is already projected.
        let unknown = ghinvite_core::delivery::CreateReceipt {
            outcome: ghinvite_core::delivery::CreateOutcome::OutcomeUnknown,
            confirmed_at: None,
            ..receipt.clone()
        };
        storage.project_delivery(&unknown).await.unwrap();
        let receipt = ghinvite_core::delivery::CreateReceipt {
            revision: 2,
            ..receipt
        };
        sqlx::query("CREATE TRIGGER fail_delivery_audit BEFORE INSERT ON audit_events WHEN NEW.target_kind='github_invitation' BEGIN SELECT RAISE(ABORT, 'fixture audit unavailable'); END").execute(&fault).await.unwrap();
        assert!(storage.project_delivery(&receipt).await.is_err());
        assert!(
            storage
                .list_delivery_for_request(receipt.command.request_id)
                .await
                .unwrap()
                .iter()
                .any(|row| row == &unknown)
        );
        assert!(
            storage
                .list_audit_events(100, Some(kind.parse().unwrap()), AuditPosition::Latest)
                .await
                .unwrap()
                .events
                .is_empty()
        );
        sqlx::query("DROP TRIGGER fail_delivery_audit")
            .execute(&fault)
            .await
            .unwrap();
        storage.project_delivery(&receipt).await.unwrap();
        let events = storage
            .list_audit_events(100, Some(kind.parse().unwrap()), AuditPosition::Latest)
            .await
            .unwrap()
            .events;
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].actor_kind.to_string(), actor);
        assert_eq!(events[0].occurred_at, receipt.confirmed_at.unwrap());
        storage.project_delivery(&receipt).await.unwrap();
        assert_eq!(
            storage
                .list_audit_events(100, Some(kind.parse().unwrap()), AuditPosition::Latest)
                .await
                .unwrap()
                .events,
            events
        );
    }
    fault.close().await;
    drop(storage);
    std::fs::remove_file(path).unwrap();
}

#[tokio::test]
async fn accepted_request_becomes_queryable_with_one_use_and_audit() {
    let storage = SqlxStorage::in_memory().await.unwrap();
    parents(&storage).await;
    let envelope = envelope();
    storage.apply_transition(&envelope).await.unwrap();
    let link = storage
        .get_invitation_link_by_id(envelope.link.link_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(link.uses_count, 1);
    assert_eq!(link.repos[0].repo_full_name, "acme/api");
    let pending = storage.pending_request_page(100, None).await.unwrap().rows;
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].justification.as_deref(), Some("Access please"));
    assert_eq!(
        pending[0].decision_deadline,
        Some("2026-09-21T01:00:00Z".parse().unwrap()),
        "the pending queue must expose the persisted decision deadline"
    );
    assert_eq!(
        storage
            .count_pending_requests_for_account(100)
            .await
            .unwrap(),
        1
    );
    for request in [
        storage
            .get_invitation_request(pending[0].request_id)
            .await
            .unwrap()
            .unwrap(),
        storage
            .request_history(100, link.id, None)
            .await
            .unwrap()
            .requests
            .remove(0),
    ] {
        assert_eq!(
            serde_json::to_value(request).unwrap()["decision_deadline"],
            json!("2026-09-21T01:00:00Z"),
            "all request reads must expose the persisted decision deadline"
        );
    }
    assert_eq!(
        storage
            .get_invitation_request(pending[0].request_id)
            .await
            .unwrap()
            .unwrap()
            .decision_deadline,
        Some("2026-09-21T01:00:00Z".parse().unwrap())
    );
    let events = storage
        .list_audit_events(100, None, AuditPosition::Latest)
        .await
        .unwrap();
    assert_eq!(events.events.len(), 1);
}

#[tokio::test]
async fn maximum_escaped_command_payload_remains_projectable() {
    let storage = SqlxStorage::in_memory().await.unwrap();
    parents(&storage).await;
    let mut input = envelope();
    input.link.creation.internal_note = Some("\u{0001}".repeat(16_384));
    input.requests[0].justification = Some("\u{0001}".repeat(16_384));
    storage.apply_transition(&input).await.unwrap();
    let request = storage
        .get_invitation_request(input.requests[0].request_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(request.justification, input.requests[0].justification);
}

#[tokio::test]
async fn restored_relational_identity_conflicts_are_not_hidden_by_snapshot_json() {
    // A separate connection represents an inconsistent restore/repair. Verify
    // results through public storage, not through inspection of private columns.
    let directory = std::env::temp_dir();
    let path = directory.join(format!(
        "ghinvite-projection-{}.sqlite",
        ghinvite_core::RequestId::new()
    ));
    let storage = SqlxStorage::at_path(&path).await.unwrap();
    parents(&storage).await;
    let input = envelope();
    storage.apply_transition(&input).await.unwrap();
    let pool = sqlx::SqlitePool::connect(&format!("sqlite:{}", path.display()))
        .await
        .unwrap();
    for (corrupt, repair) in [
        (
            "UPDATE invitation_links SET account_id = 101",
            "UPDATE invitation_links SET account_id = 100",
        ),
        (
            "UPDATE invitation_requests SET requester_id = 7",
            "UPDATE invitation_requests SET requester_id = 8",
        ),
        (
            "UPDATE invitation_link_repos SET repo_full_name = 'other/repo'",
            "UPDATE invitation_link_repos SET repo_full_name = 'acme/api'",
        ),
        (
            "UPDATE audit_events SET account_id = 101",
            "UPDATE audit_events SET account_id = 100",
        ),
    ] {
        sqlx::query(corrupt).execute(&pool).await.unwrap();
        assert!(
            matches!(
                storage.apply_transition(&input).await,
                Err(ghinvite_core::storage::Error::ProjectionInvariant(_))
            ),
            "{corrupt}"
        );
        sqlx::query(repair).execute(&pool).await.unwrap();
        storage.apply_transition(&input).await.unwrap();
    }
    assert_eq!(
        storage
            .list_invitation_links_for_account(100)
            .await
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        storage
            .list_audit_events(100, None, AuditPosition::Latest)
            .await
            .unwrap()
            .events
            .len(),
        1
    );
    pool.close().await;
    drop(storage);
    std::fs::remove_file(path).unwrap();
}

#[tokio::test]
async fn conflicting_duplicate_events_roll_back_the_entire_envelope() {
    let storage = SqlxStorage::in_memory().await.unwrap();
    parents(&storage).await;
    let mut input = envelope();
    let mut conflicting = input.events[0].clone();
    conflicting.evaluated_at += chrono::Duration::seconds(1);
    input.events.push(conflicting);
    assert!(matches!(
        storage.apply_transition(&input).await,
        Err(ghinvite_core::storage::Error::ProjectionInvariant(_))
    ));
    assert!(
        storage
            .list_invitation_links_for_account(100)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        storage
            .count_pending_requests_for_account(100)
            .await
            .unwrap(),
        0
    );
    assert!(
        storage
            .list_audit_events(100, None, AuditPosition::Latest)
            .await
            .unwrap()
            .events
            .is_empty()
    );
}

#[tokio::test]
async fn reordered_snapshots_keep_independent_revisions_and_historical_events() {
    let storage = SqlxStorage::in_memory().await.unwrap();
    parents(&storage).await;
    let old = envelope();
    let mut revoked = old.clone();
    revoked.link.revision = 3;
    revoked.link.revoked_at = Some("2026-09-14T02:00:00Z".parse().unwrap());
    revoked.link.revoked_by = Some(7);
    revoked.requests.clear();
    revoked.events.clear();
    storage.apply_transition(&revoked).await.unwrap();
    // A stale link must not suppress a missing request or historical event.
    storage.apply_transition(&old).await.unwrap();
    storage.apply_transition(&old).await.unwrap();
    let mut terminal = old.clone();
    terminal.requests[0].revision = 2;
    terminal.requests[0].state = ghinvite_core::RequestState::Expired;
    storage.apply_transition(&terminal).await.unwrap();
    storage.apply_transition(&old).await.unwrap();
    let link = storage
        .get_invitation_link_by_id(old.link.link_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(link.uses_count, 1);
    assert!(link.revoked_at.is_some());
    let requests = storage
        .request_history(100, old.link.link_id, None)
        .await
        .unwrap()
        .requests;
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].state, ghinvite_core::RequestState::Expired);
    assert_eq!(
        storage
            .list_audit_events(100, None, AuditPosition::Latest)
            .await
            .unwrap()
            .events
            .len(),
        1
    );
}

#[tokio::test]
async fn parent_repair_retries_without_overwriting_owned_facts() {
    let storage = SqlxStorage::in_memory().await.unwrap();
    let input = envelope();
    assert!(matches!(
        storage.apply_transition(&input).await,
        Err(ghinvite_core::storage::Error::ProjectionDependency)
    ));
    assert!(storage.get_user(7).await.unwrap().is_none());
    parents(&storage).await;
    let when = "2026-09-15T00:00:00Z".parse().unwrap();
    storage
        .mark_installation_uninstalled(1, when)
        .await
        .unwrap();
    storage
        .upsert_user(&User {
            user_id: 8,
            login: "renamed".into(),
            avatar_url: Some("https://example.org/avatar".into()),
            last_seen_at: when,
        })
        .await
        .unwrap();
    storage.apply_transition(&input).await.unwrap();
    assert_eq!(storage.get_user(8).await.unwrap().unwrap().login, "renamed");
    assert_eq!(
        storage
            .get_installation(1)
            .await
            .unwrap()
            .unwrap()
            .uninstalled_at,
        Some(when)
    );
    assert_eq!(
        storage
            .request_history(100, input.link.link_id, None)
            .await
            .unwrap()
            .requests
            .len(),
        1
    );
}

#[tokio::test]
async fn invariant_conflicts_are_observable_and_redrive_is_atomic() {
    let storage = SqlxStorage::in_memory().await.unwrap();
    parents(&storage).await;
    let input = envelope();
    storage.apply_transition(&input).await.unwrap();
    let mut conflicts = vec![];
    let mut changed = input.clone();
    changed.link.revision = 1; // stale immutable identity still conflicts
    changed.link.creation.account_id = 101;
    changed.link.creation.admin.account_id = 101;
    changed.requests.clear();
    changed.events.clear();
    conflicts.push(changed);
    let mut changed = input.clone();
    changed.link.revision = 3;
    changed.requests[0].revision = 2;
    changed.requests[0].requester_id = 7;
    conflicts.push(changed);
    let mut changed = input.clone();
    changed.link.revision = 3;
    changed.events[0].actor_id = Some(7);
    conflicts.push(changed);
    for changed in conflicts {
        assert!(matches!(
            storage.apply_transition(&changed).await,
            Err(ghinvite_core::storage::Error::ProjectionInvariant(_))
        ));
        assert_eq!(
            storage
                .get_invitation_link_by_id(input.link.link_id)
                .await
                .unwrap()
                .unwrap()
                .uses_count,
            1
        );
        assert_eq!(
            storage
                .count_pending_requests_for_account(100)
                .await
                .unwrap(),
            1
        );
    }
    storage.apply_transition(&input).await.unwrap();
    assert_eq!(
        storage
            .list_audit_events(100, None, AuditPosition::Latest)
            .await
            .unwrap()
            .events
            .len(),
        1
    );
}

#[tokio::test]
async fn reordered_readmission_does_not_require_old_request_projection_first() {
    let storage = SqlxStorage::in_memory().await.unwrap();
    parents(&storage).await;
    let old = envelope();
    let mut fresh = old.clone();
    fresh.link.revision = 4;
    fresh.link.uses = 2;
    fresh.requests[0].request_id = ghinvite_core::RequestId::new();
    fresh.events.clear();
    storage.apply_transition(&fresh).await.unwrap();
    storage.apply_transition(&old).await.unwrap();
    let mut expired = old.clone();
    expired.requests[0].state = ghinvite_core::RequestState::Expired;
    expired.requests[0].revision = 2;
    storage.apply_transition(&expired).await.unwrap();
    let pending = storage.pending_request_page(100, None).await.unwrap().rows;
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].request_id, fresh.requests[0].request_id);
    assert_eq!(
        storage
            .count_pending_requests_for_account(100)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        storage
            .get_invitation_link_by_id(old.link.link_id)
            .await
            .unwrap()
            .unwrap()
            .uses_count,
        2
    );
}
