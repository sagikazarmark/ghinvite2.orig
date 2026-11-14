use anykms_core::Ciphertext;
use anykms_recipes::envelope::Padding;
use base64::{
    Engine,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use kekring::{ContextEnvelopeSync, Keyring};
use kekring::{EncryptionContext, purposes};
use std::fmt::Write;
fn bytes(s: &str) -> Vec<u8> {
    s.as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| u8::from_str_radix(std::str::from_utf8(c).unwrap(), 16).unwrap())
        .collect()
}
#[test]
fn independent_openssl_vectors() {
    let cases: Vec<serde_json::Value> =
        serde_json::from_str(include_str!("vectors/keyring-v1.json")).unwrap();
    for v in cases {
        let mut builder = Keyring::builder(v["name"].as_str().unwrap());
        for (i, root) in v["root_hex"].as_array().unwrap().iter().enumerate() {
            let version = u32::try_from(i).unwrap() + 1;
            if v["present_versions"]
                .as_array()
                .is_none_or(|vs| vs.iter().any(|n| n.as_u64() == Some(u64::from(version))))
            {
                builder = builder
                    .generation(version, bytes(root.as_str().unwrap()).try_into().unwrap())
                    .unwrap();
            }
        }
        let ring = builder
            .current(u32::try_from(v["current"].as_u64().unwrap()).unwrap())
            .unwrap()
            .build()
            .unwrap();
        assert_eq!(
            ring.fingerprint().digest,
            v["fingerprint"].as_str().unwrap()
        );
        if let Some(kcv) = ring.key_check_value(1) {
            assert_eq!(kcv.to_vec(), bytes(v["kcv_hex"].as_str().unwrap()));
        }
        let context = EncryptionContext::try_from(&purposes::Session {
            session_id: v["context"]["session_id"].as_str().unwrap(),
        })
        .unwrap();
        let e = ring.context_envelope(Padding::NONE).unwrap();
        let ct = Ciphertext::from(
            STANDARD
                .decode(v["envelope_b64"].as_str().unwrap())
                .unwrap(),
        );
        let result = e.decrypt_sync(&ct, &context);
        if let Some(kind) = v["expected_error"].as_str() {
            assert_eq!(
                format!("{:?}", result.unwrap_err().kind()),
                kind,
                "{}",
                v["id"]
            );
            if v["id"] == "swapped-dek" {
                assert_eq!(
                    format!("{:?}", e.rewrap_current_sync(&ct).unwrap_err().kind()),
                    kind
                );
            }
        } else {
            assert_eq!(
                &*result.unwrap(),
                &bytes(v["plaintext_hex"].as_str().unwrap())
            );
            assert_eq!(context.as_aad(), bytes(v["aad_hex"].as_str().unwrap()));
        }
    }
}
#[test]
fn parser_rules_and_hygiene() {
    let v: Vec<serde_json::Value> =
        serde_json::from_str(include_str!("vectors/keyring-v1.json")).unwrap();
    let root = bytes(v[0]["root_hex"][0].as_str().unwrap());
    let key = URL_SAFE_NO_PAD.encode(root);
    let descriptor = format!(
        "akr1;name=vectors;current=1;k1={key}.{}",
        v[0]["kcv_hex"].as_str().unwrap()
    );
    let ring = Keyring::parse(&descriptor).unwrap();
    assert_eq!(
        Keyring::parse(&(descriptor.clone() + "\n"))
            .unwrap()
            .fingerprint(),
        ring.fingerprint()
    );
    let mut fields: Vec<_> = descriptor.split(';').collect();
    fields[1..].reverse();
    assert_eq!(
        Keyring::parse(&fields.join(";")).unwrap().fingerprint(),
        ring.fingerprint()
    );
    let invalids = [
        descriptor.replace("akr1", "akr2"),
        descriptor.replace("vectors", "other"),
        descriptor.replace("current=1", "current=01"),
        descriptor.replace("current=1", "current=0"),
        descriptor.replace("current=1", "current=2"),
        descriptor.replace("k1=", "k2="),
        descriptor.replace("k1=", "k01="),
        descriptor.replace("name=vectors", "name=Vectors"),
        descriptor.replace(&key, &format!("{key}=")),
        descriptor.clone() + ";current=1",
        descriptor.clone() + ";extra=x",
        descriptor.clone() + "\n\n",
        descriptor.clone() + " ",
        descriptor.replace(&key, &"A".repeat(43)),
        descriptor.replace("k1=", "k0="),
    ];
    for bad in invalids {
        let e = Keyring::parse(&bad).unwrap_err();
        assert_eq!(e.kind(), anykms_core::ErrorKind::Configuration);
        assert!(std::error::Error::source(&e).is_none());
        for s in key.as_bytes().windows(8) {
            assert!(!format!("{e:?} {e}").contains(std::str::from_utf8(s).unwrap()));
        }
    }
}

kekring::purpose! { pub struct ContextVector<'a> = "vectors-context" { left:&'a str, right:&'a str } }
#[test]
fn context_golden_ambiguity_pairs() {
    let cases: Vec<serde_json::Value> =
        serde_json::from_str(include_str!("vectors/context-v1.json")).unwrap();
    let mut seen = std::collections::BTreeSet::new();
    for v in cases {
        let ctx = EncryptionContext::try_from(&ContextVector {
            left: v["context"]["left"].as_str().unwrap(),
            right: v["context"]["right"].as_str().unwrap(),
        })
        .unwrap();
        assert_eq!(ctx.as_aad(), bytes(v["aad_hex"].as_str().unwrap()));
        assert!(seen.insert(ctx.as_aad().to_vec()));
    }
}
fn descriptor(name: &str, current: u32, roots: &[(u32, [u8; 32])]) -> String {
    use hkdf::Hkdf;
    use sha2::Sha256;
    let mut out = format!("akr1;name={name};current={current}");
    for (v, root) in roots {
        let mut info = b"anykms-keyring/v1/kcv".to_vec();
        info.push(u8::try_from(name.len()).unwrap());
        info.extend(name.as_bytes());
        info.extend(v.to_be_bytes());
        let mut kcv = [0; 8];
        Hkdf::<Sha256>::new(Some(b"anykms-keyring/v1/salt"), root)
            .expand(&info, &mut kcv)
            .unwrap();
        write!(out, ";k{v}={}.", URL_SAFE_NO_PAD.encode(root)).unwrap();
        for b in kcv {
            write!(out, "{b:02x}").unwrap();
        }
    }
    out
}
#[test]
fn complete_descriptor_boundaries() {
    let valid = descriptor("boundary", 1, &[(1, [1; 32])]);
    assert!(Keyring::parse(&valid).is_ok());
    let roots: Vec<_> = (1..=64)
        .map(|v| (v, [u8::try_from(v).unwrap(); 32]))
        .collect();
    assert_eq!(
        Keyring::parse(&descriptor("boundary", 64, &roots))
            .unwrap()
            .versions()
            .len(),
        64
    );
    let mut too_many = roots.clone();
    too_many.push((65, [65; 32]));
    let invalids = vec![
        descriptor("boundary", 1, &[]),
        descriptor("boundary", 1, &[(1, [0; 32])]),
        descriptor("boundary", 1, &[(1, [1; 32]), (2, [1; 32])]),
        descriptor("boundary", 1, &[(1, [1; 32]), (1, [2; 32])]),
        descriptor("boundary", 65, &too_many),
        descriptor("", 1, &[(1, [1; 32])]),
        descriptor("-a", 1, &[(1, [1; 32])]),
        descriptor(&"a".repeat(65), 1, &[(1, [1; 32])]),
        valid.replace("current=1", "current=4294967296"),
        valid.replace("current=1", "current=+1"),
        valid.replace("name=boundary", "name=boundary;name=boundary"),
        valid.clone() + ";",
        valid.clone() + "\r\n",
        valid.replace(";current=1", ""),
        valid.replace(";name=boundary", ""),
        valid.replace("k1=", "key1="),
        valid.replace("k1=", "k4294967296="),
        valid.replace("AQEBA", "!QEBA"),
        valid.replace("AQEBA", "AQ EB"),
    ];
    for value in invalids {
        let e = Keyring::parse(&value).unwrap_err();
        assert_eq!(e.kind(), anykms_core::ErrorKind::Configuration);
        assert!(std::error::Error::source(&e).is_none());
    }
    assert!(Keyring::parse(&descriptor(&"a".repeat(64), 1, &[(1, [1; 32])])).is_ok());
    let base = Keyring::parse(&valid).unwrap().fingerprint();
    assert_ne!(
        base,
        Keyring::parse(&descriptor("other", 1, &[(1, [1; 32])]))
            .unwrap()
            .fingerprint()
    );
    assert_ne!(
        base,
        Keyring::parse(&descriptor("boundary", 1, &[(1, [2; 32])]))
            .unwrap()
            .fingerprint()
    );
}
