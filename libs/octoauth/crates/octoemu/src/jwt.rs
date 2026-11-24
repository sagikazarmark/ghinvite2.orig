//! App JWT verification: RS256 against any of the App's registered public
//! keys, `iss` (client ID or App ID), `iat` and `exp` on the virtual clock.
//!
//! Documented by GitHub
//! (<https://docs.github.com/en/apps/creating-github-apps/authenticating-with-a-github-app/generating-a-json-web-token-jwt-for-a-github-app>):
//! RS256, `iss` is the client ID or the App ID, `iat` is required, `exp` is
//! at most 10 minutes in the future. The rest was measured against GitHub
//! once (fidelity scenarios `jwt-error-matrix` and `jwt-skew-tolerance`,
//! 2026-11-24) and stays `assumed` until the live suite verifies it again:
//! the error messages, how far ahead `iat` may lie, an `exp` more than
//! 10 minutes after `iat` (accepted: `exp` is bounded by now only) and a
//! numeric string `iat` (accepted).

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
/// little ahead of GitHub's. Measured (behaviour `jwt-iat-tolerance`):
/// GitHub accepted an `iat` 45 s and 60 s ahead of its `Date` and refused
/// one 300 s ahead; the strictest bound those allow.
pub(crate) const IAT_FUTURE_TOLERANCE: i64 = 60;

/// The longest validity GitHub accepts: `exp` at most 10 minutes after
/// GitHub's current time (documented; measured: 600 s accepted, 630 s
/// refused). `exp` is not bounded by `iat` (measured: 660 s after `iat`
/// is accepted; behaviour `jwt-exp-after-iat`).
pub(crate) const MAX_LIFETIME: i64 = 600;

/// Why a JWT was rejected; each becomes a 401 with [`Self::message`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum JwtError {
    /// Not three base64url parts of JSON, or not RS256 (`alg: none`
    /// included).
    Malformed,
    /// No registered key verifies the signature.
    Undecodable,
    /// `iss` is no App's client ID and not a number (missing included).
    IssuerNotInteger,
    /// `iss` is a number naming no App.
    UnknownIssuer,
    /// No `iat`.
    MissingIssuedAt,
    /// `iat` is not a number.
    IssuedAt,
    /// `iat` more than [`IAT_FUTURE_TOLERANCE`] ahead.
    IssuedInFuture,
    /// `exp` missing or already past.
    Expired,
    /// `exp` is not a number.
    Expiration,
    /// `exp` more than 10 minutes after now (documented).
    TooLong,
}

impl JwtError {
    /// GitHub's message (`assumed`, behaviour `jwt-error-messages`):
    /// measured for every case but a non-numeric `iat` or `exp` and an
    /// unknown numeric `iss`, whose texts are community-reported.
    pub(crate) fn message(self) -> &'static str {
        match self {
            Self::Malformed | Self::IssuedInFuture | Self::Expired => crate::auth::BAD_CREDENTIALS,
            Self::Undecodable => "A JSON web token could not be decoded",
            Self::IssuerNotInteger => "'Issuer' claim ('iss') must be an Integer",
            Self::UnknownIssuer => "Integration not found",
            Self::MissingIssuedAt => "Missing 'issued at' claim ('iat') in assertion",
            Self::IssuedAt => {
                "'Issued at' claim ('iat') must be an Integer representing the time that the assertion was issued"
            }
            Self::Expiration => {
                "'Expiration time' claim ('exp') must be a numeric value representing the future time at which the assertion expires"
            }
            Self::TooLong => "'Expiration time' claim ('exp') is too far in the future",
        }
    }
}

/// A verified App JWT.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Verified {
    pub(crate) app_id: u64,
    /// Accepted only thanks to [`IAT_FUTURE_TOLERANCE`] (an assumed rule).
    pub(crate) used_iat_tolerance: bool,
    /// `exp` more than [`MAX_LIFETIME`] after `iat`: accepted on a rule
    /// measured once (assumed).
    pub(crate) exp_beyond_iat: bool,
}

fn decode_json(part: &str) -> Result<Value, JwtError> {
    let bytes = URL_SAFE_NO_PAD
        .decode(part)
        .map_err(|_| JwtError::Malformed)?;
    serde_json::from_slice(&bytes).map_err(|_| JwtError::Malformed)
}

/// A time claim: a JSON integer, or a string of one (measured for `iat`:
/// GitHub accepts `"1767225600"`). `None` without the claim, `Some(None)`
/// when it is not a number.
fn time_claim(claims: &Value, name: &str) -> Option<Option<i64>> {
    claims.get(name).map(|value| match value {
        Value::Number(n) => n.as_i64(),
        Value::String(text) => text.trim().parse().ok(),
        _ => None,
    })
}

/// The App `iss` names: its client ID, or its App ID as a string or number.
fn issuer<'a>(world: &'a Seed, claims: &Value) -> Result<&'a App, JwtError> {
    let id = match claims.get("iss") {
        Some(Value::String(iss)) => {
            if let Some(app) = world.apps.iter().find(|app| app.client_id == *iss) {
                return Ok(app);
            }
            iss.parse::<u64>().map_err(|_| JwtError::IssuerNotInteger)?
        }
        Some(Value::Number(n)) => n.as_u64().ok_or(JwtError::IssuerNotInteger)?,
        _ => return Err(JwtError::IssuerNotInteger),
    };
    world
        .apps
        .iter()
        .find(|app| app.id == id)
        .ok_or(JwtError::UnknownIssuer)
}

fn verifies(app: &App, signing_input: &str, signature: &[u8]) -> bool {
    let Ok(signature) = Signature::try_from(signature) else {
        return false;
    };
    app.public_keys.iter().any(|pem| {
        let pem = pem.trim();
        let key = rsa::PublicKey::from_public_key_pem(pem)
            .or_else(|_| rsa::PublicKey::from_pkcs1_pem(pem));
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
        return Err(JwtError::Malformed);
    };
    let header = decode_json(header)?;
    if header.get("alg").and_then(Value::as_str) != Some("RS256") {
        return Err(JwtError::Malformed);
    }
    let claims = decode_json(payload)?;
    if !claims.is_object() {
        return Err(JwtError::Malformed);
    }
    let signature = URL_SAFE_NO_PAD
        .decode(signature)
        .map_err(|_| JwtError::Malformed)?;
    let app = issuer(world, &claims)?;
    let signing_input = &jwt[..header_and_payload_len(jwt)];
    if !verifies(app, signing_input, &signature) {
        return Err(JwtError::Undecodable);
    }
    let issued_at = time_claim(&claims, "iat")
        .ok_or(JwtError::MissingIssuedAt)?
        .ok_or(JwtError::IssuedAt)?;
    if issued_at > now.saturating_add(IAT_FUTURE_TOLERANCE) {
        return Err(JwtError::IssuedInFuture);
    }
    let expires_at = time_claim(&claims, "exp")
        .ok_or(JwtError::Expired)?
        .ok_or(JwtError::Expiration)?;
    if expires_at <= now {
        return Err(JwtError::Expired);
    }
    if expires_at.saturating_sub(now) > MAX_LIFETIME {
        return Err(JwtError::TooLong);
    }
    Ok(Verified {
        app_id: app.id,
        used_iat_tolerance: issued_at > now,
        exp_beyond_iat: expires_at.saturating_sub(issued_at) > MAX_LIFETIME,
    })
}

/// The length of `header.payload` in a three-part JWT.
fn header_and_payload_len(jwt: &str) -> usize {
    jwt.rfind('.').unwrap_or(jwt.len())
}
