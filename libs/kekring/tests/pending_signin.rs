use anykms_recipes::envelope::Padding;
use kekring::{ContextEnvelopeSync, Keyring};
use kekring::{EncryptionContext, purposes};

#[test]
fn additive_pending_signin_purpose_has_stable_context_and_rejects_substitution() {
    let ring = Keyring::builder("p1")
        .generation(1, [7; 32])
        .unwrap()
        .current(1)
        .unwrap()
        .build()
        .unwrap();
    let pending = EncryptionContext::try_from(&purposes::PendingSignIn {
        session_id: "sid-a",
    })
    .unwrap();
    assert_eq!(
        pending.as_aad(),
        b"anykms-ctx/v1\0\x02\x07purpose\0\x0epending-signin\x0asession_id\0\x05sid-a"
    );
    let session = EncryptionContext::try_from(&purposes::Session {
        session_id: "sid-a",
    })
    .unwrap();
    // Existing purpose encoding must remain byte-for-byte unchanged.
    assert_eq!(
        session.as_aad(),
        b"anykms-ctx/v1\0\x02\x07purpose\0\x07session\x0asession_id\0\x05sid-a"
    );
    let other = EncryptionContext::try_from(&purposes::PendingSignIn {
        session_id: "sid-b",
    })
    .unwrap();
    let envelope = ring.context_envelope(Padding::NONE).unwrap();
    let ciphertext = envelope
        .encrypt_sync(b"STATE_MARKER PKCE_MARKER", &pending)
        .unwrap();
    assert_eq!(
        &*envelope.decrypt_sync(&ciphertext, &pending).unwrap(),
        b"STATE_MARKER PKCE_MARKER"
    );
    assert!(envelope.decrypt_sync(&ciphertext, &session).is_err());
    assert!(envelope.decrypt_sync(&ciphertext, &other).is_err());
    let old = envelope.encrypt_sync(b"session record", &session).unwrap();
    assert!(envelope.decrypt_sync(&old, &pending).is_err());
}
