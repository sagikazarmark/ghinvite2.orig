//! `FakeLinkAuthority` answers the web's real `LinkAuthority` the way the
//! `InvitationLink` object does (ADR 0008), including the failures web
//! recovery depends on: a failure before the command applied, and a command
//! that applied but whose acknowledgement was lost.

#[path = "common/link_authority.rs"]
mod fake;

use chrono::{Duration, Utc};
use fake::FakeLinkAuthority;
use ghinvite_core::request_lifecycle::{
    DecideRequest, DecisionAction, DecisionOutcome, LifecycleOperationId,
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

#[tokio::test]
async fn a_decision_past_the_deadline_expires_the_request() {
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
