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
}
