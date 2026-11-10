use super::*;
use ghinvite_core::{
    storage::email_vault::EmailVaultStorage,
    targets::{MemberGrant, OrganizationTarget},
};
use std::sync::Arc;

fn plan() -> PreparedEmailRows {
    PreparedEmailRows::prepare(
        BulkBatchId::new(),
        42,
        9,
        [
            "removed@example.com",
            "selected@example.com",
            "selected@example.com",
        ]
        .into_iter()
        .enumerate()
        .map(|(i, value)| RawRow {
            row: i as u16 + 1,
            value: Zeroizing::new(value.into()),
        })
        .collect(),
    )
    .unwrap()
}
fn command(email: &UniqueEmail) -> EmailCreateCommand {
    EmailCreateCommand {
        batch: email.binding.batch,
        account_id: email.binding.account_id,
        installation_id: email.binding.installation_id,
        email_ref: email.binding.email_ref,
        organization: OrganizationTarget {
            org_id: 42,
            org_login: "acme".into(),
            grant: MemberGrant::Member,
        },
        teams: vec![],
        approved_by: 7,
        approved_at: chrono::Utc::now(),
    }
}
async fn vault() -> (Arc<ghinvite_storage_sqlx::SqlxStorage>, EmailVault) {
    let storage = Arc::new(
        ghinvite_storage_sqlx::SqlxStorage::in_memory()
            .await
            .unwrap(),
    );
    let ring = Arc::new(
        anykms_keyring::Keyring::builder("p1-email")
            .generation(1, [7; 32])
            .unwrap()
            .current(1)
            .unwrap()
            .build()
            .unwrap(),
    );
    (storage.clone(), EmailVault::new(storage, ring))
}
#[tokio::test]
async fn only_selected_original_row_is_staged_and_retry_preserves_exact_ciphertext() {
    let p = plan();
    let (storage, vault) = vault().await;
    let selected = command(&p.unique[1]);
    let now = chrono::Utc::now().timestamp();
    p.stage_selected(&vault, &[selected.clone(), selected.clone()], now)
        .await
        .unwrap();
    assert!(
        storage
            .email_record(p.unique[0].binding.email_ref)
            .await
            .unwrap()
            .is_none()
    );
    let first = storage
        .email_record(selected.email_ref)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first.row_number, 2);
    assert!(first.activated_at.is_none());
    assert!(
        !first
            .ciphertext
            .as_ref()
            .unwrap()
            .contains("selected@example.com")
    );
    p.stage_selected(&vault, std::slice::from_ref(&selected), now + 1)
        .await
        .unwrap();
    assert_eq!(
        storage
            .email_record(selected.email_ref)
            .await
            .unwrap()
            .unwrap(),
        first
    );
    p.stage_selected(&vault, &[], now + 2).await.unwrap();
    assert!(
        storage
            .email_record(p.unique[0].binding.email_ref)
            .await
            .unwrap()
            .is_none()
    );
}
#[tokio::test]
async fn all_bindings_are_validated_before_any_row_is_staged() {
    let p = plan();
    let (storage, vault) = vault().await;
    let good = command(&p.unique[0]);
    let correct = command(&p.unique[1]);
    let mut mismatches = vec![];
    let mut wrong = correct.clone();
    wrong.batch.row = 3;
    mismatches.push(wrong);
    let mut wrong = correct.clone();
    wrong.batch.batch_id = BulkBatchId::new();
    mismatches.push(wrong);
    let mut wrong = correct.clone();
    wrong.installation_id += 1;
    mismatches.push(wrong);
    let mut wrong = correct.clone();
    wrong.account_id += 1;
    wrong.organization.org_id += 1;
    mismatches.push(wrong);
    let mut wrong = correct.clone();
    wrong.email_ref = EmailRef::from_bytes([99; 32]);
    mismatches.push(wrong);
    let mut wrong = correct;
    wrong.approved_by = 0;
    mismatches.push(wrong);
    for wrong in mismatches {
        assert_eq!(
            p.stage_selected(
                &vault,
                &[good.clone(), wrong],
                chrono::Utc::now().timestamp()
            )
            .await,
            Err(PreparationError::Unavailable)
        );
        for email in &p.unique {
            assert!(
                storage
                    .email_record(email.binding.email_ref)
                    .await
                    .unwrap()
                    .is_none()
            );
        }
    }
}
