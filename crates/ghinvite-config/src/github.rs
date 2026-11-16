//! GitHub's base URLs: the REST API, the web origin (OAuth pages) and the
//! avatar CDN.
//!
//! Production always talks to GitHub. Outside production a deployment can
//! point all three at a stand-in such as the `octoemu` emulator with
//! `GITHUB_API_BASE`, `GITHUB_WEB_BASE` and `GITHUB_AVATAR_BASE`; each one
//! unset keeps GitHub's. A production composition root rejects any of them.
//!
//! Every base follows octoauth's rule (`ghinvite_github::rest::parse_base_url`):
//! `https`, or `http` on a loopback host, without credentials, query or
//! fragment. A trailing slash is optional; every client joins paths onto
//! the base the same way with or without it.

use crate::apps::{AppDeploymentError, bad, production_override};
use url::Url;

/// GitHub's REST API.
pub const DEFAULT_API_BASE: &str = ghinvite_github::rest::DEFAULT_API_BASE;
/// GitHub's web origin, which serves the OAuth pages.
pub const DEFAULT_WEB_BASE: &str = "https://github.com";
/// GitHub's avatar CDN.
pub const DEFAULT_AVATAR_BASE: &str = ghinvite_github::avatar::DEFAULT_AVATAR_BASE;

/// The variables that override GitHub's bases, outside production only.
pub const OVERRIDES: [&str; 3] = ["GITHUB_API_BASE", "GITHUB_WEB_BASE", "GITHUB_AVATAR_BASE"];

/// Whether a composition root serves production. Each root decides this
/// from what it already is, never from a variable:
///
/// | Root | Production | Development |
/// |---|---|---|
/// | `ghinvite-web-server` | no arguments | `--local-dev` |
/// | `ghinvite-workflows-server` | no arguments | `--local-unsigned` |
/// | the Workers | ordinary (deployed) builds | `runtime-tests` builds |
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Deployment {
    /// Talks to GitHub; any `GITHUB_*_BASE` is a configuration error.
    Production,
    /// Local development and test harnesses: `GITHUB_*_BASE` may point at a
    /// stand-in.
    Development,
}

/// The base URLs every GitHub client of a deployment uses.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GithubBases {
    api: Url,
    web: Url,
    avatar: Url,
}

impl GithubBases {
    /// GitHub itself.
    pub fn github() -> Self {
        Self {
            api: Url::parse(DEFAULT_API_BASE).expect("GitHub's API base is valid"),
            web: Url::parse(DEFAULT_WEB_BASE).expect("GitHub's web base is valid"),
            avatar: Url::parse(DEFAULT_AVATAR_BASE).expect("GitHub's avatar base is valid"),
        }
    }

    /// `GITHUB_API_BASE`, `GITHUB_WEB_BASE` and `GITHUB_AVATAR_BASE`, each
    /// defaulting to GitHub's. In [`Deployment::Production`] setting any of
    /// them fails (a production-override error naming the variable); outside
    /// production an invalid base fails with its name. Errors never contain
    /// the value.
    pub fn from_config(
        deployment: Deployment,
        mut read: impl FnMut(&'static str) -> Option<String>,
    ) -> Result<Self, AppDeploymentError> {
        let mut bases = Self::github();
        for (field, base) in
            OVERRIDES
                .into_iter()
                .zip([&mut bases.api, &mut bases.web, &mut bases.avatar])
        {
            let Some(value) = read(field) else {
                continue;
            };
            if deployment == Deployment::Production {
                return Err(production_override(field));
            }
            *base = ghinvite_github::rest::parse_base_url(&value).ok_or_else(|| bad(field))?;
        }
        Ok(bases)
    }

    /// The REST API base (`https://api.github.com/` by default).
    pub fn api(&self) -> &str {
        self.api.as_str()
    }

    /// The web origin serving `/login/oauth/*` (`https://github.com/`).
    pub fn web(&self) -> &str {
        self.web.as_str()
    }

    /// The avatar CDN (`https://avatars.githubusercontent.com/`).
    pub fn avatar(&self) -> &str {
        self.avatar.as_str()
    }
}

impl Default for GithubBases {
    fn default() -> Self {
        Self::github()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apps::DeploymentErrorKind;
    use std::collections::BTreeMap;

    fn parse(
        deployment: Deployment,
        values: &[(&'static str, &str)],
    ) -> Result<GithubBases, AppDeploymentError> {
        let values: BTreeMap<_, _> = values.iter().copied().collect();
        GithubBases::from_config(deployment, |field| values.get(field).map(|v| v.to_string()))
    }

    #[test]
    fn unset_overrides_are_github_in_every_deployment() {
        for deployment in [Deployment::Production, Deployment::Development] {
            let bases = parse(deployment, &[]).unwrap();
            assert_eq!(bases, GithubBases::github());
            assert_eq!(bases.api(), "https://api.github.com/");
            assert_eq!(bases.web(), "https://github.com/");
            assert_eq!(bases.avatar(), "https://avatars.githubusercontent.com/");
        }
    }

    #[test]
    fn production_rejects_every_override_by_name() {
        for field in OVERRIDES {
            for value in ["https://api.github.com", "http://127.0.0.1:1", "", "SECRET"] {
                let error = parse(Deployment::Production, &[(field, value)]).unwrap_err();
                assert_eq!(error.field, field);
                assert_eq!(error.kind, DeploymentErrorKind::ProductionOverride);
                let message = error.to_string();
                assert!(message.contains(field), "{message}");
                assert!(message.contains("production"), "{message}");
                assert!(!message.contains("SECRET"), "{message}");
            }
        }
    }

    #[test]
    fn development_accepts_https_and_loopback_http() {
        let bases = parse(
            Deployment::Development,
            &[
                ("GITHUB_API_BASE", "http://127.0.0.1:3001"),
                ("GITHUB_WEB_BASE", "http://localhost:3001/"),
                ("GITHUB_AVATAR_BASE", "https://ghe.example.com/avatars"),
            ],
        )
        .unwrap();
        assert_eq!(bases.api(), "http://127.0.0.1:3001/");
        assert_eq!(bases.web(), "http://localhost:3001/");
        assert_eq!(bases.avatar(), "https://ghe.example.com/avatars");
        let bases = parse(
            Deployment::Development,
            &[("GITHUB_API_BASE", "http://[::1]:2")],
        )
        .unwrap();
        assert_eq!(bases.api(), "http://[::1]:2/");
        // The others keep GitHub's.
        assert_eq!(bases.web(), "https://github.com/");
        assert_eq!(bases.avatar(), "https://avatars.githubusercontent.com/");
    }

    #[test]
    fn development_rejects_invalid_bases_by_name_never_value() {
        for field in OVERRIDES {
            for value in [
                "http://ghe.example.com",
                "http://10.0.0.1:3001",
                "https://user:SECRET@ghe.example.com",
                "https://ghe.example.com/?q=SECRET",
                "https://ghe.example.com/#SECRET",
                "ftp://ghe.example.com",
                "mailto:SECRET@example.com",
                "SECRET",
                "",
            ] {
                let error = parse(Deployment::Development, &[(field, value)]).unwrap_err();
                assert_eq!(error.field, field, "{value}");
                assert_eq!(error.kind, DeploymentErrorKind::Invalid);
                assert!(!format!("{error} {error:?}").contains("SECRET"));
            }
        }
    }

    #[test]
    fn a_trailing_slash_is_optional() {
        for (with, without) in [
            ("http://127.0.0.1:9/", "http://127.0.0.1:9"),
            ("http://127.0.0.1:9/api/v3/", "http://127.0.0.1:9/api/v3"),
        ] {
            let api = |value| {
                parse(Deployment::Development, &[("GITHUB_API_BASE", value)])
                    .unwrap()
                    .api()
                    .trim_end_matches('/')
                    .to_owned()
            };
            assert_eq!(api(with), api(without));
        }
        let avatar = |value| {
            let bases = parse(Deployment::Development, &[("GITHUB_AVATAR_BASE", value)]).unwrap();
            ghinvite_github::avatar::canonical_url_at(
                bases.avatar(),
                ghinvite_github::ids::UserId::new(7),
            )
        };
        assert_eq!(
            avatar("http://127.0.0.1:9/avatars/"),
            avatar("http://127.0.0.1:9/avatars")
        );
    }
}
