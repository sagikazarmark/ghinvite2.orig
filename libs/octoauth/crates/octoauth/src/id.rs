//! Numeric and string GitHub identities.

use crate::error::ConfigError;

macro_rules! numeric_id {
    ($name:ident, $doc:literal) => {
        #[doc = $doc]
        #[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
        pub struct $name(u64);
        impl $name {
            /// Wrap a numeric GitHub identifier.
            pub const fn new(value: u64) -> Self {
                Self(value)
            }

            /// The numeric identifier.
            pub const fn get(self) -> u64 {
                self.0
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                self.0.fmt(f)
            }
        }

        impl From<u64> for $name {
            fn from(value: u64) -> Self {
                Self(value)
            }
        }
    };
}

numeric_id!(AppId, "Numeric GitHub App ID.");
numeric_id!(InstallationId, "Numeric GitHub App installation ID.");

/// A GitHub App's client ID (`Iv23…`). GitHub recommends it as the JWT issuer.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub(crate) struct ClientId(String);

impl ClientId {
    /// Validate a nonempty identifier of at most 255 ASCII letters, digits, `_`, `-` or `.`.
    pub(crate) fn new(value: impl Into<String>) -> Result<Self, ConfigError> {
        let value = value.into();
        if value.is_empty()
            || value.len() > 255
            || !value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
        {
            return Err(ConfigError::new("client_id", "identifier"));
        }
        Ok(Self(value))
    }

    /// The client ID.
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_id_grammar() {
        assert_eq!(
            ClientId::new("Iv23.ab-c_d").unwrap().as_str(),
            "Iv23.ab-c_d"
        );
        for bad in ["", "has space", "new\nline", &"x".repeat(256)] {
            let error = ClientId::new(bad).unwrap_err();
            assert_eq!(error.to_string(), "invalid client_id (identifier)");
        }
    }

    #[test]
    fn numeric_ids_round_trip() {
        assert_eq!(AppId::new(7).get(), 7);
        assert_eq!(InstallationId::new(9).to_string(), "9");
    }
}
