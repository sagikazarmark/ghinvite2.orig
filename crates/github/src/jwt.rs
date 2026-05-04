//! Manual RS256 JWT minting for the GitHub App credentials.
//!
//! Why manual: `ring` (the default crypto backend for `jsonwebtoken`) does not
//! cleanly target wasm32-unknown-unknown without elaborate setup. The math is
//! 30 lines; doing it by hand keeps the wasm story simple.

use crate::error::{Error, Result};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use chrono::{DateTime, Utc};
use rsa::pkcs1v15::SigningKey;
use rsa::pkcs8::DecodePrivateKey;
use rsa::signature::{SignatureEncoding, Signer as _};
use rsa::RsaPrivateKey;
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
        let claims_bytes = serde_json::to_vec(&claims)
            .map_err(|e| Error::Jwt(format!("encoding claims: {e}")))?;
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
    use rsa::pkcs8::DecodePrivateKey;
    use rsa::pkcs8::EncodePublicKey;
    use rsa::RsaPublicKey;

    /// Test-only RSA-2048 key in PKCS#8 PEM. NOT a secret — generated solely
    /// for unit tests to avoid the ~20s `RsaPrivateKey::new` cost on every
    /// `cargo test` invocation.
    ///
    /// Regenerate with:
    ///   openssl genpkey -algorithm RSA -pkcs8 \
    ///     -pkeyopt rsa_keygen_bits:2048
    const TEST_KEY_PEM: &str = r#"-----BEGIN PRIVATE KEY-----
MIIEvgIBADANBgkqhkiG9w0BAQEFAASCBKgwggSkAgEAAoIBAQCc3pg/HqNy636+
jYcJJQ0PWqIWNiospFvVrodEv9mylhstB3phBbF98ESYNIOrKP9jzTxL0aMadwp2
Ln0/Rjg9iU41B9/U4GFjgtS+wky066j7lUHQdj8dPcuFHIRsQlOjvOBJgovMXzse
tcX0XzjLKOJtiWG7mWsVLLu5+8x9u+LXI5mCA2JnGw1A5tiv88sAeHSc0oTK230z
c78BG9YmZj/ZoIv0IWQE6iGO9/R2XAULS0z0OZuzuzStdDyimnXvgMwBQMIiQVki
cKqv3vuHOXUUkaJk3XaWHbOgV7fELjbOeNtH57aFL32Ftzer0eIma13aNDThpMUn
tLzj+rN5AgMBAAECggEAAYCN4YXaz688X5k1zRmkTdlpKOOkJMw+ny9WI+tr5RTg
2TFDnV5hw2iLrM/AICFkf6/JXbPNJAQP9Wi1m5RhyVbeIqEeJjjvVpwvPhpL3bKN
6jnU/2JqF8IENJSjNBewmuZRP51byholdAJbQGWxVfWz/foVSOFiJOawotCfCTTh
n1Z0WoLywjtyym3zr37Kzs2Vj4mGiK4vfUrwIIW1h2uVP6udYNbK53vfNul1uaoi
1/aDkKHv2Wok03XfhGvZbk10sqDcH0IXEDlPmBegXEUTnnvTRXFVReU4K8Jg3Sck
inxKzGFYr8JIvr4q+AcRr0sM5IjOPvE3L41bVSAZwQKBgQDR9zn7TQH7nZH7FxDI
v7jSD2JeHB2nAJsjFpnVDNAU5YMiMglhSBn+iQFyw0OnL9Fa/J2DkbTICZIlaXeH
fups3t+9lOA0GD8a1aNc4U3JyGQUQCHqBCVC5HfCl9ZnZNCQwfxewujKW29rvdGi
Oo7L9hW9YXuON6ED1F+dJtF6uQKBgQC/QzrecAfZPKrzEY7K6M4bkNnkzzdZ5afX
33f0RTuXL1y1eH2fDMIqQOckwI7kAx4/BQnnY4R6L9vYJMq3SIQXZw1hEwJV4bOM
A13Xw5C7+ww1BnhTRyGKaopuUF7gSK7L5wFfal74cHRjeeCAGZMTGqEJV/nqfeS9
7B8sNUeewQKBgAkgpzueeGSYz/zLXuZrNzyigJM4w6074IKg++UAHpeZ9p5o8HFz
MfYXvKFhjbJZ6M78xlgu4F4F1H2d3R1dzhEXi0BxlWGOYEfpW6WxAbGw7XDX7OGA
dqI2zmH+Ocra3ho85Jy1+mq5mNllMhTMWOLS+tT1xOpEztIczF9Hjbm5AoGBAIH3
t2ssCclO9oOR7MxpgpUsy0Q2o1BNRM7mpeaxnRrRLliKdiK8UrzPucI5r1+11rnQ
PLil4YH+P5ATAEWn20rj1i2e8zlU0+NS7lQOKq3ynIrzyJQeg+ZBG6x2pOIXweAB
K+egqsR79jsauLmTp2OV9tQYmlUEE4oTh+NMmUyBAoGBAJbLNHFGG3q4XdpNhCRH
fv2Aay3hQt1u6Az8QA7gbJFGhEl68z0iUw/cpoQaXuh2wnn7gOgR3biuPqgYV+rT
h82eR6l5FaOWjeKzCcMkdYq+hOuE/yyj/28NQyyWrxrazDyg1UfRPDqsP0orEHHi
E/Rvxrz5RdBcyXC8vdrkMc7m
-----END PRIVATE KEY-----
"#;

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
