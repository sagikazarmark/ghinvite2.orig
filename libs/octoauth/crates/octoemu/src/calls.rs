//! The call log: one record per API, web or avatar request, with no secret
//! in it. Admin requests (`/__octoemu/…`) are not recorded.

use axum::http::{HeaderMap, Uri, header::AUTHORIZATION};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

/// One recorded request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Call {
    /// Position in the log, from 1; keeps counting across
    /// `DELETE /__octoemu/calls`.
    pub seq: u64,
    /// When the response was sent, by the virtual clock.
    pub at: DateTime<Utc>,
    /// The HTTP method.
    pub method: String,
    /// The path and query, with secret-looking query values replaced by
    /// `<redacted>` and [`octoscrub`] run over the rest.
    pub path: String,
    /// Which part of the emulator was called.
    pub surface: Surface,
    /// The matched route as `METHOD /pattern`, or `None` when nothing is
    /// implemented there (the 501 fallback).
    pub route: Option<String>,
    /// The shape of the `Authorization` header; never its value.
    pub credential: CredentialKind,
    /// Who the route verified the caller to be; `None` without a credential,
    /// when the credential was rejected, and on the 501 fallback.
    #[serde(default)]
    pub identity: Option<Identity>,
    /// The response status.
    pub status: u16,
    /// The `X-GitHub-Request-Id` the response carried.
    pub request_id: String,
}

/// The part of the emulator a path belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Surface {
    /// The REST API (`api.github.com`), at `/`.
    Api,
    /// github.com's web pages, at `/login/…`.
    Web,
    /// Avatar images (`avatars.githubusercontent.com`), at `/avatars/…`.
    Avatars,
    /// The emulator's own API, at `/__octoemu/…`.
    Admin,
}

impl Surface {
    /// The surface `path` belongs to.
    pub fn of(path: &str) -> Self {
        let under = |prefix: &str| {
            path.strip_prefix(prefix)
                .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
        };
        if under("/__octoemu") {
            Self::Admin
        } else if under("/login") {
            Self::Web
        } else if under("/avatars") {
            Self::Avatars
        } else {
            Self::Api
        }
    }
}

/// A verified caller, as the call log records it: identities and a
/// redacted token ID, never a secret.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Identity {
    /// An App, by a JWT that verified against one of its keys.
    App {
        /// The App ID.
        app_id: u64,
    },
    /// An installation, by a valid installation token.
    Installation {
        /// The App ID.
        app_id: u64,
        /// The installation ID.
        installation_id: u64,
        /// The token's redacted ID: the first 16 bytes of its SHA-256 in hex,
        /// as octoauth's `TokenId` displays it.
        token_id: String,
    },
}

/// What an `Authorization` header looks like, judged by shape alone. It is
/// not a verdict: [`Call::identity`] holds what the route verified.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CredentialKind {
    /// No `Authorization` header.
    None,
    /// A JWT (an App's credential).
    Jwt,
    /// `ghs_…`: an installation token.
    InstallationToken,
    /// `ghu_…`: a user access token from a GitHub App.
    UserToken,
    /// `gho_…`: an OAuth App token.
    OauthToken,
    /// `ghp_…` or `github_pat_…`: a personal access token.
    PersonalAccessToken,
    /// `Basic …` (client ID and secret).
    Basic,
    /// Anything else.
    Unknown,
}

impl CredentialKind {
    /// Classifies the request's `Authorization` header.
    pub fn of(headers: &HeaderMap) -> Self {
        let Some(value) = headers.get(AUTHORIZATION) else {
            return Self::None;
        };
        let Ok(value) = value.to_str() else {
            return Self::Unknown;
        };
        let (scheme, credential) = value.trim().split_once(' ').unwrap_or(("", value));
        let credential = credential.trim();
        match scheme.to_ascii_lowercase().as_str() {
            "basic" => Self::Basic,
            "bearer" | "token" => Self::of_token(credential),
            _ => Self::Unknown,
        }
    }

    fn of_token(token: &str) -> Self {
        if token.starts_with("ghs_") {
            Self::InstallationToken
        } else if token.starts_with("ghu_") {
            Self::UserToken
        } else if token.starts_with("gho_") {
            Self::OauthToken
        } else if token.starts_with("ghp_") || token.starts_with("github_pat_") {
            Self::PersonalAccessToken
        } else if token.starts_with("eyJ") && token.split('.').count() == 3 {
            Self::Jwt
        } else {
            Self::Unknown
        }
    }
}

const REDACTED: &str = "<redacted>";

/// Query parameter names whose values are never recorded.
fn secret_parameter(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    matches!(
        name.as_str(),
        "code" | "state" | "code_verifier" | "client_secret" | "password" | "key" | "jwt"
    ) || ["token", "secret", "signature"]
        .iter()
        .any(|word| name.contains(word))
}

/// The path and query of `uri` with secret values removed.
pub(crate) fn redacted_path(uri: &Uri) -> String {
    let path = uri.path();
    let recorded = match uri.query() {
        None => path.to_owned(),
        Some(query) => {
            let query = query
                .split('&')
                .map(|pair| match pair.split_once('=') {
                    Some((name, _)) if secret_parameter(name) => format!("{name}={REDACTED}"),
                    _ => pair.to_owned(),
                })
                .collect::<Vec<_>>()
                .join("&");
            format!("{path}?{query}")
        }
    };
    // A backstop for tokens in path segments and unexpected parameter names.
    octoscrub::scrub(&recorded).into_owned()
}

/// The most records kept; older ones are dropped first.
pub(crate) const CAPACITY: usize = 10_000;

/// The log itself.
#[derive(Debug, Default)]
pub(crate) struct CallLog {
    calls: VecDeque<Call>,
    dropped: u64,
}

impl CallLog {
    pub(crate) fn record(&mut self, call: Call) {
        if self.calls.len() == CAPACITY {
            self.calls.pop_front();
            self.dropped += 1;
        }
        self.calls.push_back(call);
    }

    pub(crate) fn calls(&self) -> Vec<Call> {
        self.calls.iter().cloned().collect()
    }

    /// Records dropped to stay within [`CAPACITY`] since the last clear.
    pub(crate) fn dropped(&self) -> u64 {
        self.dropped
    }

    pub(crate) fn clear(&mut self) {
        self.calls.clear();
        self.dropped = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn kind(authorization: &str) -> CredentialKind {
        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, HeaderValue::from_str(authorization).unwrap());
        CredentialKind::of(&headers)
    }

    #[test]
    fn classifies_credentials_by_shape() {
        assert_eq!(CredentialKind::of(&HeaderMap::new()), CredentialKind::None);
        assert_eq!(
            kind("Bearer eyJhbGciOiJSUzI1NiJ9.e30.c2ln"),
            CredentialKind::Jwt
        );
        assert_eq!(
            kind("token ghs_12_eyJhbGciOiJSUzI1NiJ9.e30.c2ln"),
            CredentialKind::InstallationToken
        );
        assert_eq!(kind("Bearer ghu_abc"), CredentialKind::UserToken);
        assert_eq!(kind("bearer gho_abc"), CredentialKind::OauthToken);
        assert_eq!(
            kind("token github_pat_abc"),
            CredentialKind::PersonalAccessToken
        );
        assert_eq!(kind("Basic SXYxOnNlY3JldA=="), CredentialKind::Basic);
        assert_eq!(kind("Bearer opaque"), CredentialKind::Unknown);
        assert_eq!(kind("Digest x"), CredentialKind::Unknown);
    }

    #[test]
    fn redacts_secret_query_values() {
        let uri: Uri =
            "/login/oauth/access_token?client_id=Iv1.x&client_secret=s3&code=abc&page=2&my_token=t"
                .parse()
                .unwrap();
        assert_eq!(
            redacted_path(&uri),
            "/login/oauth/access_token?client_id=Iv1.x&client_secret=<redacted>&code=<redacted>&page=2&my_token=<redacted>"
        );
        let token = format!("ghu_{}", "a1".repeat(18));
        let uri: Uri = format!("/applications/Iv1.x/token/{token}")
            .parse()
            .unwrap();
        assert_eq!(redacted_path(&uri), "/applications/Iv1.x/token/<redacted>");
    }

    #[test]
    fn classifies_surfaces() {
        assert_eq!(Surface::of("/__octoemu/calls"), Surface::Admin);
        assert_eq!(Surface::of("/__octoemu"), Surface::Admin);
        assert_eq!(Surface::of("/__octoemuX"), Surface::Api);
        assert_eq!(Surface::of("/login/oauth/authorize"), Surface::Web);
        assert_eq!(Surface::of("/avatars/u/1"), Surface::Avatars);
        assert_eq!(Surface::of("/repos/a/b"), Surface::Api);
        assert_eq!(Surface::of("/"), Surface::Api);
    }

    #[test]
    fn keeps_a_bounded_log() {
        let mut log = CallLog::default();
        let call = |seq| Call {
            seq,
            at: Utc::now(),
            method: "GET".into(),
            path: "/".into(),
            surface: Surface::Api,
            route: None,
            credential: CredentialKind::None,
            identity: None,
            status: 501,
            request_id: String::new(),
        };
        for seq in 0..CAPACITY as u64 + 3 {
            log.record(call(seq));
        }
        assert_eq!(log.calls().len(), CAPACITY);
        assert_eq!(log.calls()[0].seq, 3);
        assert_eq!(log.dropped(), 3);
        log.clear();
        assert!(log.calls().is_empty());
        assert_eq!(log.dropped(), 0);
    }
}
