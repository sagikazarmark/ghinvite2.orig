use rand::RngCore;
use serde::{Deserialize, Serialize};
use std::fmt;
use subtle::ConstantTimeEq;

const SLUG_LEN: usize = 16;
const ALPHABET: &[u8; 62] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

/// 16-character base62 secret used in share-link URLs.
///
/// `PartialEq`/`Eq` are derived for struct equality (e.g. test assertions on
/// records that contain a `Slug`); use [`Slug::ct_eq`] when comparing a
/// user-supplied slug against a stored one to avoid timing-based enumeration.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Slug(String);

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum SlugError {
    #[error("slug must be exactly {SLUG_LEN} characters")]
    BadLength,
    #[error("slug contains non-base62 character")]
    BadCharacter,
}

impl Slug {
    /// Generate a new random slug from the given RNG.
    /// In production, pass `rand::thread_rng()` or `rand::rngs::OsRng`.
    /// In tests, pass a seeded `ChaCha8Rng` for determinism.
    pub fn generate<R: RngCore>(rng: &mut R) -> Self {
        let mut bytes = [0u8; SLUG_LEN];
        let mut out = String::with_capacity(SLUG_LEN);
        rng.fill_bytes(&mut bytes);
        for b in bytes {
            // Reject-free mod 62 is biased but the bias is tiny (256 % 62 = 8 / 256 ≈ 3%);
            // for a 16-character slug at 95+ bits of entropy that's irrelevant security-wise.
            out.push(ALPHABET[(b % 62) as usize] as char);
        }
        Self(out)
    }

    pub fn from_string(s: String) -> Result<Self, SlugError> {
        if s.len() != SLUG_LEN {
            return Err(SlugError::BadLength);
        }
        if !s.bytes().all(|b| ALPHABET.contains(&b)) {
            return Err(SlugError::BadCharacter);
        }
        Ok(Self(s))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Constant-time equality. Use this whenever comparing a user-supplied slug
    /// against a stored slug to prevent timing-based enumeration.
    pub fn ct_eq(&self, other: &Slug) -> bool {
        self.0.as_bytes().ct_eq(other.0.as_bytes()).into()
    }
}

impl fmt::Display for Slug {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::SeedableRng;
    use rand_chacha::ChaCha8Rng;

    #[test]
    fn generate_produces_16_base62_chars() {
        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let slug = Slug::generate(&mut rng);
        assert_eq!(slug.as_str().len(), 16);
        assert!(slug.as_str().bytes().all(|b| ALPHABET.contains(&b)));
    }

    #[test]
    fn deterministic_under_seeded_rng() {
        let mut a = ChaCha8Rng::seed_from_u64(42);
        let mut b = ChaCha8Rng::seed_from_u64(42);
        assert_eq!(
            Slug::generate(&mut a).as_str(),
            Slug::generate(&mut b).as_str()
        );
    }

    #[test]
    fn from_string_validates_length() {
        assert!(matches!(
            Slug::from_string("short".into()),
            Err(SlugError::BadLength)
        ));
        assert!(matches!(
            Slug::from_string("seventeen-chars-x".into()),
            Err(SlugError::BadLength)
        ));
    }

    #[test]
    fn from_string_validates_alphabet() {
        let invalid = "abcdef0123456!@#"; // 16 chars, but '!' '@' '#' not base62
        assert!(matches!(
            Slug::from_string(invalid.into()),
            Err(SlugError::BadCharacter)
        ));
    }

    #[test]
    fn from_string_round_trips_valid_slug() {
        let mut rng = ChaCha8Rng::seed_from_u64(7);
        let s = Slug::generate(&mut rng);
        let parsed = Slug::from_string(s.as_str().to_string()).unwrap();
        assert!(s.ct_eq(&parsed));
    }

    #[test]
    fn ct_eq_distinguishes_different_slugs() {
        let mut r1 = ChaCha8Rng::seed_from_u64(1);
        let mut r2 = ChaCha8Rng::seed_from_u64(2);
        let a = Slug::generate(&mut r1);
        let b = Slug::generate(&mut r2);
        assert!(!a.ct_eq(&b));
    }
}
