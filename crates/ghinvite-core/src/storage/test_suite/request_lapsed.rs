use super::*;
use crate::request_lifecycle::TerminalDecision;
use crate::storage::Error;
use crate::storage::projection::{
    ApprovedRequestProjection, AuditIntent, RequestProjectionEnvelope,
};

pub async fn scenario_request_lapsed<S: Storage + ProjectionStorage>(s: S) {
    s.put_app_installation(&crate::account::Installation::repo(
        (sample_account(1, 9001, "acme")).clone(),
    ))
    .await
    .unwrap();
    s.upsert_user(&sample_user(701, "requester")).await.unwrap();
    s.upsert_user(&sample_user(702, "approver")).await.unwrap();
    let link = sample_link(9001, 1, 702);
    seed(&s, &link, &[], 1).await;
    for initially_pending in [false, true] {
        let pending = sample_request(link.id, 701);
        let initial = fixture::request_snapshot(&link, &pending, 1);
        if initially_pending {
            s.apply_request(&RequestProjectionEnvelope {
                approval: None,
                request: initial.clone(),
                events: vec![],
            })
            .await
            .unwrap();
        }
        let approved_at = pending.created_at + chrono::Duration::seconds(1);
        let mut approved = initial.clone();
        approved.state = RequestState::Approved;
        approved.revision = 2;
        approved.decision = Some(TerminalDecision {
            decision_id: format!("request.approved/{}", pending.id),
            decided_by: Some(702),
            effective_at: approved_at,
            evaluated_at: approved_at,
            decline_reason: None,
            requester_message: None,
            cancellation_reason: None,
        });
        let approval = ApprovedRequestProjection {
            reissue: None,
            request: approved.clone(),
            events: vec![AuditIntent {
                event_id: approved.decision.as_ref().unwrap().decision_id.clone(),
                kind: EventType::RequestApproved,
                actor_id: Some(702),
                target_id: pending.id.to_string(),
                effective_at: approved_at,
                evaluated_at: approved_at,
            }],
        };
        let mut lapsed = approved.clone();
        lapsed.state = RequestState::Lapsed;
        lapsed.revision = 3;
        let completed_at = approved_at + chrono::Duration::days(7);
        let envelope = RequestProjectionEnvelope {
            approval: Some(approval.clone()),
            request: lapsed.clone(),
            events: vec![AuditIntent {
                event_id: format!("request.lapsed/{}", pending.id),
                kind: EventType::RequestLapsed,
                actor_id: None,
                target_id: pending.id.to_string(),
                effective_at: completed_at,
                evaluated_at: completed_at,
            }],
        };
        // Completion arrives before its original approval. Both snapshots and
        // their immutable audit facts are replayed in one atomic transaction.
        for _ in 0..3 {
            s.apply_request(&envelope).await.unwrap();
        }
        // Original approval and pending messages arriving later cannot reopen it.
        for request in [approved.clone(), initial] {
            s.apply_request(&RequestProjectionEnvelope {
                approval: None,
                request,
                events: vec![],
            })
            .await
            .unwrap();
        }
        let row = s.get_invitation_request(pending.id).await.unwrap().unwrap();
        assert_eq!(row.state, RequestState::Lapsed);
        assert_eq!(row.decided_at, Some(approved_at));
        assert_eq!(row.decided_by, Some(702));
        assert_eq!(row.cancellation_reason, None);
        assert_eq!(row.decline_reason, None);
        assert_eq!(
            s.request_history(9001, link.id, None)
                .await
                .unwrap()
                .requests
                .iter()
                .find(|r| r.id == pending.id)
                .unwrap(),
            &row
        );
        let audit = s
            .list_audit_events(
                9001,
                Some(EventType::RequestLapsed),
                super::super::AuditPosition::Latest,
            )
            .await
            .unwrap();
        let matching: Vec<_> = audit
            .events
            .iter()
            .filter(|event| event.target_id == pending.id.to_string())
            .collect();
        assert_eq!(matching.len(), 1);
        assert_eq!(matching[0].occurred_at, completed_at);
        assert_eq!(matching[0].actor_kind, ActorKind::System);
        let mut invalid = Vec::new();
        let mut missing = envelope.clone();
        missing.approval = None;
        invalid.push(missing);
        let mut pending_proof = envelope.clone();
        pending_proof.approval.as_mut().unwrap().request.state = RequestState::Pending;
        invalid.push(pending_proof);
        let mut replaced = envelope.clone();
        replaced.request.decision.as_mut().unwrap().decided_by = Some(701);
        invalid.push(replaced);
        let mut equal = envelope.clone();
        equal.request.revision = 2;
        invalid.push(equal);
        let mut earlier = envelope.clone();
        earlier.events[0].effective_at = approved_at - chrono::Duration::seconds(1);
        invalid.push(earlier);
        let mut actor = envelope.clone();
        actor.events[0].actor_id = Some(702);
        invalid.push(actor);
        let mut bad_id = envelope.clone();
        bad_id.events[0].event_id.push_str("/changed");
        invalid.push(bad_id);
        let mut changed_event = envelope.clone();
        changed_event.events[0].effective_at += chrono::Duration::seconds(1);
        changed_event.events[0].evaluated_at += chrono::Duration::seconds(1);
        invalid.push(changed_event);
        // Even changing both proof and current decision cannot rewrite retained SQL authority.
        let mut both = envelope.clone();
        both.request.revision = 4;
        both.request.decision.as_mut().unwrap().effective_at += chrono::Duration::seconds(1);
        both.approval.as_mut().unwrap().request.decision = both.request.decision.clone();
        invalid.push(both);
        for input in invalid {
            assert!(matches!(
                s.apply_request(&input).await,
                Err(Error::ProjectionInvariant(_))
            ));
            assert_eq!(
                s.get_invitation_request(pending.id).await.unwrap().unwrap(),
                row
            );
        }
        let mut reopen = approved;
        reopen.revision = 4;
        assert!(matches!(
            s.apply_request(&RequestProjectionEnvelope {
                approval: None,
                request: reopen,
                events: vec![]
            })
            .await,
            Err(Error::ProjectionInvariant(_))
        ));
    }
    // A differently completed request cannot be relabelled using a fabricated
    // approved predecessor, and no lapse audit is left behind on rollback.
    let pending = sample_request(link.id, 701);
    let mut declined = fixture::request_snapshot(&link, &pending, 2);
    declined.state = RequestState::Declined;
    declined.decision = Some(TerminalDecision {
        decision_id: format!("declined/{}", pending.id),
        decided_by: Some(702),
        effective_at: pending.created_at,
        evaluated_at: pending.created_at,
        decline_reason: Some("No".into()),
        requester_message: None,
        cancellation_reason: None,
    });
    s.apply_request(&RequestProjectionEnvelope {
        approval: None,
        request: declined.clone(),
        events: vec![],
    })
    .await
    .unwrap();
    let mut approved = declined.clone();
    approved.state = RequestState::Approved;
    approved.revision = 3;
    approved.decision.as_mut().unwrap().decline_reason = None;
    let mut lapsed = approved.clone();
    lapsed.state = RequestState::Lapsed;
    lapsed.revision = 4;
    assert!(matches!(
        s.apply_request(&RequestProjectionEnvelope {
            approval: Some(ApprovedRequestProjection {
                reissue: None,
                request: approved,
                events: vec![]
            }),
            request: lapsed,
            events: vec![AuditIntent {
                event_id: format!("request.lapsed/{}", pending.id),
                kind: EventType::RequestLapsed,
                actor_id: None,
                target_id: pending.id.to_string(),
                effective_at: pending.created_at,
                evaluated_at: pending.created_at
            }],
        })
        .await,
        Err(Error::ProjectionInvariant(_))
    ));
    assert_eq!(
        s.get_invitation_request(pending.id)
            .await
            .unwrap()
            .unwrap()
            .state,
        RequestState::Declined
    );
}
