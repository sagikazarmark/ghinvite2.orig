//! Actual interrupted batch outbox recovery after an earlier child retires its mailbox.
use super::*;
use ghinvite_core::{
    bulk::{Entry, Manifest, Resolution, Review, RowEvidence, Submission},
    bulk_commands::{BatchQuery, CancelBatch, CreateBatch, SubmitBatch},
    invitation_link::AccountAdmin,
    storage::RecordStorage,
    targets::Targets,
};
fn operation() -> ghinvite_core::request_lifecycle::LifecycleOperationId {
    ghinvite_core::RequestId::new()
        .to_string()
        .try_into()
        .unwrap()
}
async fn batch(f: &Fixture, staged_at: Option<i64>) -> (SubmitBatch, Vec<EmailCreateCommand>) {
    f.storage
        .upsert_user(&ghinvite_core::User {
            user_id: 7,
            login: "creator".into(),
            avatar_url: None,
            github_created_at: None,
            last_seen_at: Utc::now(),
        })
        .await
        .unwrap();
    let first = EmailCreateCommand {
        batch: BatchRef {
            batch_id: BulkBatchId::new(),
            row: 1,
        },
        account_id: 100,
        installation_id: 2,
        organization: OrganizationTarget {
            org_id: 100,
            org_login: "acme".into(),
            grant: MemberGrant::Member,
        },
        teams: vec![],
        email_ref: EmailRef::from_bytes([8; 32]),
        approved_by: 7,
        approved_at: Utc::now(),
    };
    let mut second = first.clone();
    second.batch.row = 2;
    second.email_ref = EmailRef::from_bytes([9; 32]);
    let emails = vec![first.clone(), second];
    if let Some(at) = staged_at {
        for command in &emails {
            let address = if command.batch.row == 1 {
                MAIL
            } else {
                "BULK60_EMAIL_PRIVACY_MARKER+2@example.com"
            };
            f.vault
                .stage(
                    &binding(command),
                    &ghinvite_email_vault::Address::parse(address).unwrap(),
                    at,
                )
                .await
                .unwrap();
        }
    }
    let query = BatchQuery {
        batch_id: first.batch.batch_id,
        admin: AccountAdmin {
            account_id: 100,
            user_id: 7,
        },
    };
    let review = Review::new(
        Manifest {
            batch_id: query.batch_id,
            admin: query.admin.clone(),
            installation_id: 2,
            organization_installation_id: None,
            targets: Targets {
                organization: Some(first.organization.clone()),
                ..Default::default()
            },
            access_lifetime_seconds: None,
            entries: emails
                .iter()
                .map(|c| Entry::Email {
                    email_ref: c.email_ref,
                })
                .collect(),
        },
        emails
            .iter()
            .map(|c| RowEvidence {
                row: c.batch.row,
                resolution: Resolution::Email,
                all_targets_satisfied: false,
                observed_at: first.approved_at,
                next_budget_slot: None,
                previews: vec![],
            })
            .collect(),
        first.approved_at,
    )
    .unwrap();
    let created =
        f.h.restate
            .invoke(
                &Call::object("BulkInviteBatch", &query.batch_id.to_string(), "create"),
                Some(&json!(CreateBatch {
                    operation_id: operation(),
                    review
                })),
                None,
            )
            .await;
    assert_eq!(created.status, 200, "{created:?}");
    (
        SubmitBatch {
            query,
            operation_id: operation(),
            expected_revision: 1,
            submission: Submission {
                submitted_by: 7,
                requests: vec![],
                emails: emails.clone(),
                personal_links: vec![],
                submitted_at: first.approved_at,
            },
        },
        emails,
    )
}
fn binding(c: &EmailCreateCommand) -> EmailVaultBinding {
    EmailVaultBinding {
        email_ref: c.email_ref,
        batch: c.batch,
        account_id: c.account_id,
        installation_id: c.installation_id,
    }
}
async fn retired(
    f: &Fixture,
    command: &EmailCreateCommand,
) -> ghinvite_core::storage::email_vault::EmailVaultRecord {
    tokio::time::timeout(std::time::Duration::from_secs(15), async {
        loop {
            let row = f
                .storage
                .email_record(command.email_ref)
                .await
                .unwrap()
                .unwrap();
            if row.retired_at.is_some() {
                break row;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("first email must finish while later activation is unavailable")
}
#[tokio::test]
async fn killed_batch_submit_recovers_after_first_email_retirement_without_reactivating_it() {
    let f = Fixture::new(201).await;
    let (submit, emails) = batch(&f, Some(Utc::now().timestamp())).await;
    f.storage
        .debug_set_email_activation_failure(Some(2))
        .await
        .unwrap();
    let key = submit.query.batch_id.to_string();
    let pending =
        f.h.restate
            .invoke(
                &Call::object("BulkInviteBatch", &key, "submit").send(),
                Some(&json!(submit)),
                None,
            )
            .await;
    assert_eq!(pending.status, 202, "{pending:?}");
    let original = retired(&f, &emails[0]).await;
    assert_eq!(f.posts(), 1);
    f.h.restate.admin().kill(pending.invocation_id()).await;
    f.h.restate
        .admin()
        .await_status(pending.invocation_id(), &["completed"])
        .await;
    f.storage
        .debug_set_email_activation_failure(None)
        .await
        .unwrap();
    let replay = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        f.h.restate.invoke(
            &Call::object("BulkInviteBatch", &key, "submit"),
            Some(&json!(submit)),
            None,
        ),
    )
    .await
    .expect("exact Submit must not retry activation of the retired first email forever");
    assert_eq!(replay.status, 200, "{replay:?}");
    retired(&f, &emails[1]).await;
    assert_eq!(f.posts(), 2);
    assert_eq!(
        f.storage
            .email_record(emails[0].email_ref)
            .await
            .unwrap()
            .unwrap(),
        original
    );
    let again =
        f.h.restate
            .invoke(
                &Call::object("BulkInviteBatch", &key, "submit"),
                Some(&json!(submit)),
                None,
            )
            .await;
    assert_eq!(again.body, replay.body);
    let cancelled =
        f.h.restate
            .invoke(
                &Call::object("BulkInviteBatch", &key, "cancel_remaining"),
                Some(&json!(CancelBatch {
                    query: submit.query.clone(),
                    operation_id: operation()
                })),
                None,
            )
            .await;
    assert_eq!(cancelled.status, 200, "{cancelled:?}");
    assert_eq!(cancelled.body["already_written"], 2);
    for id in [
        replay.invocation_id(),
        again.invocation_id(),
        cancelled.invocation_id(),
    ] {
        for entry in f.h.restate.admin().journal(id).await {
            assert!(!entry.raw_contains(MAIL));
            assert!(!entry.raw_contains("BULK60_EMAIL_PRIVACY_MARKER"));
            assert!(!entry.raw_contains("ghs_EMAIL_TOKEN_MARKER"));
        }
    }
}

#[tokio::test]
async fn exact_completed_email_owner_recovers_without_ciphertext_and_mismatch_is_refused() {
    for mismatch in [false, true] {
        let f = Fixture::new(201).await;
        let (submit, emails) = batch(&f, Some(Utc::now().timestamp())).await;
        let mut owned = emails[0].clone();
        if mismatch {
            owned.approved_by = 8;
        }
        assert!(
            f.storage
                .activate_email(&binding(&owned), Utc::now().timestamp())
                .await
                .unwrap()
        );
        // Keep the unrelated second recovery row active while exercising the
        // first row's terminal-plus-horizon purge.
        assert!(
            f.storage
                .activate_email(&binding(&emails[1]), Utc::now().timestamp())
                .await
                .unwrap()
        );
        let original = f.create(&owned).await;
        let row = retired(&f, &owned).await;
        assert_eq!(
            f.storage
                .prune_emails(row.purge_after.unwrap())
                .await
                .unwrap(),
            1
        );
        assert!(
            f.storage
                .email_record(owned.email_ref)
                .await
                .unwrap()
                .unwrap()
                .ciphertext
                .is_none()
        );
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            f.h.restate.invoke(
                &Call::object(
                    "BulkInviteBatch",
                    &submit.query.batch_id.to_string(),
                    "submit",
                ),
                Some(&json!(submit)),
                None,
            ),
        )
        .await
        .expect("known exact owner must not require retired ciphertext");
        assert_eq!(
            result.status,
            if mismatch { 409 } else { 200 },
            "{result:?}"
        );
        if !mismatch {
            retired(&f, &emails[1]).await;
        }
        assert_eq!(f.posts(), if mismatch { 1 } else { 2 });
        let current =
            f.h.restate
                .invoke(
                    &Call::object("EmailInviteDelivery", &owned.delivery_key(), "status"),
                    None::<&Value>,
                    None,
                )
                .await;
        assert_eq!(current.body, original);
        assert!(
            f.storage
                .email_record(owned.email_ref)
                .await
                .unwrap()
                .unwrap()
                .ciphertext
                .is_none()
        );
    }
}

#[tokio::test]
async fn missing_and_expired_email_recovery_handoff_blocked_without_fence_and_cancel_remains_available()
 {
    for staged_at in [None, Some(Utc::now().timestamp() - 86_401)] {
        let f = Fixture::new(201).await;
        let (submit, emails) = batch(&f, staged_at).await;
        let key = submit.query.batch_id.to_string();
        let accepted = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            f.h.restate.invoke(
                &Call::object("BulkInviteBatch", &key, "submit"),
                Some(&json!(submit)),
                None,
            ),
        )
        .await
        .expect("definite unavailable activation must not stall accepted batch");
        assert_eq!(accepted.status, 200, "{accepted:?}");
        for email in &emails {
            tokio::time::timeout(std::time::Duration::from_secs(10), async {
                loop {
                    let status = f
                        .h
                        .restate
                        .invoke(
                            &Call::object("EmailInviteDelivery", &email.delivery_key(), "status"),
                            None::<&Value>,
                            None,
                        )
                        .await;
                    if status.body["outcome"]
                        == json!({"kind":"blocked", "reason":"email_recovery_unavailable"})
                    {
                        break;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                }
            })
            .await
            .expect("owner must retain precise recovery-unavailable result");
            assert!(
                !f.storage
                    .email_attempt_exists(&ghinvite_core::email_delivery::EmailAttempt {
                        create: email.clone()
                    })
                    .await
                    .unwrap()
            );
        }
        assert_eq!(f.posts(), 0);
        let replay =
            f.h.restate
                .invoke(
                    &Call::object("BulkInviteBatch", &key, "submit"),
                    Some(&json!(submit)),
                    None,
                )
                .await;
        assert_eq!(replay.body, accepted.body);
        let cancelled =
            f.h.restate
                .invoke(
                    &Call::object("BulkInviteBatch", &key, "cancel_remaining"),
                    Some(&json!(CancelBatch {
                        query: submit.query.clone(),
                        operation_id: operation()
                    })),
                    None,
                )
                .await;
        assert_eq!(cancelled.status, 200, "{cancelled:?}");
        assert_eq!(cancelled.body["aborted"], 2);
        assert_eq!(f.posts(), 0);
    }
}
