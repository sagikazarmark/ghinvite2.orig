use super::super::projection::{AuditIntent, RequestProjectionEnvelope};
use super::*;
use crate::request_lifecycle::TerminalDecision;
use crate::storage::Error;

pub async fn scenario_request_cancellation<S: Storage + ProjectionStorage>(s: S) {
    s.put_app_installation(&crate::account::Installation::repo(
        (sample_account(1, 9001, "acme")).clone(),
    ))
    .await
    .unwrap();
    s.upsert_user(&sample_user(701, "requester")).await.unwrap();
    let link = sample_link(9001, 1, 701);
    seed(&s, &link, &[], 1).await;
    let pending = sample_request(link.id, 701);
    seed(&s, &link, std::slice::from_ref(&pending), 1).await;
    let mut cancelled = fixture::request_snapshot(&link, &pending, 2);
    cancelled.state = RequestState::Cancelled;
    let at = pending.created_at + chrono::Duration::seconds(1);
    cancelled.decision = Some(TerminalDecision {
        decision_id: format!("request.cancelled/{}", pending.id),
        decided_by: None,
        effective_at: at,
        evaluated_at: at,
        decline_reason: None,
        requester_message: None,
        cancellation_reason: Some(crate::RequestCancellationReason::LinkRevoked),
    });
    let envelope = RequestProjectionEnvelope {
        approval: None,
        request: cancelled.clone(),
        events: vec![AuditIntent {
            event_id: format!("request.cancelled/{}", pending.id),
            kind: EventType::RequestCancelled,
            actor_id: None,
            target_id: pending.id.to_string(),
            effective_at: at,
            evaluated_at: at,
        }],
    };
    for _ in 0..3 {
        s.apply_request(&envelope).await.unwrap();
    }
    // A delayed pending projection cannot reopen cancellation.
    s.apply_request(&RequestProjectionEnvelope {
        approval: None,
        request: fixture::request_snapshot(&link, &pending, 1),
        events: vec![],
    })
    .await
    .unwrap();
    let row = s.get_invitation_request(pending.id).await.unwrap().unwrap();
    assert_eq!(row.state, RequestState::Cancelled);
    assert_eq!(
        row.cancellation_reason,
        Some(crate::RequestCancellationReason::LinkRevoked)
    );
    assert_eq!(row.decline_reason, None);
    assert_eq!(row.decision_deadline, cancelled.decision_deadline);
    let events = s
        .list_audit_events(
            9001,
            Some(EventType::RequestCancelled),
            super::super::AuditPosition::Latest,
        )
        .await
        .unwrap()
        .events;
    assert_eq!(events.len(), 1);
    assert_eq!(
        events[0].metadata,
        serde_json::json!({"reason":"link_revoked"})
    );
    assert_eq!(events[0].actor_kind, ActorKind::System);
    let history = s.request_history(9001, link.id, None).await.unwrap();
    assert_eq!(
        history.requests[0].cancellation_reason,
        row.cancellation_reason
    );
    let mut missing_reason = envelope.clone();
    missing_reason
        .request
        .decision
        .as_mut()
        .unwrap()
        .cancellation_reason = None;
    assert!(matches!(
        s.apply_request(&missing_reason).await,
        Err(Error::ProjectionInvariant(_))
    ));
    let mut wrong_state = envelope.clone();
    wrong_state.request.state = RequestState::Declined;
    assert!(matches!(
        s.apply_request(&wrong_state).await,
        Err(Error::ProjectionInvariant(_))
    ));
    let mut replace = envelope.clone();
    replace.request.revision += 1;
    replace.request.state = RequestState::Approved;
    replace
        .request
        .decision
        .as_mut()
        .unwrap()
        .cancellation_reason = None;
    replace.events.clear();
    assert!(matches!(
        s.apply_request(&replace).await,
        Err(Error::ProjectionInvariant(_))
    ));
    let mut changed_reason = envelope;
    changed_reason.request.revision += 1;
    changed_reason
        .request
        .decision
        .as_mut()
        .unwrap()
        .effective_at += chrono::Duration::seconds(1);
    changed_reason.events.clear();
    assert!(matches!(
        s.apply_request(&changed_reason).await,
        Err(Error::ProjectionInvariant(_))
    ));
    assert_eq!(
        s.get_invitation_request(pending.id).await.unwrap(),
        Some(row)
    );
}

pub async fn scenario_revoke_receipt<S: Storage + ProjectionStorage>(s: S) {
    use crate::revocation::*;
    s.put_app_installation(&crate::account::Installation::repo(
        (sample_account(1, 9001, "acme")).clone(),
    ))
    .await
    .unwrap();
    s.upsert_user(&sample_user(701, "owner")).await.unwrap();
    let mut link = sample_link(9001, 1, 701);
    let pending = sample_request(link.id, 701);
    seed(&s, &link, std::slice::from_ref(&pending), 1).await;
    assert_eq!(
        s.revoke_preview(9001, link.id).await.unwrap(),
        Some(RevokePreview {
            pending_requests: 1,
            pending_invitations: 0
        })
    );
    assert_eq!(s.revoke_preview(9002, link.id).await.unwrap(), None);
    let at = link.created_at + chrono::Duration::hours(1);
    link.revoked_at = Some(at);
    link.revoked_by = Some(701);
    s.apply_transition(&fixture::envelope(&link, 2))
        .await
        .unwrap();
    let projection = RevocationProjection {
        command: RevokeCommand {
            link_id: link.id,
            admin: crate::invitation_link::AccountAdmin {
                account_id: 9001,
                user_id: 701,
            },
            operation_id: link.id.to_string().try_into().unwrap(),
            cascade: RevokeCascade {
                decline_pending_requests: true,
                ..Default::default()
            },
        },
        accepted: RevokeAccepted {
            addressed_access_grants: 0,
            addressed_requests: 1,
            addressed_deliveries: 0,
        },
        revision: 2,
        at,
    };
    for _ in 0..3 {
        s.apply_revocation(&projection).await.unwrap();
    }
    let events = s
        .list_audit_events(
            9001,
            Some(EventType::InvitationLinkRevokeCascadeRequested),
            super::super::AuditPosition::Latest,
        )
        .await
        .unwrap()
        .events;
    assert_eq!(events.len(), 1);
    assert_eq!(
        events[0].metadata,
        serde_json::json!({"options":projection.command.cascade,"addressed_requests":1,"addressed_deliveries":0})
    );
    let mut changed = projection.clone();
    changed.accepted.addressed_requests = 2;
    assert!(matches!(
        s.apply_revocation(&changed).await,
        Err(Error::ProjectionInvariant(_))
    ));
    let mut changed = projection.clone();
    changed.command.cascade.decline_pending_requests = false;
    assert!(matches!(
        s.apply_revocation(&changed).await,
        Err(Error::ProjectionInvariant(_))
    ));
    let mut changed = projection.clone();
    changed.command.operation_id = crate::RequestId::new().to_string().try_into().unwrap();
    assert!(matches!(
        s.apply_revocation(&changed).await,
        Err(Error::ProjectionInvariant(_))
    ));
    s.apply_revocation(&projection).await.unwrap();
    assert_eq!(
        s.get_invitation_request(pending.id)
            .await
            .unwrap()
            .unwrap()
            .state,
        RequestState::Pending,
        "revocation projection never decides a request"
    );
}
