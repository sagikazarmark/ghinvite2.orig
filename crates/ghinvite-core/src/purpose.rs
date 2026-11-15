//! The records ghinvite encrypts under its keyrings, each a [`kekring::Purpose`]: the canonical
//! AAD purpose name, the fields that bind a ciphertext to its row and the envelope padding. The
//! AAD of a record is built with `kekring::Aad::try_from(&record)`; every value must be
//! recoverable from trusted row data before decrypting.
//!
//! The names and field keys are part of every stored envelope's AAD: renaming one makes those
//! envelopes unreadable. A new record shape is a new purpose (`session-v2`), never an edit.
use kekring::{Aad, Padding, Purpose};
use std::fmt;

/// An encrypted session record, bound to its storage key.
pub struct Session<'a> {
    pub session_id: &'a str,
}

/// The encrypted PKCE/state of a sign-in in progress, bound to the trusted session ID.
pub struct PendingSignIn<'a> {
    pub session_id: &'a str,
}

/// An attempt continuation, bound to its plaintext scope and binding columns.
pub struct AttemptContinuation<'a> {
    pub scope: &'a str,
    pub binding: &'a str,
}

/// A recovery address in the email vault, bound to the vault row identity and batch manifest.
pub struct BulkEmail<'a> {
    pub email_ref: &'a str,
    pub batch_id: &'a str,
    pub row: u64,
    pub account_id: u64,
    pub installation_id: u64,
}

/// A reviewer-visible address in review escrow; same identity as [`BulkEmail`], another purpose.
pub struct BulkEmailReview<'a> {
    pub email_ref: &'a str,
    pub batch_id: &'a str,
    pub row: u64,
    pub account_id: u64,
    pub installation_id: u64,
}

impl Purpose for Session<'_> {
    const NAME: &'static str = "session";
    const FIELDS: &'static [&'static str] = &["session_id"];
}

impl Purpose for PendingSignIn<'_> {
    const NAME: &'static str = "pending-signin";
    const FIELDS: &'static [&'static str] = &["session_id"];
}

impl Purpose for AttemptContinuation<'_> {
    const NAME: &'static str = "attempt-continuation";
    const FIELDS: &'static [&'static str] = &["scope", "binding"];
}

const EMAIL_FIELDS: &[&str] = &[
    "email_ref",
    "batch_id",
    "row",
    "account_id",
    "installation_id",
];

impl Purpose for BulkEmail<'_> {
    const NAME: &'static str = "bulk-email";
    const FIELDS: &'static [&'static str] = EMAIL_FIELDS;
    const PADDING: Padding = Padding::block(256);
}

impl Purpose for BulkEmailReview<'_> {
    const NAME: &'static str = "bulk-email-review";
    const FIELDS: &'static [&'static str] = EMAIL_FIELDS;
    const PADDING: Padding = Padding::block(256);
}

impl TryFrom<&Session<'_>> for Aad {
    type Error = kekring::Error;
    fn try_from(v: &Session<'_>) -> Result<Self, Self::Error> {
        Aad::builder::<Session>()
            .str("session_id", v.session_id)
            .build()
    }
}

impl TryFrom<&PendingSignIn<'_>> for Aad {
    type Error = kekring::Error;
    fn try_from(v: &PendingSignIn<'_>) -> Result<Self, Self::Error> {
        Aad::builder::<PendingSignIn>()
            .str("session_id", v.session_id)
            .build()
    }
}

impl TryFrom<&AttemptContinuation<'_>> for Aad {
    type Error = kekring::Error;
    fn try_from(v: &AttemptContinuation<'_>) -> Result<Self, Self::Error> {
        Aad::builder::<AttemptContinuation>()
            .str("scope", v.scope)
            .str("binding", v.binding)
            .build()
    }
}

impl TryFrom<&BulkEmail<'_>> for Aad {
    type Error = kekring::Error;
    fn try_from(v: &BulkEmail<'_>) -> Result<Self, Self::Error> {
        Aad::builder::<BulkEmail>()
            .str("email_ref", v.email_ref)
            .str("batch_id", v.batch_id)
            .u64("row", v.row)
            .u64("account_id", v.account_id)
            .u64("installation_id", v.installation_id)
            .build()
    }
}

impl TryFrom<&BulkEmailReview<'_>> for Aad {
    type Error = kekring::Error;
    fn try_from(v: &BulkEmailReview<'_>) -> Result<Self, Self::Error> {
        Aad::builder::<BulkEmailReview>()
            .str("email_ref", v.email_ref)
            .str("batch_id", v.batch_id)
            .u64("row", v.row)
            .u64("account_id", v.account_id)
            .u64("installation_id", v.installation_id)
            .build()
    }
}

/// Identifiers are bound, not shown.
macro_rules! debug_purpose_only {
    ($($ty:ident),*) => {$(
        impl fmt::Debug for $ty<'_> {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.debug_struct(stringify!($ty))
                    .field("purpose", &<Self as Purpose>::NAME)
                    .finish_non_exhaustive()
            }
        }
    )*};
}
debug_purpose_only!(
    Session,
    PendingSignIn,
    AttemptContinuation,
    BulkEmail,
    BulkEmailReview
);

#[cfg(test)]
mod tests {
    use super::*;

    /// The bytes every stored envelope of these purposes was written with.
    #[test]
    fn aad_bytes_are_pinned() {
        let session = Aad::try_from(&Session {
            session_id: "sid-a",
        })
        .unwrap();
        assert_eq!(
            session.as_bytes(),
            b"anykms/aad/v1\0\0\x02\0\x07purpose\0\x07session\0\x0asession_id\0\x05sid-a"
        );
        let pending = Aad::try_from(&PendingSignIn {
            session_id: "sid-a",
        })
        .unwrap();
        assert_eq!(
            pending.as_bytes(),
            b"anykms/aad/v1\0\0\x02\0\x07purpose\0\x0epending-signin\0\x0asession_id\0\x05sid-a"
        );
        let continuation = Aad::try_from(&AttemptContinuation {
            scope: "invite",
            binding: "row",
        })
        .unwrap();
        assert_eq!(
            continuation.as_bytes(),
            b"anykms/aad/v1\0\0\x03\0\x07binding\0\x03row\0\x07purpose\0\x14attempt-continuation\0\x05scope\0\x06invite"
        );
        let email = |review: bool| {
            let (email_ref, batch_id, row, account_id, installation_id) = ("e-1", "b-1", 2, 3, 4);
            if review {
                Aad::try_from(&BulkEmailReview {
                    email_ref,
                    batch_id,
                    row,
                    account_id,
                    installation_id,
                })
            } else {
                Aad::try_from(&BulkEmail {
                    email_ref,
                    batch_id,
                    row,
                    account_id,
                    installation_id,
                })
            }
            .unwrap()
        };
        assert_eq!(
            email(false).as_bytes(),
            b"anykms/aad/v1\0\0\x06\0\x0aaccount_id\0\x013\0\x08batch_id\0\x03b-1\0\x09email_ref\0\x03e-1\0\x0finstallation_id\0\x014\0\x07purpose\0\x0abulk-email\0\x03row\0\x012"
        );
        assert_ne!(email(false).as_bytes(), email(true).as_bytes());
        assert_eq!(email(true).padding(), Padding::block(256));
        assert_eq!(session.padding(), Padding::NONE);
    }

    #[test]
    fn purposes_never_share_an_aad() {
        let session = Aad::try_from(&Session { session_id: "x" }).unwrap();
        let pending = Aad::try_from(&PendingSignIn { session_id: "x" }).unwrap();
        assert_ne!(session.as_bytes(), pending.as_bytes());
        assert!(Aad::try_from(&Session { session_id: "" }).is_err());
        assert!(
            !format!(
                "{:?}",
                Session {
                    session_id: "secret-id"
                }
            )
            .contains("secret-id")
        );
    }
}
