//! Manual RS256 JWT minting for the GitHub App credentials.
//!
//! Why manual: `ring` (the default crypto backend for `jsonwebtoken`) does not
//! cleanly target wasm32-unknown-unknown without elaborate setup. The math is
//! 30 lines; doing it by hand keeps the wasm story simple.

use crate::error::{Error, Result};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::{DateTime, Utc};
use rsa::RsaPrivateKey;
use rsa::pkcs1v15::SigningKey;
use rsa::pkcs8::DecodePrivateKey;
use rsa::signature::{SignatureEncoding, Signer as _};
use serde::Serialize;
use sha2::Sha256;

/// Holds the App's RSA private key plus the App ID. Constructed once at
/// startup (the key sits in `Workers secrets`), then cloned into long-lived
/// `InstallationClient`s.
#[derive(Clone)]
pub struct AppJwtSigner {
    /// GitHub App ID — appears as the `iss` claim.
    pub app_id: u64,
    inner: RsaPrivateKey,
}

impl AppJwtSigner {
    /// Parse the App private key from PKCS#8 PEM (the format GitHub gives
    /// you when downloading the key).
    pub fn from_pkcs8_pem(app_id: u64, pem: &str) -> Result<Self> {
        let inner = RsaPrivateKey::from_pkcs8_pem(pem)
            .map_err(|e| Error::InvalidInput(format!("rsa pkcs8 pem: {e}")))?;
        Ok(Self { app_id, inner })
    }

    /// Mint a JWT. `now` is parameterized so tests can pin the clock; in
    /// production callers pass `Utc::now()`. The token lives 9 minutes; GitHub
    /// accepts up to 10 minutes and we leave a 60-second clock-skew buffer.
    pub fn sign(&self, now: DateTime<Utc>) -> Result<String> {
        let header = b"{\"alg\":\"RS256\",\"typ\":\"JWT\"}";
        let header_b64 = URL_SAFE_NO_PAD.encode(header);

        #[derive(Serialize)]
        struct Claims {
            iat: i64,
            exp: i64,
            iss: u64,
        }
        let claims = Claims {
            iat: now.timestamp() - 60, // backdated to absorb clock skew
            exp: now.timestamp() + (9 * 60),
            iss: self.app_id,
        };
        let claims_bytes =
            serde_json::to_vec(&claims).map_err(|e| Error::Jwt(format!("encoding claims: {e}")))?;
        let claims_b64 = URL_SAFE_NO_PAD.encode(&claims_bytes);

        let signing_input = format!("{header_b64}.{claims_b64}");
        let signing_key = SigningKey::<Sha256>::new(self.inner.clone());
        let signature = signing_key.sign(signing_input.as_bytes());
        let sig_b64 = URL_SAFE_NO_PAD.encode(signature.to_bytes());

        Ok(format!("{signing_input}.{sig_b64}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use jsonwebtoken::{Algorithm, DecodingKey, Validation};
    use rsa::RsaPublicKey;
    use rsa::pkcs8::DecodePrivateKey;
    use rsa::pkcs8::EncodePublicKey;

    /// Test-only RSA-2048 key in PKCS#8 PEM. NOT a secret — generated solely
    /// for unit tests to avoid the ~20s `RsaPrivateKey::new` cost on every
    /// `cargo test` invocation. Shared with `installation.rs`'s test module
    /// via the sibling `jwt_test_key.pem` file.
    ///
    /// Regenerate with:
    ///   openssl genpkey -algorithm RSA -pkcs8 \
    ///     -pkeyopt rsa_keygen_bits:2048
    const TEST_KEY_PEM: &str = include_str!("jwt_test_key.pem");

    #[test]
    fn signs_a_jwt_that_jsonwebtoken_can_verify() {
        let signer = AppJwtSigner::from_pkcs8_pem(99, TEST_KEY_PEM).unwrap();

        // Re-derive the public key from the same PEM so the test stays
        // self-contained (no separate public-key fixture).
        let priv_key = RsaPrivateKey::from_pkcs8_pem(TEST_KEY_PEM).unwrap();
        let pub_key = RsaPublicKey::from(&priv_key);
        let pub_pem = pub_key
            .to_public_key_pem(rsa::pkcs8::LineEnding::LF)
            .unwrap();

        let now = Utc.with_ymd_and_hms(2026, 5, 4, 12, 0, 0).unwrap();
        let jwt = signer.sign(now).unwrap();

        let key = DecodingKey::from_rsa_pem(pub_pem.as_bytes()).unwrap();
        let mut validation = Validation::new(Algorithm::RS256);
        validation.set_issuer(&["99"]);
        validation.validate_exp = true;
        validation.leeway = 120;
        validation.required_spec_claims = std::collections::HashSet::new();
        let token_data =
            jsonwebtoken::decode::<serde_json::Value>(&jwt, &key, &validation).unwrap();
        assert_eq!(token_data.claims["iss"], 99);
        assert!(token_data.claims["iat"].as_i64().unwrap() <= now.timestamp());
        assert!(token_data.claims["exp"].as_i64().unwrap() > now.timestamp());
    }

    #[test]
    fn rejects_garbage_pem() {
        match AppJwtSigner::from_pkcs8_pem(1, "not a pem") {
            Err(Error::InvalidInput(_)) => (),
            Err(other) => panic!("expected InvalidInput, got {other:?}"),
            Ok(_) => panic!("expected error, got Ok"),
        }
    }
}
