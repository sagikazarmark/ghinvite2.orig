//! [`InstallationInfo`]: what `GET /app/installations/{id}` reports.

use crate::error::Error;
use crate::id::{AppId, InstallationId};
use crate::transport::Response;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::time::SystemTime;

/// An installation as GitHub reports it, from [`Installation::get`]. Holds
/// no secrets.
///
/// [`Installation::get`]: crate::Installation::get
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct InstallationInfo {
    /// The installation ID (always the one requested).
    pub id: InstallationId,

    /// The App it belongs to (always the requesting App).
    pub app_id: AppId,

    /// The account the App is installed on.
    pub account: InstallationAccount,

    /// The granted permissions: GitHub's names (`members`, `metadata`, …) and
    /// access levels (`read`, `write`, `admin`), as GitHub sends them, so
    /// permissions newer than this crate are kept. See
    /// [`permission`](Self::permission).
    pub permissions: BTreeMap<String, String>,

    /// Which repositories the installation can access.
    pub repository_selection: RepositorySelection,

    /// When the installation was suspended (whole seconds); `None` if it is
    /// not.
    pub suspended_at: Option<SystemTime>,
}

impl InstallationInfo {
    /// The granted access level for permission `name` (GitHub's name, e.g.
    /// `"members"`), if any.
    pub fn permission(&self, name: &str) -> Option<&str> {
        self.permissions.get(name).map(String::as_str)
    }
}

/// The account an installation belongs to.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct InstallationAccount {
    /// The account's numeric ID (never 0).
    pub id: u64,

    /// The user or organization login; an enterprise's slug.
    pub login: String,

    /// The kind of account.
    pub kind: AccountKind,
}

/// The kind of account an App is installed on.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
#[non_exhaustive]
pub enum AccountKind {
    /// A personal account.
    User,

    /// An organization.
    Organization,

    /// An enterprise.
    Enterprise,

    /// A kind this crate does not know.
    Unknown,
}

/// Which repositories an installation can access.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
#[non_exhaustive]
pub enum RepositorySelection {
    /// Every repository of the account, including future ones.
    All,

    /// Only the repositories chosen at installation.
    Selected,

    /// A value this crate does not know.
    Unknown,
}

impl InstallationInfo {
    /// Parse a 200 of `GET /app/installations/{id}`. A body that does not
    /// describe installation `id` of App `app_id` is [`Error::Decode`]: an
    /// answer about another installation is never trusted.
    pub(super) fn from_response(
        response: &Response,
        app_id: AppId,
        id: InstallationId,
    ) -> Result<Self, Error> {
        #[derive(Deserialize)]
        struct Wire {
            id: u64,
            app_id: u64,
            account: Account,
            permissions: BTreeMap<String, String>,
            repository_selection: String,
            /// Required, but may be `null`.
            #[serde(deserialize_with = "Option::deserialize")]
            suspended_at: Option<String>,
            target_type: Option<String>,
        }

        #[derive(Deserialize)]
        struct Account {
            id: u64,
            login: Option<String>,
            slug: Option<String>,
            #[serde(rename = "type")]
            kind: Option<String>,
        }

        let wire: Wire = serde_json::from_slice(&response.body).map_err(|_| Error::Decode)?;
        if wire.id != id.get() || wire.app_id != app_id.get() || wire.account.id == 0 {
            return Err(Error::Decode);
        }
        let login = [wire.account.login, wire.account.slug]
            .into_iter()
            .flatten()
            .find(|login| !login.is_empty())
            .ok_or(Error::Decode)?;
        // Enterprise accounts carry no `type`; the installation's
        // `target_type` names the kind for every account.
        let kind = match wire.account.kind.or(wire.target_type).as_deref() {
            Some("User") => AccountKind::User,
            Some("Organization") => AccountKind::Organization,
            Some("Enterprise") => AccountKind::Enterprise,
            _ => AccountKind::Unknown,
        };
        let repository_selection = match wire.repository_selection.as_str() {
            "all" => RepositorySelection::All,
            "selected" => RepositorySelection::Selected,
            _ => RepositorySelection::Unknown,
        };
        let suspended_at = wire
            .suspended_at
            .map(|at| {
                let seconds = chrono::DateTime::parse_from_rfc3339(&at)
                    .map_err(|_| Error::Decode)?
                    .timestamp();
                super::unix_time(seconds).ok_or(Error::Decode)
            })
            .transpose()?;
        Ok(Self {
            id,
            app_id,
            account: InstallationAccount {
                id: wire.account.id,
                login,
                kind,
            },
            permissions: wire.permissions,
            repository_selection,
            suspended_at,
        })
    }
}
