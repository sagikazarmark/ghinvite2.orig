//! Validated Restate request identity, shared by native and Worker entry points.
use restate_sdk::endpoint::{Builder, Endpoint};

#[derive(Clone, Debug)]
pub struct EndpointIdentity {
    keys: Vec<String>,
}

#[derive(Clone, Copy, thiserror::Error)]
#[error("RESTATE_IDENTITY_KEYS must contain valid publickeyv1 verification keys")]
pub struct IdentityConfigError;

impl std::fmt::Debug for IdentityConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(self, f)
    }
}

impl EndpointIdentity {
    /// A comma/ASCII-whitespace separated list. Missing, empty or invalid input
    /// never turns request verification off. All keys participate in rotation.
    pub fn required(value: Option<&str>) -> Result<Self, IdentityConfigError> {
        let keys: Vec<String> = value
            .ok_or(IdentityConfigError)?
            .split(|c: char| c == ',' || c.is_ascii_whitespace())
            .filter(|key| !key.is_empty())
            .map(str::to_owned)
            .collect();
        if keys.is_empty() {
            return Err(IdentityConfigError);
        }
        let identity = Self { keys };
        // Delegate key-format validation to the SDK; its parser diagnostics
        // may include input characters and must not cross this boundary.
        identity.apply(Endpoint::builder())?;
        Ok(identity)
    }

    /// Explicit local harness use only. Deployment adapters must call required.
    pub fn local_unsigned() -> Self {
        Self { keys: Vec::new() }
    }

    pub(crate) fn apply(&self, mut builder: Builder) -> Result<Builder, IdentityConfigError> {
        // The SDK verifies with jsonwebtoken, which has no backend of its own
        // here (see `request_jwt`).
        crate::request_jwt::install();
        for key in &self.keys {
            builder = builder.identity_key(key).map_err(|_| IdentityConfigError)?;
        }
        Ok(builder)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const FIRST: &str = "publickeyv1_ChjENKeMvCtRnqG2mrBK1HmPKufgFUc98K8B3ononQvp";
    const SECOND: &str = "publickeyv1_w7YHemBctH5Ck2nQRQ47iBBqhNHy4FV7t2Usbye2A6f";

    #[test]
    fn missing_empty_and_invalid_lists_fail_without_input_diagnostics() {
        for value in [
            None,
            Some(""),
            Some(" , \n"),
            Some("SECRET_MARKER"),
            Some("publickeyv1_SECRET_MARKER"),
            Some("publickeyv1_111"),
        ] {
            let error = EndpointIdentity::required(value).unwrap_err();
            assert_eq!(
                error.to_string(),
                "RESTATE_IDENTITY_KEYS must contain valid publickeyv1 verification keys"
            );
            assert!(!format!("{error:?}").contains("SECRET_MARKER"));
        }
        assert!(EndpointIdentity::required(Some(&format!("{FIRST},SECRET_MARKER"))).is_err());
    }

    #[test]
    fn rotation_keeps_every_valid_key() {
        let identity = EndpointIdentity::required(Some(&format!("{FIRST},\n {SECOND}"))).unwrap();
        assert_eq!(identity.keys, [FIRST, SECOND]);
        identity.apply(Endpoint::builder()).unwrap();
        assert!(EndpointIdentity::local_unsigned().keys.is_empty());
    }

    /// A Restate-signed request, as `tests/worker/restate-identity.mjs` makes
    /// one: an EdDSA JWT whose audience is the request path.
    fn signed_health(key: &ed25519_dalek::SigningKey, alg: &str) -> http::Request<String> {
        use base64::Engine as _;
        use base64::engine::general_purpose::URL_SAFE_NO_PAD;
        use ed25519_dalek::Signer as _;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let header = URL_SAFE_NO_PAD.encode(format!(r#"{{"alg":"{alg}","typ":"JWT"}}"#));
        let claims = URL_SAFE_NO_PAD.encode(
            serde_json::json!({ "aud": "/health", "iat": now, "nbf": now - 1, "exp": now + 60 })
                .to_string(),
        );
        let input = format!("{header}.{claims}");
        let signature = URL_SAFE_NO_PAD.encode(key.sign(input.as_bytes()).to_bytes());
        http::Request::get("/health")
            .header("x-restate-signature-scheme", "v1")
            .header("x-restate-jwt-v1", format!("{input}.{signature}"))
            .body(String::new())
            .unwrap()
    }

    fn public_key(key: &ed25519_dalek::SigningKey) -> String {
        let encoded = bs58::encode(key.verifying_key().to_bytes()).into_string();
        format!("publickeyv1_{encoded}")
    }

    #[test]
    fn endpoint_accepts_only_requests_signed_by_a_configured_key() {
        let signer = ed25519_dalek::SigningKey::from_bytes(&[7; 32]);
        let other = ed25519_dalek::SigningKey::from_bytes(&[9; 32]);
        let keys = format!("{},{}", public_key(&other), public_key(&signer));
        let identity = EndpointIdentity::required(Some(&keys)).unwrap();
        let endpoint = identity.apply(Endpoint::builder()).unwrap().build();
        let status = |request| endpoint.handle(request).status().as_u16();

        assert_eq!(status(signed_health(&signer, "EdDSA")), 200);
        assert_eq!(status(signed_health(&other, "EdDSA")), 200);
        let stranger = ed25519_dalek::SigningKey::from_bytes(&[11; 32]);
        assert_eq!(status(signed_health(&stranger, "EdDSA")), 401);
        assert_eq!(status(signed_health(&signer, "HS256")), 401);
        // Another key's signature over the same input.
        let mut forged = signed_health(&signer, "EdDSA");
        let stolen = signed_health(&stranger, "EdDSA");
        let input = forged.headers()["x-restate-jwt-v1"].to_str().unwrap();
        let input = input.rsplit_once('.').unwrap().0.to_owned();
        let signature = stolen.headers()["x-restate-jwt-v1"].to_str().unwrap();
        let signature = signature.rsplit_once('.').unwrap().1;
        forged.headers_mut().insert(
            "x-restate-jwt-v1",
            format!("{input}.{signature}").parse().unwrap(),
        );
        assert_eq!(status(forged), 401);
        assert_eq!(
            status(http::Request::get("/health").body(String::new()).unwrap()),
            401
        );
    }
}
