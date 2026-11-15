#![forbid(unsafe_code)]
#![doc = include_str!("../README.md")]
pub mod aad;
mod envelope;
mod kek;
mod keyring;
mod provider;
mod transition;

pub use aad::{Aad, AadBuilder, Purpose};
/// The anykms types in kekring's API, so a crate that only declares purposes needs no anykms
/// dependency of its own.
pub use anykms_core::{Ciphertext, Error, ErrorKind};
pub use anykms_recipes::envelope::Padding;
pub use envelope::KeyringEnvelope;
pub use kek::{BLOB_LEN, DEK_LEN, Kek};
pub use keyring::{Fingerprint, Keyring, KeyringBuilder, MAX_GENERATIONS, MAX_NAME_LEN};
pub use provider::{KekringProvider, ProviderKey};
pub use transition::{TransitionError, validate_transition};

use anykms_core::{Operation, Result};

/// The wire labels: one-way contracts, specified in `docs/kekring-format.md`. Changing any of
/// them makes every stored envelope unreadable.
pub mod labels {
    /// The backend name in errors, and the provider's URI scheme.
    pub const SCHEME: &str = "kekring";
    /// The descriptor's first field: the descriptor format version.
    pub const DESCRIPTOR_PREFIX: &str = "kekring1";
    /// The first four bytes of a wrapped key: the blob format version.
    pub const BLOB_MAGIC: [u8; 4] = *b"krw1";
    /// HKDF-Extract salt (constant; the keyring name in each `info` separates keyrings).
    pub const SALT: &[u8] = b"kekring/v1/salt";
    /// HKDF-Expand label of a generation's KEK.
    pub const KEK: &[u8] = b"kekring/v1/kek";
    /// HKDF-Expand label of a generation's key id.
    pub const KID: &[u8] = b"kekring/v1/kid";
    /// Hash prefix of the fingerprint.
    pub const FP: &[u8] = b"kekring/v1/fp";
}

pub(crate) fn error(kind: ErrorKind, op: Operation, msg: impl Into<String>) -> Error {
    Error::new(kind, labels::SCHEME, op, msg.into())
}

/// The stable [`BackendDetail::code`](anykms_core::BackendDetail::code)s of unwrap failures:
/// part of this backend's contract. They survive `EnvelopeAead::decrypt`, and none comes with a
/// retry hint (the descriptor is immutable for the process, so retrying in process never helps).
///
/// | Condition | Kind | Code |
/// |---|---|---|
/// | version above the descriptor's high-water mark | `DecryptFailed` | `VersionAboveHighWaterMark` |
/// | version at or below it, not listed (removed) | `KeyVersionUnusable` | `GenerationRemoved` |
/// | key id mismatch for a listed version | `DecryptFailed` | `KeyIdMismatch` |
/// | wrong magic or length, failed integrity check | `DecryptFailed` | none |
///
/// An application that must tolerate rollout skew (a reader deployed with an older descriptor
/// than a writer) branches on [`Code::VersionAboveHighWaterMark`], not on the kind.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Code {
    VersionAboveHighWaterMark,
    GenerationRemoved,
    KeyIdMismatch,
}

impl Code {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::VersionAboveHighWaterMark => "VersionAboveHighWaterMark",
            Self::GenerationRemoved => "GenerationRemoved",
            Self::KeyIdMismatch => "KeyIdMismatch",
        }
    }

    /// The code of an error this backend raised, through any layer that keeps the detail and
    /// the backend name (the envelope recipe does).
    #[must_use]
    pub fn of(error: &Error) -> Option<Self> {
        if error.backend() != labels::SCHEME {
            return None;
        }
        match error.detail()?.code()? {
            "VersionAboveHighWaterMark" => Some(Self::VersionAboveHighWaterMark),
            "GenerationRemoved" => Some(Self::GenerationRemoved),
            "KeyIdMismatch" => Some(Self::KeyIdMismatch),
            _ => None,
        }
    }
}

impl std::fmt::Display for Code {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Keyring {
    /// The keyring as an anykms key-wrapping key.
    #[must_use]
    pub fn kek(&self) -> Kek {
        Kek::new(self)
    }

    /// The envelope applications encrypt with.
    #[must_use]
    pub fn envelope(&self) -> KeyringEnvelope {
        KeyringEnvelope::new(self)
    }
}
