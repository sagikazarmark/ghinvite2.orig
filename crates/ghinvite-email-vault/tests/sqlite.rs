use ghinvite_core::{
    bulk::{BatchRef, BulkBatchId},
    email_delivery::EmailRef,
    storage::{
        Result,
        email_vault::{EmailVaultBinding, EmailVaultStorage, NewEmailRecord},
    },
};
use ghinvite_email_vault::{Address, EmailVault, Error};
use ghinvite_storage_sqlx::SqlxStorage;
use kekring::Keyring;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
const NOW: i64 = 1_800_000_000;
fn ring(generation: u32) -> Arc<Keyring> {
    Arc::new(
        Keyring::builder("p1")
            .generation(generation, [generation as u8; 32])
            .unwrap()
            .current(generation)
            .unwrap()
            .build()
            .unwrap(),
    )
}
fn binding() -> EmailVaultBinding {
    EmailVaultBinding {
        email_ref: EmailRef::from_bytes([7; 32]),
        batch: BatchRef {
            batch_id: BulkBatchId::new(),
            row: 1,
        },
        account_id: 42,
        installation_id: 9,
    }
}
#[tokio::test]
async fn encrypted_exact_retry_active_gate_rollout_and_terminal_cleanup() {
    let storage = Arc::new(SqlxStorage::in_memory().await.unwrap());
    let vault = EmailVault::new(storage.clone(), ring(2));
    let binding = binding();
    let email = Address::parse("PRIVATE_EMAIL_MARKER+tag@example.com").unwrap();
    vault.stage(&binding, &email, NOW).await.unwrap();
    let original = storage
        .email_record(binding.email_ref)
        .await
        .unwrap()
        .unwrap();
    let wire = serde_json::to_string(&original).unwrap();
    assert!(!wire.contains("PRIVATE_EMAIL_MARKER"));
    assert!(!wire.contains("example.com"));
    assert!(!format!("{original:?}").contains(original.ciphertext.as_ref().unwrap()));
    assert_eq!(
        vault.load_active(&binding, NOW).await.unwrap_err(),
        Error::NotActive
    );
    vault.stage(&binding, &email, NOW + 1).await.unwrap();
    assert_eq!(
        storage
            .email_record(binding.email_ref)
            .await
            .unwrap()
            .unwrap(),
        original
    );
    assert_eq!(
        vault
            .stage(&binding, &Address::parse("other@example.com").unwrap(), NOW)
            .await,
        Err(Error::Conflict)
    );
    assert!(storage.activate_email(&binding, NOW + 2).await.unwrap());
    assert!(storage.activate_email(&binding, NOW + 3).await.unwrap());
    assert_eq!(
        storage
            .email_record(binding.email_ref)
            .await
            .unwrap()
            .unwrap()
            .activated_at,
        Some(NOW + 2)
    );
    let lagging = EmailVault::new(storage.clone(), ring(1));
    let activated = storage
        .email_record(binding.email_ref)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        lagging.load_active(&binding, NOW + 3).await.unwrap_err(),
        Error::Unavailable
    );
    assert_eq!(
        storage
            .email_record(binding.email_ref)
            .await
            .unwrap()
            .unwrap(),
        activated
    );
    assert_eq!(
        vault
            .load_active(&binding, NOW + 200_000)
            .await
            .unwrap()
            .as_str(),
        email.as_str()
    );
    assert!(storage.retire_email(&binding, NOW + 200_001).await.unwrap());
    assert!(storage.retire_email(&binding, NOW + 200_100).await.unwrap());
    assert_eq!(
        storage
            .email_record(binding.email_ref)
            .await
            .unwrap()
            .unwrap()
            .retired_at,
        Some(NOW + 200_001)
    );
    assert_eq!(
        vault
            .load_active(&binding, NOW + 200_002)
            .await
            .unwrap_err(),
        Error::NotActive
    );
    assert_eq!(storage.prune_emails(NOW + 286_400).await.unwrap(), 0);
    assert_eq!(storage.prune_emails(NOW + 286_401).await.unwrap(), 1);
    let tombstone = storage
        .email_record(binding.email_ref)
        .await
        .unwrap()
        .unwrap();
    assert!(tombstone.ciphertext.is_none());
    assert!(
        !storage
            .activate_email(&binding, NOW + 286_402)
            .await
            .unwrap()
    );
    assert_eq!(
        vault.stage(&binding, &email, NOW + 286_402).await,
        Err(Error::Expired)
    );
}
#[tokio::test]
async fn complete_context_binding_and_copied_ciphertext_cannot_release_mailbox() {
    let storage = Arc::new(SqlxStorage::in_memory().await.unwrap());
    let vault = EmailVault::new(storage.clone(), ring(1));
    let original = binding();
    vault
        .stage(
            &original,
            &Address::parse("PRIVATE_CONTEXT@example.com").unwrap(),
            NOW,
        )
        .await
        .unwrap();
    storage.activate_email(&original, NOW).await.unwrap();
    let ciphertext = storage
        .email_record(original.email_ref)
        .await
        .unwrap()
        .unwrap()
        .ciphertext
        .unwrap();
    for n in 0..5 {
        let mut other = original.clone();
        match n {
            0 => other.email_ref = EmailRef::from_bytes([10; 32]),
            1 => other.batch.row = 2,
            2 => other.batch.batch_id = BulkBatchId::new(),
            3 => other.account_id = 43,
            _ => other.installation_id = 10,
        }
        let storage = Arc::new(SqlxStorage::in_memory().await.unwrap());
        let vault = EmailVault::new(storage.clone(), ring(1));
        assert!(
            storage
                .stage_email(&NewEmailRecord {
                    binding: other.clone(),
                    ciphertext: ciphertext.clone(),
                    created_at: NOW
                })
                .await
                .unwrap()
        );
        storage.activate_email(&other, NOW).await.unwrap();
        assert_eq!(
            vault.load_active(&other, NOW).await.unwrap_err(),
            Error::Unavailable
        );
        assert_eq!(
            storage
                .email_record(other.email_ref)
                .await
                .unwrap()
                .unwrap()
                .ciphertext,
            Some(ciphertext.clone())
        );
    }
    let mut wrong = original.clone();
    wrong.account_id = 99;
    assert!(!storage.activate_email(&wrong, NOW).await.unwrap());
    assert!(!storage.retire_email(&wrong, NOW).await.unwrap());
    assert_eq!(
        vault.load_active(&wrong, NOW).await.unwrap_err(),
        Error::Conflict
    );
}
#[tokio::test]
async fn concurrent_staging_has_one_immutable_mailbox() {
    let storage = Arc::new(SqlxStorage::in_memory().await.unwrap());
    let vault = Arc::new(EmailVault::new(storage.clone(), ring(1)));
    let binding = binding();
    let mut tasks = vec![];
    for n in 0..12 {
        let vault = vault.clone();
        let binding = binding.clone();
        tasks.push(tokio::spawn(async move {
            let email = Address::parse(&format!("private{n}@example.com")).unwrap();
            vault.stage(&binding, &email, NOW).await
        }));
    }
    let mut success = 0;
    for t in tasks {
        match t.await.unwrap() {
            Ok(()) => success += 1,
            Err(error) => assert_eq!(error, Error::Conflict),
        }
    }
    assert_eq!(success, 1);
    assert!(storage.activate_email(&binding, NOW).await.unwrap());
    assert!(
        vault
            .load_active(&binding, NOW)
            .await
            .unwrap()
            .as_str()
            .starts_with("private")
    );
}
#[tokio::test]
async fn cleanup_is_bounded_expired_staging_cannot_activate_and_active_is_retained() {
    let storage = SqlxStorage::in_memory().await.unwrap();
    let base = binding();
    for n in 0..102 {
        let mut b = base.clone();
        b.email_ref = EmailRef::from_bytes([n; 32]);
        assert!(
            storage
                .stage_email(&NewEmailRecord {
                    binding: b,
                    ciphertext: "AQID".into(),
                    created_at: NOW
                })
                .await
                .unwrap()
        );
    }
    let mut active = base.clone();
    active.email_ref = EmailRef::from_bytes([101; 32]);
    assert!(storage.activate_email(&active, NOW + 1).await.unwrap());
    assert!(!storage.activate_email(&base, NOW + 86_400).await.unwrap());
    assert_eq!(storage.prune_emails(NOW + 86_400).await.unwrap(), 100);
    assert_eq!(storage.prune_emails(NOW + 86_400).await.unwrap(), 1);
    assert_eq!(storage.prune_emails(NOW + 86_400).await.unwrap(), 0);
    assert!(
        storage
            .email_record(active.email_ref)
            .await
            .unwrap()
            .unwrap()
            .ciphertext
            .is_some()
    );
}
struct LostAck {
    inner: Arc<SqlxStorage>,
    lose: AtomicBool,
}
#[async_trait::async_trait]
impl EmailVaultStorage for LostAck {
    async fn stage_email(&self, r: &NewEmailRecord) -> Result<bool> {
        let answer = self.inner.stage_email(r).await?;
        if self.lose.swap(false, Ordering::SeqCst) {
            return Err(ghinvite_core::storage::Error::Database(
                "fixed lost acknowledgement".into(),
            ));
        }
        Ok(answer)
    }
    async fn email_record(
        &self,
        r: EmailRef,
    ) -> Result<Option<ghinvite_core::storage::email_vault::EmailVaultRecord>> {
        self.inner.email_record(r).await
    }
    async fn activate_email(&self, b: &EmailVaultBinding, n: i64) -> Result<bool> {
        self.inner.activate_email(b, n).await
    }
    async fn retire_email(&self, b: &EmailVaultBinding, n: i64) -> Result<bool> {
        self.inner.retire_email(b, n).await
    }
    async fn prune_emails(&self, n: i64) -> Result<u64> {
        self.inner.prune_emails(n).await
    }
}
#[tokio::test]
async fn lost_ack_retries_same_ciphertext_and_does_not_release_unactivated_address() {
    let storage = Arc::new(SqlxStorage::in_memory().await.unwrap());
    let wrapper = Arc::new(LostAck {
        inner: storage.clone(),
        lose: AtomicBool::new(true),
    });
    let vault = EmailVault::new(wrapper, ring(1));
    let b = binding();
    let email = Address::parse("private@example.com").unwrap();
    assert_eq!(vault.stage(&b, &email, NOW).await, Err(Error::Unavailable));
    let original = storage.email_record(b.email_ref).await.unwrap().unwrap();
    assert_eq!(
        vault.load_active(&b, NOW).await.unwrap_err(),
        Error::NotActive
    );
    vault.stage(&b, &email, NOW + 1).await.unwrap();
    assert_eq!(
        storage.email_record(b.email_ref).await.unwrap().unwrap(),
        original
    );
}

/// GM-00 (PD-9): the encrypted vault payload is `{"email": Address::as_str(),
/// "digest": SHA-256(email)}`, and a retried `stage` compares digests. GM-07
/// moved `Address` out of gh-toolkit, so its canonical form is pinned here.
#[tokio::test]
async fn golden_address_canonical_form() {
    for (input, canonical) in [
        ("Ada.Lovelace+GH@Example.COM", "Ada.Lovelace+GH@example.com"),
        ("x@BÜCHER.Example", "x@xn--bcher-kva.example"),
        (
            "o'neil!#$%&*/=?^_`{|}~-@sub.example.org",
            "o'neil!#$%&*/=?^_`{|}~-@sub.example.org",
        ),
    ] {
        assert_eq!(Address::parse(input).unwrap().as_str(), canonical);
    }
    let storage = Arc::new(SqlxStorage::in_memory().await.unwrap());
    let vault = EmailVault::new(storage, ring(2));
    let binding = binding();
    let staged = Address::parse("Ada.Lovelace+GH@Example.COM").unwrap();
    vault.stage(&binding, &staged, NOW).await.unwrap();
    let same = Address::parse("Ada.Lovelace+GH@example.com").unwrap();
    vault.stage(&binding, &same, NOW + 1).await.unwrap();
    let local_case = Address::parse("ada.lovelace+gh@example.com").unwrap();
    assert_eq!(
        vault.stage(&binding, &local_case, NOW + 1).await,
        Err(Error::Conflict)
    );
}
