use ghinvite_core::storage::test_suite::scenario_request_lapsed;
use ghinvite_storage_sqlx::SqlxStorage;

#[tokio::test]
async fn raw_lapsed_writes_preserve_approval_and_terminal_state() {
    let path = std::env::temp_dir().join(format!(
        "ghinvite-lapsed-{}.sqlite",
        ghinvite_core::RequestId::new()
    ));
    let storage = SqlxStorage::at_path(&path).await.unwrap();
    scenario_request_lapsed(storage.clone()).await;
    let pool = sqlx::SqlitePool::connect(&format!("sqlite:{}", path.display()))
        .await
        .unwrap();
    let before: Vec<(String,String,i64,String)> = sqlx::query_as("SELECT id,state,projection_revision,projection_content FROM invitation_requests ORDER BY id").fetch_all(&pool).await.unwrap();
    let lapsed = before.iter().find(|row| row.1 == "lapsed").unwrap();
    let declined = before.iter().find(|row| row.1 == "declined").unwrap();
    for update in [
        "state='approved'",
        "state='pending'",
        "decided_by=701",
        "decided_at='2099-01-01T00:00:00Z'",
        "projection_revision=projection_revision-1",
        "projection_content=json_set(projection_content,'$.decision.decision_id','changed')",
    ] {
        let error = sqlx::query(&format!(
            "UPDATE invitation_requests SET {update} WHERE id=?"
        ))
        .bind(&lapsed.0)
        .execute(&pool)
        .await
        .unwrap_err();
        assert!(error.to_string().contains("invalid request lapse"));
        let after: Vec<(String,String,i64,String)> = sqlx::query_as("SELECT id,state,projection_revision,projection_content FROM invitation_requests ORDER BY id").fetch_all(&pool).await.unwrap();
        assert_eq!(before, after);
    }
    assert!(sqlx::query("UPDATE invitation_requests SET state='lapsed',projection_revision=projection_revision+1 WHERE id=?").bind(&declined.0).execute(&pool).await.unwrap_err().to_string().contains("invalid request lapse"));
    assert!(sqlx::query("INSERT INTO invitation_requests(id,invitation_link_id,requester_id,state,created_at,projection_revision) SELECT 'missing-approval',invitation_link_id,requester_id,'lapsed',created_at,3 FROM invitation_requests WHERE id=?").bind(&lapsed.0).execute(&pool).await.unwrap_err().to_string().contains("invalid request lapse"));
    // A late SQL failure must roll back the approval repair as well as lapse.
    use ghinvite_core::storage::{
        RecordStorage,
        projection::{
            ApprovedRequestProjection, AuditIntent, ProjectionStorage, RequestProjectionEnvelope,
        },
    };
    let mut snapshot: ghinvite_core::request_lifecycle::RequestSnapshot =
        serde_json::from_str(&lapsed.3).unwrap();
    snapshot.request_id = ghinvite_core::RequestId::new();
    snapshot.decision.as_mut().unwrap().decision_id = format!("approval/{}", snapshot.request_id);
    let mut approval = snapshot.clone();
    approval.state = ghinvite_core::RequestState::Approved;
    approval.revision = snapshot.revision - 1;
    let at = snapshot.decision.as_ref().unwrap().effective_at;
    let event = AuditIntent {
        event_id: format!("request.lapsed/{}", snapshot.request_id),
        kind: ghinvite_core::audit::EventType::RequestLapsed,
        actor_id: None,
        target_id: snapshot.request_id.to_string(),
        effective_at: at,
        evaluated_at: at,
    };
    sqlx::query("CREATE TRIGGER fail_lapse AFTER UPDATE ON invitation_requests WHEN NEW.state='lapsed' BEGIN SELECT RAISE(ABORT,'injected completion failure'); END").execute(&pool).await.unwrap();
    let envelope = RequestProjectionEnvelope {
        approval: Some(ApprovedRequestProjection {
            reissue: None,
            request: approval,
            events: vec![],
        }),
        request: snapshot,
        events: vec![event],
    };
    assert!(storage.apply_request(&envelope).await.is_err());
    assert!(
        storage
            .get_invitation_request(envelope.request.request_id)
            .await
            .unwrap()
            .is_none()
    );
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_events WHERE target_id=?")
        .bind(envelope.request.request_id.to_string())
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
    sqlx::query("DROP TRIGGER fail_lapse")
        .execute(&pool)
        .await
        .unwrap();
    storage.apply_request(&envelope).await.unwrap();
    assert_eq!(
        storage
            .get_invitation_request(envelope.request.request_id)
            .await
            .unwrap()
            .unwrap()
            .state,
        ghinvite_core::RequestState::Lapsed
    );
    pool.close().await;
    drop(storage);
    std::fs::remove_file(path).unwrap();
}
