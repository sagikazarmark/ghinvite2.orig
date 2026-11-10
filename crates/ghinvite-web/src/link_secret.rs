//! Public invitation capability. Only its hash crosses the web boundary.
//!
//! SHA-256 covers the decoded 32 random bytes, not their base64url spelling.
//! The sole plaintext output consumes the value for the one-time web response.
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use ghinvite_core::SecretHash;
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

pub(crate) struct LinkSecret(Zeroizing<[u8; 32]>);

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub(crate) enum SecretError {
    #[error("invitation code is invalid")]
    Invalid,
    #[error("invitation code generation is unavailable")]
    EntropyUnavailable,
}

impl std::fmt::Debug for LinkSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("LinkSecret([REDACTED])")
    }
}

impl LinkSecret {
    /// Invoke within the incoming web request, including on Workers. There is
    /// ordinary builds use system entropy with no cached random material or
    /// deterministic fallback. The dedicated fixture feature requires bootstrap.
    pub(crate) fn generate() -> Result<Self, SecretError> {
        let mut bytes = Zeroizing::new([0; 32]);
        super::link_secret_entropy::fill(&mut bytes)
            .map_err(|_| SecretError::EntropyUnavailable)?;
        Ok(Self(bytes))
    }

    /// Accept exactly one canonical spelling; no trimming, URL decoding, ULID,
    /// padded base64, or permissive decoding aliases.
    pub(crate) fn parse(encoded: &str) -> Result<Self, SecretError> {
        if encoded.len() != 43
            || !encoded
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return Err(SecretError::Invalid);
        }
        let mut bytes = Zeroizing::new([0; 32]);
        let written = URL_SAFE_NO_PAD
            .decode_slice(encoded, bytes.as_mut())
            .map_err(|_| SecretError::Invalid)?;
        if written != 32 {
            return Err(SecretError::Invalid);
        }
        Ok(Self(bytes))
    }

    pub(crate) fn hash(&self) -> SecretHash {
        SecretHash::from_bytes(Sha256::digest(&self.0[..]).into())
    }

    /// The caller must render this only in the show-once web response after
    /// the hash index is committed. Never send it to Restate or a redirect URL.
    pub(crate) fn show_once(self) -> Zeroizing<String> {
        Zeroizing::new(URL_SAFE_NO_PAD.encode(&self.0[..]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes_decoded_bytes_and_round_trips_canonical_encoding() {
        let encoded = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
        let secret = LinkSecret::parse(encoded).unwrap();
        // SHA-256 of 32 zero bytes, not the 43 ASCII base64url characters.
        assert_eq!(
            secret.hash().as_hex(),
            "66687aadf862bd776c8fc18b8e9f8e20089714856ee233b3902a591d0d5f2925"
        );
        assert_eq!(secret.show_once().as_str(), encoded);
    }

    #[test]
    fn rejects_ulids_lengths_alphabets_and_noncanonical_trailing_bits() {
        for encoded in [
            "",
            "01ARZ3NDEKTSV4RRFFQ69G5FAV", // former public ULID
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA", // short
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA", // long
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=", // padding
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA+", // standard base64
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA/",
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA%",
            " AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\n",
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAé",
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAB", // nonzero pad bits
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAC",
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAD",
        ] {
            assert!(matches!(
                LinkSecret::parse(encoded),
                Err(SecretError::Invalid)
            ));
        }
    }

    #[test]
    fn secret_debug_and_errors_never_include_input() {
        let marker = "U0VDUkVUX01BUktFUl8wMTIzNDU2Nzg5MDEyMzQ1Njc";
        let secret = LinkSecret::parse(marker).unwrap();
        assert_eq!(format!("{secret:?}"), "LinkSecret([REDACTED])");
        let invalid = format!("{marker}!");
        let error = LinkSecret::parse(&invalid).unwrap_err();
        assert_eq!(error.to_string(), "invitation code is invalid");
        assert!(!format!("{error:?}").contains(marker));
    }

    #[test]
    #[cfg(not(feature = "link-secret-fixture"))]
    fn request_local_generation_produces_distinct_full_length_secrets() {
        let mut hashes = std::collections::HashSet::new();
        for _ in 0..256 {
            let secret = LinkSecret::generate().unwrap();
            let hash = secret.hash();
            assert!(hashes.insert(hash.as_hex()));
            let encoded = secret.show_once();
            assert_eq!(encoded.len(), 43);
            assert!(
                LinkSecret::parse(&encoded)
                    .unwrap()
                    .hash()
                    .constant_time_eq(&hash)
            );
        }
    }
}
