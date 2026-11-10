use super::*;
use crate::{
    delivery::CreateCommand,
    reissue::*,
    reissue_projection::*,
    request_lifecycle::TerminalDecision,
    storage::projection::{ApprovedRequestProjection, AuditIntent, RequestProjectionEnvelope},
};

pub async fn scenario_request_reissue<S: Storage + ProjectionStorage>(s: S) {
    s.put_app_installation(&crate::account::Installation::repo(
        (sample_account(1, 9001, "acme")).clone(),
    ))
    .await
    .unwrap();
    s.upsert_user(&sample_user(701, "requester")).await.unwrap();
    s.upsert_user(&sample_user(702, "admin")).await.unwrap();
    let link = sample_link(9001, 1, 702);
    seed(&s, &link, &[], 1).await;
    for (reversed, legacy) in [(false, false), (true, false), (false, true), (true, true)] {
        let request = sample_request(link.id, 701);
        let mut original = fixture::request_snapshot(&link, &request, 2);
        original.state = RequestState::Approved;
        let at = request.created_at + chrono::Duration::seconds(1);
        original.decision = Some(TerminalDecision {
            decision_id: format!("approval/{}", request.id),
            decided_by: Some(702),
            effective_at: at,
            evaluated_at: at,
            decline_reason: None,
            requester_message: None,
            cancellation_reason: None,
        });
        let old = CreateCommand {
            budget_attribution: Default::default(),
            access_lifetime_seconds: None,
            budget_share_pct: 50,
            invitation_id: legacy.then(GithubInvitationId::new),
            delivery_generation: 1,
            link_id: Some(link.id),
            batch: None,
            request_id: request.id,
            approval_id: original.decision.as_ref().unwrap().decision_id.clone(),
            account_id: 9001,
            installation_id: 1,
            requester_id: 701,
            repo_id: 10,
            repo_full_name: "acme/api".into(),
            permission: Permission::Pull,
            approved_at: at,
        };
        let mut first = original.clone();
        first.state = RequestState::Lapsed;
        first.revision = 3;
        let first_lapse = RequestProjectionEnvelope {
            approval: Some(ApprovedRequestProjection {
                reissue: None,
                request: original.clone(),
                events: vec![],
            }),
            request: first.clone(),
            events: vec![lapse(&first, at)],
        };
        let mut after = first.clone();
        after.state = RequestState::Approved;
        after.revision = 4;
        let mut next = old.clone();
        next.delivery_generation = 2;
        next.invitation_id = legacy.then(GithubInvitationId::new);
        next.budget_attribution = BudgetAttribution::AdministratorReissue;
        let proof = RequestReissueProjection {
            command: ReissueCommand {
                operation_id: RequestId::new().to_string().try_into().unwrap(),
                request_id: request.id,
                admin: crate::invitation_link::AccountAdmin {
                    account_id: 9001,
                    user_id: 702,
                },
                target: ReissueTarget::Repository { repo_id: 10 },
            },
            before: first,
            after: after.clone(),
            original_approval: original.clone(),
            approval_events: vec![],
            delivery: ReissuedDelivery::Repository {
                before: old,
                after: next,
            },
            at,
        };
        let mut final_snapshot = after.clone();
        final_snapshot.state = RequestState::Lapsed;
        final_snapshot.revision = 5;
        let final_lapse = RequestProjectionEnvelope {
            approval: Some(ApprovedRequestProjection {
                reissue: Some(Box::new(proof.clone())),
                request: after,
                events: vec![],
            }),
            request: final_snapshot.clone(),
            events: vec![lapse(&final_snapshot, at + chrono::Duration::days(7))],
        };
        if reversed {
            s.apply_request(&final_lapse).await.unwrap();
        } else {
            s.apply_request(&first_lapse).await.unwrap();
        }
        s.apply_reissue(&proof).await.unwrap();
        if !reversed {
            assert_eq!(
                s.get_invitation_request(request.id)
                    .await
                    .unwrap()
                    .unwrap()
                    .state,
                RequestState::Approved
            );
        }
        s.apply_request(&final_lapse).await.unwrap();
        // Delayed previous epoch snapshots and repeated exact operation cannot reopen.
        for _ in 0..2 {
            s.apply_reissue(&proof).await.unwrap();
            s.apply_request(&first_lapse).await.unwrap();
        }
        assert_eq!(
            s.get_invitation_request(request.id)
                .await
                .unwrap()
                .unwrap()
                .state,
            RequestState::Lapsed
        );
        let mut conflict = proof.clone();
        conflict.at += chrono::Duration::seconds(1);
        assert!(s.apply_reissue(&conflict).await.is_err());
        let mut wrong = proof.clone();
        wrong.after.decision.as_mut().unwrap().decided_by = Some(701);
        assert!(s.apply_reissue(&wrong).await.is_err());
        let mut wrong = proof.clone();
        if let ReissuedDelivery::Repository { after, .. } = &mut wrong.delivery {
            after.permission = Permission::Admin;
        }
        assert!(s.apply_reissue(&wrong).await.is_err());
        // Epoch three can arrive before epoch two's standalone projection.
        let mut later = proof.clone();
        later.command.operation_id = RequestId::new().to_string().try_into().unwrap();
        later.before = final_snapshot;
        later.after = later.before.clone();
        later.after.state = RequestState::Approved;
        later.after.revision = 6;
        if let ReissuedDelivery::Repository { before, after } = &mut later.delivery {
            *before = after.clone();
            after.delivery_generation = 3;
            after.invitation_id = legacy.then(GithubInvitationId::new);
        }
        s.apply_reissue(&later).await.unwrap();
        s.apply_request(&final_lapse).await.unwrap();
        s.apply_reissue(&proof).await.unwrap();
        let row = s.get_invitation_request(request.id).await.unwrap().unwrap();
        assert_eq!(row.state, RequestState::Approved);
        assert_eq!(row.decided_by, Some(702));
        assert_eq!(row.decided_at, Some(at));
    }
}
fn lapse(
    snapshot: &crate::request_lifecycle::RequestSnapshot,
    at: chrono::DateTime<chrono::Utc>,
) -> AuditIntent {
    AuditIntent {
        event_id: format!(
            "request.lapsed/{}/{}",
            snapshot.request_id, snapshot.revision
        ),
        kind: EventType::RequestLapsed,
        actor_id: None,
        target_id: snapshot.request_id.to_string(),
        effective_at: at,
        evaluated_at: at,
    }
}
