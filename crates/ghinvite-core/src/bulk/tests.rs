use super::*;
use crate::targets::{MemberGrant, OrganizationTarget, RepoRole, RepoTarget, Targets};

fn now() -> DateTime<Utc> {
    "2026-10-17T10:00:00Z".parse().unwrap()
}
fn manifest(entries: Vec<Entry>) -> Manifest {
    Manifest {
        batch_id: BulkBatchId::new(),
        admin: crate::invitation_link::AccountAdmin {
            account_id: 42,
            user_id: 7,
        },
        installation_id: 5,
        organization_installation_id: None,
        targets: Targets {
            repos: vec![RepoTarget {
                repo_id: 10,
                repo_full_name: "acme/api".into(),
                grant: RepoRole::Write,
            }],
            ..Targets::default()
        },
        access_lifetime_seconds: None,
        entries,
    }
}
fn handle(login: &str) -> Entry {
    Entry::Handle {
        login: login.into(),
    }
}
fn evidence(row: u16, id: u64) -> RowEvidence {
    RowEvidence {
        previews: Vec::new(),
        row,
        resolution: Resolution::Found {
            user_id: id,
            login: format!("user-{id}"),
        },
        all_targets_satisfied: false,
        observed_at: now(),
        next_budget_slot: None,
    }
}
fn request(review: &Review, row: u16, id: u64) -> RowRequest {
    RowRequest {
        source: BatchRef {
            batch_id: review.manifest.batch_id,
            row,
        },
        request_id: crate::RequestId::new(),
        requester_id: id,
        requester_login: format!("user-{id}"),
    }
}
fn submit(requests: Vec<RowRequest>) -> Submission {
    Submission {
        submitted_by: 7,
        requests,
        emails: vec![],
        personal_links: vec![],
        submitted_at: now(),
    }
}

#[test]
fn cap_and_target_validation_precede_review_and_never_accept_a_fake_origin() {
    assert_eq!(
        manifest(vec![handle("alice"); 201]).normalize(),
        Err(Refusal::TooManyRows)
    );
    assert!(manifest(vec![handle("alice"); 200]).normalize().is_ok());
    assert_eq!(manifest(vec![]).normalize(), Err(Refusal::TooManyRows));
    assert_eq!(
        manifest(vec![handle("bad/name")]).normalize(),
        Err(Refusal::InvalidEntry)
    );
    assert_eq!(
        manifest(vec![Entry::Email {
            email_ref: crate::email_delivery::EmailRef::from_bytes([1; 32])
        }])
        .normalize(),
        Err(Refusal::InvalidEntry)
    );
    let source = BatchRef {
        batch_id: BulkBatchId::new(),
        row: 1,
    };
    assert!(validate_origin(None, Some(source)).is_ok());
    assert!(validate_origin(None, None).is_err());
    assert!(validate_origin(Some(crate::InvitationLinkId::new()), Some(source)).is_err());
    assert!(BatchRef { row: 0, ..source }.validate().is_err());
    assert!(BatchRef { row: 201, ..source }.validate().is_err());
}

#[test]
fn review_detects_numeric_aliases_case_duplicates_and_keeps_budget_and_role_evidence() {
    let mut rows = vec![
        evidence(1, 91),
        evidence(2, 91),
        evidence(3, 93),
        evidence(4, 94),
    ];
    rows[2].all_targets_satisfied = true;
    rows[3].next_budget_slot = Some(now() + chrono::Duration::hours(24));
    let review = Review::new(
        manifest(vec![
            handle(" @Alice "),
            handle("old-alias"),
            handle("bob"),
            handle("carol"),
        ]),
        rows,
        now(),
    )
    .unwrap();
    assert_eq!(review.manifest.entries[0], handle("Alice"));
    assert_eq!(review.rows[1].status, ReviewStatus::Duplicate { of_row: 1 });
    assert_eq!(review.rows[2].status, ReviewStatus::AlreadySatisfied);
    assert_eq!(
        review.rows[3].status,
        ReviewStatus::OverBudget {
            until: now() + chrono::Duration::hours(24)
        }
    );
    let duplicate = Review::new(
        manifest(vec![handle("ALICE"), handle("alice")]),
        vec![evidence(1, 91), evidence(2, 92)],
        now(),
    )
    .unwrap();
    assert_eq!(
        duplicate.rows[1].status,
        ReviewStatus::Duplicate { of_row: 1 }
    );
    let mut future = evidence(1, 91);
    future.observed_at += chrono::Duration::seconds(1);
    assert_eq!(
        Review::new(manifest(vec![handle("alice")]), vec![future], now()),
        Err(Refusal::InvalidReview)
    );
}

#[test]
fn removed_rows_never_allocate_requests_and_stale_review_submission_cannot_change_state() {
    let mut review = Review::new(
        manifest(vec![handle("alice"), handle("bob"), handle("carol")]),
        vec![evidence(1, 91), evidence(2, 92), evidence(3, 93)],
        now(),
    )
    .unwrap();
    review
        .edit(1, BTreeSet::from([2]), BTreeSet::new())
        .unwrap();
    let before = review.clone();
    let correct = submit(vec![request(&review, 1, 91), request(&review, 3, 93)]);
    assert_eq!(
        review.submit(1, correct.clone()),
        Err(Refusal::ReviewChanged)
    );
    assert_eq!(review, before);
    let extra = submit(vec![
        request(&review, 1, 91),
        request(&review, 2, 92),
        request(&review, 3, 93),
    ]);
    assert_eq!(review.submit(2, extra), Err(Refusal::InvalidSubmission));
    assert_eq!(review, before);
    review.submit(2, correct.clone()).unwrap();
    assert_eq!(review.submitted, Some(correct));
    assert_eq!(
        review.edit(2, BTreeSet::from([1]), BTreeSet::new()),
        Err(Refusal::AlreadySubmitted)
    );
}

#[test]
fn mandatory_review_binds_exact_numeric_users_and_distinct_request_ids() {
    let mut review = Review::new(
        manifest(vec![handle("alice"), handle("bob")]),
        vec![evidence(1, 91), evidence(2, 92)],
        now(),
    )
    .unwrap();
    let first = request(&review, 1, 91);
    let mut second = request(&review, 2, 92);
    second.request_id = first.request_id;
    assert_eq!(
        review.submit(1, submit(vec![first, second])),
        Err(Refusal::InvalidSubmission)
    );
    let wrong = request(&review, 1, 99);
    assert_eq!(
        review.submit(1, submit(vec![wrong, request(&review, 2, 92)])),
        Err(Refusal::InvalidSubmission)
    );
    assert!(review.submitted.is_none());
    let malformed = RowEvidence {
        previews: Vec::new(),
        resolution: Resolution::NotFound,
        ..evidence(1, 91)
    };
    let mut unresolved =
        Review::new(manifest(vec![handle("missing")]), vec![malformed], now()).unwrap();
    assert_eq!(
        unresolved.submit(1, submit(vec![])),
        Err(Refusal::UnresolvedRows)
    );
}

#[test]
fn mixed_email_rows_require_explicit_personal_link_and_exact_org_command() {
    let email_ref = crate::email_delivery::EmailRef::from_bytes([7; 32]);
    let mut input = manifest(vec![Entry::Email { email_ref }]);
    input.organization_installation_id = Some(6);
    input.targets.organization = Some(OrganizationTarget {
        org_id: 42,
        org_login: "acme".into(),
        grant: MemberGrant::Member,
    });
    let mut review = Review::new(
        input,
        vec![RowEvidence {
            previews: Vec::new(),
            resolution: Resolution::Email,
            ..evidence(1, 91)
        }],
        now(),
    )
    .unwrap();
    assert_eq!(review.rows[0].status, ReviewStatus::NeedsPersonalLink);
    let source = BatchRef {
        batch_id: review.manifest.batch_id,
        row: 1,
    };
    let email = crate::email_delivery::EmailCreateCommand {
        batch: source,
        account_id: 42,
        installation_id: 6,
        organization: review.manifest.targets.organization.clone().unwrap(),
        teams: vec![],
        email_ref,
        approved_by: 7,
        approved_at: now(),
    };
    let link = PersonalLink {
        source,
        link_id: crate::InvitationLinkId::new(),
        secret_sha256: crate::SecretHash::from_bytes([8; 32]),
    };
    let submission = Submission {
        submitted_by: 7,
        requests: vec![],
        emails: vec![email.clone()],
        personal_links: vec![link],
        submitted_at: now(),
    };
    assert_eq!(
        review.submit(1, submission.clone()),
        Err(Refusal::UnresolvedRows)
    );
    review
        .edit(1, BTreeSet::new(), BTreeSet::from([1]))
        .unwrap();
    let mut wrong = submission.clone();
    wrong.emails[0].installation_id = 5;
    assert_eq!(review.submit(2, wrong), Err(Refusal::InvalidSubmission));
    review.submit(2, submission).unwrap();
    let encoded = serde_json::to_string(&review).unwrap();
    assert!(!encoded.contains("@"));
    assert!(!encoded.contains("https://"));
    assert_eq!(serde_json::from_str::<Review>(&encoded).unwrap(), review);
}

#[test]
fn legacy_linked_origin_wire_is_unchanged_and_batch_source_is_explicit() {
    let id = crate::InvitationLinkId::new();
    let linked: crate::delivery_control::Identity=serde_json::from_value(serde_json::json!({"link_id":id,"request_id":crate::RequestId::new(),"kind":"repository","target_id":10,"delivery_generation":1})).unwrap();
    let value = serde_json::to_value(&linked).unwrap();
    assert_eq!(value["link_id"], serde_json::json!(id));
    assert!(value.get("batch").is_none());
    let batch = crate::delivery_control::Identity {
        link_id: None,
        batch: Some(BatchRef {
            batch_id: BulkBatchId::new(),
            row: 1,
        }),
        ..linked
    };
    let value = serde_json::to_value(&batch).unwrap();
    assert!(value.get("link_id").is_none());
    assert_eq!(value["batch"]["row"], 1);
}

#[test]
fn unavailable_resolution_and_incomplete_target_previews_block_submission() {
    let input = manifest(vec![handle("alice")]);
    let mut row = evidence(1, 8);
    row.resolution = Resolution::Unavailable;
    let review = Review::new(input.clone(), vec![row], now()).unwrap();
    assert_eq!(review.rows[0].status, ReviewStatus::Unavailable);
    let mut row = evidence(1, 8);
    row.previews = vec![crate::bulk_preview::TargetPreview {
        target: crate::bulk_preview::PreviewTarget::Repository { repo_id: 1 },
        observed: crate::bulk_preview::ObservedAccess::Unavailable,
        action: crate::bulk_preview::PreviewAction::Unavailable,
    }];
    let review = Review::new(input, vec![row], now()).unwrap();
    assert_eq!(review.rows[0].status, ReviewStatus::Unavailable);
}
