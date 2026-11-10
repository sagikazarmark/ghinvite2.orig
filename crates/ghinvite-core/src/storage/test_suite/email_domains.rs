use super::*;
use crate::email_domain::{AsciiDomain, DomainEvidence, DomainRule};
use crate::request_lifecycle::TerminalDecision;
use crate::storage::projection::{
    ApprovedRequestProjection, AuditIntent, RequestProjectionEnvelope,
};

pub async fn scenario_email_domains<S: Storage + ProjectionStorage>(s: S) {
    s.put_app_installation(&crate::account::Installation::repo(
        (sample_account(1, 9001, "acme")).clone(),
    ))
    .await
    .unwrap();
    s.upsert_user(&sample_user(701, "requester")).await.unwrap();
    s.upsert_user(&sample_user(702, "approver")).await.unwrap();
    let mut link = sample_link(9001, 1, 702);
    link.email_domains = vec![
        DomainRule::parse("EXAMPLE.COM.").unwrap(),
        DomainRule::parse("*.bücher.example").unwrap(),
    ];
    link.email_domains.sort();
    seed(&s, &link, &[], 1).await;
    assert_eq!(
        s.get_invitation_link_by_id(link.id).await.unwrap().unwrap(),
        link
    );
    let mut pending = sample_request(link.id, 701);
    pending.decision_deadline = Some(pending.created_at + chrono::Duration::days(7));
    let evidence = DomainEvidence {
        link_id: link.id,
        matched_domains: vec![
            AsciiDomain::parse("EXAMPLE.COM").unwrap(),
            AsciiDomain::parse("a.bücher.example").unwrap(),
        ],
        checked_at: pending.created_at - chrono::Duration::minutes(10),
    };
    pending.domain_evidence = Some(evidence.clone());
    let initial = fixture::request_snapshot(&link, &pending, 1);
    let initial_projection = RequestProjectionEnvelope {
        approval: None,
        request: initial.clone(),
        events: vec![],
    };
    s.apply_request(&initial_projection).await.unwrap();
    assert_eq!(
        s.get_invitation_request(pending.id).await.unwrap().unwrap(),
        pending
    );
    assert_eq!(
        s.request_history(9001, link.id, None)
            .await
            .unwrap()
            .requests[0]
            .domain_evidence,
        Some(evidence.clone())
    );
    assert!(
        s.request_history(9002, link.id, None)
            .await
            .unwrap()
            .requests
            .is_empty()
    );
    // Completion can arrive before approval; its original domain evidence remains
    // historical evidence, not a new ten-minute authorization check.
    let mut approved = initial.clone();
    approved.state = RequestState::Approved;
    approved.revision = 2;
    approved.decision = Some(TerminalDecision {
        decision_id: format!("approved/{}", pending.id),
        decided_by: Some(702),
        effective_at: pending.created_at,
        evaluated_at: pending.created_at,
        decline_reason: None,
        requester_message: None,
        cancellation_reason: None,
    });
    let mut lapsed = approved.clone();
    lapsed.state = RequestState::Lapsed;
    lapsed.revision = 3;
    let completion = pending.created_at + chrono::Duration::days(8);
    let projection = RequestProjectionEnvelope {
        approval: Some(ApprovedRequestProjection {
            reissue: None,
            request: approved,
            events: vec![],
        }),
        request: lapsed,
        events: vec![AuditIntent {
            event_id: format!("request.lapsed/{}", pending.id),
            kind: EventType::RequestLapsed,
            actor_id: None,
            target_id: pending.id.to_string(),
            effective_at: completion,
            evaluated_at: completion,
        }],
    };
    s.apply_request(&projection).await.unwrap();
    s.apply_request(&projection).await.unwrap();
    s.apply_request(&initial_projection).await.unwrap();
    let final_row = s.get_invitation_request(pending.id).await.unwrap().unwrap();
    assert_eq!(final_row.state, RequestState::Lapsed);
    assert_eq!(final_row.domain_evidence, Some(evidence.clone()));
    // Reject both invalid first admission evidence and attempts to rewrite a
    // valid historical proof, without leaving a new request behind.
    let mut bad: Vec<Option<DomainEvidence>> = vec![None];
    for mode in 0..8 {
        let mut proof = evidence.clone();
        match mode {
            0 => proof.link_id = InvitationLinkId::new(),
            1 => proof.checked_at = pending.created_at + chrono::Duration::nanoseconds(1),
            2 => proof.checked_at -= chrono::Duration::nanoseconds(1),
            3 => proof.matched_domains.clear(),
            4 => {
                proof.matched_domains =
                    vec![AsciiDomain::parse("users.noreply.github.com").unwrap()]
            }
            5 => proof.matched_domains = vec![AsciiDomain::parse("xn--bcher-kva.example").unwrap()],
            6 => proof
                .matched_domains
                .push(AsciiDomain::parse("evil-example.com").unwrap()),
            _ => proof.matched_domains = vec![AsciiDomain::parse("notexample.com").unwrap()],
        }
        bad.push(Some(proof));
    }
    for proof in bad {
        let mut request = initial.clone();
        request.request_id = RequestId::new();
        request.domain_evidence = proof;
        let id = request.request_id;
        assert!(
            s.apply_request(&RequestProjectionEnvelope {
                approval: None,
                request,
                events: vec![]
            })
            .await
            .is_err()
        );
        assert!(s.get_invitation_request(id).await.unwrap().is_none());
    }
    let mut changed = projection.clone();
    changed
        .request
        .domain_evidence
        .as_mut()
        .unwrap()
        .matched_domains
        .pop();
    changed.approval.as_mut().unwrap().request.domain_evidence =
        changed.request.domain_evidence.clone();
    assert!(s.apply_request(&changed).await.is_err());
    assert_eq!(
        s.get_invitation_request(pending.id).await.unwrap().unwrap(),
        final_row
    );
    let mut changed_link = link.clone();
    changed_link.email_domains.clear();
    assert!(
        s.apply_transition(&fixture::envelope(&changed_link, 2))
            .await
            .is_err()
    );
    // Unrestricted historical wire stays empty/None, including reads. Evidence
    // must not be smuggled into an unrestricted request projection.
    let unrestricted = sample_link(9001, 1, 702);
    seed(&s, &unrestricted, &[], 1).await;
    let unreq = sample_request(unrestricted.id, 701);
    let mut unshot = fixture::request_snapshot(&unrestricted, &unreq, 1);
    s.apply_request(&RequestProjectionEnvelope {
        approval: None,
        request: unshot.clone(),
        events: vec![],
    })
    .await
    .unwrap();
    assert!(
        s.get_invitation_request(unreq.id)
            .await
            .unwrap()
            .unwrap()
            .domain_evidence
            .is_none()
    );
    unshot.request_id = RequestId::new();
    let mut unproof = evidence;
    unproof.link_id = unrestricted.id;
    unproof.checked_at = unshot.admitted_at;
    unshot.domain_evidence = Some(unproof);
    assert!(
        s.apply_request(&RequestProjectionEnvelope {
            approval: None,
            request: unshot,
            events: vec![]
        })
        .await
        .is_err()
    );
}
