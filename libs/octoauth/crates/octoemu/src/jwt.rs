//! App JWT verification: RS256 against any of the App's registered public
//! keys, `iss` (client ID or App ID), `iat` and `exp` on the virtual clock.
//!
//! Documented by GitHub
//! (<https://docs.github.com/en/apps/creating-github-apps/authenticating-with-a-github-app/generating-a-json-web-token-jwt-for-a-github-app>):
//! RS256, `iss` is the client ID or the App ID, `iat` is required, `exp` is
//! at most 10 minutes in the future. The error messages are
//! community-reported and the tolerance for an `iat` in the future is
//! unmeasured, so both are `assumed`.

use crate::seed::{App, Seed};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rsa::pkcs1::DecodeRsaPublicKey;
use rsa::pkcs1v15::{Signature, VerifyingKey};
use rsa::pkcs8::DecodePublicKey;
use rsa::signature::Verifier;
use serde_json::Value;
use sha2::Sha256;

/// How far in the future `iat` may lie, in seconds: a client's clock a
/// little ahead of GitHub's. GitHub's tolerance is unmeasured (`assumed`,
/// behaviour `jwt-iat-tolerance`); GitHub recommends backdating `iat` by 60 s.
pub(crate) const IAT_FUTURE_TOLERANCE: i64 = 30;

/// The longest validity GitHub accepts: `exp` at most 10 minutes after
/// GitHub's current time (documented), and after `iat` (community-reported).
pub(crate) const MAX_LIFETIME: i64 = 600;

/// Why a JWT was rejected; each becomes a 401 with [`Self::message`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum JwtError {
    /// Not three base64url parts of JSON, not RS256, or a bad signature.
    Undecodable,
    /// `iss` names no App.
    UnknownIssuer,
    /// `iat` missing, not an integer, or too far in the future.
    IssuedAt,
    /// `exp` missing, not an integer, or already past.
    Expiration,
    /// `exp` more than 10 minutes after now (documented).
    TooLong,
    /// `exp` more than 10 minutes after `iat`, though not after now
    /// (community-reported, `assumed`: behaviour `jwt-exp-after-iat`).
    TooLongAfterIat,
}

impl JwtError {
    /// GitHub's message, as community-reported (`assumed`, behaviour
    /// `jwt-error-messages`).
    pub(crate) fn message(self) -> &'static str {
        match self {
            Self::Undecodable => "A JSON web token could not be decoded",
            Self::UnknownIssuer => "Integration not found",
            Self::IssuedAt => {
                "'Issued at' claim ('iat') must be an Integer representing the time that the assertion was issued"
            }
            Self::Expiration => {
                "'Expiration time' claim ('exp') must be a numeric value representing the future time at which the assertion expires"
            }
            Self::TooLong | Self::TooLongAfterIat => {
                "'Expiration time' claim ('exp') is too far in the future"
            }
        }
    }
}

/// A verified App JWT.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Verified {
    pub(crate) app_id: u64,
    /// Accepted only thanks to [`IAT_FUTURE_TOLERANCE`] (an assumed rule).
    pub(crate) used_iat_tolerance: bool,
}

fn decode_json(part: &str) -> Result<Value, JwtError> {
    let bytes = URL_SAFE_NO_PAD
        .decode(part)
        .map_err(|_| JwtError::Undecodable)?;
    serde_json::from_slice(&bytes).map_err(|_| JwtError::Undecodable)
}

/// An integer claim (a JSON integer; GitHub's message says "Integer").
fn integer(claims: &Value, name: &str) -> Option<i64> {
    claims.get(name).and_then(Value::as_i64)
}

/// The App `iss` names: its client ID, or its App ID as a string or number.
fn issuer<'a>(world: &'a Seed, claims: &Value) -> Option<&'a App> {
    match claims.get("iss")? {
        Value::String(iss) => world
            .apps
            .iter()
            .find(|app| app.client_id == *iss || app.id.to_string() == *iss),
        Value::Number(n) => {
            let id = n.as_u64()?;
            world.apps.iter().find(|app| app.id == id)
        }
        _ => None,
    }
}

fn verifies(app: &App, signing_input: &str, signature: &[u8]) -> bool {
    let Ok(signature) = Signature::try_from(signature) else {
        return false;
    };
    app.public_keys.iter().any(|pem| {
        let pem = pem.trim();
        let key = rsa::RsaPublicKey::from_public_key_pem(pem)
            .or_else(|_| rsa::RsaPublicKey::from_pkcs1_pem(pem));
        key.is_ok_and(|key| {
            VerifyingKey::<Sha256>::new(key)
                .verify(signing_input.as_bytes(), &signature)
                .is_ok()
        })
    })
}

/// Verifies `jwt` against the world's Apps at Unix time `now`.
pub(crate) fn verify(world: &Seed, jwt: &str, now: i64) -> Result<Verified, JwtError> {
    let mut parts = jwt.split('.');
    let (Some(header), Some(payload), Some(signature), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(JwtError::Undecodable);
    };
    let header = decode_json(header)?;
    if header.get("alg").and_then(Value::as_str) != Some("RS256") {
        return Err(JwtError::Undecodable);
    }
    let claims = decode_json(payload)?;
    if !claims.is_object() {
        return Err(JwtError::Undecodable);
    }
    let signature = URL_SAFE_NO_PAD
        .decode(signature)
        .map_err(|_| JwtError::Undecodable)?;
    let app = issuer(world, &claims).ok_or(JwtError::UnknownIssuer)?;
    let signing_input = &jwt[..header_and_payload_len(jwt)];
    if !verifies(app, signing_input, &signature) {
        return Err(JwtError::Undecodable);
    }
    let issued_at = integer(&claims, "iat").ok_or(JwtError::IssuedAt)?;
    if issued_at > now.saturating_add(IAT_FUTURE_TOLERANCE) {
        return Err(JwtError::IssuedAt);
    }
    let expires_at = integer(&claims, "exp").ok_or(JwtError::Expiration)?;
    if expires_at <= now {
        return Err(JwtError::Expiration);
    }
    if expires_at.saturating_sub(now) > MAX_LIFETIME {
        return Err(JwtError::TooLong);
    }
    if expires_at.saturating_sub(issued_at) > MAX_LIFETIME {
        return Err(JwtError::TooLongAfterIat);
    }
    Ok(Verified {
        app_id: app.id,
        used_iat_tolerance: issued_at > now,
    })
}

/// The length of `header.payload` in a three-part JWT.
fn header_and_payload_len(jwt: &str) -> usize {
    jwt.rfind('.').unwrap_or(jwt.len())
}
