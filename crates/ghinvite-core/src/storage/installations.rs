//! Installation rows, shared by SQLite and D1.
use crate::{Account, AccountType, SelectedRepos};
use chrono::{DateTime, Utc};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct AccountRow {
    pub installation_id: u64,
    pub account_id: u64,
    pub account_login: String,
    pub account_type: AccountType,
    pub installed_at: DateTime<Utc>,
    pub uninstalled_at: Option<DateTime<Utc>>,
    pub selected_repos: String,
}
impl AccountRow {
    pub fn into_account(self) -> super::Result<Account> {
        Ok(Account {
            installation_id: self.installation_id,
            account_id: self.account_id,
            account_login: self.account_login,
            account_type: self.account_type,
            installed_at: self.installed_at,
            uninstalled_at: self.uninstalled_at,
            selected_repos: parse_selected_repos(&self.selected_repos)?,
        })
    }
}

pub fn parse_selected_repos(raw: &str) -> super::Result<SelectedRepos> {
    if raw == "all" {
        return Ok(SelectedRepos::All);
    }
    let v: Vec<u64> = serde_json::from_str(raw)
        .map_err(|e| super::Error::Corrupt(format!("selected_repos JSON: {e}")))?;
    super::integer::validate(&v)?;
    Ok(SelectedRepos::Subset(v))
}

pub fn encode_selected_repos(s: &SelectedRepos) -> String {
    match s {
        SelectedRepos::All => "all".to_string(),
        SelectedRepos::Subset(v) => serde_json::to_string(v).expect("vec<u64> serializes"),
    }
}

/// Canonical upsert never changes the immutable app/installation/account binding.
pub const PUT_APP: &str = "INSERT INTO installations(app,installation_id,account_id,account_login,account_type,installed_at,uninstalled_at,selected_repos,suspended_at,permissions_json,installed_by_user_id,installed_by_login,projection_revision,projection_content) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14) ON CONFLICT(app,installation_id) DO UPDATE SET account_login=excluded.account_login,uninstalled_at=excluded.uninstalled_at,selected_repos=excluded.selected_repos,suspended_at=excluded.suspended_at,permissions_json=excluded.permissions_json,installed_by_user_id=excluded.installed_by_user_id,installed_by_login=excluded.installed_by_login,projection_revision=excluded.projection_revision,projection_content=excluded.projection_content WHERE installations.account_id=excluded.account_id AND installations.account_type=excluded.account_type AND installations.installed_at=excluded.installed_at AND excluded.projection_revision>installations.projection_revision AND (installations.uninstalled_at IS NULL OR installations.uninstalled_at IS excluded.uninstalled_at) AND (installations.installed_by_user_id IS NULL OR excluded.installed_by_user_id IS NOT NULL)";
pub const GET_APP: &str = "SELECT * FROM installations WHERE app=?1 AND installation_id=?2";
pub const ACTIVE_APP: &str =
    "SELECT * FROM installations WHERE app=?1 AND account_id=?2 AND uninstalled_at IS NULL";
pub const ACTIVE_APP_LOGIN: &str =
    "SELECT * FROM installations WHERE app=?1 AND account_login=?2 AND uninstalled_at IS NULL";
pub const LATEST_APP_LOGIN: &str = "SELECT * FROM installations WHERE app=?1 AND account_login=?2 ORDER BY installed_at DESC,installation_id DESC LIMIT 1";
pub const LIST_ACTIVE_APP: &str = "SELECT * FROM installations WHERE app=?1 AND uninstalled_at IS NULL ORDER BY installed_at,installation_id";
pub const PUT_SETTINGS: &str = "INSERT INTO account_settings(account_id,repo_admin_delegation,changed_by,changed_at,projection_revision,projection_content) VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(account_id) DO UPDATE SET repo_admin_delegation=excluded.repo_admin_delegation,changed_by=excluded.changed_by,changed_at=excluded.changed_at,projection_revision=excluded.projection_revision,projection_content=excluded.projection_content WHERE excluded.projection_revision>account_settings.projection_revision";
pub const GET_SETTINGS: &str = "SELECT account_id,repo_admin_delegation,changed_by,changed_at,projection_revision FROM account_settings WHERE account_id=?1";

pub fn encode_app(value: &crate::account::Installation) -> super::Result<String> {
    let valid_id = |id| id > 0 && id <= super::integer::MAX_EXACT;
    let valid_name =
        |name: &str| !name.is_empty() && name.len() <= 100 && !name.chars().any(char::is_control);
    if !valid_id(value.account.installation_id)
        || !valid_id(value.account.account_id)
        || !valid_id(value.revision)
        || !valid_name(&value.account.account_login)
        || value.installed_by_user_id.is_some() != value.installed_by_login.is_some()
        || value.installed_by_user_id.is_some_and(|id| !valid_id(id))
        || value
            .installed_by_login
            .as_deref()
            .is_some_and(|name| !valid_name(name))
        || value.permissions.len() > 256
        || value.permissions.keys().any(|key| {
            key.is_empty()
                || key.len() > 64
                || !key
                    .bytes()
                    .all(|c| c.is_ascii_lowercase() || c == b'_' || c.is_ascii_digit())
        })
        || matches!(&value.account.selected_repos,SelectedRepos::Subset(ids) if ids.iter().any(|id| !valid_id(*id)))
    {
        return Err(super::Error::ProjectionInvariant(
            "invalid installation snapshot".into(),
        ));
    }
    super::integer::validate(value)?;
    serde_json::to_string(value)
        .map_err(|_| super::Error::ProjectionInvariant("invalid installation snapshot".into()))
}
pub fn applied_app(
    expected: &crate::account::Installation,
    actual: Option<crate::account::Installation>,
) -> super::Result<()> {
    let Some(actual) = actual else {
        return Err(super::Error::ProjectionInvariant(
            "installation snapshot missing".into(),
        ));
    };
    if actual.app != expected.app
        || actual.account.installation_id != expected.account.installation_id
        || actual.account.account_id != expected.account.account_id
        || actual.account.account_type != expected.account.account_type
        || actual.account.installed_at != expected.account.installed_at
        || actual.revision < expected.revision
        || (actual.revision == expected.revision && actual != *expected)
    {
        return Err(super::Error::ProjectionInvariant(
            "installation snapshot conflict".into(),
        ));
    }
    Ok(())
}
pub fn encode_settings(value: &crate::account::AccountSettings) -> super::Result<String> {
    if value.account_id == 0 || value.changed_by == 0 || value.revision == 0 {
        return Err(super::Error::ProjectionInvariant(
            "invalid settings snapshot".into(),
        ));
    }
    super::integer::validate(value)?;
    serde_json::to_string(value)
        .map_err(|_| super::Error::ProjectionInvariant("invalid settings snapshot".into()))
}
pub fn applied_settings(
    expected: &crate::account::AccountSettings,
    actual: Option<crate::account::AccountSettings>,
) -> super::Result<()> {
    if actual.is_none_or(|actual| {
        actual.account_id != expected.account_id
            || actual.revision < expected.revision
            || (actual.revision == expected.revision && actual != *expected)
    }) {
        return Err(super::Error::ProjectionInvariant(
            "settings snapshot conflict".into(),
        ));
    }
    Ok(())
}
#[derive(Debug, Deserialize)]
pub struct AppRow {
    #[serde(flatten)]
    pub account: AccountRow,
    pub app: crate::account::App,
    pub projection_revision: u64,
    pub suspended_at: Option<DateTime<Utc>>,
    pub permissions_json: String,
    pub installed_by_user_id: Option<u64>,
    pub installed_by_login: Option<String>,
}
impl AppRow {
    pub fn decode(self) -> super::Result<crate::account::Installation> {
        let permissions = serde_json::from_str(&self.permissions_json)
            .map_err(|_| super::Error::Corrupt("invalid installation permissions".into()))?;
        Ok(crate::account::Installation {
            app: self.app,
            revision: self.projection_revision,
            account: self.account.into_account()?,
            suspended_at: self.suspended_at,
            permissions,
            installed_by_user_id: self.installed_by_user_id,
            installed_by_login: self.installed_by_login,
        })
    }
}
#[derive(Debug, Deserialize)]
pub struct SettingsRow {
    pub account_id: u64,
    pub projection_revision: u64,
    pub repo_admin_delegation: i64,
    pub changed_by: u64,
    pub changed_at: DateTime<Utc>,
}
impl SettingsRow {
    pub fn decode(self) -> super::Result<crate::account::AccountSettings> {
        if !matches!(self.repo_admin_delegation, 0 | 1) {
            return Err(super::Error::Corrupt("invalid delegation flag".into()));
        }
        Ok(crate::account::AccountSettings {
            account_id: self.account_id,
            revision: self.projection_revision,
            repo_admin_delegation: self.repo_admin_delegation == 1,
            changed_by: self.changed_by,
            changed_at: self.changed_at,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_selected_repos_all() {
        assert_eq!(parse_selected_repos("all").unwrap(), SelectedRepos::All);
    }

    #[test]
    fn parse_selected_repos_subset() {
        assert_eq!(
            parse_selected_repos("[1,2,3]").unwrap(),
            SelectedRepos::Subset(vec![1, 2, 3])
        );
    }

    #[test]
    fn parse_selected_repos_rejects_garbage() {
        assert!(matches!(
            parse_selected_repos("not json"),
            Err(super::super::Error::Corrupt(_))
        ));
    }

    #[test]
    fn encode_selected_repos_round_trip() {
        for s in [
            SelectedRepos::All,
            SelectedRepos::Subset(vec![]),
            SelectedRepos::Subset(vec![10, 20, 30]),
        ] {
            assert_eq!(parse_selected_repos(&encode_selected_repos(&s)).unwrap(), s);
        }
    }
}
