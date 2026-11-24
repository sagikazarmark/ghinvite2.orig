//! github.com's OAuth web flow for GitHub Apps (GE-05): the authorize page
//! (`GET /login/oauth/authorize`, with its consent form posting back to the
//! same path) and the token endpoint (`POST /login/oauth/access_token`) for
//! codes and refresh tokens.
//!
//! The authorize page lists the seeded users; a test without a browser
//! passes `login=<user>` and, with auto-consent on (the default), is
//! redirected straight back with a code. Errors follow GitHub's
//! troubleshooting pages: `redirect_uri_mismatch`, `access_denied` (an admin
//! toggle) and `application_suspended` redirect to the registered callback;
//! the token endpoint answers `incorrect_client_credentials`,
//! `redirect_uri_mismatch`, `bad_verification_code`, `bad_refresh_token` and
//! `unverified_user_email`, with HTTP 200 (behaviour `oauth-error-status`),
//! and an unknown client with a 404 `{"error": "Not Found"}` (measured).

#![allow(
    clippy::result_large_err,
    reason = "a rejection is the response to send, built once per request"
)]
use crate::form::{self, html_escape};
use crate::seed::{App, Seed, User, UserKind};
use crate::state::{Shared, SharedState};
use crate::user_tokens::{DEFAULT_TOKENS_PER_HOUR, TOKENS_PER_USER_APP};
use axum::body::Bytes;
use axum::extract::State;
use axum::http::header::{ACCEPT, CACHE_CONTROL, CONTENT_TYPE, LOCATION};
use axum::http::{HeaderMap, HeaderValue, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

/// How the authorize page answers, changed through the admin API
/// (`/__octoemu/oauth…`).
#[derive(Clone, Debug)]
pub(crate) struct OAuthControls {
    /// `login=<seeded user>` redirects straight back with a code, as if
    /// the user had already authorized the App (default on).
    pub(crate) auto_consent: bool,
    /// Every authorization is declined (`error=access_denied`).
    pub(crate) deny_all: bool,
    /// Users (lowercased logins) who decline every authorization.
    pub(crate) denied: BTreeSet<String>,
}

impl Default for OAuthControls {
    fn default() -> Self {
        Self {
            auto_consent: true,
            deny_all: false,
            denied: BTreeSet::new(),
        }
    }
}

impl OAuthControls {
    /// Whether `login` declines.
    pub(crate) fn denies(&self, login: &str) -> bool {
        self.deny_all || self.denied.contains(&login.to_lowercase())
    }

    /// As `GET /__octoemu/oauth` and `/state` show it.
    pub(crate) fn view(&self, world: &Seed) -> Value {
        json!({
            "auto_consent": self.auto_consent,
            "deny_all": self.deny_all,
            "denied_users": self.denied,
            "user_tokens_per_hour": tokens_per_hour(world),
        })
    }
}

/// New user access tokens per user and App per hour before authorizing
/// needs the browser again.
pub(crate) fn tokens_per_hour(world: &Seed) -> u32 {
    world
        .settings
        .user_tokens_per_hour
        .unwrap_or(DEFAULT_TOKENS_PER_HOUR)
}

/// `error_uri` of the authorize errors, as GitHub's troubleshooting page
/// shows them; absolute on docs.github.com, as the token errors' are
/// (measured there, assumed here).
const AUTHORIZE_ERROR_URI: &str = "https://docs.github.com/apps/building-integrations/setting-up-and-registering-oauth-apps/troubleshooting-authorization-request-errors/";

/// `error_uri` of the token errors (measured).
const TOKEN_ERROR_URI: &str = "https://docs.github.com/apps/managing-oauth-apps/troubleshooting-oauth-app-access-token-request-errors/";

/// An authorize error: `(error, error_description, anchor)`.
struct AuthorizeError(&'static str, &'static str, &'static str);

const REDIRECT_MISMATCH: AuthorizeError = AuthorizeError(
    "redirect_uri_mismatch",
    "The redirect_uri MUST match the registered callback URL for this application.",
    "#redirect-uri-mismatch",
);
const ACCESS_DENIED: AuthorizeError = AuthorizeError(
    "access_denied",
    "The user has denied your application access.",
    "#access-denied",
);
const SUSPENDED: AuthorizeError = AuthorizeError(
    "application_suspended",
    "Your application has been suspended. Contact support@github.com.",
    "#application-suspended",
);

/// A token-endpoint error: `(error, error_description, error_uri anchor)`.
struct TokenError(&'static str, &'static str, &'static str);

const INCORRECT_CLIENT: TokenError = TokenError(
    "incorrect_client_credentials",
    "The client_id and/or client_secret passed are incorrect.",
    "#incorrect-client-credentials",
);
const TOKEN_REDIRECT_MISMATCH: TokenError = TokenError(
    "redirect_uri_mismatch",
    "The redirect_uri MUST match the registered callback URL for this application.",
    "#redirect-uri-mismatch2",
);
const BAD_CODE: TokenError = TokenError(
    "bad_verification_code",
    "The code passed is incorrect or expired.",
    "#bad-verification-code",
);
const UNVERIFIED_EMAIL: TokenError = TokenError(
    "unverified_user_email",
    "The user must have a verified primary email.",
    "#unverified_user_email",
);
// Measured: GitHub points a bad refresh token at the bad code's anchor.
const BAD_REFRESH: TokenError = TokenError(
    "bad_refresh_token",
    "The refresh token passed is incorrect or expired.",
    "#bad-verification-code",
);

// --- redirect URIs -----------------------------------------------------------

/// `(scheme, host, port, rest)` of an absolute http(s) URL.
fn split_url(url: &str) -> Option<(&str, &str, Option<&str>, &str)> {
    let (scheme, rest) = url.split_once("://")?;
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, path) = rest.split_at(end);
    let (host, port) = if let Some(bracketed) = authority.strip_prefix('[') {
        let (host, after) = bracketed.split_once(']')?;
        (host, after.strip_prefix(':'))
    } else {
        match authority.rsplit_once(':') {
            Some((host, port)) => (host, Some(port)),
            None => (authority, None),
        }
    };
    Some((scheme, host, port, path))
}

fn loopback(host: &str) -> bool {
    host == "localhost"
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

/// How `redirect_uri` matches one of the App's callback URLs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Match {
    Exact,
    /// Only by ignoring a loopback URL's port (documented for OAuth apps).
    LoopbackPort,
    None,
}

fn matches_callback(app: &App, redirect_uri: &str) -> Match {
    if app.callback_urls.iter().any(|c| c == redirect_uri) {
        return Match::Exact;
    }
    let Some((scheme, host, _, rest)) = split_url(redirect_uri) else {
        return Match::None;
    };
    let by_port = app.callback_urls.iter().any(|callback| {
        split_url(callback).is_some_and(|(s, h, _, r)| {
            s == scheme && h.eq_ignore_ascii_case(host) && r == rest && loopback(host)
        })
    });
    if by_port {
        Match::LoopbackPort
    } else {
        Match::None
    }
}

/// Whether `redirect_uri` is one of `app`'s callbacks, refusing a match
/// that rests on the loopback-port rule in refuse-assumed mode.
fn registered(shared: &Shared, app: &App, redirect_uri: &str) -> Result<bool, Response> {
    match matches_callback(app, redirect_uri) {
        Match::Exact => Ok(true),
        Match::LoopbackPort => match shared.refuse("redirect-uri-loopback-port") {
            Some(refusal) => Err(refusal),
            None => Ok(true),
        },
        Match::None => Ok(false),
    }
}

// --- the authorize page -----------------------------------------------------------

fn redirect(location: &str) -> Response {
    let mut response = StatusCode::FOUND.into_response();
    if let Ok(value) = HeaderValue::from_str(location) {
        response.headers_mut().insert(LOCATION, value);
    }
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

fn error_redirect(callback: &str, error: &AuthorizeError, state: Option<&str>) -> Response {
    let uri = format!("{AUTHORIZE_ERROR_URI}{}", error.2);
    let mut pairs = vec![
        ("error", error.0),
        ("error_description", error.1),
        ("error_uri", uri.as_str()),
    ];
    if let Some(state) = state {
        pairs.push(("state", state));
    }
    redirect(&form::with_query(callback, pairs))
}

fn html(status: StatusCode, title: &str, body: &str) -> Response {
    let page = format!(
        "<!DOCTYPE html>\n<html lang=\"en\"><head><meta charset=\"utf-8\">\
         <title>{title}</title></head>\n<body>\n<h1>{title}</h1>\n{body}\n</body></html>\n",
        title = html_escape(title),
    );
    (
        status,
        [
            (
                CONTENT_TYPE,
                HeaderValue::from_static("text/html; charset=utf-8"),
            ),
            (CACHE_CONTROL, HeaderValue::from_static("no-store")),
        ],
        page,
    )
        .into_response()
}

/// An error GitHub shows on its own page rather than redirecting
/// (behaviour `authorize-page-errors`).
fn error_page(shared: &Shared, status: StatusCode, message: &str) -> Response {
    shared.refuse("authorize-page-errors").unwrap_or_else(|| {
        html(
            status,
            "Authorization failed",
            &format!("<p>{}</p>", html_escape(message)),
        )
    })
}

/// The parameters the page passes through its consent forms.
const PASSED_THROUGH: &[&str] = &[
    "client_id",
    "redirect_uri",
    "state",
    "code_challenge",
    "code_challenge_method",
    "allow_signup",
];

fn consent_page(
    params: &[(String, String)],
    app: &App,
    users: &[User],
    notice: Option<&str>,
) -> Response {
    let hidden: String = PASSED_THROUGH
        .iter()
        .filter_map(|name| {
            form::get(params, name).map(|value| {
                format!(
                    "<input type=\"hidden\" name=\"{name}\" value=\"{}\">",
                    html_escape(value)
                )
            })
        })
        .collect();
    let mut body = String::new();
    if let Some(notice) = notice {
        body.push_str(&format!(
            "<p class=\"notice\">{}</p>\n",
            html_escape(notice)
        ));
    }
    body.push_str("<p>Sign in as one of the seeded users:</p>\n<ul>\n");
    for user in users {
        let login = html_escape(&user.login);
        body.push_str(&format!(
            "<li><form method=\"post\" action=\"/login/oauth/authorize\">{hidden}\
             <input type=\"hidden\" name=\"login\" value=\"{login}\">\
             <button type=\"submit\" name=\"authorize\" value=\"1\" data-login=\"{login}\">\
             Authorize as {login}</button></form></li>\n"
        ));
    }
    body.push_str(&format!(
        "</ul>\n<form method=\"post\" action=\"/login/oauth/authorize\">{hidden}\
         <button type=\"submit\" name=\"cancel\" value=\"1\">Cancel</button></form>"
    ));
    html(
        StatusCode::OK,
        &format!("Authorize {}", app.name.as_deref().unwrap_or(&app.slug)),
        &body,
    )
}

/// What the browser asked for.
enum Choice {
    /// `GET`: show the page, or consent for `login=` automatically.
    Page,
    /// The consent form: authorize as this login.
    Authorize(String),
    /// The consent form's cancel button.
    Cancel,
}

/// `GET /login/oauth/authorize`.
pub(crate) async fn authorize(State(shared): State<SharedState>, uri: Uri) -> Response {
    let params = form::parse(uri.query().unwrap_or_default());
    authorize_request(&shared, &params, Choice::Page)
}

/// `POST /login/oauth/authorize`: the consent form (`login` and
/// `authorize`, or `cancel`, plus the request's parameters).
pub(crate) async fn consent(State(shared): State<SharedState>, body: Bytes) -> Response {
    let params = form::parse(&String::from_utf8_lossy(&body));
    let choice = if form::get(&params, "cancel").is_some() {
        Choice::Cancel
    } else {
        match form::get(&params, "login") {
            Some(login) => Choice::Authorize(login.to_owned()),
            None => Choice::Page,
        }
    };
    authorize_request(&shared, &params, choice)
}

/// A PKCE challenge GitHub accepts: S256 only, 43 base64url characters,
/// both parameters or neither.
fn challenge(params: &[(String, String)]) -> Result<Option<String>, &'static str> {
    match (
        form::get(params, "code_challenge"),
        form::get(params, "code_challenge_method"),
    ) {
        (None, None) => Ok(None),
        (Some(challenge), Some("S256"))
            if challenge.len() == 43
                && challenge
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_') =>
        {
            Ok(Some(challenge.to_owned()))
        }
        (Some(_), Some("S256")) => Err("code_challenge must be 43 base64url characters"),
        (Some(_), Some(_)) => Err("code_challenge_method must be S256"),
        (Some(_), None) => Err("code_challenge needs code_challenge_method=S256"),
        (None, Some(_)) => Err("code_challenge_method without code_challenge"),
    }
}

fn authorize_request(shared: &Shared, params: &[(String, String)], choice: Choice) -> Response {
    let mut state = shared.state();
    let now = state.clock.now();
    let Some(app) = form::get(params, "client_id")
        .and_then(|id| state.world.apps.iter().find(|app| app.client_id == id))
        .cloned()
    else {
        return error_page(shared, StatusCode::NOT_FOUND, "Unknown client_id.");
    };
    let oauth_state = form::get(params, "state");
    let Some(first_callback) = app.callback_urls.first().cloned() else {
        return error_page(
            shared,
            StatusCode::BAD_REQUEST,
            "The application has no callback URL.",
        );
    };
    let callback = match form::get(params, "redirect_uri") {
        Some(given) => match registered(shared, &app, given) {
            Ok(true) => given.to_owned(),
            Ok(false) => return error_redirect(&first_callback, &REDIRECT_MISMATCH, oauth_state),
            Err(refusal) => return refusal,
        },
        None => first_callback,
    };
    if app.suspended {
        return error_redirect(&callback, &SUSPENDED, oauth_state);
    }
    let challenge = match challenge(params) {
        Ok(challenge) => challenge,
        Err(message) => return error_page(shared, StatusCode::BAD_REQUEST, message),
    };
    let user_named = |login: &str| {
        state
            .world
            .users
            .iter()
            .find(|u| u.kind == UserKind::User && u.login.eq_ignore_ascii_case(login))
            .cloned()
    };
    let users: Vec<User> = state
        .world
        .users
        .iter()
        .filter(|u| u.kind == UserKind::User)
        .cloned()
        .collect();
    let user = match choice {
        Choice::Cancel => return error_redirect(&callback, &ACCESS_DENIED, oauth_state),
        Choice::Authorize(login) => match user_named(&login) {
            Some(user) => user,
            None => return consent_page(params, &app, &users, Some("No such user.")),
        },
        Choice::Page => {
            let wanted = form::get(params, "login").and_then(user_named);
            let select = form::get(params, "prompt") == Some("select_account");
            match wanted {
                Some(user) if state.oauth.auto_consent && !select => {
                    let limit =
                        usize::try_from(tokens_per_hour(&state.world)).unwrap_or(usize::MAX);
                    if state.user_tokens.minted_last_hour(app.id, &user.login, now) >= limit {
                        if let Some(refusal) = shared.refuse("user-token-hourly-limit") {
                            return refusal;
                        }
                        return consent_page(
                            params,
                            &app,
                            &[user],
                            Some("Too many new tokens this hour: authorize again."),
                        );
                    }
                    user
                }
                _ => return consent_page(params, &app, &users, None),
            }
        }
    };
    if state.oauth.denies(&user.login) {
        return error_redirect(&callback, &ACCESS_DENIED, oauth_state);
    }
    let code =
        state
            .user_tokens
            .issue_code(app.id, user.id, &user.login, &callback, challenge, now);
    let mut pairs = vec![("code", code.as_str())];
    if let Some(oauth_state) = oauth_state {
        pairs.push(("state", oauth_state));
    }
    redirect(&form::with_query(&callback, pairs))
}

// --- the token endpoint ----------------------------------------------------------------

/// The request's parameters: a JSON object or a form body, then the query.
fn token_params(headers: &HeaderMap, uri: &Uri, body: &[u8]) -> Vec<(String, String)> {
    let json_body = headers
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.to_ascii_lowercase().starts_with("application/json"));
    let mut params = if json_body {
        match serde_json::from_slice::<Value>(body) {
            Ok(Value::Object(object)) => object
                .into_iter()
                .filter_map(|(name, value)| match value {
                    Value::String(s) => Some((name, s)),
                    Value::Number(n) => Some((name, n.to_string())),
                    _ => None,
                })
                .collect(),
            _ => Vec::new(),
        }
    } else {
        form::parse(&String::from_utf8_lossy(body))
    };
    params.extend(form::parse(uri.query().unwrap_or_default()));
    params
}

/// Answers in JSON (`Accept: application/json`) or, by default, form
/// encoding (documented).
struct Reply {
    json: bool,
}

impl Reply {
    fn send(&self, fields: &[(&str, Value)]) -> Response {
        self.send_with(StatusCode::OK, fields)
    }

    fn send_with(&self, status: StatusCode, fields: &[(&str, Value)]) -> Response {
        if self.json {
            let object: serde_json::Map<String, Value> = fields
                .iter()
                .map(|(name, value)| ((*name).to_owned(), value.clone()))
                .collect();
            crate::response::json(status, &Value::Object(object))
        } else {
            let text: Vec<(&str, String)> = fields
                .iter()
                .map(|(name, value)| {
                    let text = match value {
                        Value::String(s) => s.clone(),
                        other => other.to_string(),
                    };
                    (*name, text)
                })
                .collect();
            (
                status,
                [(
                    CONTENT_TYPE,
                    HeaderValue::from_static("application/x-www-form-urlencoded; charset=utf-8"),
                )],
                form::serialize(text.iter().map(|(n, v)| (*n, v.as_str()))),
            )
                .into_response()
        }
    }

    /// An error, with HTTP 200 (behaviour `oauth-error-status`); a guessed
    /// description is withheld in refuse-assumed mode.
    fn error(&self, shared: &Shared, error: &TokenError, assumed_text: Option<&str>) -> Response {
        if let Some(refusal) = shared.refuse("oauth-error-status") {
            return refusal;
        }
        let description = match assumed_text {
            Some(behaviour) => shared.message(behaviour, error.1),
            None => error.1.into(),
        };
        self.send(&[
            ("error", error.0.into()),
            ("error_description", description.as_ref().into()),
            ("error_uri", format!("{TOKEN_ERROR_URI}{}", error.2).into()),
        ])
    }
}

/// `POST /login/oauth/access_token`: a code (`grant_type` absent,
/// `authorization_code` or anything but `refresh_token`: behaviour
/// `unsupported-grant-type`) or a refresh token (`grant_type=refresh_token`)
/// for a user access token. A `client_id` naming no App is a 404
/// `{"error": "Not Found"}`; a known one with the wrong secret is
/// `incorrect_client_credentials` (measured).
pub(crate) async fn access_token(
    State(shared): State<SharedState>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> Response {
    let params = token_params(&headers, &uri, &body);
    let reply = Reply {
        json: headers
            .get(ACCEPT)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.to_ascii_lowercase().contains("application/json")),
    };
    let get = |name: &str| form::get(&params, name).filter(|v| !v.is_empty());
    let mut state = shared.state();
    let now = state.clock.now();
    let Some(app) = get("client_id")
        .and_then(|id| state.world.apps.iter().find(|app| app.client_id == id))
        .cloned()
    else {
        if let Some(refusal) = shared.refuse("oauth-error-status") {
            return refusal;
        }
        return reply.send_with(StatusCode::NOT_FOUND, &[("error", "Not Found".into())]);
    };
    if get("client_secret") != Some(app.client_secret.as_str()) {
        return reply.error(&shared, &INCORRECT_CLIENT, None);
    }
    let grant_type = get("grant_type");
    if !matches!(
        grant_type,
        None | Some("authorization_code" | "refresh_token")
    ) && let Some(refusal) = shared.refuse("unsupported-grant-type")
    {
        return refusal;
    }
    // GitHub reads any grant type but a refresh as the code grant
    // (measured: `password` with a bad code is `bad_verification_code`).
    let (user_id, login, counted) = if grant_type == Some("refresh_token") {
        let Some(refreshed) = get("refresh_token")
            .and_then(|token| state.user_tokens.redeem_refresh(token, app.id, now))
        else {
            return reply.error(&shared, &BAD_REFRESH, Some("bad-refresh-token-text"));
        };
        (refreshed.user_id, refreshed.login, false)
    } else {
        let Some(code) =
            get("code").and_then(|code| state.user_tokens.take_code(code, app.id, now))
        else {
            return reply.error(&shared, &BAD_CODE, None);
        };
        if let Some(given) = get("redirect_uri") {
            match registered(&shared, &app, given) {
                Ok(true) if given == code.redirect_uri => {}
                Ok(_) => return reply.error(&shared, &TOKEN_REDIRECT_MISMATCH, None),
                Err(refusal) => return refusal,
            }
        }
        let proven = match (&code.challenge, get("code_verifier")) {
            (None, None) => true,
            (Some(challenge), Some(verifier)) => {
                URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes())) == *challenge
            }
            _ => false,
        };
        if !proven {
            if let Some(refusal) = shared.refuse("pkce-exchange-errors") {
                return refusal;
            }
            return reply.error(&shared, &BAD_CODE, None);
        }
        let verified = state
            .world
            .users
            .iter()
            .find(|u| u.id == code.user_id)
            .is_none_or(|user| {
                user.email_addresses()
                    .iter()
                    .find(|e| e.primary)
                    .is_none_or(|e| e.verified)
            });
        if !verified {
            return reply.error(&shared, &UNVERIFIED_EMAIL, None);
        }
        (code.user_id, code.login, true)
    };
    if state.user_tokens.live_count(app.id, user_id, now) >= TOKENS_PER_USER_APP
        && let Some(refusal) = shared.refuse("user-token-cap")
    {
        return refusal;
    }
    let minted = state.user_tokens.mint_user_token(
        app.id,
        user_id,
        &login,
        app.expire_user_tokens,
        counted,
        now,
    );
    drop(state);
    let mut fields: Vec<(&str, Value)> = vec![("access_token", minted.token.into())];
    if let Some(refresh) = minted.refresh_token {
        fields.push((
            "expires_in",
            crate::user_tokens::ACCESS_LIFETIME.num_seconds().into(),
        ));
        fields.push(("refresh_token", refresh.into()));
        fields.push((
            "refresh_token_expires_in",
            crate::user_tokens::REFRESH_LIFETIME.num_seconds().into(),
        ));
    }
    fields.push(("scope", "".into()));
    fields.push(("token_type", "bearer".into()));
    reply.send(&fields)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redirect_uris_match_exactly_or_by_loopback_port() {
        let app = App::new(1, "a", "Iv1.a", "s")
            .callback_url("https://app.test/cb")
            .callback_url("http://127.0.0.1:8080/cb");
        assert_eq!(matches_callback(&app, "https://app.test/cb"), Match::Exact);
        assert_eq!(matches_callback(&app, "https://app.test/cb/x"), Match::None);
        assert_eq!(
            matches_callback(&app, "https://app.test:444/cb"),
            Match::None
        );
        assert_eq!(
            matches_callback(&app, "http://127.0.0.1:9999/cb"),
            Match::LoopbackPort
        );
        assert_eq!(
            matches_callback(&app, "http://127.0.0.1:9999/other"),
            Match::None
        );
        assert_eq!(matches_callback(&app, "not a url"), Match::None);
    }

    #[test]
    fn pkce_parameters() {
        let p = |pairs: &[(&str, &str)]| -> Vec<(String, String)> {
            pairs
                .iter()
                .map(|(a, b)| ((*a).into(), (*b).into()))
                .collect()
        };
        let good = "a".repeat(43);
        assert_eq!(challenge(&p(&[])), Ok(None));
        assert_eq!(
            challenge(&p(&[
                ("code_challenge", &good),
                ("code_challenge_method", "S256")
            ])),
            Ok(Some(good.clone()))
        );
        assert!(
            challenge(&p(&[
                ("code_challenge", &good),
                ("code_challenge_method", "plain")
            ]))
            .is_err()
        );
        assert!(
            challenge(&p(&[
                ("code_challenge", "short"),
                ("code_challenge_method", "S256")
            ]))
            .is_err()
        );
        assert!(challenge(&p(&[("code_challenge", &good)])).is_err());
        assert!(challenge(&p(&[("code_challenge_method", "S256")])).is_err());
    }
}
