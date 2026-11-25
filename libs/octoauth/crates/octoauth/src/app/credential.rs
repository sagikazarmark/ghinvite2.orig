//! The app's credential: an RS256 JWT, signed by the [`AppKeySigner`].
//!
//! Hand-rolled: the JWT is a fixed header, three claims and one PKCS#1 v1.5
//! signature, so a JWT library would add dependencies without removing code
//! (key validation stays ours either way). The JWT reaches the public API
//! only through [`AppCredential::expose_secret`].

use super::key::AppKeySigner;
use crate::error::SigningError;
use crate::id::AppId;
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use zeroize::Zeroizing;

/// `iat` is backdated to tolerate a local clock running ahead of GitHub's.
const IAT_BACKDATE: i64 = 60;

/// `exp - now`. GitHub rejects an `exp` more than ten minutes past its own
/// current time; 540 leaves a minute of margin.
const LIFETIME: i64 = 540;

/// The latest `exp` GitHub accepts, in seconds after its current time.
const MAX_EXP_AHEAD: i64 = 600;

/// `{"alg":"RS256","typ":"JWT"}`, base64url.
const HEADER: &str = "eyJhbGciOiJSUzI1NiIsInR5cCI6IkpXVCJ9";

/// Sign the app's credential, valid from `now - 60` to `now + 540` and issued
/// by `issuer` (the client ID, or the App ID in decimal). No skew correction,
/// no caching.
pub(crate) fn sign(signer: &dyn AppKeySigner, issuer: &str, now: i64) -> Result<Jwt, SigningError> {
    let (issued_at, expires_at) = window(now).ok_or(SigningError::InvalidTime)?;
    let claims = serde_json::json!({
        "iat": issued_at,
        "exp": expires_at,
        "iss": issuer,
    });
    let claims = serde_json::to_vec(&claims).map_err(|_| SigningError::Failed)?;
    let signing_input = format!("{HEADER}.{}", URL_SAFE_NO_PAD.encode(claims));
    let signature = signer.sign_rs256(signing_input.as_bytes())?;
    // 2048–4096-bit RSA signatures only.
    if !(256..=512).contains(&signature.len()) {
        return Err(SigningError::Failed);
    }
    Ok(Jwt {
        issued_at,
        expires_at,
        token: Zeroizing::new(format!(
            "{signing_input}.{}",
            URL_SAFE_NO_PAD.encode(signature)
        )),
    })
}

/// The `(iat, exp)` claims of a credential signed at `now`, if representable.
pub(crate) fn window(now: i64) -> Option<(i64, i64)> {
    let issued_at = now.checked_sub(IAT_BACKDATE).filter(|t| *t >= 0)?;
    let expires_at = now.checked_add(LIFETIME)?;
    Some((issued_at, expires_at))
}

/// Whether GitHub, at its time `github_now`, accepts a credential with this
/// `(iat, exp)` window. Strict: GitHub's observed leeway is not relied on.
pub(crate) fn accepted_at((issued_at, expires_at): (i64, i64), github_now: i64) -> bool {
    issued_at <= github_now
        && github_now < expires_at
        && expires_at.saturating_sub(github_now) <= MAX_EXP_AHEAD
}

/// The app's signed credential (a JWT). Not `Clone`; `Debug` is redacted;
/// zeroized on drop.
pub(crate) struct Jwt {
    issued_at: i64,
    expires_at: i64,
    token: Zeroizing<String>,
}

impl Jwt {
    /// The `(iat, exp)` claims.
    pub(crate) fn window(&self) -> (i64, i64) {
        (self.issued_at, self.expires_at)
    }

    /// `Bearer <credential>`, marked sensitive so reqwest redacts it in `Debug`.
    pub(crate) fn authorization(&self) -> reqwest::header::HeaderValue {
        crate::transport::bearer(&self.token)
    }

    pub(crate) fn expose(&self) -> &str {
        &self.token
    }
}

impl std::fmt::Debug for Jwt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Jwt(..)")
    }
}

/// The app's signed credential (a JWT), from [`App::credential`], for
/// app-level endpoints octoauth does not wrap, such as
/// `GET /app/installation-requests`. Send it as a bearer token.
///
/// It is the credential octoauth itself sends: reused for up to 5 minutes,
/// so at least 4 minutes of validity remain when [`App::credential`]
/// returns it. Get one per request; do not keep it.
///
/// Prefer [`App::with_credential`], which gives your request the clock-skew
/// correction of octoauth's own: after a 401 that GitHub's `Date` shows a
/// skewed host clock caused, it retries once with a corrected credential.
/// With a bare credential from [`App::credential`], a 401 is final unless
/// you re-learn the skew yourself: make one octoauth call (e.g.
/// [`Installation::get`] for an installation you know), then get a new
/// credential and retry once. If that call fails with
/// [`Error::AppCredentialsRejected`], do not retry: the key or the app
/// identity is wrong, or the clock is beyond correction.
///
/// A secret: not `Clone`, `Display` or `Serialize`; `Debug` shows the App ID
/// only; zeroized when the last holder (this or the app's cache) drops it.
///
/// ```no_run
/// # async fn example(app: &octoauth::App) -> Result<(), Box<dyn std::error::Error>> {
/// let credential = app.credential()?;
/// let response = reqwest::Client::new()
///     .get("https://api.github.com/app/installation-requests")
///     .bearer_auth(credential.expose_secret())
///     .send()
///     .await?;
/// # Ok(()) }
/// ```
///
/// [`App::credential`]: crate::App::credential
/// [`App::with_credential`]: crate::App::with_credential
/// [`Installation::get`]: crate::Installation::get
/// [`Error::AppCredentialsRejected`]: crate::Error::AppCredentialsRejected
pub struct AppCredential {
    app_id: AppId,
    expires_at: SystemTime,
    jwt: Arc<Jwt>,
}

impl AppCredential {
    pub(crate) fn new(app_id: AppId, jwt: Arc<Jwt>) -> Self {
        // `exp` is positive: signing requires `iat >= 0`.
        let exp = u64::try_from(jwt.window().1).unwrap_or(0);
        Self {
            app_id,
            expires_at: UNIX_EPOCH + Duration::from_secs(exp),
            jwt,
        }
    }

    /// The App the credential authenticates.
    pub fn app_id(&self) -> AppId {
        self.app_id
    }

    /// The JWT's `exp`, on the signing clock: the local clock plus the
    /// correction octoauth last measured against GitHub's.
    pub fn expires_at(&self) -> SystemTime {
        self.expires_at
    }

    /// The JWT. Use it only to authenticate a request; do not log, store or
    /// serialize it.
    pub fn expose_secret(&self) -> &str {
        self.jwt.expose()
    }
}

impl std::fmt::Debug for AppCredential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppCredential")
            .field("app_id", &self.app_id)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::key::SigningKey;
    use jsonwebtoken::{Algorithm, DecodingKey, Validation};
    use rsa::pkcs8::EncodePublicKey;

    const PEM: &str = include_str!("../../tests/fixtures/rsa2048.pem");

    const NOW: i64 = 1_800_000_000;

    fn key() -> SigningKey {
        SigningKey::from_pem(PEM).unwrap()
    }

    fn verify(credential: &Jwt) -> serde_json::Value {
        let public = key()
            .public_key()
            .to_public_key_pem(rsa::pkcs8::LineEnding::LF)
            .unwrap();
        let mut validation = Validation::new(Algorithm::RS256);
        validation.validate_exp = false;
        validation.required_spec_claims.clear();
        jsonwebtoken::decode::<serde_json::Value>(
            credential.expose(),
            &DecodingKey::from_rsa_pem(public.as_bytes()).unwrap(),
            &validation,
        )
        .unwrap()
        .claims
    }

    #[test]
    fn claims() {
        let credential = sign(&key(), "Iv23.repo", NOW).unwrap();
        let claims = verify(&credential);
        assert_eq!(claims["iss"], "Iv23.repo");
        assert_eq!(claims["iat"], NOW - 60);
        assert_eq!(claims["exp"], NOW + 540);
        assert_eq!(claims.as_object().unwrap().len(), 3);
    }

    #[test]
    fn acceptance_window() {
        let window = window(NOW).unwrap();
        assert_eq!(window, (NOW - 60, NOW + 540));
        assert!(accepted_at(window, NOW));
        assert!(accepted_at(window, NOW - 60));
        assert!(!accepted_at(window, NOW - 61), "iat in the future");
        assert!(accepted_at(window, NOW + 539));
        assert!(!accepted_at(window, NOW + 540), "expired");
        assert!(accepted_at((NOW, NOW + 600), NOW));
        assert!(!accepted_at((NOW, NOW + 601), NOW), "exp too far ahead");
        assert_eq!(credential_window(&sign(&key(), "1", NOW).unwrap()), window);
    }

    fn credential_window(credential: &Jwt) -> (i64, i64) {
        credential.window()
    }

    #[test]
    fn header_is_rs256() {
        let credential = sign(&key(), "1", NOW).unwrap();
        let header = jsonwebtoken::decode_header(credential.expose()).unwrap();
        assert_eq!(header.alg, Algorithm::RS256);
        assert_eq!(header.typ.as_deref(), Some("JWT"));
    }

    #[test]
    fn invalid_time_is_rejected() {
        let key = key();
        let at = |now| sign(&key, "1", now).map(|_| ());
        assert_eq!(at(59), Err(SigningError::InvalidTime));
        assert_eq!(at(i64::MAX), Err(SigningError::InvalidTime));
        assert_eq!(at(60), Ok(()));
    }

    struct Fixed(Vec<u8>);
    impl AppKeySigner for Fixed {
        fn sign_rs256(&self, _: &[u8]) -> Result<Vec<u8>, SigningError> {
            Ok(self.0.clone())
        }
    }

    #[test]
    fn custom_signer_is_used_and_length_checked() {
        let credential = sign(&Fixed(vec![7; 256]), "1", NOW).unwrap();
        assert!(
            credential
                .expose()
                .ends_with(&URL_SAFE_NO_PAD.encode([7u8; 256]))
        );
        for len in [0, 255, 513] {
            assert_eq!(
                sign(&Fixed(vec![7; len]), "1", NOW).map(|_| ()),
                Err(SigningError::Failed)
            );
        }
    }

    #[test]
    fn authorization_header_is_sensitive_bearer() {
        let credential = sign(&key(), "1", NOW).unwrap();
        assert_eq!(format!("{credential:?}"), "Jwt(..)");
        let value = credential.authorization();
        assert!(value.is_sensitive());
        assert_eq!(
            value.to_str().unwrap(),
            format!("Bearer {}", credential.expose())
        );
    }

    #[test]
    fn public_credential_exposes_only_on_request() {
        let jwt = Arc::new(sign(&key(), "1", NOW).unwrap());
        let credential = AppCredential::new(AppId::new(7), jwt.clone());
        assert_eq!(
            format!("{credential:?}"),
            "AppCredential { app_id: AppId(7), .. }"
        );
        assert!(std::ptr::eq(credential.expose_secret(), jwt.expose()));
        assert_eq!(credential.app_id(), AppId::new(7));
        assert_eq!(
            credential.expires_at(),
            UNIX_EPOCH + Duration::from_secs((NOW + 540) as u64)
        );
    }
}
