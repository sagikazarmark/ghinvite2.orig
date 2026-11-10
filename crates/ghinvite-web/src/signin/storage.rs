//! Production pending-state adapter: encrypted envelopes and atomic SQL take.
use super::{PendingSignIn, PendingSignInStore, StateUnavailable};
use anykms_core::Ciphertext;
use anykms_keyring::{ContextEnvelopeSync, Keyring};
use anykms_keyring::{EncryptionContext, purposes};
use anykms_recipes::envelope::Padding;
use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use ghinvite_core::clock::{Clock, SystemClock};
use ghinvite_core::storage::pending_signin::{
    PendingSignInKey, PendingSignInStorage, StoredPendingSignIn,
};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use zeroize::Zeroizing;

const LIFETIME_SECONDS: i64 = 1800;

/// The caller supplies the canonical session ID recovered by trusted session
/// middleware. A callback parameter is never a source for this identifier.
/// The `/login` and `/oauth/callback` routes use this adapter.
pub struct SqlPendingSignInStore {
    storage: Arc<dyn PendingSignInStorage>,
    keyring: Arc<Keyring>,
    clock: Arc<dyn Clock>,
}
impl std::fmt::Debug for SqlPendingSignInStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SqlPendingSignInStore(..)")
    }
}
impl SqlPendingSignInStore {
    pub fn new(storage: Arc<dyn PendingSignInStorage>, keyring: Arc<Keyring>) -> Self {
        Self {
            storage,
            keyring,
            clock: Arc::new(SystemClock),
        }
    }
    /// Inject a trusted server clock; never derive it from callback input.
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }
    fn keyring_error(&self, error: anykms_core::Error) -> StateUnavailable {
        if error.kind() == anykms_core::ErrorKind::Configuration {
            tracing::error!(alert = "p1_keyring_configuration", kind = ?error.kind(), fingerprint = ?self.keyring.fingerprint(), "pending sign-in encryption unavailable");
        } else {
            tracing::warn!(kind = ?error.kind(), fingerprint = ?self.keyring.fingerprint(), "pending sign-in encryption unavailable");
        }
        if error.kind() == anykms_core::ErrorKind::UnknownKeyVersion {
            StateUnavailable::KeyringRetry
        } else {
            StateUnavailable::Unavailable
        }
    }
    fn binding(
        session_id: &str,
    ) -> Result<(PendingSignInKey, EncryptionContext), StateUnavailable> {
        let id = session_id
            .parse::<tower_sessions::session::Id>()
            .map_err(|_| StateUnavailable::Unavailable)?;
        if id.to_string() != session_id {
            return Err(StateUnavailable::Unavailable);
        }
        let mut hash = Sha256::new();
        hash.update(b"p1:pending-signin:v1\0");
        hash.update(session_id.as_bytes());
        let key = PendingSignInKey::from_bytes(hash.finalize().into());
        let context = EncryptionContext::try_from(&purposes::PendingSignIn { session_id })
            .map_err(|_| StateUnavailable::Unavailable)?;
        Ok((key, context))
    }
}
#[async_trait]
impl PendingSignInStore for SqlPendingSignInStore {
    async fn replace(
        &self,
        session_id: &str,
        pending: PendingSignIn,
    ) -> Result<(), StateUnavailable> {
        let (key, context) = Self::binding(session_id)?;
        let now = self.clock.now();
        let expires_at = pending
            .issued_at
            .checked_add(LIFETIME_SECONDS)
            .ok_or(StateUnavailable::Unavailable)?;
        if pending.issued_at > now || pending.issued_at < 0 || expires_at <= now {
            return Err(StateUnavailable::Unavailable);
        }
        let plaintext = Zeroizing::new(
            serde_json::to_vec(&pending).map_err(|_| StateUnavailable::Unavailable)?,
        );
        let ciphertext = self
            .keyring
            .context_envelope(Padding::NONE)
            .and_then(|envelope| envelope.encrypt_sync(&plaintext, &context))
            .map_err(|error| self.keyring_error(error))?;
        let record = StoredPendingSignIn {
            ciphertext: STANDARD.encode(ciphertext.as_bytes()),
            expires_at,
        };
        self.storage
            .replace_pending_signin(key, &record, now)
            .await
            .map_err(|_| StateUnavailable::Unavailable)
    }
    async fn take(&self, session_id: &str) -> Result<Option<PendingSignIn>, StateUnavailable> {
        let (key, context) = Self::binding(session_id)?;
        // Read without consuming: lagging key generations/configuration must
        // remain retryable. Exact-record CAS fences replacement and contenders.
        let Some(record) = self
            .storage
            .read_pending_signin(key)
            .await
            .map_err(|_| StateUnavailable::Unavailable)?
        else {
            return Ok(None);
        };
        let now = self.clock.now();
        if record.expires_at <= now {
            self.storage
                .consume_pending_signin(key, &record)
                .await
                .map_err(|_| StateUnavailable::Unavailable)?;
            return Ok(None);
        }
        let ciphertext = Ciphertext::from(
            STANDARD
                .decode(&record.ciphertext)
                .map_err(|_| StateUnavailable::Unavailable)?,
        );
        let plaintext = self
            .keyring
            .context_envelope(Padding::NONE)
            .and_then(|envelope| envelope.decrypt_sync(&ciphertext, &context))
            .map_err(|error| self.keyring_error(error))?;
        let pending: PendingSignIn =
            serde_json::from_slice(&plaintext).map_err(|_| StateUnavailable::Unavailable)?;
        if pending.issued_at < 0
            || pending.issued_at > now
            || pending.issued_at.checked_add(LIFETIME_SECONDS) != Some(record.expires_at)
        {
            return Err(StateUnavailable::Unavailable);
        }
        // Decrypted material stays local until the exact deletion is committed.
        // On uncertain acknowledgement it is dropped and cannot reach exchange.
        if !self
            .storage
            .consume_pending_signin(key, &record)
            .await
            .map_err(|_| StateUnavailable::Unavailable)?
        {
            return Ok(None);
        }
        Ok(Some(pending))
    }
}

/// Runtime-test-only conformance, absent from production features. It exercises
/// the same context constructor and envelope backend as the real adapter.
#[cfg(feature = "runtime-tests")]
pub fn verify_context_separation(
    keyring: &Keyring,
    session_id: &str,
) -> Result<(), StateUnavailable> {
    let (_, context) = SqlPendingSignInStore::binding(session_id)?;
    let other = tower_sessions::session::Id::default().to_string();
    let (_, other_context) = SqlPendingSignInStore::binding(&other)?;
    let session_context = EncryptionContext::try_from(&purposes::Session { session_id })
        .map_err(|_| StateUnavailable::Unavailable)?;
    let envelope = keyring
        .context_envelope(Padding::NONE)
        .map_err(|_| StateUnavailable::Unavailable)?;
    let ciphertext = envelope
        .encrypt_sync(b"pending-signin-context-fixture", &context)
        .map_err(|_| StateUnavailable::Unavailable)?;
    if &*envelope
        .decrypt_sync(&ciphertext, &context)
        .map_err(|_| StateUnavailable::Unavailable)?
        != b"pending-signin-context-fixture"
        || envelope.decrypt_sync(&ciphertext, &other_context).is_ok()
        || envelope.decrypt_sync(&ciphertext, &session_context).is_ok()
    {
        return Err(StateUnavailable::Unavailable);
    }
    let old = envelope
        .encrypt_sync(b"session-context-fixture", &session_context)
        .map_err(|_| StateUnavailable::Unavailable)?;
    if envelope.decrypt_sync(&old, &context).is_ok() {
        return Err(StateUnavailable::Unavailable);
    }
    Ok(())
}
