//! The concrete envelope over a keyring: the only way applications encrypt with it.
use crate::kek::{Kek, blob_version};
use crate::{Aad, ErrorKind, Keyring, Operation, Result};
use anykms_core::{Aead, Ciphertext};
use anykms_recipes::{Envelope, EnvelopeAead};
use std::future::Future;
use std::task::{Context, Poll, Waker};
use zeroize::Zeroizing;

/// anykms envelope v1 under a [`Keyring`], bound to canonical [`Aad`]. New envelopes take the
/// purpose's padding; the envelope's `kek_id` is the keyring name. Built only by
/// [`Keyring::envelope`].
///
/// # Synchronous use
///
/// Every `*_sync` method polls its future once. It completes on that poll, because the KEK is in
/// process and the envelope recipe computes inside the poll; a `Pending` (impossible with this
/// KEK) is a `Backend` error, never a wait. A call blocks the thread for the AES passes, needs no
/// runtime and works on wasm32.
#[derive(Clone, Debug)]
pub struct KeyringEnvelope {
    inner: EnvelopeAead<Kek>,
    ring: Keyring,
}

/// Polls once; see [Synchronous use](KeyringEnvelope#synchronous-use).
fn ready<T>(future: impl Future<Output = Result<T>>) -> Result<T> {
    let mut future = std::pin::pin!(future);
    match future
        .as_mut()
        .poll(&mut Context::from_waker(Waker::noop()))
    {
        Poll::Ready(v) => v,
        Poll::Pending => Err(crate::error(
            ErrorKind::Backend,
            Operation::App,
            "KEK future did not complete on its first poll",
        )),
    }
}

impl KeyringEnvelope {
    pub(crate) fn new(ring: &Keyring) -> Self {
        Self {
            inner: EnvelopeAead::new(ring.name(), Kek::new(ring))
                .expect("a keyring name is at most 64 bytes"),
            ring: ring.clone(),
        }
    }

    #[must_use]
    pub fn keyring(&self) -> &Keyring {
        &self.ring
    }

    /// Encrypts under a fresh data key wrapped by the current generation.
    ///
    /// # Errors
    /// `Backend` if the random source fails; `InvalidArgument` for a payload the padding refuses.
    pub async fn encrypt(&self, plaintext: &[u8], aad: &Aad) -> Result<Ciphertext> {
        self.inner
            .clone()
            .with_padding(aad.padding())
            .encrypt(plaintext, aad.as_bytes())
            .await
    }

    /// # Errors
    /// `DecryptFailed` for another keyring's, a tampered or a foreign envelope or another AAD,
    /// with the code `VersionAboveHighWaterMark` or `KeyIdMismatch` where the wrapped key names
    /// a version above the high-water mark or a key id that is not this keyring's;
    /// `KeyVersionUnusable` (`GenerationRemoved`) for a removed generation.
    pub async fn decrypt(&self, ciphertext: &Ciphertext, aad: &Aad) -> Result<Zeroizing<Vec<u8>>> {
        self.inner.decrypt(ciphertext, aad.as_bytes()).await
    }

    /// Rewraps the data key under the current generation in this process (`rewrap_local`: unwrap,
    /// check the key commitment, wrap). The payload and the data key are unchanged, so this is
    /// rotation hygiene, never compromise recovery (see [`reencrypt`](Self::reencrypt)).
    /// Deterministic: rewrapping an envelope that is already current returns the same bytes.
    ///
    /// # Errors
    /// As [`decrypt`](Self::decrypt), for the wrapped key and the key commitment only.
    pub async fn rewrap_current(&self, ciphertext: &Ciphertext) -> Result<Ciphertext> {
        self.inner.rewrap_local(ciphertext).await
    }

    /// [`rewrap_current`](Self::rewrap_current) that first opens the payload under `aad`, so an
    /// envelope copied from another row or a corrupted payload fails instead of being rewrapped.
    ///
    /// # Errors
    /// As [`decrypt`](Self::decrypt).
    pub async fn rewrap_verified(&self, ciphertext: &Ciphertext, aad: &Aad) -> Result<Ciphertext> {
        self.inner.rewrap_verified(ciphertext, aad.as_bytes()).await
    }

    /// Decrypts and encrypts again under a fresh data key and the current generation: the
    /// recovery tool after a descriptor leak, since every rewrap keeps the exposed data key.
    ///
    /// # Errors
    /// As [`decrypt`](Self::decrypt) and [`encrypt`](Self::encrypt).
    pub async fn reencrypt(&self, ciphertext: &Ciphertext, aad: &Aad) -> Result<Ciphertext> {
        let plaintext = self.decrypt(ciphertext, aad).await?;
        self.encrypt(&plaintext, aad).await
    }

    /// The generation an envelope's data key is wrapped under, read from the unauthenticated
    /// wrapped-key header.
    ///
    /// # Errors
    /// `InvalidArgument` for bytes that are no envelope or no kekring wrapped key;
    /// `DecryptFailed` for an envelope of another keyring (`kek_id`).
    pub fn ciphertext_version(&self, ciphertext: &Ciphertext) -> Result<u32> {
        let env = Envelope::parse(ciphertext.as_bytes())?;
        if env.kek_id() != self.ring.name().as_bytes() {
            return Err(crate::error(
                ErrorKind::DecryptFailed,
                Operation::Decrypt,
                "envelope kek_id is not this keyring's name",
            ));
        }
        blob_version(env.wrapped_dek().as_bytes()).ok_or_else(|| {
            crate::error(
                ErrorKind::InvalidArgument,
                Operation::Decrypt,
                "not a kekring wrapped key",
            )
        })
    }

    /// Whether an envelope's wrapped key names a generation other than the current one: selects
    /// rows for [`rewrap_current`](Self::rewrap_current) without authenticating them (anyone who
    /// can write a row can make it name another version; the rewrap itself checks).
    ///
    /// # Errors
    /// As [`ciphertext_version`](Self::ciphertext_version).
    pub fn needs_rewrap(&self, ciphertext: &Ciphertext) -> Result<bool> {
        Ok(self.ciphertext_version(ciphertext)? != self.ring.current_version())
    }

    /// [`encrypt`](Self::encrypt), synchronously.
    ///
    /// # Errors
    /// As [`encrypt`](Self::encrypt).
    pub fn encrypt_sync(&self, plaintext: &[u8], aad: &Aad) -> Result<Ciphertext> {
        ready(self.encrypt(plaintext, aad))
    }

    /// [`decrypt`](Self::decrypt), synchronously.
    ///
    /// # Errors
    /// As [`decrypt`](Self::decrypt).
    pub fn decrypt_sync(&self, ciphertext: &Ciphertext, aad: &Aad) -> Result<Zeroizing<Vec<u8>>> {
        ready(self.decrypt(ciphertext, aad))
    }

    /// [`rewrap_current`](Self::rewrap_current), synchronously.
    ///
    /// # Errors
    /// As [`rewrap_current`](Self::rewrap_current).
    pub fn rewrap_current_sync(&self, ciphertext: &Ciphertext) -> Result<Ciphertext> {
        ready(self.rewrap_current(ciphertext))
    }

    /// [`rewrap_verified`](Self::rewrap_verified), synchronously.
    ///
    /// # Errors
    /// As [`rewrap_verified`](Self::rewrap_verified).
    pub fn rewrap_verified_sync(&self, ciphertext: &Ciphertext, aad: &Aad) -> Result<Ciphertext> {
        ready(self.rewrap_verified(ciphertext, aad))
    }

    /// [`reencrypt`](Self::reencrypt), synchronously.
    ///
    /// # Errors
    /// As [`reencrypt`](Self::reencrypt).
    pub fn reencrypt_sync(&self, ciphertext: &Ciphertext, aad: &Aad) -> Result<Ciphertext> {
        ready(self.reencrypt(ciphertext, aad))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pending_is_backend_error_without_blocking() {
        let e: anykms_core::Error = ready::<()>(std::future::pending()).unwrap_err();
        assert_eq!(e.kind(), ErrorKind::Backend);
    }
}
