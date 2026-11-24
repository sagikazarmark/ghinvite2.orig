//! The request rules the authority and the web tests' fake authority share.
use crate::request_lifecycle::{
    DecisionAction, DecisionOutcome, PENDING_LIFETIME, RequestSnapshot, RequesterView,
    RevisionExhausted, TerminalDecision,
};
use crate::{InvitationLinkId, RequestCancellationReason, RequestId, RequestState};
use chrono::{DateTime, Duration, Utc};

const ADMIN: u64 = 7;

fn now() -> DateTime<Utc> {
    DateTime::from_timestamp(1_800_000_000, 0).unwrap()
}

fn pending(deadline: DateTime<Utc>) -> RequestSnapshot {
    RequestSnapshot {
        domain_evidence: None,
        request_id: RequestId::new(),
        link_id: Some(InvitationLinkId::new()),
        batch: None,
        account_id: 100,
        requester_id: 8,
        justification: None,
        state: RequestState::Pending,
        admitted_at: deadline - PENDING_LIFETIME,
        decision_deadline: Some(deadline),
        revision: 1,
        decision: None,
    }
}

fn decline(reason: Option<&str>) -> DecisionAction {
    DecisionAction::Decline {
        reason: reason.map(Into::into),
        requester_message: None,
    }
}

fn decided(action: &DecisionAction) -> RequestSnapshot {
    let mut request = pending(now() + Duration::days(1));
    assert_eq!(
        request.decide(ADMIN, action, now()),
        DecisionOutcome::Applied
    );
    request
}

#[test]
fn expiry_waits_for_the_deadline_and_is_effective_at_it() {
    let mut request = pending(now() + Duration::milliseconds(1));
    let before = request.clone();
    assert!(!request.expire_due(now()));
    assert_eq!(request, before);

    let deadline = now() - Duration::minutes(5);
    let mut request = pending(deadline);
    assert!(request.expire_due(now()));
    assert_eq!(request.state, RequestState::Expired);
    assert_eq!(request.revision, 2);
    assert_eq!(
        request.decision,
        Some(TerminalDecision {
            decision_id: format!("request.expired/{}", request.request_id),
            decided_by: None,
            effective_at: deadline,
            evaluated_at: now(),
            decline_reason: None,
            requester_message: None,
            cancellation_reason: None,
        })
    );
    assert!(!request.expire_due(now()), "a request expires once");
}

#[test]
fn approval_before_the_deadline_is_effective_now() {
    let request = decided(&DecisionAction::Approve);
    assert_eq!(request.state, RequestState::Approved);
    assert_eq!(request.revision, 2);
    assert_eq!(
        request.decision,
        Some(TerminalDecision {
            decision_id: format!("request.approved/{}", request.request_id),
            decided_by: Some(ADMIN),
            effective_at: now(),
            evaluated_at: now(),
            decline_reason: None,
            requester_message: None,
            cancellation_reason: None,
        })
    );
}

#[test]
fn decline_keeps_its_reason_and_requester_message() {
    let request = decided(&DecisionAction::Decline {
        reason: Some("Not a contributor".into()),
        requester_message: Some("Ask your lead.".into()),
    });
    assert_eq!(request.state, RequestState::Declined);
    let decision = request.decision.unwrap();
    assert_eq!(
        decision.decision_id,
        format!("request.declined/{}", request.request_id)
    );
    assert_eq!(decision.decided_by, Some(ADMIN));
    assert_eq!(
        decision.decline_reason.as_deref(),
        Some("Not a contributor")
    );
    assert_eq!(
        decision.requester_message.as_deref(),
        Some("Ask your lead.")
    );
}

#[test]
fn expiry_at_the_deadline_wins_over_a_late_decision() {
    let deadline = now() - Duration::minutes(5);
    let mut request = pending(deadline);
    assert_eq!(
        request.decide(ADMIN, &DecisionAction::Approve, now()),
        DecisionOutcome::Incompatible
    );
    assert_eq!(request.state, RequestState::Expired);
    let decision = request.decision.unwrap();
    assert_eq!(decision.decided_by, None);
    assert_eq!(decision.effective_at, deadline);
}

#[test]
fn the_same_decision_on_a_decided_request_is_already_completed() {
    for action in [DecisionAction::Approve, decline(Some("Not a contributor"))] {
        let mut request = decided(&action);
        let before = request.clone();
        assert_eq!(
            request.decide(ADMIN, &action, now()),
            DecisionOutcome::AlreadyCompleted,
            "{action:?}"
        );
        assert_eq!(request, before);
    }
}

#[test]
fn a_decision_the_request_did_not_take_is_incompatible() {
    let approved = decided(&DecisionAction::Approve);
    let declined = decided(&decline(Some("Not a contributor")));
    let mut expired = pending(now());
    assert!(expired.expire_due(now()));
    for (request, action) in [
        (&approved, decline(None)),
        (&declined, DecisionAction::Approve),
        // A decline under another reason is another decision.
        (&declined, decline(Some("Spam"))),
        (&declined, decline(None)),
        (&expired, DecisionAction::Approve),
    ] {
        assert_eq!(
            request.clone().decide(ADMIN, &action, now()),
            DecisionOutcome::Incompatible,
            "state={} action={action:?}",
            request.state
        );
    }
}

#[test]
fn a_revoked_link_cancels_a_pending_request_which_then_takes_no_decision() {
    let mut request = pending(now() + Duration::days(1));
    assert_eq!(request.cancel_for_revoked_link(now()), Ok(true));
    assert_eq!(request.state, RequestState::Cancelled);
    let decision = request.decision.clone().unwrap();
    assert_eq!(
        decision.decision_id,
        format!("request.cancelled/{}", request.request_id)
    );
    assert_eq!(decision.decided_by, None);
    assert_eq!(
        decision.cancellation_reason,
        Some(RequestCancellationReason::LinkRevoked)
    );
    for action in [DecisionAction::Approve, decline(None)] {
        assert_eq!(
            request.clone().decide(ADMIN, &action, now()),
            DecisionOutcome::NotPending
        );
    }
}

#[test]
fn a_revoked_link_expires_an_overdue_request_and_leaves_a_decided_one() {
    let mut overdue = pending(now());
    assert_eq!(overdue.cancel_for_revoked_link(now()), Ok(true));
    assert_eq!(overdue.state, RequestState::Expired);

    let mut approved = decided(&DecisionAction::Approve);
    let before = approved.clone();
    assert_eq!(approved.cancel_for_revoked_link(now()), Ok(false));
    assert_eq!(approved, before);

    // At the revision limit neither cascade transition applies: not the
    // cancellation, and not the expiry an overdue request would take.
    for deadline in [now() + Duration::days(1), now()] {
        let mut exhausted = pending(deadline);
        exhausted.revision = crate::storage::integer::MAX_EXACT;
        let before = exhausted.clone();
        assert_eq!(
            exhausted.cancel_for_revoked_link(now()),
            Err(RevisionExhausted),
            "deadline {deadline}"
        );
        assert_eq!(exhausted, before);
    }
}

#[test]
fn a_requester_never_sees_the_decline_reason() {
    let mut declined = decided(&DecisionAction::Decline {
        reason: Some("Not a contributor".into()),
        requester_message: Some("Ask your lead.".into()),
    });
    declined.domain_evidence = Some(crate::email_domain::DomainEvidence {
        link_id: declined.link_id.unwrap(),
        matched_domains: vec![],
        checked_at: now(),
    });

    let seen = declined.clone().requester_view();
    let decision = seen.decision.as_ref().unwrap();
    assert_eq!(decision.decline_reason, None);
    assert_eq!(
        decision.requester_message.as_deref(),
        Some("Ask your lead.")
    );
    assert_eq!(
        decision.decision_id,
        declined.decision.as_ref().unwrap().decision_id
    );
    assert_eq!(seen.domain_evidence, declined.domain_evidence);

    // Request-ID navigation holds no link capability.
    let view = RequesterView::new(declined, Default::default(), vec![], None);
    assert_eq!(view.request.domain_evidence, None);
    assert_eq!(view.request.decision.unwrap().decline_reason, None);
}

#[test]
fn a_decline_is_normalized_and_bounded() {
    let mut action = DecisionAction::Decline {
        reason: Some(" \n ".into()),
        requester_message: Some("  Try again later.  ".into()),
    };
    action.normalize().unwrap();
    assert_eq!(
        action,
        DecisionAction::Decline {
            reason: None,
            requester_message: Some("Try again later.".into()),
        }
    );
    assert!(decline(Some(&"x".repeat(16_384))).normalize().is_ok());
    assert_eq!(
        decline(Some(&"x".repeat(16_385))).normalize(),
        Err("invalid reason")
    );
}
