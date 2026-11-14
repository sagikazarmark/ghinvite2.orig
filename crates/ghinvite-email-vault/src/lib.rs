//! Protected recovery mailbox, shared by the web edge and delivery write run.
//! No HTTP, Restate or UI policy. Only an activated, exactly bound record can
//! release a transient Address; raw email never becomes a command or receipt.
mod address;
mod review;
pub use address::{Address, InvalidAddress};
use anykms_core::Ciphertext;
use anykms_recipes::envelope::Padding;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use ghinvite_core::storage::email_vault::{
    EmailVaultBinding, EmailVaultRecord, EmailVaultStorage, NewEmailRecord,
};
use kekring::{ContextEnvelopeSync, Keyring};
use kekring::{EncryptionContext, purposes};
pub use review::ReviewEmail;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use subtle::ConstantTimeEq;
use zeroize::{Zeroize, Zeroizing};
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error("email recovery unavailable")]
    Unavailable,
    #[error("email recovery binding conflict")]
    Conflict,
    #[error("email recovery expired")]
    Expired,
    #[error("email recovery is not active")]
    NotActive,
}
pub struct EmailVault {
    storage: Arc<dyn EmailVaultStorage>,
    keyring: Arc<Keyring>,
    review: Option<Arc<dyn ghinvite_core::storage::email_review::EmailReviewStorage>>,
}
impl EmailVault {
    pub fn new(storage: Arc<dyn EmailVaultStorage>, keyring: Arc<Keyring>) -> Self {
        Self {
            storage,
            keyring,
            review: None,
        }
    }
    fn context(binding: &EmailVaultBinding) -> Result<EncryptionContext, Error> {
        binding.validate().map_err(|_| Error::Unavailable)?;
        EncryptionContext::try_from(&purposes::BulkEmail {
            email_ref: &binding.email_ref.as_hex(),
            batch_id: &binding.batch.batch_id.to_string(),
            row: u64::from(binding.batch.row),
            account_id: binding.account_id,
            installation_id: binding.installation_id,
        })
        .map_err(|_| Error::Unavailable)
    }
    fn keyring_error(&self, error: anykms_core::Error) -> Error {
        if error.kind() == anykms_core::ErrorKind::Configuration {
            tracing::error!(alert="p1_keyring_configuration",kind=?error.kind(),fingerprint=?self.keyring.fingerprint(),"email recovery encryption unavailable");
        } else {
            tracing::warn!(kind=?error.kind(),fingerprint=?self.keyring.fingerprint(),"email recovery encryption unavailable");
        }
        Error::Unavailable
    }
    /// Immutable staging. Retrying after a lost storage acknowledgement compares
    /// the existing protected value; it never replaces the ciphertext or context.
    pub async fn stage(
        &self,
        binding: &EmailVaultBinding,
        address: &Address,
        now: i64,
    ) -> Result<(), Error> {
        ghinvite_core::storage::email_vault::time(now).map_err(|_| Error::Unavailable)?;
        let context = Self::context(binding)?;
        if let Some(existing) = self
            .storage
            .email_record(binding.email_ref)
            .await
            .map_err(|_| Error::Unavailable)?
        {
            return self.same_address(binding, &existing, address, now);
        }
        #[derive(Serialize)]
        struct Payload<'a> {
            email: &'a str,
            digest: [u8; 32],
        }
        let payload = Payload {
            email: address.as_str(),
            digest: Sha256::digest(address.as_str().as_bytes()).into(),
        };
        let plaintext =
            Zeroizing::new(serde_json::to_vec(&payload).map_err(|_| Error::Unavailable)?);
        let ciphertext = self
            .keyring
            .context_envelope(Padding::block(256))
            .and_then(|e| e.encrypt_sync(&plaintext, &context))
            .map_err(|e| self.keyring_error(e))?;
        let record = NewEmailRecord {
            binding: binding.clone(),
            ciphertext: STANDARD.encode(ciphertext.as_bytes()),
            created_at: now,
        };
        if self
            .storage
            .stage_email(&record)
            .await
            .map_err(|_| Error::Unavailable)?
        {
            return Ok(());
        }
        let existing = self
            .storage
            .email_record(binding.email_ref)
            .await
            .map_err(|_| Error::Unavailable)?
            .ok_or(Error::Unavailable)?;
        self.same_address(binding, &existing, address, now)
    }
    fn same_address(
        &self,
        binding: &EmailVaultBinding,
        record: &EmailVaultRecord,
        address: &Address,
        now: i64,
    ) -> Result<(), Error> {
        let existing = self.open(binding, record, now)?;
        let expected = Sha256::digest(address.as_str().as_bytes());
        let actual = Sha256::digest(existing.as_str().as_bytes());
        if !bool::from(expected.ct_eq(&actual)) {
            return Err(Error::Conflict);
        }
        Ok(())
    }
    /// Only call within the fenced write run; never return Address from ctx.run.
    pub async fn load_active(
        &self,
        binding: &EmailVaultBinding,
        now: i64,
    ) -> Result<Address, Error> {
        ghinvite_core::storage::email_vault::time(now).map_err(|_| Error::Unavailable)?;
        let record = self
            .storage
            .email_record(binding.email_ref)
            .await
            .map_err(|_| Error::Unavailable)?
            .ok_or(Error::Unavailable)?;
        if record.binding() != *binding {
            return Err(Error::Conflict);
        }
        if record.activated_at.is_none_or(|at| at > now) || record.retired_at.is_some() {
            return Err(Error::NotActive);
        }
        self.open(binding, &record, now)
    }
    fn open(
        &self,
        binding: &EmailVaultBinding,
        record: &EmailVaultRecord,
        now: i64,
    ) -> Result<Address, Error> {
        record.validate().map_err(|_| Error::Unavailable)?;
        if record.binding() != *binding {
            return Err(Error::Conflict);
        }
        if !record.readable_at(now) {
            return Err(Error::Expired);
        }
        let context = Self::context(binding)?;
        let ciphertext = Ciphertext::from(
            STANDARD
                .decode(record.ciphertext.as_ref().ok_or(Error::Expired)?)
                .map_err(|_| Error::Unavailable)?,
        );
        let plaintext = self
            .keyring
            .context_envelope(Padding::block(256))
            .and_then(|e| e.decrypt_sync(&ciphertext, &context))
            .map_err(|e| self.keyring_error(e))?;
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Payload {
            email: String,
            digest: [u8; 32],
        }
        impl Drop for Payload {
            fn drop(&mut self) {
                self.email.zeroize();
                self.digest.zeroize();
            }
        }
        let payload: Payload =
            serde_json::from_slice(&plaintext).map_err(|_| Error::Unavailable)?;
        let digest = Sha256::digest(payload.email.as_bytes());
        if !bool::from(digest.as_slice().ct_eq(&payload.digest)) {
            return Err(Error::Unavailable);
        }
        Address::parse(&payload.email).map_err(|_| Error::Unavailable)
    }
}
impl std::fmt::Debug for EmailVault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("EmailVault(..)")
    }
}
