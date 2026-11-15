use async_trait::async_trait;
use ghinvite_core::{
    bulk::{BatchRef, BulkBatchId},
    email_delivery::{EmailCreateCommand, EmailRef},
    storage::{
        self,
        email_review::{EmailReviewStorage, NewReviewEmail, ReviewEmailRecord},
        email_vault::{EmailVaultBinding, EmailVaultStorage},
    },
    targets::{MemberGrant, OrganizationTarget},
};
use ghinvite_email_vault::{Address, EmailVault, Error, ReviewEmail};
use ghinvite_storage_sqlx::SqlxStorage;
use kekring::Keyring;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
const NOW: i64 = 1_800_000_000;
const MAIL: &str = "REVIEW_ESCROW_PRIVATE_MARKER+tag@example.com";
fn ring(generation: u32) -> Arc<Keyring> {
    Arc::new(
        Keyring::builder("p1-email")
            .generation(generation, [generation as u8; 32])
            .current(generation)
            .build()
            .unwrap(),
    )
}
fn binding(batch: BulkBatchId, row: u16) -> EmailVaultBinding {
    let mut bytes = [0; 32];
    bytes[..2].copy_from_slice(&row.to_be_bytes());
    EmailVaultBinding {
        email_ref: EmailRef::from_bytes(bytes),
        batch: BatchRef {
            batch_id: batch,
            row,
        },
        account_id: 42,
        installation_id: 9,
    }
}
fn command(b: &EmailVaultBinding) -> EmailCreateCommand {
    EmailCreateCommand {
        batch: b.batch,
        account_id: b.account_id,
        installation_id: b.installation_id,
        email_ref: b.email_ref,
        organization: OrganizationTarget {
            org_id: b.account_id,
            org_login: "acme".into(),
            grant: MemberGrant::Member,
        },
        teams: vec![],
        approved_by: 43,
        approved_at: chrono::DateTime::from_timestamp(NOW, 0).unwrap(),
    }
}
fn vault(s: Arc<SqlxStorage>, k: Arc<Keyring>) -> EmailVault {
    EmailVault::new(s.clone(), k).with_review_storage(s)
}
async fn retain(v: &EmailVault, b: &EmailVaultBinding, now: i64) {
    let a = Address::parse(MAIL).unwrap();
    v.retain_review(
        &[ReviewEmail {
            binding: b,
            address: &a,
        }],
        now,
    )
    .await
    .unwrap();
}
#[tokio::test]
async fn another_authorized_actor_stages_selected_refs_without_browser_context_or_expiry_extension()
{
    let s = Arc::new(SqlxStorage::in_memory().await.unwrap());
    let creator = vault(s.clone(), ring(1));
    let other = vault(s.clone(), ring(1));
    let batch = BulkBatchId::new();
    let first = binding(batch, 1);
    let removed = binding(batch, 2);
    retain(&creator, &first, NOW).await;
    retain(&creator, &removed, NOW).await;
    assert!(s.email_record(first.email_ref).await.unwrap().is_none());
    let original = s.review_email(first.email_ref).await.unwrap().unwrap();
    assert_eq!(original.expires_at, NOW + 3600);
    assert!(!original.ciphertext.as_ref().unwrap().contains(MAIL));
    assert!(!format!("{original:?}").contains(MAIL));
    let c = command(&first);
    other
        .stage_reviewed(std::slice::from_ref(&c), NOW + 30)
        .await
        .unwrap();
    let staged = s.email_record(first.email_ref).await.unwrap().unwrap();
    assert_eq!(staged.created_at, NOW + 30);
    assert!(staged.activated_at.is_none());
    assert!(s.email_record(removed.email_ref).await.unwrap().is_none());
    other.stage_reviewed(&[c], NOW + 60).await.unwrap();
    retain(&creator, &first, NOW + 60).await;
    assert_eq!(
        s.email_record(first.email_ref).await.unwrap().unwrap(),
        staged
    );
    assert_eq!(
        s.review_email(first.email_ref).await.unwrap().unwrap(),
        original
    );
}
#[tokio::test]
async fn entire_selection_decrypts_before_first_write_and_wrong_context_never_stages() {
    let s = Arc::new(SqlxStorage::in_memory().await.unwrap());
    let v = vault(s.clone(), ring(1));
    let batch = BulkBatchId::new();
    let good = binding(batch, 1);
    let expired = binding(batch, 2);
    retain(&v, &good, NOW).await;
    retain(&v, &expired, NOW - 3600).await;
    assert_eq!(
        v.stage_reviewed(&[command(&good), command(&expired)], NOW)
            .await,
        Err(Error::Expired)
    );
    assert!(s.email_record(good.email_ref).await.unwrap().is_none());
    for kind in 0..5 {
        let mut c = command(&good);
        match kind {
            0 => c.batch.batch_id = BulkBatchId::new(),
            1 => c.batch.row = 3,
            2 => {
                c.account_id = 44;
                c.organization.org_id = 44
            }
            3 => c.installation_id = 10,
            _ => c.approved_by = 0,
        };
        assert!(v.stage_reviewed(&[command(&good), c], NOW).await.is_err());
        assert!(s.email_record(good.email_ref).await.unwrap().is_none());
    }
}
#[tokio::test]
async fn rollout_configuration_and_ciphertext_purpose_context_failures_preserve_review() {
    let s = Arc::new(SqlxStorage::in_memory().await.unwrap());
    let writer = vault(s.clone(), ring(2));
    let batch = BulkBatchId::new();
    let b = binding(batch, 1);
    retain(&writer, &b, NOW).await;
    let original = s.review_email(b.email_ref).await.unwrap().unwrap();
    assert_eq!(
        vault(s.clone(), ring(1))
            .stage_reviewed(&[command(&b)], NOW)
            .await,
        Err(Error::Unavailable)
    );
    let wrong = Arc::new(
        Keyring::builder("p1-email")
            .generation(2, [99; 32])
            .current(2)
            .build()
            .unwrap(),
    );
    assert_eq!(
        vault(s.clone(), wrong)
            .stage_reviewed(&[command(&b)], NOW)
            .await,
        Err(Error::Unavailable)
    );
    assert_eq!(
        s.review_email(b.email_ref).await.unwrap().unwrap(),
        original
    );
    assert!(s.email_record(b.email_ref).await.unwrap().is_none());
    let copied = binding(batch, 2);
    s.retain_review_email(&NewReviewEmail {
        binding: copied.clone(),
        ciphertext: original.ciphertext.unwrap(),
        created_at: NOW,
    })
    .await
    .unwrap();
    assert_eq!(
        writer.stage_reviewed(&[command(&copied)], NOW).await,
        Err(Error::Unavailable)
    );
    let delivery = binding(batch, 3);
    writer
        .stage(&delivery, &Address::parse(MAIL).unwrap(), NOW)
        .await
        .unwrap();
    let raw = s.email_record(delivery.email_ref).await.unwrap().unwrap();
    s.retain_review_email(&NewReviewEmail {
        binding: delivery.clone(),
        ciphertext: raw.ciphertext.unwrap(),
        created_at: NOW,
    })
    .await
    .unwrap();
    assert_eq!(
        writer.stage_reviewed(&[command(&delivery)], NOW).await,
        Err(Error::Unavailable)
    );
    writer.stage_reviewed(&[command(&b)], NOW).await.unwrap();
}
struct LostAck {
    inner: Arc<SqlxStorage>,
    lose: AtomicBool,
}
#[async_trait]
impl EmailReviewStorage for LostAck {
    async fn retain_review_email(&self, r: &NewReviewEmail) -> storage::Result<bool> {
        let result = self.inner.retain_review_email(r).await?;
        if self.lose.swap(false, Ordering::SeqCst) {
            Err(storage::Error::Database(
                "injected committed acknowledgement loss".into(),
            ))
        } else {
            Ok(result)
        }
    }
    async fn review_email(&self, r: EmailRef) -> storage::Result<Option<ReviewEmailRecord>> {
        self.inner.review_email(r).await
    }
    async fn clear_review_email(&self, b: &EmailVaultBinding, n: i64) -> storage::Result<bool> {
        self.inner.clear_review_email(b, n).await
    }
    async fn prune_review_emails(&self, n: i64) -> storage::Result<u64> {
        self.inner.prune_review_emails(n).await
    }
}
#[tokio::test]
async fn committed_ack_loss_and_concurrent_retention_keep_original_identity_and_deadline() {
    let s = Arc::new(SqlxStorage::in_memory().await.unwrap());
    let v = EmailVault::new(s.clone(), ring(1)).with_review_storage(Arc::new(LostAck {
        inner: s.clone(),
        lose: AtomicBool::new(true),
    }));
    let b = binding(BulkBatchId::new(), 1);
    let a = Address::parse(MAIL).unwrap();
    assert_eq!(
        v.retain_review(
            &[ReviewEmail {
                binding: &b,
                address: &a
            }],
            NOW
        )
        .await,
        Err(Error::Unavailable)
    );
    let original = s.review_email(b.email_ref).await.unwrap().unwrap();
    let v1 = vault(s.clone(), ring(1));
    let v2 = vault(s.clone(), ring(1));
    let input = [ReviewEmail {
        binding: &b,
        address: &a,
    }];
    let (r1, r2) = tokio::join!(
        v1.retain_review(&input, NOW + 1),
        v2.retain_review(&input, NOW + 2)
    );
    r1.unwrap();
    r2.unwrap();
    assert_eq!(
        s.review_email(b.email_ref).await.unwrap().unwrap(),
        original
    );
    let changed = Address::parse("other@example.com").unwrap();
    assert_eq!(
        v.retain_review(
            &[ReviewEmail {
                binding: &b,
                address: &changed
            }],
            NOW + 3
        )
        .await,
        Err(Error::Conflict)
    );
    let mut other = b.clone();
    other.email_ref = EmailRef::from_bytes([88; 32]);
    assert_eq!(
        v.retain_review(
            &[ReviewEmail {
                binding: &other,
                address: &a
            }],
            NOW + 3
        )
        .await,
        Err(Error::Conflict)
    );
}
#[tokio::test]
async fn removed_retirement_is_exact_idempotent_and_never_changes_activated_delivery() {
    let s = Arc::new(SqlxStorage::in_memory().await.unwrap());
    let v = vault(s.clone(), ring(1));
    let b = binding(BulkBatchId::new(), 1);
    retain(&v, &b, NOW).await;
    v.stage_reviewed(&[command(&b)], NOW).await.unwrap();
    s.activate_email(&b, NOW).await.unwrap();
    let delivery = s.email_record(b.email_ref).await.unwrap().unwrap();
    let mut wrong = b.clone();
    wrong.installation_id += 1;
    assert_eq!(
        v.retire_removed(&[b.clone(), wrong], NOW + 1).await,
        Err(Error::Conflict)
    );
    assert!(
        s.review_email(b.email_ref)
            .await
            .unwrap()
            .unwrap()
            .cleared_at
            .is_none()
    );
    v.retire_removed(std::slice::from_ref(&b), NOW + 1)
        .await
        .unwrap();
    v.retire_removed(std::slice::from_ref(&b), NOW + 2)
        .await
        .unwrap();
    let tombstone = s.review_email(b.email_ref).await.unwrap().unwrap();
    assert!(tombstone.ciphertext.is_none());
    assert_eq!(tombstone.cleared_at, Some(NOW + 1));
    assert_eq!(
        s.email_record(b.email_ref).await.unwrap().unwrap(),
        delivery
    );
    let a = Address::parse(MAIL).unwrap();
    assert_eq!(
        v.retain_review(
            &[ReviewEmail {
                binding: &b,
                address: &a
            }],
            NOW + 3
        )
        .await,
        Err(Error::Expired)
    );
}
#[tokio::test]
async fn cleanup_at_fixed_hour_is_bounded_and_preserves_nonreassignable_tombstones() {
    let s = Arc::new(SqlxStorage::in_memory().await.unwrap());
    let v = vault(s.clone(), ring(1));
    let batch = BulkBatchId::new();
    for row in 1..=101 {
        retain(&v, &binding(batch, row), NOW).await;
    }
    assert_eq!(s.prune_review_emails(NOW + 3599).await.unwrap(), 0);
    assert_eq!(s.prune_review_emails(NOW + 3600).await.unwrap(), 100);
    assert_eq!(s.prune_review_emails(NOW + 3600).await.unwrap(), 1);
    assert_eq!(s.prune_review_emails(NOW + 3601).await.unwrap(), 0);
    for row in 1..=101 {
        let r = s
            .review_email(binding(batch, row).email_ref)
            .await
            .unwrap()
            .unwrap();
        assert!(r.ciphertext.is_none());
        assert_eq!(r.expires_at, NOW + 3600);
    }
}
