//! Transient bulk parsing. Mailboxes never appear in the manifest or Debug output.
use ghinvite_core::{
    bulk::{BatchRef, BulkBatchId, Entry, MAX_ROWS},
    email_delivery::{EmailCreateCommand, EmailRef},
    storage::email_vault::EmailVaultBinding,
};
use ghinvite_email_vault::{Address, EmailVault};
use zeroize::Zeroizing;

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum PreparationError {
    #[error("a batch must contain between one and 200 rows")]
    Rows,
    #[error("invalid email address in row {row}")]
    Address { row: u16 },
    #[error("email preparation is unavailable")]
    Unavailable,
}
/// Input row numbers are permanent; removal does not renumber a reviewed batch.
/// Own the strings so dropping the input zeroizes the original pasted addresses.
pub struct RawRow {
    pub row: u16,
    pub value: Zeroizing<String>,
}
impl std::fmt::Debug for RawRow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RawRow")
            .field("row", &self.row)
            .finish_non_exhaustive()
    }
}
struct UniqueEmail {
    binding: EmailVaultBinding,
    address: Address,
}
pub struct PreparedEmailRows {
    entries: Vec<Entry>,
    unique: Vec<UniqueEmail>,
}
impl std::fmt::Debug for PreparedEmailRows {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreparedEmailRows")
            .field("rows", &self.entries.len())
            .field("unique_email_rows", &self.unique.len())
            .finish()
    }
}
impl PreparedEmailRows {
    /// This parser runs before lookup/staging. Handle spelling is preserved for
    /// Manifest::normalize; only rows containing @ away from the first position
    /// are treated as addresses (a leading @ is a GitHub-handle prefix).
    pub fn prepare(
        batch_id: BulkBatchId,
        account_id: u64,
        installation_id: u64,
        rows: Vec<RawRow>,
    ) -> Result<Self, PreparationError> {
        if rows.is_empty()
            || rows.len() > MAX_ROWS
            || rows
                .iter()
                .enumerate()
                .any(|(i, r)| usize::from(r.row) != i + 1)
        {
            return Err(PreparationError::Rows);
        }
        let mut entries = Vec::with_capacity(rows.len());
        let mut unique: Vec<UniqueEmail> = vec![];
        for row in rows {
            let value = row.value.trim();
            if !value.trim_start_matches('@').contains('@') {
                entries.push(Entry::Handle {
                    login: value.to_owned(),
                });
                continue;
            }
            let address =
                Address::parse(value).map_err(|_| PreparationError::Address { row: row.row })?;
            let reference = if let Some(prior) = unique
                .iter()
                .find(|p| p.address.as_str() == address.as_str())
            {
                prior.binding.email_ref
            } else {
                let mut bytes = Zeroizing::new([0; 32]);
                getrandom::fill(&mut *bytes).map_err(|_| PreparationError::Unavailable)?;
                let email_ref = EmailRef::from_bytes(*bytes);
                let binding = EmailVaultBinding {
                    email_ref,
                    batch: BatchRef {
                        batch_id,
                        row: row.row,
                    },
                    account_id,
                    installation_id,
                };
                binding
                    .validate()
                    .map_err(|_| PreparationError::Unavailable)?;
                unique.push(UniqueEmail { binding, address });
                email_ref
            };
            entries.push(Entry::Email {
                email_ref: reference,
            });
        }
        Ok(Self { entries, unique })
    }
    /// Transient display only, after restoring this actor/session-bound plan.
    /// Render `Address::as_str()` through the HTML escaper; never copy it into
    /// Manifest, audit metadata, logs, or a durable response receipt. Duplicate
    /// rows resolve to the same normalized address without extra vault reads.
    pub fn review_address(&self, row: u16) -> Option<&Address> {
        let Entry::Email { email_ref } = self.entries.get(usize::from(row.checked_sub(1)?))? else {
            return None;
        };
        self.unique
            .iter()
            .find(|entry| entry.binding.email_ref == *email_ref)
            .map(|entry| &entry.address)
    }
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }
    /// Publish only protected review recovery; this does not stage delivery.
    /// Call after retaining this exact allocation and checking current authority.
    pub async fn retain_review(
        &self,
        vault: &EmailVault,
        now: i64,
    ) -> Result<(), PreparationError> {
        let entries = self
            .unique
            .iter()
            .map(|entry| ghinvite_email_vault::ReviewEmail {
                binding: &entry.binding,
                address: &entry.address,
            })
            .collect::<Vec<_>>();
        vault
            .retain_review(&entries, now)
            .await
            .map_err(|_| PreparationError::Unavailable)
    }
    /// Retain this exact plan through the protected web continuation before calling
    /// stage. An uncertain write retries the same reference, never allocates anew.
    pub async fn stage(&self, vault: &EmailVault, now: i64) -> Result<(), PreparationError> {
        for email in &self.unique {
            vault
                .stage(&email.binding, &email.address, now)
                .await
                .map_err(|_| PreparationError::Unavailable)?;
        }
        Ok(())
    }
    /// Validate the entire selected submission before writing any recovery row.
    /// Removed rows and duplicate occurrences never acquire vault records.
    /// Authorization and the reviewed target/actor binding remain the caller's
    /// responsibility; this boundary checks the exact prepared address binding.
    pub async fn stage_selected(
        &self,
        vault: &EmailVault,
        submitted: &[EmailCreateCommand],
        now: i64,
    ) -> Result<(), PreparationError> {
        if submitted.len() > MAX_ROWS {
            return Err(PreparationError::Unavailable);
        }
        let mut selected = Vec::with_capacity(submitted.len());
        for command in submitted {
            command
                .validate()
                .map_err(|_| PreparationError::Unavailable)?;
            let binding = EmailVaultBinding {
                email_ref: command.email_ref,
                batch: command.batch,
                account_id: command.account_id,
                installation_id: command.installation_id,
            };
            let index = self
                .unique
                .iter()
                .position(|email| email.binding == binding)
                .ok_or(PreparationError::Unavailable)?;
            if !selected.contains(&index) {
                selected.push(index);
            }
        }
        for index in selected {
            let email = &self.unique[index];
            vault
                .stage(&email.binding, &email.address, now)
                .await
                .map_err(|_| PreparationError::Unavailable)?;
        }
        Ok(())
    }
}
#[cfg(test)]
#[path = "email_rows/selected_tests.rs"]
mod selected_tests;
#[cfg(test)]
mod tests {
    use super::*;
    fn rows(values: &[&str]) -> Vec<RawRow> {
        values
            .iter()
            .enumerate()
            .map(|(n, v)| RawRow {
                row: n as u16 + 1,
                value: Zeroizing::new((*v).into()),
            })
            .collect()
    }
    #[test]
    fn normalized_duplicate_mailboxes_share_only_first_ref_and_handles_remain_for_review() {
        let p = PreparedEmailRows::prepare(
            BulkBatchId::new(),
            42,
            9,
            rows(&[
                "ALICE+tag@BÜCHER.example",
                "@someone",
                "ALICE+tag@xn--bcher-kva.example",
                "alice+tag@xn--bcher-kva.example",
            ]),
        )
        .unwrap();
        assert_eq!(p.entries[0], p.entries[2]);
        assert_ne!(p.entries[0], p.entries[3]);
        assert_eq!(p.unique.len(), 2);
        assert_eq!(
            p.review_address(1).unwrap().as_str(),
            "ALICE+tag@xn--bcher-kva.example"
        );
        assert_eq!(
            p.review_address(3).unwrap().as_str(),
            p.review_address(1).unwrap().as_str()
        );
        assert!(p.review_address(0).is_none());
        assert!(p.review_address(2).is_none());
        assert!(p.review_address(5).is_none());
        assert_eq!(p.unique[0].binding.batch.row, 1);
        assert_eq!(p.unique[1].binding.batch.row, 4);
        assert_eq!(
            p.entries[1],
            Entry::Handle {
                login: "@someone".into()
            }
        );
        let wire = serde_json::to_string(p.entries()).unwrap();
        assert!(!wire.contains("ALICE"));
        assert!(!format!("{p:?}").contains("ALICE"));
    }
    #[test]
    fn malformed_email_error_exposes_only_row_and_bound_before_entropy_or_io() {
        assert_eq!(
            PreparedEmailRows::prepare(
                BulkBatchId::new(),
                42,
                9,
                rows(&["ok@example.com", "PRIVATE INVALID@example.com"])
            )
            .unwrap_err(),
            PreparationError::Address { row: 2 }
        );
        assert_eq!(
            PreparedEmailRows::prepare(BulkBatchId::new(), 42, 9, rows(&vec!["a@b.test"; 201]))
                .unwrap_err(),
            PreparationError::Rows
        );
    }
}

/// Trusted current session plus immutable batch authority. This is recovery
/// context, never a substitute for the route's CSRF/current-authority checks.
pub struct EmailRowsScope {
    pub(crate) session: String,
    pub(crate) batch_id: BulkBatchId,
    pub(crate) account_id: u64,
    pub(crate) installation_id: u64,
    pub(crate) session_expires: i64,
}
impl EmailRowsScope {
    pub fn new(
        tower: &tower_sessions::Session,
        batch_id: BulkBatchId,
        account_id: u64,
        installation_id: u64,
    ) -> Result<Self, PreparationError> {
        if account_id == 0
            || installation_id == 0
            || account_id > ghinvite_core::storage::integer::MAX_EXACT
            || installation_id > ghinvite_core::storage::integer::MAX_EXACT
        {
            return Err(PreparationError::Unavailable);
        }
        Ok(Self {
            session: tower.id().ok_or(PreparationError::Unavailable)?.to_string(),
            batch_id,
            account_id,
            installation_id,
            session_expires: tower.expiry_date().unix_timestamp(),
        })
    }
    fn context(&self) -> Result<kekring::EncryptionContext, PreparationError> {
        kekring::EncryptionContext::try_from(&kekring::purposes::AttemptContinuation {
            scope: &format!(
                "bulk-email-plan/v1/{}/{}/{}",
                self.batch_id, self.account_id, self.installation_id
            ),
            binding: &self.session,
        })
        .map_err(|_| PreparationError::Unavailable)
    }
}
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RecoveryEmail {
    binding: EmailVaultBinding,
    email: String,
}
impl Drop for RecoveryEmail {
    fn drop(&mut self) {
        use zeroize::Zeroize;
        self.email.zeroize();
    }
}
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RecoveryPlan {
    version: u8,
    created_at: i64,
    expires_at: i64,
    entries: Vec<Entry>,
    unique: Vec<RecoveryEmail>,
}
fn envelope_error(keyring: &kekring::Keyring, error: anykms_core::Error) -> PreparationError {
    if error.kind() == anykms_core::ErrorKind::Configuration {
        tracing::error!(alert="p1_keyring_configuration",kind=?error.kind(),fingerprint=?keyring.fingerprint(),"email preparation encryption unavailable");
    } else {
        tracing::warn!(kind=?error.kind(),fingerprint=?keyring.fingerprint(),"email preparation encryption unavailable");
    }
    PreparationError::Unavailable
}
impl PreparedEmailRows {
    /// Persist only this ciphertext before the first stage attempt. On a lost
    /// acknowledgement, restore it to retry precisely the same row/reference set.
    pub fn seal(
        &self,
        keyring: &kekring::Keyring,
        scope: &EmailRowsScope,
        now: i64,
    ) -> Result<String, PreparationError> {
        use base64::Engine as _;
        use kekring::ContextEnvelopeSync;
        if now <= 0
            || scope.session_expires <= now
            || self.unique.iter().any(|e| {
                e.binding.batch.batch_id != scope.batch_id
                    || e.binding.account_id != scope.account_id
                    || e.binding.installation_id != scope.installation_id
            })
        {
            return Err(PreparationError::Unavailable);
        }
        let payload = RecoveryPlan {
            version: 1,
            created_at: now,
            expires_at: scope
                .session_expires
                .min(now.checked_add(3600).ok_or(PreparationError::Unavailable)?),
            entries: self.entries.clone(),
            unique: self
                .unique
                .iter()
                .map(|e| RecoveryEmail {
                    binding: e.binding.clone(),
                    email: e.address.as_str().into(),
                })
                .collect(),
        };
        let plaintext = Zeroizing::new(
            serde_json::to_vec(&payload).map_err(|_| PreparationError::Unavailable)?,
        );
        let context = scope.context()?;
        let encrypted = keyring
            .context_envelope(anykms_recipes::envelope::Padding::NONE)
            .and_then(|e| e.encrypt_sync(&plaintext, &context))
            .map_err(|e| envelope_error(keyring, e))?;
        Ok(base64::engine::general_purpose::STANDARD.encode(encrypted.as_bytes()))
    }
    pub fn restore(
        ciphertext: &str,
        keyring: &kekring::Keyring,
        scope: &EmailRowsScope,
        now: i64,
    ) -> Result<Self, PreparationError> {
        use base64::Engine as _;
        use kekring::ContextEnvelopeSync;
        if ciphertext.len() > 262144 || now <= 0 || scope.session_expires <= now {
            return Err(PreparationError::Unavailable);
        }
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(ciphertext)
            .map_err(|_| PreparationError::Unavailable)?;
        let context = scope.context()?;
        let plaintext = keyring
            .context_envelope(anykms_recipes::envelope::Padding::NONE)
            .and_then(|e| e.decrypt_sync(&anykms_core::Ciphertext::from(bytes), &context))
            .map_err(|e| envelope_error(keyring, e))?;
        let plaintext = Zeroizing::new(plaintext);
        let payload: RecoveryPlan =
            serde_json::from_slice(&plaintext).map_err(|_| PreparationError::Unavailable)?;
        if payload.version != 1
            || payload.created_at > now
            || payload.expires_at <= now
            || payload.expires_at > scope.session_expires
            || payload.expires_at - payload.created_at > 3600
            || payload.entries.is_empty()
            || payload.entries.len() > MAX_ROWS
        {
            return Err(PreparationError::Unavailable);
        }
        let mut unique: Vec<UniqueEmail> = vec![];
        for record in &payload.unique {
            let b = &record.binding;
            b.validate().map_err(|_| PreparationError::Unavailable)?;
            if b.batch.batch_id != scope.batch_id
                || b.account_id != scope.account_id
                || b.installation_id != scope.installation_id
                || payload.entries.get(usize::from(b.batch.row) - 1)
                    != Some(&Entry::Email {
                        email_ref: b.email_ref,
                    })
                || unique.iter().any(|e| e.binding.email_ref == b.email_ref)
            {
                return Err(PreparationError::Unavailable);
            }
            let address =
                Address::parse(&record.email).map_err(|_| PreparationError::Unavailable)?;
            if unique
                .iter()
                .any(|e| e.address.as_str() == address.as_str())
            {
                return Err(PreparationError::Unavailable);
            }
            unique.push(UniqueEmail {
                binding: b.clone(),
                address,
            });
        }
        for (index, entry) in payload.entries.iter().enumerate() {
            if let Entry::Email { email_ref } = entry
                && !unique.iter().any(|e| {
                    e.binding.email_ref == *email_ref
                        && usize::from(e.binding.batch.row) <= index + 1
                })
            {
                return Err(PreparationError::Unavailable);
            }
        }
        Ok(Self {
            entries: payload.entries,
            unique,
        })
    }
}

#[cfg(test)]
mod recovery_tests {
    use super::*;
    fn scope(batch_id: BulkBatchId) -> EmailRowsScope {
        EmailRowsScope {
            session: "trusted-session-1".into(),
            batch_id,
            account_id: 42,
            installation_id: 9,
            session_expires: 1800010000,
        }
    }
    fn ring() -> kekring::Keyring {
        kekring::Keyring::builder("p1")
            .generation(1, [7; 32])
            .unwrap()
            .current(1)
            .unwrap()
            .build()
            .unwrap()
    }
    #[test]
    fn encrypted_plan_retains_exact_refs_and_checks_every_context_component_and_expiry() {
        let batch = BulkBatchId::new();
        let scope = scope(batch);
        let keyring = ring();
        let rows = vec![
            RawRow {
                row: 1,
                value: Zeroizing::new("PLAN_MAILBOX_MARKER+tag@example.com".into()),
            },
            RawRow {
                row: 2,
                value: Zeroizing::new("PLAN_MAILBOX_MARKER+tag@EXAMPLE.com".into()),
            },
        ];
        let p = PreparedEmailRows::prepare(batch, 42, 9, rows).unwrap();
        let sealed = p.seal(&keyring, &scope, 1800000000).unwrap();
        assert!(!sealed.contains("PLAN_MAILBOX"));
        let restored = PreparedEmailRows::restore(&sealed, &keyring, &scope, 1800000010).unwrap();
        assert_eq!(restored.entries(), p.entries());
        assert_eq!(restored.unique[0].binding, p.unique[0].binding);
        assert_eq!(restored.unique.len(), 1);
        assert_eq!(
            restored.unique[0].address.as_str(),
            "PLAN_MAILBOX_MARKER+tag@example.com"
        );
        for n in 0..4 {
            let mut wrong = EmailRowsScope {
                session: scope.session.clone(),
                batch_id: batch,
                account_id: 42,
                installation_id: 9,
                session_expires: scope.session_expires,
            };
            match n {
                0 => wrong.session = "other-session".into(),
                1 => wrong.batch_id = BulkBatchId::new(),
                2 => wrong.account_id = 43,
                _ => wrong.installation_id = 10,
            };
            assert!(PreparedEmailRows::restore(&sealed, &keyring, &wrong, 1800000010).is_err());
        }
        assert!(PreparedEmailRows::restore(&sealed, &keyring, &scope, 1800003600).is_err());
        assert!(PreparedEmailRows::restore(&sealed, &keyring, &scope, 1799999999).is_err());
    }
}
