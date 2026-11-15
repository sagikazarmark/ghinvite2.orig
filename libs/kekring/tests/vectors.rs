//! The committed vectors: `kekring-v1.json`, re-derived by `scripts/kekring-vectors.mjs` with
//! Node/OpenSSL (`node scripts/kekring-vectors.mjs --check`), and upstream anykms's
//! `aad_v1.json`, copied unchanged.
use anykms_core::{Ciphertext, ErrorKind, KeyWrap};
use base64::{Engine, engine::general_purpose::STANDARD};
use kekring::{Aad, Code, Keyring, Padding, Purpose, aad::Rule};
use serde_json::Value;

fn bytes(s: &str) -> Vec<u8> {
    s.as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| u8::from_str_radix(std::str::from_utf8(c).unwrap(), 16).unwrap())
        .collect()
}

fn load(json: &str) -> Value {
    serde_json::from_str(json).unwrap()
}

fn kekring() -> Value {
    load(include_str!("vectors/kekring-v1.json"))
}

fn str_of(v: &Value) -> &str {
    v.as_str().unwrap()
}

/// Polls once: every kekring future completes on its first poll.
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

fn kind(e: &anykms_core::Error) -> String {
    format!("{:?}", e.kind())
}

#[test]
fn derivation_through_kid_and_wrap() {
    let v = kekring();
    for d in v["derivation"].as_array().unwrap() {
        let version = u32::try_from(d["version"].as_u64().unwrap()).unwrap();
        let root: [u8; 32] = bytes(str_of(&d["root_hex"])).try_into().unwrap();
        let ring = Keyring::builder(str_of(&d["name"]))
            .generation(version, root)
            .current(version)
            .build()
            .unwrap();
        assert_eq!(
            ring.kid(version).unwrap().to_vec(),
            bytes(str_of(&d["kid_hex"]))
        );
    }
    // The KEK itself is never exposed: a wrap under it, reproduced by AES-KW in Node, pins it.
    for w in v["wraps"].as_array().unwrap() {
        let ring = Keyring::parse(str_of(&w["descriptor"])).unwrap();
        let blob = ready(ring.kek().wrap_key(&bytes(str_of(&w["dek_hex"])))).unwrap();
        assert_eq!(
            blob.as_bytes(),
            bytes(str_of(&w["blob_hex"])),
            "{}",
            w["id"]
        );
        assert_eq!(blob.as_bytes().len(), kekring::BLOB_LEN);
        assert_eq!(
            *ready(ring.kek().unwrap_key(&blob)).unwrap(),
            bytes(str_of(&w["dek_hex"]))
        );
    }
}

#[test]
fn valid_descriptors() {
    for d in kekring()["descriptors"].as_array().unwrap() {
        let text = str_of(&d["descriptor"]);
        let ring = Keyring::parse(text).unwrap();
        let fp = ring.fingerprint();
        assert_eq!(fp.name, str_of(&d["name"]));
        assert_eq!(u64::from(fp.current), d["current"].as_u64().unwrap());
        assert_eq!(u64::from(fp.high_water_mark), d["hwm"].as_u64().unwrap());
        let versions: Vec<u64> = d["versions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_u64().unwrap())
            .collect();
        assert_eq!(
            fp.versions
                .iter()
                .map(|v| u64::from(*v))
                .collect::<Vec<_>>(),
            versions
        );
        for (v, kid) in versions.iter().zip(d["kids_hex"].as_array().unwrap()) {
            let v = u32::try_from(*v).unwrap();
            assert_eq!(ring.kid(v).unwrap().to_vec(), bytes(str_of(kid)));
        }
        assert_eq!(fp.digest, str_of(&d["fingerprint"]), "{}", d["id"]);
        // The grammar is canonical: one text per keyring, less the optional newline.
        assert_eq!(
            &*ring.to_descriptor(),
            text.strip_suffix('\n').unwrap_or(text)
        );
    }
}

#[test]
fn invalid_descriptors() {
    let v = kekring();
    let invalid = v["invalid_descriptors"].as_array().unwrap();
    let mut rules = std::collections::BTreeSet::new();
    for d in invalid {
        let e = Keyring::parse(str_of(&d["descriptor"])).unwrap_err();
        assert_eq!(e.kind(), ErrorKind::Configuration, "{}", d["id"]);
        assert_eq!(
            e.detail().and_then(|d| d.code()),
            d["rule"].as_str(),
            "{}",
            d["id"]
        );
        assert!(std::error::Error::source(&e).is_none());
        // No root material in any rendering: not even 8 characters of a base64url root.
        let rendered = format!("{e:?} {e}");
        for root in str_of(&d["descriptor"])
            .split(';')
            .filter_map(|f| f.split_once('=').map(|(_, v)| v))
            .filter_map(|v| v.split_once('.').map(|(r, _)| r))
        {
            for w in root.as_bytes().windows(8) {
                assert!(
                    !rendered.contains(std::str::from_utf8(w).unwrap()),
                    "{}",
                    d["id"]
                );
            }
        }
        rules.insert(str_of(&d["rule"]).to_owned());
    }
    // Every parser rule has at least one negative vector.
    for rule in [
        "length",
        "charset",
        "prefix",
        "field_order",
        "name",
        "version",
        "generation_order",
        "generation_count",
        "root",
        "kid",
        "kid_mismatch",
        "zero_root",
        "duplicate_root",
        "current",
        "high_water_mark",
    ] {
        assert!(rules.contains(&format!("descriptor:{rule}")), "{rule}");
    }
}

#[test]
fn unwrap_reader_rules() {
    for u in kekring()["unwrap"].as_array().unwrap() {
        let ring = Keyring::parse(str_of(&u["descriptor"])).unwrap();
        let result = ready(
            ring.kek()
                .unwrap_key(&Ciphertext::from(bytes(str_of(&u["blob_hex"])))),
        );
        match u.get("dek_hex") {
            Some(dek) => assert_eq!(*result.unwrap(), bytes(str_of(dek))),
            None => {
                let e = result.unwrap_err();
                assert_eq!(kind(&e), str_of(&u["kind"]), "{}", u["id"]);
                assert_eq!(
                    Code::of(&e).map(Code::as_str),
                    u["code"].as_str(),
                    "{}",
                    u["id"]
                );
                assert_eq!(e.retry_hint(), None);
                assert_eq!(e.resource(), Some("kekring:vectors"));
            }
        }
    }
}

struct VectorSession;
impl Purpose for VectorSession {
    const NAME: &'static str = "vector-session";
    const FIELDS: &'static [&'static str] = &["session_id"];
}
struct VectorEmail;
impl Purpose for VectorEmail {
    const NAME: &'static str = "vector-email";
    const FIELDS: &'static [&'static str] = &["row", "batch_id", "account_id"];
    const PADDING: Padding = Padding::block(256);
}

fn typed_aad<P: Purpose>(fields: &Value) -> Aad {
    let mut b = Aad::builder::<P>();
    for f in fields.as_array().unwrap() {
        let key = P::FIELDS.iter().find(|k| **k == str_of(&f[0])).unwrap();
        b = b.str(key, str_of(&f[1]));
    }
    b.build().unwrap()
}

#[test]
fn envelopes() {
    for e in kekring()["envelopes"].as_array().unwrap() {
        let ring = Keyring::parse(str_of(&e["descriptor"])).unwrap();
        let envelope = ring.envelope();
        let (written, read) = match str_of(&e["purpose"]) {
            "vector-session" => (
                typed_aad::<VectorSession>(&e["fields"]),
                typed_aad::<VectorSession>(&e["read_fields"]),
            ),
            "vector-email" => (
                typed_aad::<VectorEmail>(&e["fields"]),
                typed_aad::<VectorEmail>(&e["read_fields"]),
            ),
            other => panic!("{other}"),
        };
        assert_eq!(
            written.as_bytes(),
            bytes(str_of(&e["aad_hex"])),
            "{}",
            e["id"]
        );
        let ct = Ciphertext::from(STANDARD.decode(str_of(&e["envelope_b64"])).unwrap());
        let plaintext = bytes(str_of(&e["plaintext_hex"]));
        match e["expect"].as_object() {
            None => {
                assert_eq!(*envelope.decrypt_sync(&ct, &read).unwrap(), plaintext);
                // Rewrapping a current envelope is byte-identical (AES-KW is deterministic).
                assert_eq!(envelope.rewrap_current_sync(&ct).unwrap(), ct);
                // And this implementation writes what Node wrote, for the same plaintext length.
                let ours = envelope.encrypt_sync(&plaintext, &written).unwrap();
                assert_eq!(ours.as_bytes().len(), ct.as_bytes().len());
            }
            Some(expect) => {
                let err = envelope.decrypt_sync(&ct, &read).unwrap_err();
                assert_eq!(kind(&err), str_of(&expect["kind"]), "{}", e["id"]);
                assert_eq!(
                    Code::of(&err).map(Code::as_str),
                    expect["code"].as_str(),
                    "{}",
                    e["id"]
                );
                assert_eq!(err.retry_hint(), None);
            }
        }
        if e["id"] == "swapped_slot" {
            let err = envelope.rewrap_current_sync(&ct).unwrap_err();
            assert_eq!(err.kind(), ErrorKind::DecryptFailed);
        }
    }
}

#[test]
fn upstream_aad_v1() {
    let v = load(include_str!("vectors/aad_v1.json"));
    assert_eq!(bytes(str_of(&v["label"])), kekring::aad::LABEL);
    let pairs = |f: &Value| -> Vec<(String, String)> {
        f.as_array()
            .unwrap()
            .iter()
            .map(|p| (str_of(&p[0]).to_owned(), str_of(&p[1]).to_owned()))
            .collect()
    };
    for case in v["valid"].as_array().unwrap() {
        let fields = pairs(&case["fields"]);
        let refs: Vec<(&str, &str)> = fields.iter().map(|(k, v)| (&**k, &**v)).collect();
        let encoded = Aad::encode(str_of(&case["purpose"]), &refs).unwrap();
        assert_eq!(encoded, bytes(str_of(&case["aad"])), "{}", case["name"]);
        let mut reversed = refs.clone();
        reversed.reverse();
        assert_eq!(
            Aad::encode(str_of(&case["purpose"]), &reversed).unwrap(),
            encoded
        );
    }
    for case in v["invalid"].as_array().unwrap() {
        let fields = pairs(&case["fields"]);
        let refs: Vec<(&str, &str)> = fields.iter().map(|(k, v)| (&**k, &**v)).collect();
        let e = Aad::encode(str_of(&case["purpose"]), &refs).unwrap_err();
        assert_eq!(e.kind(), ErrorKind::InvalidArgument);
        assert_eq!(
            Rule::of(&e).map(Rule::as_str),
            case["rule"].as_str(),
            "{}",
            case["name"]
        );
        // An error never carries a value.
        for (_, value) in &fields {
            if value.len() >= 4 {
                assert!(
                    !format!("{e:?} {e}").contains(value.as_str()),
                    "{}",
                    case["name"]
                );
            }
        }
    }
}
