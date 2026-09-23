//! `FakeLinkAuthority` answers the web's real `LinkAuthority` the way the
//! `InvitationLink` object does (ADR 0008), including the failures web
//! recovery depends on: a failure before the command applied, and a command
//! that applied but whose acknowledgement was lost. Requester attempts follow
//! the object's admission rules: an operation replays its receipt and is bound
//! to its first input, and a rejection is a final receipt.
//!
//! Every test here therefore exercises the web's half of the exchange — what
//! it sends and what it makes of the answer — never the rules themselves,
//! which live in `ghinvite-workflows` and are pinned by that crate's own
//! tests. A test name here that claimed a rule would hold whether or not the
//! rule did.

#[path = "common/link_authority.rs"]
mod fake;

use chrono::{Duration, Utc};
use fake::{Availability, FakeLinkAuthority};
use ghinvite_core::admission::{
    AdmissionOperationId, AdmissionReceipt, AdmissionResult, Admit, Rejection,
};
use ghinvite_core::delivery::{DispatchStage, RepositoryProgress};
use ghinvite_core::request_lifecycle::{
    DecideRequest, DecisionAction, DecisionOutcome, LifecycleOperationId, RequestStatus,
};
use ghinvite_core::storage::projection::{AccountAdmin, RequestSnapshot};
use ghinvite_core::{
    InvitationLink, InvitationLinkId, InvitationLinkRepo, Permission, RequestId, RequestState, Slug,
};
use ghinvite_web::LinkAuthority;
use ghinvite_web::link_authority::AuthorityError;

const ADMIN: AccountAdmin = AccountAdmin {
    account_id: 42,
    user_id: 42,
};

fn link() -> InvitationLink {
    InvitationLink {
        id: InvitationLinkId::new(),
        slug: Slug::from_string("FakeAuthority001".into()).unwrap(),
        installation_id: 77,
        account_id: ADMIN.account_id,
        created_by: ADMIN.user_id,
        created_at: Utc::now(),
        expires_at: None,
        max_uses: None,
        uses_count: 0,
        permission: Permission::Pull,
        approval_required: true,
        description: "Fake authority fixture".into(),
        internal_note: None,
        revoked_at: None,
        revoked_by: None,
        repos: vec![InvitationLinkRepo {
            repo_id: 10,
            repo_full_name: "octocat/api".into(),
        }],
    }
}

fn pending(link: &InvitationLink, deadline: chrono::DateTime<Utc>) -> RequestSnapshot {
    RequestSnapshot {
        request_id: RequestId::new(),
        link_id: link.id,
        account_id: link.account_id,
        requester_id: 99,
        justification: None,
        state: RequestState::Pending,
        admitted_at: deadline - Duration::days(7),
        decision_deadline: Some(deadline),
        revision: 1,
        decision: None,
    }
}

fn decide(request: &RequestSnapshot, action: DecisionAction) -> DecideRequest {
    DecideRequest {
        link_id: request.link_id,
        request_id: request.request_id,
        operation_id: LifecycleOperationId::try_from(RequestId::new().to_string()).unwrap(),
        admin: ADMIN,
        action,
    }
}

fn admin_command(link: &InvitationLink) -> ghinvite_core::admission::AdminLinkCommand {
    ghinvite_core::admission::AdminLinkCommand {
        link_id: link.id,
        admin: ADMIN,
    }
}

async fn authority_with(link: &InvitationLink) -> (FakeLinkAuthority, LinkAuthority) {
    let fake = FakeLinkAuthority::start().await;
    fake.seed_link(link);
    let authority = LinkAuthority::new(fake.client());
    (fake, authority)
}

#[tokio::test]
async fn a_decision_applies_once_and_its_operation_replays_the_receipt() {
    let link = link();
    let (fake, authority) = authority_with(&link).await;
    let request = pending(&link, Utc::now() + Duration::days(7));
    fake.seed_request(request.clone());
    let approve = decide(&request, DecisionAction::Approve);

    assert!(
        authority
            .decision_status(approve.clone())
            .await
            .unwrap()
            .is_none(),
        "nothing is retained before the decision"
    );
    let receipt = authority.decide(approve.clone()).await.unwrap();
    assert_eq!(receipt.outcome, DecisionOutcome::Applied);
    assert_eq!(receipt.request.state, RequestState::Approved);
    assert_eq!(receipt.request.revision, 2);
    assert_eq!(authority.decide(approve.clone()).await.unwrap(), receipt);
    assert_eq!(
        authority.decision_status(approve.clone()).await.unwrap(),
        Some(receipt)
    );
    assert_eq!(
        fake.request(request.request_id).unwrap().state,
        RequestState::Approved
    );

    // The operation is bound to its first input.
    let mut changed = approve.clone();
    changed.action = DecisionAction::Decline { reason: None };
    assert!(matches!(
        authority.decide(changed.clone()).await,
        Err(AuthorityError::Conflict)
    ));
    assert!(matches!(
        authority.decision_status(changed).await,
        Err(AuthorityError::Conflict)
    ));

    // A new operation on the decided request is already completed or
    // incompatible, never applied again.
    let again = authority
        .decide(decide(&request, DecisionAction::Approve))
        .await
        .unwrap();
    assert_eq!(again.outcome, DecisionOutcome::AlreadyCompleted);
    let opposite = authority
        .decide(decide(&request, DecisionAction::Decline { reason: None }))
        .await
        .unwrap();
    assert_eq!(opposite.outcome, DecisionOutcome::Incompatible);
    assert_eq!(opposite.request.state, RequestState::Approved);
}

/// The arbitration itself is `rules::transition_request`, pinned by
/// `expiry_at_the_deadline_wins_over_a_late_command`; what is checked here is
/// that the web reports the expiry the authority answers rather than the
/// approval it asked for.
#[tokio::test]
async fn a_decision_the_authority_expired_is_reported_incompatible() {
    let link = link();
    let (fake, authority) = authority_with(&link).await;
    let request = pending(&link, Utc::now() - Duration::hours(1));
    fake.seed_request(request.clone());
    let receipt = authority
        .decide(decide(&request, DecisionAction::Approve))
        .await
        .unwrap();
    assert_eq!(receipt.outcome, DecisionOutcome::Incompatible);
    assert_eq!(receipt.request.state, RequestState::Expired);
}

#[tokio::test]
async fn decisions_for_unknown_or_foreign_links_and_requests_are_missing() {
    let link = link();
    let (fake, authority) = authority_with(&link).await;
    let request = pending(&link, Utc::now() + Duration::days(7));
    // The request was never admitted.
    assert!(matches!(
        authority
            .decide(decide(&request, DecisionAction::Approve))
            .await,
        Err(AuthorityError::Missing)
    ));
    fake.seed_request(request.clone());
    let mut foreign = decide(&request, DecisionAction::Approve);
    foreign.admin.account_id = 7;
    assert!(matches!(
        authority.decide(foreign.clone()).await,
        Err(AuthorityError::Missing)
    ));
    assert!(matches!(
        authority.decision_status(foreign).await,
        Err(AuthorityError::Missing)
    ));
    let mut unknown = decide(&request, DecisionAction::Approve);
    unknown.link_id = InvitationLinkId::new();
    assert!(matches!(
        authority.decide(unknown).await,
        Err(AuthorityError::Missing)
    ));
}

#[tokio::test]
async fn a_lost_acknowledgement_leaves_the_outcome_unknown_after_applying() {
    let link = link();
    let (fake, authority) = authority_with(&link).await;
    fake.lose_acknowledgements("revoke");

    assert!(matches!(
        authority.revoke(admin_command(&link)).await,
        Err(AuthorityError::Unknown(_))
    ));
    let status = authority.link_status(admin_command(&link)).await.unwrap();
    assert!(status.revoked_at.is_some(), "the revocation applied");
    // A replay takes no effect, so its acknowledgement arrives.
    assert_eq!(
        authority.revoke(admin_command(&link)).await.unwrap(),
        status
    );
    assert_eq!(fake.applied(), ["revoke"]);
    assert_eq!(fake.calls(), ["revoke", "link_status", "revoke"]);
}

#[tokio::test]
async fn a_lost_decision_acknowledgement_is_readable_from_decision_status() {
    let link = link();
    let (fake, authority) = authority_with(&link).await;
    let request = pending(&link, Utc::now() + Duration::days(7));
    fake.seed_request(request.clone());
    fake.lose_acknowledgements("decide");
    let approve = decide(&request, DecisionAction::Approve);

    assert!(matches!(
        authority.decide(approve.clone()).await,
        Err(AuthorityError::Unknown(_))
    ));
    let receipt = authority.decision_status(approve.clone()).await.unwrap();
    assert_eq!(receipt.unwrap().outcome, DecisionOutcome::Applied);
    assert_eq!(fake.received::<DecideRequest>("decide"), [approve]);
}

#[tokio::test]
async fn a_one_shot_failure_answers_once_before_applying() {
    let link = link();
    let (fake, authority) = authority_with(&link).await;
    fake.fail_once("revoke", 503);

    assert!(matches!(
        authority.revoke(admin_command(&link)).await,
        Err(AuthorityError::Unknown(_))
    ));
    assert!(fake.link(link.id).unwrap().revoked_at.is_none());
    assert!(
        authority
            .revoke(admin_command(&link))
            .await
            .unwrap()
            .revoked_at
            .is_some()
    );
    assert_eq!(fake.applied(), ["revoke"]);
}

#[tokio::test]
async fn a_creation_replay_returns_the_original_receipt() {
    let link = link();
    let fake = FakeLinkAuthority::start().await;
    let authority = LinkAuthority::new(fake.client());
    let mut creation = fake::snapshot(&link).creation;
    // The authority canonicalises the repository scope before binding it.
    creation.repos.push(InvitationLinkRepo {
        repo_id: 5,
        repo_full_name: "octocat/web".into(),
    });
    let created = authority.create(creation.clone()).await.unwrap();
    assert_eq!(created.creation.repos[0].repo_id, 5);
    authority.revoke(admin_command(&link)).await.unwrap();
    assert_eq!(authority.create(creation).await.unwrap(), created);
}

fn admit(link: &InvitationLink, requester_id: u64, justification: Option<&str>) -> Admit {
    Admit {
        link_id: link.id,
        operation_id: AdmissionOperationId::try_from(RequestId::new().to_string()).unwrap(),
        requester_id,
        justification: justification.map(str::to_owned),
    }
}

fn accepted(receipt: &AdmissionReceipt) -> RequestId {
    match receipt.result {
        AdmissionResult::Accepted { request_id, .. } => request_id,
        AdmissionResult::Rejected { .. } => panic!("expected acceptance, got {receipt:?}"),
    }
}

fn rejection(receipt: &AdmissionReceipt) -> Rejection {
    match &receipt.result {
        AdmissionResult::Rejected { reason } => reason.clone(),
        AdmissionResult::Accepted { .. } => panic!("expected a rejection, got {receipt:?}"),
    }
}

#[tokio::test]
async fn an_attempt_admits_once_and_its_operation_replays_the_receipt() {
    let link = link();
    let (fake, authority) = authority_with(&link).await;
    let command = admit(&link, 99, Some("  ship it  "));

    let prepared = authority.prepare(command.clone()).await.unwrap();
    assert_eq!(prepared.input.justification.as_deref(), Some("ship it"));
    assert!(prepared.receipt.is_none());
    let receipt = authority.admit(command.clone()).await.unwrap();
    let request = fake.request(accepted(&receipt)).unwrap();
    assert_eq!(request.state, RequestState::Pending);
    assert_eq!(request.justification.as_deref(), Some("ship it"));
    assert_eq!(fake.link(link.id).unwrap().uses, 1);

    assert_eq!(authority.admit(command.clone()).await.unwrap(), receipt);
    assert_eq!(
        authority.prepare(command.clone()).await.unwrap().receipt,
        Some(receipt)
    );
    assert_eq!(fake.applied(), ["prepare_attempt", "admit"]);

    // The operation is bound to its first input, prepared or admitted.
    let mut changed = command.clone();
    changed.justification = Some("edited".into());
    assert!(matches!(
        authority.prepare(changed.clone()).await,
        Err(AuthorityError::Conflict)
    ));
    assert!(matches!(
        authority.admit(changed).await,
        Err(AuthorityError::Conflict)
    ));
    let prepared = admit(&link, 99, Some("prepared"));
    authority.prepare(prepared.clone()).await.unwrap();
    let mut changed = prepared;
    changed.justification = None;
    assert!(matches!(
        authority.admit(changed).await,
        Err(AuthorityError::Conflict)
    ));
}

#[tokio::test]
async fn rejections_are_final_receipts() {
    let link = link();
    let (fake, authority) = authority_with(&link).await;
    authority.admit(admit(&link, 99, None)).await.unwrap();

    // One pending or approved request per requester.
    let again = admit(&link, 99, None);
    let receipt = authority.admit(again.clone()).await.unwrap();
    assert_eq!(rejection(&receipt), Rejection::ExistingRequest);
    assert_eq!(authority.admit(again).await.unwrap(), receipt);

    fake.set_availability(
        link.account_id,
        Availability::Unavailable(Rejection::RepositoryUnavailable),
    );
    let receipt = authority.admit(admit(&link, 100, None)).await.unwrap();
    assert_eq!(rejection(&receipt), Rejection::RepositoryUnavailable);

    authority.revoke(admin_command(&link)).await.unwrap();
    let receipt = authority.admit(admit(&link, 101, None)).await.unwrap();
    assert_eq!(rejection(&receipt), Rejection::Revoked);
    assert_eq!(fake.link(link.id).unwrap().uses, 1);
}

#[tokio::test]
async fn unknown_availability_leaves_the_same_attempt_retryable() {
    let link = link();
    let (fake, authority) = authority_with(&link).await;
    fake.set_availability(link.account_id, Availability::Unknown);
    let command = admit(&link, 99, None);

    let error = authority.admit(command.clone()).await.unwrap_err();
    assert!(
        matches!(&error, AuthorityError::Unknown(failure) if failure.upstream_status() == Some(503)),
        "{error:?}"
    );
    assert!(fake.requests().is_empty(), "nothing was admitted");
    fake.set_availability(link.account_id, Availability::Available);
    accepted(&authority.admit(command).await.unwrap());
}

#[tokio::test]
async fn the_requester_page_recovers_the_latest_attempt_and_conceals_the_rest() {
    let mut link = link();
    link.approval_required = false;
    let (_fake, authority) = authority_with(&link).await;
    let code = link.slug.as_str();

    let page = authority.requester_page(code, 99, None).await.unwrap();
    assert!(page.can_start_fresh);
    assert!(page.attempt.is_none() && page.request.is_none());

    let command = admit(&link, 99, None);
    authority.prepare(command.clone()).await.unwrap();
    let page = authority.requester_page(code, 99, None).await.unwrap();
    assert_eq!(page.attempt.unwrap().input, command);
    let receipt = authority.admit(command.clone()).await.unwrap();
    let page = authority
        .requester_page(code, 99, Some(command.operation_id.clone()))
        .await
        .unwrap();
    assert_eq!(page.attempt.unwrap().receipt, Some(receipt.clone()));
    assert_eq!(page.request.unwrap().state, RequestState::Approved);
    assert!(!page.can_start_fresh, "the approved request blocks");

    // Another requester's attempt, or one never made, is not this one's.
    for (requester, operation) in [
        (100, Some(command.operation_id.clone())),
        (99, Some(admit(&link, 99, None).operation_id)),
    ] {
        assert!(matches!(
            authority.requester_page(code, requester, operation).await,
            Err(AuthorityError::Missing)
        ));
    }
    // A revoked link offers nothing to a requester without an attempt.
    authority.revoke(admin_command(&link)).await.unwrap();
    assert!(matches!(
        authority.requester_page(code, 100, None).await,
        Err(AuthorityError::Missing)
    ));
    let page = authority.requester_page(code, 99, None).await.unwrap();
    assert_eq!(page.request.unwrap().request_id, accepted(&receipt));
}

/// The redaction itself is `rules::requester_view`, pinned by
/// `a_requester_never_sees_the_decline_reason`; what is checked here is that
/// the web carries the authority's declined answer through to the page as it
/// was given, redaction included, and stops blocking on it.
#[tokio::test]
async fn a_declined_request_reaches_the_requester_page_without_a_reason() {
    let link = link();
    let (fake, authority) = authority_with(&link).await;
    let command = admit(&link, 99, None);
    let request = accepted(&authority.admit(command).await.unwrap());
    let pending = fake.request(request).unwrap();
    authority
        .decide(decide(
            &pending,
            DecisionAction::Decline {
                reason: Some("Private admin context".into()),
            },
        ))
        .await
        .unwrap();

    let page = authority
        .requester_page(link.slug.as_str(), 99, None)
        .await
        .unwrap();
    let request = page.request.unwrap();
    assert_eq!(request.state, RequestState::Declined);
    assert_eq!(request.decision.unwrap().decline_reason, None);
    assert!(page.can_start_fresh, "a declined request no longer blocks");
}

#[tokio::test]
async fn delivery_progress_covers_an_approved_request_in_scope_order() {
    let mut link = link();
    link.approval_required = false;
    let (fake, authority) = authority_with(&link).await;
    let request = accepted(&authority.admit(admit(&link, 99, None)).await.unwrap());
    let query = |requester_id| RequestStatus {
        link_id: link.id,
        request_id: request,
        requester_id,
    };

    assert_eq!(
        authority.delivery_progress(query(99)).await.unwrap(),
        [RepositoryProgress {
            repo_id: 10,
            stage: DispatchStage::Approved
        }]
    );
    let planned = vec![RepositoryProgress {
        repo_id: 10,
        stage: DispatchStage::Planned,
    }];
    fake.set_delivery_progress(request, planned.clone());
    assert_eq!(
        authority.delivery_progress(query(99)).await.unwrap(),
        planned
    );
    assert!(matches!(
        authority.delivery_progress(query(100)).await,
        Err(AuthorityError::Missing)
    ));

    let pending = pending(&link, Utc::now() + Duration::days(7));
    fake.seed_request(pending.clone());
    let progress = authority
        .delivery_progress(RequestStatus {
            link_id: link.id,
            request_id: pending.request_id,
            requester_id: pending.requester_id,
        })
        .await
        .unwrap();
    assert!(progress.is_empty(), "only an approved request is delivered");
}

#[tokio::test]
async fn attempts_without_a_requester_or_with_oversized_input_are_refused() {
    let link = link();
    let (fake, authority) = authority_with(&link).await;
    let nobody = admit(&link, 0, None);
    assert!(matches!(
        authority.prepare(nobody.clone()).await,
        Err(AuthorityError::Invalid)
    ));
    assert!(matches!(
        authority.admit(nobody).await,
        Err(AuthorityError::Missing)
    ));
    let oversized = admit(&link, 99, Some(&"x".repeat(16_385)));
    assert!(matches!(
        authority.prepare(oversized.clone()).await,
        Err(AuthorityError::Invalid)
    ));
    assert!(matches!(
        authority.admit(oversized).await,
        Err(AuthorityError::Invalid)
    ));
    assert!(fake.applied().is_empty());
}

#[tokio::test]
async fn a_seeded_admission_is_what_preparing_it_again_answers() {
    let link = link();
    let (fake, authority) = authority_with(&link).await;
    let command = admit(&link, 99, Some("Native request"));
    let receipt = AdmissionReceipt {
        decided_at: Utc::now(),
        result: AdmissionResult::Rejected {
            reason: Rejection::ExistingRequest,
        },
    };
    fake.seed_admission(command.clone(), receipt.clone());
    let attempt = authority.prepare(command).await.unwrap();
    assert_eq!(attempt.receipt, Some(receipt));
    assert!(fake.applied().is_empty());
}
