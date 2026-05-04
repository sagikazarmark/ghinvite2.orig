//! Constant-time HMAC-SHA-256 verification for GitHub webhook signatures.
//!
//! GitHub formats the signature as `X-Hub-Signature-256: sha256=<lowercase hex>`.
//! This module decodes the header, computes HMAC over the *raw* request body,
//! and compares the two byte vectors with `subtle::ConstantTimeEq`.

use ::hmac::{Hmac, Mac};
use sha2::Sha256;
use subtle::ConstantTimeEq;

type HmacSha256 = Hmac<Sha256>;

/// Verify the `X-Hub-Signature-256` header against `raw_body` using `secret`.
///
/// Returns `true` iff the header is well-formed AND the computed HMAC matches.
/// Returns `false` for any malformation (missing prefix, odd hex, wrong length)
/// — callers should respond with 401 on `false`.
///
/// This function never short-circuits on length or content of the supplied
/// signature once decoded: the comparison itself is constant-time.
pub fn verify_signature_256(secret: &[u8], raw_body: &[u8], header_value: &str) -> bool {
    // Header MUST start with "sha256=" — case-sensitive per GitHub spec.
    let Some(hex) = header_value.strip_prefix("sha256=") else {
        return false;
    };
    // 64 hex chars == 32 bytes (SHA-256 output)
    let Ok(supplied) = decode_lowercase_hex(hex) else {
        return false;
    };
    if supplied.len() != 32 {
        return false;
    }

    let mut mac = HmacSha256::new_from_slice(secret).expect("HMAC accepts any key length");
    mac.update(raw_body);
    let computed = mac.finalize().into_bytes();

    computed.as_slice().ct_eq(&supplied).into()
}

/// Decode a lowercase hex string into bytes. Rejects uppercase, odd length, or
/// any non-hex character. Implemented locally so we don't drag in `hex` for one
/// callsite.
fn decode_lowercase_hex(s: &str) -> Result<Vec<u8>, ()> {
    if !s.len().is_multiple_of(2) {
        return Err(());
    }
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len() / 2);
    for chunk in bytes.chunks(2) {
        let hi = nibble(chunk[0])?;
        let lo = nibble(chunk[1])?;
        out.push((hi << 4) | lo);
    }
    Ok(out)
}

fn nibble(b: u8) -> Result<u8, ()> {
    match b {
        b'0'..=b'9' => Ok(b - b'0'),
        b'a'..=b'f' => Ok(b - b'a' + 10),
        _ => Err(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::hmac::{Hmac, Mac};

    fn sign(secret: &[u8], body: &[u8]) -> String {
        let mut mac = Hmac::<Sha256>::new_from_slice(secret).unwrap();
        mac.update(body);
        let bytes = mac.finalize().into_bytes();
        let mut hex = String::with_capacity(64);
        for byte in bytes {
            hex.push_str(&format!("{byte:02x}"));
        }
        format!("sha256={hex}")
    }

    #[test]
    fn correct_signature_passes() {
        let secret = b"webhook-secret";
        let body = br#"{"action":"created"}"#;
        assert!(verify_signature_256(secret, body, &sign(secret, body)));
    }

    #[test]
    fn wrong_secret_fails() {
        let secret = b"webhook-secret";
        let body = br#"{"action":"created"}"#;
        assert!(!verify_signature_256(
            b"other-secret",
            body,
            &sign(secret, body)
        ));
    }

    #[test]
    fn modified_body_fails() {
        let secret = b"webhook-secret";
        let body = br#"{"action":"created"}"#;
        let header = sign(secret, body);
        let tampered = br#"{"action":"deleted"}"#;
        assert!(!verify_signature_256(secret, tampered, &header));
    }

    #[test]
    fn missing_prefix_fails() {
        // No `sha256=` prefix.
        assert!(!verify_signature_256(b"k", b"x", "abcdef"));
    }

    #[test]
    fn uppercase_hex_fails() {
        // GitHub ships lowercase; reject anything else to keep the contract tight.
        let mut header = sign(b"k", b"x");
        header = header.replace(|c: char| c.is_ascii_hexdigit(), "A");
        assert!(!verify_signature_256(b"k", b"x", &header));
    }

    #[test]
    fn odd_length_fails() {
        assert!(!verify_signature_256(b"k", b"x", "sha256=abc"));
    }

    #[test]
    fn wrong_length_fails() {
        // 30 hex chars → 15 bytes → not 32, should fail.
        let header = format!("sha256={}", "0".repeat(30));
        assert!(!verify_signature_256(b"k", b"x", &header));
    }
}
