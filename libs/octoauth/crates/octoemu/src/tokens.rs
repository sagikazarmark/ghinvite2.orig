//! Installation access tokens: minted in GitHub's stateless format
//! (`ghs_<APPID>_<JWT>`, about 520 characters), signed with a key drawn per
//! emulator instance, and looked up on every use so revocation, expiry and
//! scope are enforced.
//!
//! The inner JWT is HS256 over `{iss, sub, aud, app_id, installation_id,
//! jti, iat, nbf, exp, pad}`; GitHub's own claims and algorithm are not
//! public, so the inner format is `assumed` (behaviour
//! `installation-token-format`). Clients treat tokens as opaque.

use crate::seed::Access;
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::{DateTime, TimeDelta, Utc};
use hmac::{Hmac, Mac};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap};

/// An installation token's lifetime (documented: one hour).
pub(crate) const LIFETIME: TimeDelta = TimeDelta::hours(1);

/// The length minted tokens are padded to, like GitHub's (about 520).
const TARGET_LENGTH: usize = 520;

/// `{"alg":"HS256","typ":"JWT"}`, base64url.
const HEADER: &str = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9";

/// What a token was minted for. Shown (without the token) in
/// `GET /__octoemu/state`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct TokenRecord {
    /// The token's redacted identity: the first 16 bytes of its SHA-256, in
    /// hex. octoauth's `TokenId` is computed the same way.
    pub(crate) id: String,
    #[serde(skip)]
    jti: String,
    pub(crate) app_id: u64,
    pub(crate) installation_id: u64,
    /// The token's permissions (never wider than the installation's).
    pub(crate) permissions: BTreeMap<String, Access>,
    /// The repositories it is narrowed to; `None` covers every repository
    /// the installation can access, now and later.
    pub(crate) repository_ids: Option<Vec<u64>>,
    pub(crate) issued_at: DateTime<Utc>,
    pub(crate) expires_at: DateTime<Utc>,
    pub(crate) revoked: bool,
}

impl TokenRecord {
    /// Whether the token holds `name` at `access` or higher.
    pub(crate) fn has(&self, name: &str, access: Access) -> bool {
        self.permissions
            .get(name)
            .is_some_and(|held| *held >= access)
    }

    /// Whether the token may reach repository `id`, given the
    /// installation's own repositories.
    pub(crate) fn covers(&self, id: u64) -> bool {
        self.repository_ids
            .as_ref()
            .is_none_or(|ids| ids.contains(&id))
    }
}

/// Why a presented token was not accepted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Rejected {
    /// Not a token this emulator minted (format, signature or unknown ID).
    Unknown,
    /// Past `expires_at` on the virtual clock.
    Expired,
    /// Revoked with `DELETE /installation/token` or by uninstalling.
    Revoked,
}

/// Every token minted by one emulator instance.
#[derive(Debug)]
pub(crate) struct Tokens {
    key: [u8; 32],
    by_jti: HashMap<String, TokenRecord>,
}

impl Default for Tokens {
    fn default() -> Self {
        let mut key = [0; 32];
        // Without OS randomness, fall back to per-process entropy: these
        // keys only stop forgery between tests, not by an attacker.
        if getrandom_02::getrandom(&mut key).is_err() {
            let fallback = Sha256::digest(format!(
                "{:?}{}",
                std::time::SystemTime::now(),
                std::process::id()
            ));
            key.copy_from_slice(&fallback);
        }
        Self {
            key,
            by_jti: HashMap::new(),
        }
    }
}

/// The first 16 bytes of the SHA-256 of `token`, in hex.
pub(crate) fn token_id(token: &str) -> String {
    Sha256::digest(token.as_bytes())[..16]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn random_hex(bytes: usize) -> String {
    let mut buffer = vec![0; bytes];
    if getrandom_02::getrandom(&mut buffer).is_err() {
        // Uniqueness only: `jti`s also embed a sequence via the map size.
        buffer =
            Sha256::digest(format!("{:?}", std::time::Instant::now()))[..bytes.min(32)].to_vec();
    }
    buffer.iter().map(|b| format!("{b:02x}")).collect()
}

impl Tokens {
    fn mac(&self) -> Hmac<Sha256> {
        Hmac::<Sha256>::new_from_slice(&self.key).expect("HMAC takes keys of any length")
    }

    /// Mints a token valid from `now` for [`LIFETIME`].
    pub(crate) fn mint(
        &mut self,
        app_id: u64,
        installation_id: u64,
        permissions: BTreeMap<String, Access>,
        repository_ids: Option<Vec<u64>>,
        now: DateTime<Utc>,
    ) -> (String, TokenRecord) {
        let jti = format!("{}{:x}", random_hex(16), self.by_jti.len());
        let issued_at = DateTime::from_timestamp(now.timestamp(), 0).unwrap_or(now);
        let expires_at = issued_at + LIFETIME;
        let prefix = format!("ghs_{app_id}_");
        let mut pad = String::new();
        let token = loop {
            let claims = serde_json::json!({
                "iss": "octoemu",
                "sub": format!("installation/{installation_id}"),
                "aud": "api.github.com",
                "app_id": app_id,
                "installation_id": installation_id,
                "jti": jti,
                "iat": issued_at.timestamp(),
                "nbf": issued_at.timestamp(),
                "exp": expires_at.timestamp(),
                "pad": pad,
            });
            let payload = URL_SAFE_NO_PAD.encode(claims.to_string());
            let signing_input = format!("{HEADER}.{payload}");
            let mut mac = self.mac();
            mac.update(signing_input.as_bytes());
            let signature = URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes());
            let token = format!("{prefix}{signing_input}.{signature}");
            if token.len() >= TARGET_LENGTH {
                break token;
            }
            let missing = (TARGET_LENGTH - token.len()) * 3 / 4;
            pad.push_str(&random_hex(missing.div_ceil(2).max(1))[..missing.max(1)]);
        };
        let record = TokenRecord {
            id: token_id(&token),
            jti: jti.clone(),
            app_id,
            installation_id,
            permissions,
            repository_ids,
            issued_at,
            expires_at,
            revoked: false,
        };
        self.by_jti.insert(jti, record.clone());
        (token, record)
    }

    /// The record of a token this instance minted, checked for signature,
    /// expiry (at `now`) and revocation.
    pub(crate) fn verify(&self, token: &str, now: DateTime<Utc>) -> Result<&TokenRecord, Rejected> {
        let jti = self.jti_of(token).ok_or(Rejected::Unknown)?;
        let record = self.by_jti.get(&jti).ok_or(Rejected::Unknown)?;
        if record.revoked {
            Err(Rejected::Revoked)
        } else if now >= record.expires_at {
            Err(Rejected::Expired)
        } else {
            Ok(record)
        }
    }

    /// The `jti` of a well-formed, correctly signed token.
    fn jti_of(&self, token: &str) -> Option<String> {
        let rest = token.strip_prefix("ghs_")?;
        let (app_id, jwt) = rest.split_once('_')?;
        if app_id.is_empty() || !app_id.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let (signing_input, signature) = jwt.rsplit_once('.')?;
        let (header, payload) = signing_input.split_once('.')?;
        if header != HEADER {
            return None;
        }
        let signature = URL_SAFE_NO_PAD.decode(signature).ok()?;
        let mut mac = self.mac();
        mac.update(signing_input.as_bytes());
        mac.verify_slice(&signature).ok()?;
        let claims: serde_json::Value =
            serde_json::from_slice(&URL_SAFE_NO_PAD.decode(payload).ok()?).ok()?;
        if claims["app_id"].as_u64()?.to_string() != app_id {
            return None;
        }
        claims["jti"].as_str().map(str::to_owned)
    }

    /// Revokes the token with record ID `id`.
    pub(crate) fn revoke(&mut self, id: &str) {
        for record in self.by_jti.values_mut() {
            if record.id == id {
                record.revoked = true;
            }
        }
    }

    /// Revokes every token of an installation (uninstalling).
    pub(crate) fn revoke_installation(&mut self, installation_id: u64) {
        for record in self.by_jti.values_mut() {
            if record.installation_id == installation_id {
                record.revoked = true;
            }
        }
    }

    /// Every record, oldest first.
    pub(crate) fn records(&self) -> Vec<&TokenRecord> {
        let mut records: Vec<_> = self.by_jti.values().collect();
        records.sort_by(|a, b| a.issued_at.cmp(&b.issued_at).then(a.id.cmp(&b.id)));
        records
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> DateTime<Utc> {
        DateTime::from_timestamp(1_900_000_000, 0).unwrap()
    }

    #[test]
    fn mints_tokens_in_githubs_shape() {
        let mut tokens = Tokens::default();
        let (token, record) = tokens.mint(12, 34, BTreeMap::new(), None, now());
        assert!(token.starts_with("ghs_12_eyJ"), "{token}");
        assert!((TARGET_LENGTH..TARGET_LENGTH + 8).contains(&token.len()));
        assert_eq!(token.matches('.').count(), 2);
        assert!(token.bytes().all(|b| b.is_ascii_graphic()));
        // octoscrub recognises it.
        assert!(!octoscrub::scrub(&token).contains("eyJ"));
        assert_eq!(record.expires_at - record.issued_at, LIFETIME);
        assert_eq!(record.id, token_id(&token));
        assert_eq!(tokens.verify(&token, now()).unwrap(), &record);
    }

    #[test]
    fn rejects_forgeries_expiry_and_revocation() {
        let mut tokens = Tokens::default();
        let (token, record) = tokens.mint(12, 34, BTreeMap::new(), None, now());
        let other = Tokens::default();
        assert_eq!(other.verify(&token, now()), Err(Rejected::Unknown));
        let tampered = token.replacen("ghs_12_", "ghs_13_", 1);
        assert_eq!(tokens.verify(&tampered, now()), Err(Rejected::Unknown));
        let mut flipped = token.clone().into_bytes();
        let last = flipped.len() - 1;
        flipped[last] = if flipped[last] == b'A' { b'B' } else { b'A' };
        let flipped = String::from_utf8(flipped).unwrap();
        assert_eq!(tokens.verify(&flipped, now()), Err(Rejected::Unknown));
        assert_eq!(
            tokens.verify(&token, now() + LIFETIME),
            Err(Rejected::Expired)
        );
        tokens.revoke(&record.id);
        assert_eq!(tokens.verify(&token, now()), Err(Rejected::Revoked));
    }
}
