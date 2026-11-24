//! The app's private key: the [`AppKeySigner`] seam and its local
//! implementation, [`SigningKey`].

use crate::error::{ConfigError, SigningError};
use rsa::PrivateKey;
use rsa::pkcs1::DecodeRsaPrivateKey;
use rsa::pkcs1v15;
use rsa::pkcs8::DecodePrivateKey;
use rsa::signature::{SignatureEncoding, Signer as _};
use rsa::traits::PublicKeyParts;
use sha2::Sha256;
use zeroize::Zeroizing;

const MAX_PEM_BYTES: usize = 16 * 1024;

/// Signs with the GitHub App's private key. [`SigningKey`] is the local PEM implementation; implement this trait to keep the key in a
/// KMS or HSM, and pass it to [`App::new`](crate::App::new) or
/// [`App::builder`](crate::App::builder).
///
/// An implementation MUST return an RSASSA-PKCS1-v1_5 signature over SHA-256
/// (RS256) of the given bytes, made with the app's 2048–4096-bit RSA key. A
/// signature that is not 256–512 bytes long is rejected locally with
/// [`SigningError::Failed`]; any other algorithm (PSS, SHA-384/512, ECDSA, …) is
/// rejected by GitHub as [`Error::AppCredentialsRejected`](crate::Error::AppCredentialsRejected).
///
/// (What gets signed is the app's credential, a JWT that authenticates the app
/// to GitHub; an implementation never needs to inspect it.)
///
/// The method is named `sign_rs256` rather than `sign` to state the algorithm at
/// every implementation and to avoid ambiguity with `signature::Signer::sign`.
pub trait AppKeySigner: Send + Sync {
    /// Sign `signing_input` with RS256 and return the raw signature bytes, not
    /// base64-encoded.
    fn sign_rs256(&self, signing_input: &[u8]) -> Result<Vec<u8>, SigningError>;
}

impl<T: AppKeySigner + ?Sized> AppKeySigner for std::sync::Arc<T> {
    fn sign_rs256(&self, signing_input: &[u8]) -> Result<Vec<u8>, SigningError> {
        (**self).sign_rs256(signing_input)
    }
}

impl<T: AppKeySigner + ?Sized> AppKeySigner for Box<T> {
    fn sign_rs256(&self, signing_input: &[u8]) -> Result<Vec<u8>, SigningError> {
        (**self).sign_rs256(signing_input)
    }
}

/// A validated 2048–4096-bit RSA private key, ready for RS256 signing.
/// Not `Clone`; `Debug` is redacted; zeroized on drop.
///
/// GitHub issues PKCS#1 PEMs (`BEGIN RSA PRIVATE KEY`); PKCS#8
/// (`BEGIN PRIVATE KEY`) is accepted too.
///
/// This wraps `rsa::pkcs1v15::SigningKey` rather than exposing it, because
/// `rsa`'s key types derive `Debug` (printing the private exponent and primes)
/// and `Clone`, and so that `rsa` stays out of this crate's public API.
pub struct SigningKey {
    inner: pkcs1v15::SigningKey<Sha256>,
}

impl SigningKey {
    /// Parse and validate a PKCS#8 or PKCS#1 PEM. Errors never include key text.
    pub fn from_pem(pem: &str) -> Result<Self, ConfigError> {
        if pem.len() > MAX_PEM_BYTES {
            return Err(ConfigError::new("signing_key", "length"));
        }
        let pem = Zeroizing::new(pem.trim().to_owned());
        let key = PrivateKey::from_pkcs8_pem(&pem)
            .or_else(|_| PrivateKey::from_pkcs1_pem(&pem))
            .map_err(|_| ConfigError::new("signing_key", "pem"))?;
        if !(2048..=4096).contains(&key.n().bit_width()) {
            return Err(ConfigError::new("signing_key", "modulus"));
        }
        key.validate()
            .map_err(|_| ConfigError::new("signing_key", "key"))?;
        Ok(Self {
            inner: pkcs1v15::SigningKey::new(key),
        })
    }

    /// Signature length in bytes (the modulus size: 256 for GitHub's 2048-bit keys).
    pub fn signature_len(&self) -> usize {
        self.rsa().size()
    }

    fn rsa(&self) -> &PrivateKey {
        self.inner.as_ref()
    }

    /// The public half, for verifying signatures in tests.
    #[cfg(test)]
    pub(crate) fn public_key(&self) -> rsa::PublicKey {
        self.rsa().to_public_key()
    }
}

impl AppKeySigner for SigningKey {
    fn sign_rs256(&self, signing_input: &[u8]) -> Result<Vec<u8>, SigningError> {
        self.inner
            .try_sign(signing_input)
            .map(|signature| signature.to_vec())
            .map_err(|_| SigningError::Failed)
    }
}

impl std::fmt::Debug for SigningKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SigningKey(..)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsa::pkcs1::EncodeRsaPrivateKey;
    use rsa::signature::Verifier as _;

    const PEM: &str = include_str!("../../tests/fixtures/rsa2048.pem");

    const SMALL: &str = include_str!("../../tests/fixtures/rsa1024.pem");

    #[test]
    fn accepts_pkcs8_and_pkcs1() {
        let key = SigningKey::from_pem(PEM).unwrap();
        assert_eq!(key.signature_len(), 256);
        let pkcs1 = key.rsa().to_pkcs1_pem(rsa::pkcs8::LineEnding::LF).unwrap();
        assert!(pkcs1.starts_with("-----BEGIN RSA PRIVATE KEY-----"));
        assert_eq!(SigningKey::from_pem(&pkcs1).unwrap().signature_len(), 256);
    }

    #[test]
    fn rejects_bad_input_without_echoing_it() {
        for (input, what) in [
            ("not a pem", "pem"),
            (SMALL, "modulus"),
            (&"x".repeat(MAX_PEM_BYTES + 1), "length"),
        ] {
            let error = SigningKey::from_pem(input).unwrap_err();
            assert_eq!((error.field(), error.what()), ("signing_key", what));
            assert!(!format!("{error} {error:?}").contains("MII"));
        }
    }

    #[test]
    fn signs_rs256_verifiably() {
        let key = SigningKey::from_pem(PEM).unwrap();
        let signature = key.sign_rs256(b"header.claims").unwrap();
        assert_eq!(signature.len(), key.signature_len());
        // PKCS#1 v1.5 is deterministic.
        assert_eq!(signature, key.sign_rs256(b"header.claims").unwrap());
        let verifier = pkcs1v15::VerifyingKey::<Sha256>::new(key.public_key());
        let signature = pkcs1v15::Signature::try_from(signature.as_slice()).unwrap();
        assert!(verifier.verify(b"header.claims", &signature).is_ok());
        assert!(verifier.verify(b"header.claimz", &signature).is_err());
    }

    #[test]
    fn arc_and_box_delegate() {
        struct Fixed(u8);
        impl AppKeySigner for Fixed {
            fn sign_rs256(&self, _: &[u8]) -> Result<Vec<u8>, SigningError> {
                Ok(vec![self.0; 256])
            }
        }
        let arc: std::sync::Arc<dyn AppKeySigner> = std::sync::Arc::new(Fixed(7));
        let boxed: Box<dyn AppKeySigner> = Box::new(Fixed(8));
        assert_eq!(arc.sign_rs256(b"x").unwrap(), vec![7; 256]);
        assert_eq!(boxed.sign_rs256(b"x").unwrap(), vec![8; 256]);
    }

    #[test]
    fn debug_is_redacted() {
        assert_eq!(
            format!("{:?}", SigningKey::from_pem(PEM).unwrap()),
            "SigningKey(..)"
        );
    }
}
