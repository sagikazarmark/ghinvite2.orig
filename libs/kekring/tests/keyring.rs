use anykms_core::{
    Capabilities, Ciphertext, Describe, ErrorKind, KeyWrap, Registry, Semantics, VersionModel,
};
use anykms_recipes::Envelope;
use kekring::{Aad, Code, KekringProvider, Keyring, Padding, Purpose, aad::Rule};

struct Session<'a> {
    id: &'a str,
}
impl Purpose for Session<'_> {
    const NAME: &'static str = "session";
    const FIELDS: &'static [&'static str] = &["session_id"];
}
impl TryFrom<&Session<'_>> for Aad {
    type Error = anykms_core::Error;
    fn try_from(s: &Session<'_>) -> anykms_core::Result<Self> {
        Aad::builder::<Session>().str("session_id", s.id).build()
    }
}
struct Token;
impl Purpose for Token {
    const NAME: &'static str = "user-token";
    const FIELDS: &'static [&'static str] = &["app_client_id", "user_id"];
    const PADDING: Padding = Padding::block(64);
}

fn ring(current: u32, versions: &[u32]) -> Keyring {
    let mut b = Keyring::builder("tests");
    for v in versions {
        b = b.generation(*v, [u8::try_from(*v).unwrap(); 32]);
    }
    b.current(current).build().unwrap()
}

fn ctx(id: &str) -> Aad {
    Aad::try_from(&Session { id }).unwrap()
}

fn ready<T>(f: impl std::future::Future<Output = T>) -> T {
    let mut f = std::pin::pin!(f);
    match f
        .as_mut()
        .poll(&mut std::task::Context::from_waker(std::task::Waker::noop()))
    {
        std::task::Poll::Ready(v) => v,
        std::task::Poll::Pending => panic!("pending"),
    }
}

#[test]
fn builder_rules() {
    let code = |b: kekring::KeyringBuilder| {
        let e = b.build().unwrap_err();
        assert_eq!(e.kind(), ErrorKind::Configuration);
        e.detail().and_then(|d| d.code()).unwrap().to_owned()
    };
    let one = || Keyring::builder("b").generation(1, [1; 32]);
    assert_eq!(
        code(Keyring::builder("b").current(1)),
        "descriptor:generation_count"
    );
    assert_eq!(code(one()), "descriptor:current");
    assert_eq!(code(one().current(2)), "descriptor:current");
    assert_eq!(
        code(one().generation(1, [2; 32]).current(1)),
        "descriptor:duplicate_version"
    );
    assert_eq!(
        code(one().generation(2, [1; 32]).current(1)),
        "descriptor:duplicate_root"
    );
    assert_eq!(
        code(Keyring::builder("b").generation(0, [1; 32]).current(0)),
        "descriptor:version"
    );
    assert_eq!(
        code(Keyring::builder("b").generation(1, [0; 32]).current(1)),
        "descriptor:zero_root"
    );
    assert_eq!(
        code(one().current(1).high_water_mark(0)),
        "descriptor:high_water_mark"
    );
    assert_eq!(
        code(Keyring::builder("B").generation(1, [1; 32]).current(1)),
        "descriptor:name"
    );
    assert_eq!(
        code(Keyring::builder("a.b").generation(1, [1; 32]).current(1)),
        "descriptor:name"
    );
    let mut many = Keyring::builder("b");
    for v in 1..=65u32 {
        many = many.generation(v, [u8::try_from(v).unwrap(); 32]);
    }
    assert_eq!(code(many.current(1)), "descriptor:generation_count");
    let ring = one().current(1).high_water_mark(9).build().unwrap();
    assert_eq!(ring.high_water_mark(), 9);
    assert_eq!(
        Keyring::parse(&ring.to_descriptor()).unwrap().fingerprint(),
        ring.fingerprint()
    );
    assert!(
        Keyring::builder(&"a".repeat(64))
            .generation(1, [1; 32])
            .current(1)
            .build()
            .is_ok()
    );
    assert!(
        Keyring::builder(&"a".repeat(65))
            .generation(1, [1; 32])
            .current(1)
            .build()
            .is_err()
    );
}

#[test]
fn rotation_and_codes() {
    let old = ring(1, &[1]);
    let reader = ring(1, &[1, 2]);
    let promoted = ring(2, &[1, 2]);
    let removed = ring(2, &[2]);
    let context = ctx("session-a");
    let ct = old.envelope().encrypt_sync(b"secret", &context).unwrap();
    assert_eq!(
        &*reader.envelope().decrypt_sync(&ct, &context).unwrap(),
        b"secret"
    );
    let promoted_env = promoted.envelope();
    assert!(promoted_env.needs_rewrap(&ct).unwrap());
    let moved = promoted_env.rewrap_current_sync(&ct).unwrap();
    assert!(!promoted_env.needs_rewrap(&moved).unwrap());
    assert_eq!(promoted_env.ciphertext_version(&moved).unwrap(), 2);
    assert_eq!(
        Envelope::parse(ct.as_bytes()).unwrap().payload(),
        Envelope::parse(moved.as_bytes()).unwrap().payload()
    );
    assert_eq!(
        &*removed.envelope().decrypt_sync(&moved, &context).unwrap(),
        b"secret"
    );

    // Removed below the high-water mark: KeyVersionUnusable, GenerationRemoved.
    let e = removed.envelope().decrypt_sync(&ct, &context).unwrap_err();
    assert_eq!(e.kind(), ErrorKind::KeyVersionUnusable);
    assert_eq!(Code::of(&e), Some(Code::GenerationRemoved));
    // A lagging reader: DecryptFailed, VersionAboveHighWaterMark, no retry hint.
    let e = old.envelope().decrypt_sync(&moved, &context).unwrap_err();
    assert_eq!(e.kind(), ErrorKind::DecryptFailed);
    assert_eq!(Code::of(&e), Some(Code::VersionAboveHighWaterMark));
    assert_eq!(e.retry_hint(), None);
    assert_eq!(
        e.detail().and_then(|d| d.code()),
        Some("VersionAboveHighWaterMark")
    );
    // Another AAD: DecryptFailed without a code.
    let e = promoted_env
        .decrypt_sync(&ct, &ctx("session-b"))
        .unwrap_err();
    assert_eq!((e.kind(), Code::of(&e)), (ErrorKind::DecryptFailed, None));
    // The same name and version with other root material: KeyIdMismatch, never Configuration.
    let different = Keyring::builder("tests")
        .generation(1, [9; 32])
        .current(1)
        .build()
        .unwrap();
    let e = different
        .envelope()
        .decrypt_sync(&ct, &context)
        .unwrap_err();
    assert_eq!(e.kind(), ErrorKind::DecryptFailed);
    assert_eq!(Code::of(&e), Some(Code::KeyIdMismatch));
    // Rewrap paths report the same codes.
    let e = old.envelope().rewrap_current_sync(&moved).unwrap_err();
    assert_eq!(Code::of(&e), Some(Code::VersionAboveHighWaterMark));
    // A data key slot spliced from another envelope fails the key commitment.
    let second = old.envelope().encrypt_sync(b"other", &context).unwrap();
    let slot = Envelope::parse(second.as_bytes()).unwrap().wrapped_dek();
    let original = Envelope::parse(ct.as_bytes()).unwrap().wrapped_dek();
    let pos = ct
        .as_bytes()
        .windows(kekring::BLOB_LEN)
        .position(|w| w == original.as_bytes())
        .unwrap();
    let mut tampered = ct.as_bytes().to_vec();
    tampered[pos..pos + kekring::BLOB_LEN].copy_from_slice(slot.as_bytes());
    let swapped = Ciphertext::from(tampered);
    assert_eq!(
        promoted_env
            .rewrap_current_sync(&swapped)
            .unwrap_err()
            .kind(),
        ErrorKind::DecryptFailed
    );
    assert_eq!(
        promoted_env
            .rewrap_verified_sync(&swapped, &context)
            .unwrap_err()
            .kind(),
        ErrorKind::DecryptFailed
    );
    // needs_rewrap refuses another keyring's envelope.
    let foreign = Keyring::builder("foreign")
        .generation(1, [3; 32])
        .current(1)
        .build()
        .unwrap();
    let foreign_ct = foreign.envelope().encrypt_sync(b"x", &context).unwrap();
    assert_eq!(
        promoted_env.needs_rewrap(&foreign_ct).unwrap_err().kind(),
        ErrorKind::DecryptFailed
    );
}

#[test]
fn async_and_sync_agree() {
    let env = ring(1, &[1]).envelope();
    let context = ctx("x");
    let a = ready(env.encrypt(b"pt", &context)).unwrap();
    assert_eq!(&*env.decrypt_sync(&a, &context).unwrap(), b"pt");
    let s = env.encrypt_sync(b"pt", &context).unwrap();
    assert_eq!(&*ready(env.decrypt(&s, &context)).unwrap(), b"pt");
    assert_eq!(
        ready(env.rewrap_current(&s)).unwrap(),
        env.rewrap_current_sync(&s).unwrap()
    );
}

#[test]
fn padding_follows_the_purpose() {
    let env = ring(1, &[1]).envelope();
    let c = Aad::builder::<Token>()
        .str("app_client_id", "Iv1")
        .u64("user_id", 7)
        .build()
        .unwrap();
    assert_eq!(c.padding(), Padding::block(64));
    for n in 0..=200 {
        let ct = env.encrypt_sync(&vec![0; n], &c).unwrap();
        assert_eq!(
            (Envelope::parse(ct.as_bytes()).unwrap().payload().len() - 16) % 64,
            0
        );
        assert_eq!(env.decrypt_sync(&ct, &c).unwrap().len(), n);
    }
    let plain = env.encrypt_sync(b"abc", &ctx("x")).unwrap();
    assert_eq!(
        Envelope::parse(plain.as_bytes()).unwrap().payload().len(),
        3 + 16
    );
}

#[test]
fn typed_builder_rules() {
    let rule = |r: anykms_core::Result<Aad>| Rule::of(&r.unwrap_err());
    assert_eq!(
        rule(Aad::builder::<Token>().str("app_client_id", "x").build()),
        Some(Rule::MissingField)
    );
    assert_eq!(
        rule(
            Aad::builder::<Token>()
                .str("app_client_id", "x")
                .u64("user_id", 1)
                .str("extra", "y")
                .build()
        ),
        Some(Rule::UndeclaredField)
    );
    assert_eq!(
        rule(
            Aad::builder::<Token>()
                .str("app_client_id", "x")
                .u64("user_id", 1)
                .u64("user_id", 2)
                .build()
        ),
        Some(Rule::DuplicateKey)
    );
    assert_eq!(
        rule(Aad::try_from(&Session { id: "" })),
        Some(Rule::EmptyValue)
    );
    assert_eq!(
        rule(Aad::try_from(&Session {
            id: &"x".repeat(256)
        })),
        Some(Rule::ValueTooLong)
    );
    assert_eq!(
        rule(Aad::try_from(&Session { id: "a\nb" })),
        Some(Rule::ValueControlChar)
    );
    let e = Aad::try_from(&Session {
        id: "SECRET\u{7}VALUE",
    })
    .unwrap_err();
    assert_eq!(e.kind(), ErrorKind::InvalidArgument);
    assert!(!format!("{e:?} {e}").contains("SECRET"));
    let aad = ctx("sid-a");
    assert_eq!(
        aad.as_bytes(),
        b"anykms/aad/v1\0\0\x02\0\x07purpose\0\x07session\0\x0asession_id\0\x05sid-a"
    );
    assert!(!format!("{aad:?}").contains("sid-a"));
}

#[test]
fn compromise_requires_reencrypt() {
    let c = ctx("x");
    let compromised = ring(1, &[1]);
    let rotated = ring(2, &[1, 2]).envelope();
    let old = compromised.envelope().encrypt_sync(b"secret", &c).unwrap();
    let rewrapped = rotated.rewrap_current_sync(&old).unwrap();
    assert_eq!(
        Envelope::parse(old.as_bytes()).unwrap().payload(),
        Envelope::parse(rewrapped.as_bytes()).unwrap().payload()
    );
    let fresh = rotated.reencrypt_sync(&old, &c).unwrap();
    assert_ne!(
        Envelope::parse(old.as_bytes()).unwrap().payload(),
        Envelope::parse(fresh.as_bytes()).unwrap().payload()
    );
    assert_eq!(rotated.ciphertext_version(&fresh).unwrap(), 2);
    assert_eq!(&*rotated.decrypt_sync(&fresh, &c).unwrap(), b"secret");
    // After the leaked generation is removed, only the re-encrypted row reads.
    let clean = Keyring::builder("tests")
        .generation(2, [2; 32])
        .current(2)
        .high_water_mark(2)
        .build()
        .unwrap();
    assert!(clean.envelope().decrypt_sync(&fresh, &c).is_ok());
    assert_eq!(
        clean.envelope().decrypt_sync(&old, &c).unwrap_err().kind(),
        ErrorKind::KeyVersionUnusable
    );
}

#[test]
fn identifiers_only_in_debug_and_fingerprint() {
    let r = ring(1, &[1]);
    let root_b64 = "AQEBAQEB";
    assert!(!format!("{r:?}").contains(root_b64));
    assert!(!format!("{:?}", r.kek()).contains(root_b64));
    assert!(!format!("{:?}", r.envelope()).contains(root_b64));
    assert!(!format!("{:?}", Keyring::builder("x").generation(1, [1; 32])).contains("1, 1, 1"));
    assert!(r.to_descriptor().contains(root_b64));
    assert_ne!(r.fingerprint(), ring(1, &[1, 2]).fingerprint());
    assert_ne!(
        ring(1, &[1, 2]).fingerprint(),
        ring(2, &[1, 2]).fingerprint()
    );
    let hwm = Keyring::builder("tests")
        .generation(1, [1; 32])
        .current(1)
        .high_water_mark(2)
        .build()
        .unwrap();
    assert_ne!(r.fingerprint(), hwm.fingerprint());
    assert_eq!(
        hwm.fingerprint().to_string(),
        format!(
            "tests current=1 hwm=2 versions=1 fp={}",
            hwm.fingerprint().digest
        )
    );
}

#[test]
fn kek_contract() {
    let r = ring(2, &[1, 2]);
    let kek = r.kek();
    for len in [0, 16, 24, 31, 33, 40, 64] {
        assert_eq!(
            ready(kek.wrap_key(&vec![1; len])).unwrap_err().kind(),
            ErrorKind::InvalidArgument
        );
    }
    let info = ready(kek.describe()).unwrap();
    assert_eq!(info.capabilities(), Capabilities::KEY_WRAP);
    assert_eq!(info.primary_version().map(|v| v.as_str()), Some("2"));
    assert_eq!(info.versions().len(), 2);
    let s = kek.semantics().unwrap();
    assert_eq!(s.version_model(), VersionModel::Floating);
    assert!(s.ciphertext_reveals_version());
    let blob = ready(kek.wrap_key(&[5; 32])).unwrap();
    assert_eq!(
        kek.wrapped_key_version(&blob).unwrap().unwrap().as_str(),
        "2"
    );
    assert_eq!(
        kek.wrapped_key_version(&Ciphertext::from(vec![0; 56]))
            .unwrap_err()
            .kind(),
        ErrorKind::InvalidArgument
    );
}

#[test]
fn provider_through_a_registry() {
    let a = ring(1, &[1]);
    let b = Keyring::builder("other")
        .generation(1, [1; 32])
        .current(1)
        .build()
        .unwrap();
    let provider = KekringProvider::new([a.clone(), b]).unwrap();
    assert!(KekringProvider::new([a.clone(), a.clone()]).is_err());
    let registry = Registry::builder().provider(provider).build().unwrap();
    let kek = registry.key_wrap("kekring:tests").unwrap();
    let blob = ready(kek.wrap_key(&[7; 32])).unwrap();
    assert_eq!(*ready(a.kek().unwrap_key(&blob)).unwrap(), [7; 32]);
    assert!(kek.downcast_ref::<kekring::Kek>().is_some());
    let other = registry.key_wrap("kekring:other").unwrap();
    let e = ready(other.unwrap_key(&blob)).unwrap_err();
    assert_eq!(
        (e.kind(), Code::of(&e)),
        (ErrorKind::DecryptFailed, Some(Code::KeyIdMismatch))
    );
    let missing = registry.key_wrap("kekring:missing").unwrap();
    assert_eq!(
        ready(missing.wrap_key(&[7; 32])).unwrap_err().kind(),
        ErrorKind::NotFound
    );
    for bad in [
        "kekring:",
        "kekring:Upper",
        "kekring:a.b",
        "kekring://tests",
        "kekring:tests?x=1",
    ] {
        let e = registry.key_wrap(bad).unwrap_err();
        assert_eq!(
            (e.kind(), e.operation()),
            (ErrorKind::Configuration, anykms_core::Operation::ParseUri),
            "{bad}"
        );
    }
    assert!(!format!("{:?}", KekringProvider::new([a]).unwrap()).contains("AQEB"));
}
