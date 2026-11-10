use anykms_core::{Ciphertext, ErrorKind};
use anykms_keyring::{ContextEnvelopeSync, Keyring};
use anykms_keyring::{EncryptionContext, purposes};
use anykms_recipes::{Envelope, envelope::Padding};
fn ring(current: u32, versions: &[u32]) -> Keyring {
    let mut b = Keyring::builder("tests");
    for v in versions {
        b = b.generation(*v, [u8::try_from(*v).unwrap(); 32]).unwrap();
    }
    b.current(current).unwrap().build().unwrap()
}
fn ctx(id: &str) -> EncryptionContext {
    EncryptionContext::try_from(&purposes::Session { session_id: id }).unwrap()
}
#[test]
fn rotation_and_context_binding() {
    let old = ring(1, &[1]);
    let reader = ring(1, &[1, 2]);
    let promoted = ring(2, &[1, 2]);
    let removed = ring(2, &[2]);
    let context = ctx("session-a");
    let old_envelope = old.context_envelope(Padding::NONE).unwrap();
    let reader_envelope = reader.context_envelope(Padding::NONE).unwrap();
    let promoted_envelope = promoted.context_envelope(Padding::NONE).unwrap();
    let removed_envelope = removed.context_envelope(Padding::NONE).unwrap();
    let ct = old_envelope.encrypt_sync(b"secret", &context).unwrap();
    assert_eq!(
        &*reader_envelope.decrypt_sync(&ct, &context).unwrap(),
        b"secret"
    );
    assert!(promoted.needs_rewrap(&ct).unwrap());
    let moved = promoted_envelope.rewrap_current_sync(&ct).unwrap();
    assert!(!promoted.needs_rewrap(&moved).unwrap());
    assert_eq!(
        Envelope::parse(ct.as_bytes()).unwrap().payload(),
        Envelope::parse(moved.as_bytes()).unwrap().payload()
    );
    assert_eq!(
        promoted_envelope
            .ciphertext_version(&moved)
            .unwrap()
            .unwrap()
            .as_str(),
        "2"
    );
    assert_eq!(
        &*removed_envelope.decrypt_sync(&moved, &context).unwrap(),
        b"secret"
    );
    assert_eq!(
        removed_envelope
            .decrypt_sync(&ct, &context)
            .unwrap_err()
            .kind(),
        ErrorKind::KeyVersionUnusable
    );
    let error = old_envelope.decrypt_sync(&moved, &context).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::UnknownKeyVersion);
    assert_eq!(error.retry_hint(), Some(std::time::Duration::from_secs(30)));
    assert_eq!(
        promoted_envelope
            .decrypt_sync(&ct, &ctx("session-b"))
            .unwrap_err()
            .kind(),
        ErrorKind::DecryptFailed
    );
    let different = Keyring::builder("tests")
        .generation(1, [9; 32])
        .unwrap()
        .current(1)
        .unwrap()
        .build()
        .unwrap();
    assert_eq!(
        different
            .context_envelope(Padding::NONE)
            .unwrap()
            .decrypt_sync(&ct, &context)
            .unwrap_err()
            .kind(),
        ErrorKind::Configuration
    );
    let second = old_envelope.encrypt_sync(b"other", &context).unwrap();
    let slot = Envelope::parse(second.as_bytes()).unwrap().wrapped_dek();
    let original = Envelope::parse(ct.as_bytes()).unwrap().wrapped_dek();
    let pos = ct
        .as_bytes()
        .windows(original.as_bytes().len())
        .position(|reader_envelope| reader_envelope == original.as_bytes())
        .unwrap();
    let mut tampered = ct.as_bytes().to_vec();
    tampered[pos..pos + 76].copy_from_slice(slot.as_bytes());
    let swapped = Ciphertext::from(tampered);
    assert_eq!(
        promoted_envelope
            .rewrap_current_sync(&swapped)
            .unwrap_err()
            .kind(),
        ErrorKind::DecryptFailed
    );
}
#[test]
fn padding_and_constraints() {
    let k = ring(1, &[1]);
    let e = k.context_envelope(Padding::block(64)).unwrap();
    let c = EncryptionContext::try_from(&purposes::UserToken {
        app_client_id: "Iv1",
        user_id: 7,
    })
    .unwrap();
    for n in 0..=600 {
        let ct = e.encrypt_sync(&vec![0; n], &c).unwrap();
        assert_eq!(
            (Envelope::parse(ct.as_bytes()).unwrap().payload().len() - 16) % 64,
            0
        );
        assert_eq!(e.decrypt_sync(&ct, &c).unwrap().len(), n);
    }
    assert_eq!(
        e.encrypt_sync(b"x", &ctx("x")).unwrap_err().kind(),
        ErrorKind::InvalidArgument
    );
    for value in ["007", "+7", " 7", "18446744073709551616"] {
        assert!(
            EncryptionContext::builder::<purposes::UserToken>()
                .str("app_client_id", "x")
                .str("user_id", value)
                .build()
                .is_err()
        );
    }
    assert!(
        EncryptionContext::builder::<purposes::UserToken>()
            .u64("app_client_id", 1)
            .u64("user_id", 1)
            .build()
            .is_err()
    );
    assert!(EncryptionContext::try_from(&purposes::Session { session_id: "" }).is_err());
    assert!(
        EncryptionContext::try_from(&purposes::Session {
            session_id: &"x".repeat(256)
        })
        .is_err()
    );
}
#[test]
fn compromise_requires_new_dek() {
    let c = ctx("x");
    let compromised = ring(1, &[1]);
    let rotated = ring(2, &[1, 2]);
    let old = compromised
        .context_envelope(Padding::NONE)
        .unwrap()
        .encrypt_sync(b"secret", &c)
        .unwrap();
    let next = rotated.context_envelope(Padding::NONE).unwrap();
    let rewrapped = next.rewrap_current_sync(&old).unwrap();
    assert_eq!(
        Envelope::parse(old.as_bytes()).unwrap().payload(),
        Envelope::parse(rewrapped.as_bytes()).unwrap().payload()
    );
    let fresh = next
        .encrypt_sync(&next.decrypt_sync(&old, &c).unwrap(), &c)
        .unwrap();
    assert_ne!(
        Envelope::parse(old.as_bytes()).unwrap().payload(),
        Envelope::parse(fresh.as_bytes()).unwrap().payload()
    );
}
#[test]
fn fingerprint_changes_and_never_logs_keys() {
    let r = ring(1, &[1]);
    assert_ne!(r.fingerprint(), ring(1, &[1, 2]).fingerprint());
    assert_ne!(
        ring(1, &[1, 2]).fingerprint(),
        ring(2, &[1, 2]).fingerprint()
    );
    assert!(!format!("{r:?}").contains("AQEBAQEB"));
}
anykms_keyring::purpose! {pub struct Optional<'a> ="optional" { text:Option<&'a str>,number:Option<u64>,owned:Option<String> }}
#[test]
fn optional_macro() {
    let c = EncryptionContext::try_from(&Optional {
        text: None,
        number: Some(7),
        owned: None,
    })
    .unwrap();
    assert_eq!(c.get("number"), Some("7"));
    assert_eq!(c.get("text"), None);
}
#[test]
fn every_registry_purpose() {
    let _ = EncryptionContext::try_from(&purposes::InstallationTokenCache {
        app_id: 1,
        installation_id: 2,
        scope: "read@abc",
        expires_at: 3,
    })
    .unwrap();
    let _ = EncryptionContext::try_from(&purposes::AttemptContinuation {
        scope: "invite",
        binding: "row",
    })
    .unwrap();
    let c = EncryptionContext::try_from(&purposes::RestateSealed {
        r#type: "StableType",
    })
    .unwrap();
    assert_eq!(c.get("type"), Some("StableType"));
    assert!(!format!("{c:?}").contains("StableType"));
}
