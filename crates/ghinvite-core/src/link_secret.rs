//! Hash-only link capability identity. Plaintext secrets belong to the web edge.
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use subtle::ConstantTimeEq;

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(try_from = "String", into = "String")]
pub struct SecretHash([u8; 32]);
impl SecretHash {
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
    pub fn as_hex(&self) -> String {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut value = String::with_capacity(64);
        for byte in self.0 {
            value.push(HEX[(byte >> 4) as usize] as char);
            value.push(HEX[(byte & 15) as usize] as char);
        }
        value
    }
    pub fn constant_time_eq(&self, other: &Self) -> bool {
        self.0.ct_eq(&other.0).into()
    }
}
impl From<[u8; 32]> for SecretHash {
    fn from(value: [u8; 32]) -> Self {
        Self(value)
    }
}
impl From<SecretHash> for String {
    fn from(value: SecretHash) -> Self {
        value.as_hex()
    }
}
impl TryFrom<String> for SecretHash {
    type Error = &'static str;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        if value.len() != 64
            || !value
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err("invalid secret hash");
        }
        let mut bytes = [0; 32];
        let digit = |b: u8| if b <= b'9' { b - b'0' } else { b - b'a' + 10 };
        for (target, pair) in bytes.iter_mut().zip(value.as_bytes().as_chunks::<2>().0) {
            *target = digit(pair[0]) * 16 + digit(pair[1]);
        }
        Ok(Self(bytes))
    }
}
impl std::fmt::Debug for SecretHash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SecretHash([redacted])")
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LinkSecretState {
    pub secret_generation: u32,
    pub sha256: SecretHash,
}
impl LinkSecretState {
    pub fn matches(&self, presented: &SecretHash, generation: u32) -> bool {
        // Evaluate the hash comparison regardless of the public generation.
        self.sha256.constant_time_eq(presented)
            & (self.secret_generation == generation)
            & (generation != 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn strict_wire_hash_and_constant_time_matching() {
        let hash = SecretHash::from_bytes([0xab; 32]);
        let encoded = serde_json::to_string(&hash).unwrap();
        assert_eq!(encoded, format!("\"{}\"", "ab".repeat(32)));
        assert_eq!(serde_json::from_str::<SecretHash>(&encoded).unwrap(), hash);
        for invalid in ["AB".repeat(32), "a".repeat(63), "g".repeat(64)] {
            assert!(serde_json::from_value::<SecretHash>(serde_json::json!(invalid)).is_err());
        }
        let state = LinkSecretState {
            secret_generation: 1,
            sha256: hash,
        };
        assert!(state.matches(&hash, 1));
        assert!(!state.matches(&hash, 0));
        assert!(!state.matches(&hash, 2));
        assert!(!state.matches(&SecretHash::from_bytes([0; 32]), 1));
        assert!(!format!("{hash:?}").contains("abab"));
        assert!(serde_json::from_str::<LinkSecretState>("{\"secret_generation\":1}").is_err());
    }
}
