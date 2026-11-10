use super::*;
use crate::request_lifecycle::TerminalDecision;
use crate::storage::projection::{AuditIntent, RequestProjectionEnvelope};

pub async fn scenario_requester_message<S: Storage + ProjectionStorage>(s: S) {
    s.put_app_installation(&crate::account::Installation::repo(sample_account(
        1, 9001, "acme",
    )))
    .await
    .unwrap();
    s.upsert_user(&sample_user(701, "requester")).await.unwrap();
    let link = sample_link(9001, 1, 701);
    let pending = sample_request(link.id, 701);
    seed(&s, &link, std::slice::from_ref(&pending), 1).await;
    let mut declined = fixture::request_snapshot(&link, &pending, 2);
    declined.state = RequestState::Declined;
    let at = pending.created_at + chrono::Duration::seconds(1);
    declined.decision = Some(TerminalDecision {
        decision_id: format!("request.declined/{}", pending.id),
        decided_by: Some(701),
        effective_at: at,
        evaluated_at: at,
        decline_reason: Some("Private administrative context".into()),
        requester_message: Some("Please contact the team.".into()),
        cancellation_reason: None,
    });
    let envelope = RequestProjectionEnvelope {
        approval: None,
        request: declined,
        events: vec![AuditIntent {
            event_id: format!("request.declined/{}", pending.id),
            kind: EventType::RequestDeclined,
            actor_id: Some(701),
            target_id: pending.id.to_string(),
            effective_at: at,
            evaluated_at: at,
        }],
    };
    for message in ["", " space ", "bad\0control", &"x".repeat(501)] {
        let mut invalid = envelope.clone();
        invalid.request.decision.as_mut().unwrap().requester_message = Some(message.into());
        assert!(s.apply_request(&invalid).await.is_err());
        assert_eq!(
            s.get_invitation_request(pending.id)
                .await
                .unwrap()
                .unwrap()
                .state,
            RequestState::Pending
        );
    }
    let mut wrong_state = envelope.clone();
    wrong_state.request.state = RequestState::Approved;
    assert!(s.apply_request(&wrong_state).await.is_err());
    for _ in 0..2 {
        s.apply_request(&envelope).await.unwrap();
    }
    let row = s.get_invitation_request(pending.id).await.unwrap().unwrap();
    assert_eq!(
        row.requester_message.as_deref(),
        Some("Please contact the team.")
    );
    assert_eq!(
        row.decline_reason.as_deref(),
        Some("Private administrative context")
    );
    assert_eq!(
        s.request_history(9001, link.id, None)
            .await
            .unwrap()
            .requests[0]
            .requester_message,
        row.requester_message
    );
    s.apply_request(&RequestProjectionEnvelope {
        approval: None,
        request: fixture::request_snapshot(&link, &pending, 1),
        events: vec![],
    })
    .await
    .unwrap();
    for message in [
        Some("replacement".into()),
        None,
        Some("x".repeat(501)),
        Some(" space ".into()),
    ] {
        let mut changed = envelope.clone();
        changed.request.revision += 1;
        changed.request.decision.as_mut().unwrap().requester_message = message;
        assert!(s.apply_request(&changed).await.is_err());
    }
    assert_eq!(
        s.get_invitation_request(pending.id)
            .await
            .unwrap()
            .unwrap()
            .requester_message,
        row.requester_message
    );
    assert_eq!(
        s.list_audit_events(
            9001,
            Some(EventType::RequestDeclined),
            super::super::AuditPosition::Latest
        )
        .await
        .unwrap()
        .events
        .len(),
        1
    );
    // No public message is inferred from a private legacy decision.
    let pending = sample_request(link.id, 701);
    seed(&s, &link, std::slice::from_ref(&pending), 1).await;
    let mut old = envelope;
    old.request = fixture::request_snapshot(&link, &pending, 2);
    old.request.state = RequestState::Declined;
    old.request.decision = Some(TerminalDecision {
        decision_id: format!("request.declined/{}", pending.id),
        decided_by: Some(701),
        effective_at: at,
        evaluated_at: at,
        decline_reason: Some("Historical secret".into()),
        requester_message: None,
        cancellation_reason: None,
    });
    old.events[0].event_id = format!("request.declined/{}", pending.id);
    old.events[0].target_id = pending.id.to_string();
    s.apply_request(&old).await.unwrap();
    assert_eq!(
        s.get_invitation_request(pending.id)
            .await
            .unwrap()
            .unwrap()
            .requester_message,
        None
    );
    old.request.revision += 1;
    old.request.decision.as_mut().unwrap().requester_message = Some("Historical secret".into());
    assert!(s.apply_request(&old).await.is_err());
}
