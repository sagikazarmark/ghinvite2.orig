//! Installation rows, shared by SQLite and D1.
use crate::{Account, AccountType, SelectedRepos};
use chrono::{DateTime, Utc};
use serde::Deserialize;

macro_rules! select {
    ($filter:literal) => {
        concat!("SELECT installation_id, account_id, account_login, account_type, installed_at, uninstalled_at, selected_repos FROM installations", $filter)
    };
}

/// Bind installation ID, account ID, login, type, installed and uninstalled
/// times, and [`encode_selected_repos`].
pub const INSERT: &str = "INSERT INTO installations (installation_id, account_id, account_login, account_type, installed_at, uninstalled_at, selected_repos) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)";
/// Changes no row for an unknown or already uninstalled installation.
pub const MARK_UNINSTALLED: &str = "UPDATE installations SET uninstalled_at = ?1 WHERE installation_id = ?2 AND uninstalled_at IS NULL";
pub const UPDATE_REPOS: &str = "UPDATE installations SET selected_repos = ?1 WHERE installation_id = ?2 AND uninstalled_at IS NULL";
pub const GET: &str = select!(" WHERE installation_id = ?1");
pub const ACTIVE_BY_ACCOUNT: &str = select!(" WHERE account_id = ?1 AND uninstalled_at IS NULL");
pub const ACTIVE_BY_LOGIN: &str = select!(" WHERE account_login = ?1 AND uninstalled_at IS NULL");
pub const LATEST_BY_LOGIN: &str =
    select!(" WHERE account_login = ?1 ORDER BY installed_at DESC, installation_id DESC LIMIT 1");
pub const LIST_ACTIVE: &str = select!(" WHERE uninstalled_at IS NULL ORDER BY installed_at");

#[derive(Debug, Deserialize)]
pub struct InstallationRow {
    pub installation_id: u64,
    pub account_id: u64,
    pub account_login: String,
    pub account_type: AccountType,
    pub installed_at: DateTime<Utc>,
    pub uninstalled_at: Option<DateTime<Utc>>,
    pub selected_repos: String,
}

impl InstallationRow {
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
    Ok(SelectedRepos::Subset(v))
}

pub fn encode_selected_repos(s: &SelectedRepos) -> String {
    match s {
        SelectedRepos::All => "all".to_string(),
        SelectedRepos::Subset(v) => serde_json::to_string(v).expect("vec<u64> serializes"),
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
