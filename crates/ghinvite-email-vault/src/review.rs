//! No plaintext output: current-authorized web calls stage selected escrow rows.
use super::*;
use ghinvite_core::{
    email_delivery::EmailCreateCommand,
    storage::email_review::{EmailReviewStorage, NewReviewEmail, ReviewEmailRecord},
};
/// Borrowed transient input. Deliberately neither Debug nor Serialize.
pub struct ReviewEmail<'a> {
    pub binding: &'a EmailVaultBinding,
    pub address: &'a Address,
}
impl EmailVault {
    /// Web-only recovery capability. Delivery load remains activated-vault-only.
    pub fn with_review_storage(mut self, storage: Arc<dyn EmailReviewStorage>) -> Self {
        self.review = Some(storage);
        self
    }
    fn review_storage(&self) -> Result<&dyn EmailReviewStorage, Error> {
        self.review.as_deref().ok_or(Error::Unavailable)
    }
    fn review_context(binding: &EmailVaultBinding) -> Result<EncryptionContext, Error> {
        binding.validate().map_err(|_| Error::Conflict)?;
        EncryptionContext::try_from(&purposes::BulkEmailReview {
            email_ref: &binding.email_ref.as_hex(),
            batch_id: &binding.batch.batch_id.to_string(),
            row: u64::from(binding.batch.row),
            account_id: binding.account_id,
            installation_id: binding.installation_id,
        })
        .map_err(|_| Error::Unavailable)
    }
    fn review_open(
        &self,
        record: &ReviewEmailRecord,
        binding: &EmailVaultBinding,
        now: i64,
    ) -> Result<Address, Error> {
        record.validate().map_err(|_| Error::Unavailable)?;
        if record.binding() != *binding {
            return Err(Error::Conflict);
        }
        if now < record.created_at || now >= record.expires_at || record.cleared_at.is_some() {
            return Err(Error::Expired);
        }
        let bytes = STANDARD
            .decode(record.ciphertext.as_ref().ok_or(Error::Expired)?)
            .map_err(|_| Error::Unavailable)?;
        let context = Self::review_context(binding)?;
        let plaintext = Zeroizing::new(
            self.keyring
                .context_envelope(Padding::block(256))
                .and_then(|e| e.decrypt_sync(&Ciphertext::from(bytes), &context))
                .map_err(|e| self.keyring_error(e))?,
        );
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Payload {
            email: String,
        }
        impl Drop for Payload {
            fn drop(&mut self) {
                self.email.zeroize();
            }
        }
        let payload: Payload =
            serde_json::from_slice(&plaintext).map_err(|_| Error::Unavailable)?;
        Address::parse(&payload.email).map_err(|_| Error::Unavailable)
    }
    /// Retain original unique rows before publishing the authoritative review.
    /// Existing ciphertext/expiry is immutable, including after an uncertain ACK.
    pub async fn retain_review(&self, entries: &[ReviewEmail<'_>], now: i64) -> Result<(), Error> {
        ghinvite_core::storage::email_vault::time(now).map_err(|_| Error::Unavailable)?;
        if entries.len() > ghinvite_core::bulk::MAX_ROWS {
            return Err(Error::Conflict);
        }
        for (i, entry) in entries.iter().enumerate() {
            entry.binding.validate().map_err(|_| Error::Conflict)?;
            if entries[..i].iter().any(|p| {
                p.binding.email_ref == entry.binding.email_ref
                    || p.binding.batch == entry.binding.batch
            }) {
                return Err(Error::Conflict);
            }
        }
        let storage = self.review_storage()?;
        for entry in entries {
            if let Some(record) = storage
                .review_email(entry.binding.email_ref)
                .await
                .map_err(|_| Error::Unavailable)?
            {
                if self.review_open(&record, entry.binding, now)?.as_str() != entry.address.as_str()
                {
                    return Err(Error::Conflict);
                }
                continue;
            }
            #[derive(Serialize)]
            struct Payload<'a> {
                email: &'a str,
            }
            let plaintext = Zeroizing::new(
                serde_json::to_vec(&Payload {
                    email: entry.address.as_str(),
                })
                .map_err(|_| Error::Unavailable)?,
            );
            let context = Self::review_context(entry.binding)?;
            let encrypted = self
                .keyring
                .context_envelope(Padding::block(256))
                .and_then(|e| e.encrypt_sync(&plaintext, &context))
                .map_err(|e| self.keyring_error(e))?;
            let record = NewReviewEmail {
                binding: entry.binding.clone(),
                ciphertext: STANDARD.encode(encrypted.as_bytes()),
                created_at: now,
            };
            if !storage
                .retain_review_email(&record)
                .await
                .map_err(|_| Error::Unavailable)?
            {
                let existing = storage
                    .review_email(entry.binding.email_ref)
                    .await
                    .map_err(|_| Error::Unavailable)?
                    .ok_or(Error::Conflict)?;
                if self.review_open(&existing, entry.binding, now)?.as_str()
                    != entry.address.as_str()
                {
                    return Err(Error::Conflict);
                }
            }
        }
        Ok(())
    }
    /// All selected rows are validated/decrypted before any delivery staging.
    /// Call only after current full-target authority and exact review validation.
    pub async fn stage_reviewed(
        &self,
        commands: &[EmailCreateCommand],
        now: i64,
    ) -> Result<(), Error> {
        ghinvite_core::storage::email_vault::time(now).map_err(|_| Error::Unavailable)?;
        if commands.len() > ghinvite_core::bulk::MAX_ROWS {
            return Err(Error::Conflict);
        }
        let storage = self.review_storage()?;
        let mut selected: Vec<(EmailVaultBinding, Address)> = vec![];
        for command in commands {
            command.validate().map_err(|_| Error::Conflict)?;
            let binding = EmailVaultBinding {
                email_ref: command.email_ref,
                batch: command.batch,
                account_id: command.account_id,
                installation_id: command.installation_id,
            };
            if let Some((prior, _)) = selected
                .iter()
                .find(|(b, _)| b.email_ref == binding.email_ref)
            {
                if prior != &binding {
                    return Err(Error::Conflict);
                }
                continue;
            }
            let record = storage
                .review_email(binding.email_ref)
                .await
                .map_err(|_| Error::Unavailable)?
                .ok_or(Error::Expired)?;
            let address = self.review_open(&record, &binding, now)?;
            selected.push((binding, address));
        }
        for (binding, address) in &selected {
            self.stage(binding, address, now).await?;
        }
        Ok(())
    }
    /// Only after acknowledged authoritative removal. Never touches delivery vault.
    pub async fn retire_removed(
        &self,
        bindings: &[EmailVaultBinding],
        now: i64,
    ) -> Result<(), Error> {
        ghinvite_core::storage::email_vault::time(now).map_err(|_| Error::Unavailable)?;
        if bindings.len() > ghinvite_core::bulk::MAX_ROWS {
            return Err(Error::Conflict);
        }
        let storage = self.review_storage()?;
        for binding in bindings {
            binding.validate().map_err(|_| Error::Conflict)?;
            let record = storage
                .review_email(binding.email_ref)
                .await
                .map_err(|_| Error::Unavailable)?
                .ok_or(Error::Conflict)?;
            if record.binding() != *binding || now < record.created_at {
                return Err(Error::Conflict);
            }
        }
        for binding in bindings {
            if !storage
                .clear_review_email(binding, now)
                .await
                .map_err(|_| Error::Unavailable)?
            {
                return Err(Error::Conflict);
            }
        }
        Ok(())
    }
}
