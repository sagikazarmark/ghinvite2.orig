//! `Storage` implementation backed by `sqlx::SqlitePool`.

use crate::records::{InstallationRow, encode_selected_repos, u64_to_i64};
use crate::{ConflictKind, Error, GithubInvitationUpdate, RequestDecision, Result, Storage};
use async_trait::async_trait;
use audit::AuditEvent;
use chrono::{DateTime, Utc};
use domain::{
    Account, GithubInvitation, GithubInvitationId, InvitationRequest, RequestId, SelectedRepos,
    ShareLink, ShareLinkId, User,
};
use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use std::path::Path;

#[derive(Clone)]
pub struct SqlxStorage {
    pub(crate) pool: SqlitePool,
}

impl SqlxStorage {
    /// Open an in-memory SQLite database. Each call gets a fresh DB.
    /// Useful for unit and storage-suite tests.
    pub async fn in_memory() -> Result<Self> {
        let opts = SqliteConnectOptions::new()
            .in_memory(true)
            .foreign_keys(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1) // in-memory shares one connection so all queries see the same DB
            .connect_with(opts)
            .await?;
        let s = Self { pool };
        s.run_migrations().await?;
        Ok(s)
    }

    /// Open a file-backed SQLite database (creating it if missing).
    pub async fn at_path(path: &Path) -> Result<Self> {
        let opts = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .foreign_keys(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(8)
            .connect_with(opts)
            .await?;
        let s = Self { pool };
        s.run_migrations().await?;
        Ok(s)
    }

    /// Run the embedded migrations.
    pub async fn run_migrations(&self) -> Result<()> {
        // sqlx::migrate! is a proc-macro that requires a string LITERAL — not a const.
        // The path is resolved relative to CARGO_MANIFEST_DIR (this crate's Cargo.toml),
        // so from `crates/storage/` we reach `migrations/` at the workspace root.
        sqlx::migrate!("../../migrations")
            .run(&self.pool)
            .await
            .map_err(|e| crate::Error::Database(sqlx::Error::Migrate(Box::new(e))))?;
        Ok(())
    }
}

/// Map a SQLite UNIQUE-violation error to our typed [`ConflictKind`] by
/// inspecting the error message for the constraint name. Falls back to
/// `default` when no specific constraint is matched.
/// Map a SQLite UNIQUE-violation error message to our typed [`ConflictKind`].
///
/// SQLite formats its UNIQUE constraint violations as
/// `"UNIQUE constraint failed: <table>.<column>[, <table>.<column>...]"`,
/// using the underlying *column*, not the index name. We pattern-match on
/// table.column pairs to identify which constraint fired and fall back to
/// `default` for primary-key collisions and other unique violations.
fn classify_unique(db: &dyn sqlx::error::DatabaseError, default: ConflictKind) -> ConflictKind {
    let m = db.message();
    if m.contains("share_links.slug") {
        ConflictKind::DuplicateSlug
    } else if m.contains("invitation_requests.share_link_id")
        && m.contains("invitation_requests.requester_id")
    {
        ConflictKind::DuplicatePendingRequest
    } else if m.contains("installations.account_id") {
        // Only the partial unique index covers `installations.account_id`;
        // `installation_id` (PK) hits would say "installations.installation_id".
        ConflictKind::DuplicateActiveInstallation
    } else {
        default
    }
}

#[async_trait]
impl Storage for SqlxStorage {
    async fn insert_installation(&self, account: &Account) -> Result<()> {
        let res = sqlx::query(
            r#"
            INSERT INTO installations
              (installation_id, account_id, account_login, account_type, installed_at, uninstalled_at, selected_repos)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
            "#,
        )
        .bind(u64_to_i64(account.installation_id))
        .bind(u64_to_i64(account.account_id))
        .bind(&account.account_login)
        .bind(account.account_type.to_string())
        .bind(account.installed_at)
        .bind(account.uninstalled_at)
        .bind(encode_selected_repos(&account.selected_repos))
        .execute(&self.pool)
        .await;

        match res {
            Ok(_) => Ok(()),
            Err(sqlx::Error::Database(db)) if db.is_unique_violation() => Err(Error::Conflict(
                classify_unique(&*db, ConflictKind::DuplicateId),
            )),
            Err(e) => Err(Error::Database(e)),
        }
    }

    async fn mark_installation_uninstalled(
        &self,
        installation_id: u64,
        when: DateTime<Utc>,
    ) -> Result<()> {
        let res = sqlx::query(
            r#"UPDATE installations SET uninstalled_at = ?1 WHERE installation_id = ?2 AND uninstalled_at IS NULL"#,
        )
        .bind(when)
        .bind(u64_to_i64(installation_id))
        .execute(&self.pool)
        .await?;
        if res.rows_affected() == 0 {
            return Err(Error::NotFound);
        }
        Ok(())
    }

    async fn update_installation_repos(
        &self,
        installation_id: u64,
        selected: &SelectedRepos,
    ) -> Result<()> {
        let res = sqlx::query(
            r#"UPDATE installations SET selected_repos = ?1 WHERE installation_id = ?2 AND uninstalled_at IS NULL"#,
        )
        .bind(encode_selected_repos(selected))
        .bind(u64_to_i64(installation_id))
        .execute(&self.pool)
        .await?;
        if res.rows_affected() == 0 {
            return Err(Error::NotFound);
        }
        Ok(())
    }

    async fn get_installation(&self, installation_id: u64) -> Result<Option<Account>> {
        let row: Option<InstallationRow> = sqlx::query_as(
            r#"SELECT installation_id, account_id, account_login, account_type, installed_at, uninstalled_at, selected_repos
               FROM installations WHERE installation_id = ?1"#,
        )
        .bind(u64_to_i64(installation_id))
        .fetch_optional(&self.pool)
        .await?;
        row.map(|r| r.try_into_domain()).transpose()
    }

    async fn get_active_installation_by_account_id(
        &self,
        account_id: u64,
    ) -> Result<Option<Account>> {
        let row: Option<InstallationRow> = sqlx::query_as(
            r#"SELECT installation_id, account_id, account_login, account_type, installed_at, uninstalled_at, selected_repos
               FROM installations WHERE account_id = ?1 AND uninstalled_at IS NULL"#,
        )
        .bind(u64_to_i64(account_id))
        .fetch_optional(&self.pool)
        .await?;
        row.map(|r| r.try_into_domain()).transpose()
    }

    async fn get_active_installation_by_login(&self, login: &str) -> Result<Option<Account>> {
        let row: Option<InstallationRow> = sqlx::query_as(
            r#"SELECT installation_id, account_id, account_login, account_type, installed_at, uninstalled_at, selected_repos
               FROM installations WHERE account_login = ?1 AND uninstalled_at IS NULL"#,
        )
        .bind(login)
        .fetch_optional(&self.pool)
        .await?;
        row.map(|r| r.try_into_domain()).transpose()
    }

    async fn list_active_installations(&self) -> Result<Vec<Account>> {
        let rows: Vec<InstallationRow> = sqlx::query_as(
            r#"SELECT installation_id, account_id, account_login, account_type, installed_at, uninstalled_at, selected_repos
               FROM installations WHERE uninstalled_at IS NULL ORDER BY installed_at"#,
        )
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(|r| r.try_into_domain()).collect()
    }

    // --- stubs for the rest of the trait, filled in by later tasks ---
    async fn upsert_user(&self, _user: &User) -> Result<()> {
        unimplemented!("Task 17")
    }
    async fn get_user(&self, _user_id: u64) -> Result<Option<User>> {
        unimplemented!("Task 17")
    }
    async fn insert_share_link(&self, _link: &ShareLink) -> Result<()> {
        unimplemented!("Task 18")
    }
    async fn mark_share_link_revoked(
        &self,
        _id: ShareLinkId,
        _by_user: u64,
        _when: DateTime<Utc>,
    ) -> Result<()> {
        unimplemented!("Task 18")
    }
    async fn get_share_link_by_id(&self, _id: ShareLinkId) -> Result<Option<ShareLink>> {
        unimplemented!("Task 18")
    }
    async fn get_share_link_by_slug(&self, _slug: &str) -> Result<Option<ShareLink>> {
        unimplemented!("Task 18")
    }
    async fn list_share_links_for_account(&self, _account_id: u64) -> Result<Vec<ShareLink>> {
        unimplemented!("Task 18")
    }
    async fn insert_invitation_request_and_increment_uses(
        &self,
        _request: &InvitationRequest,
    ) -> Result<()> {
        unimplemented!("Task 19")
    }
    async fn record_request_decision(&self, _decision: &RequestDecision) -> Result<()> {
        unimplemented!("Task 19")
    }
    async fn get_invitation_request(&self, _id: RequestId) -> Result<Option<InvitationRequest>> {
        unimplemented!("Task 19")
    }
    async fn list_pending_requests_for_account(
        &self,
        _account_id: u64,
    ) -> Result<Vec<InvitationRequest>> {
        unimplemented!("Task 19")
    }
    async fn list_requests_for_link(
        &self,
        _link_id: ShareLinkId,
    ) -> Result<Vec<InvitationRequest>> {
        unimplemented!("Task 19")
    }
    async fn insert_github_invitation(&self, _invitation: &GithubInvitation) -> Result<()> {
        unimplemented!("Task 20")
    }
    async fn update_github_invitation(&self, _update: &GithubInvitationUpdate) -> Result<()> {
        unimplemented!("Task 20")
    }
    async fn get_github_invitation(
        &self,
        _id: GithubInvitationId,
    ) -> Result<Option<GithubInvitation>> {
        unimplemented!("Task 20")
    }
    async fn get_github_invitation_by_github_id(
        &self,
        _github_id: u64,
    ) -> Result<Option<GithubInvitation>> {
        unimplemented!("Task 20")
    }
    async fn list_pending_github_invitations_for_installation(
        &self,
        _installation_id: u64,
    ) -> Result<Vec<GithubInvitation>> {
        unimplemented!("Task 20")
    }
    async fn audit(&self, _event: &AuditEvent) -> Result<()> {
        unimplemented!("Task 21")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use domain::{AccountType, SelectedRepos};

    pub(crate) fn dt(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    pub(crate) fn sample_account(installation_id: u64, account_id: u64, login: &str) -> Account {
        Account {
            installation_id,
            account_id,
            account_login: login.to_string(),
            account_type: AccountType::Organization,
            installed_at: dt("2026-05-04T12:00:00Z"),
            uninstalled_at: None,
            selected_repos: SelectedRepos::All,
        }
    }

    #[tokio::test]
    async fn in_memory_creates_schema() {
        let s = SqlxStorage::in_memory().await.unwrap();
        let row: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM _sqlx_migrations")
            .fetch_one(&s.pool)
            .await
            .unwrap();
        assert!(row.0 >= 1, "expected at least one migration applied");
    }

    #[tokio::test]
    async fn known_tables_exist() {
        let s = SqlxStorage::in_memory().await.unwrap();
        for table in [
            "installations",
            "users",
            "share_links",
            "share_link_repos",
            "invitation_requests",
            "github_invitations",
            "audit_events",
        ] {
            let row: Option<(String,)> =
                sqlx::query_as("SELECT name FROM sqlite_master WHERE type='table' AND name=?1")
                    .bind(table)
                    .fetch_optional(&s.pool)
                    .await
                    .unwrap();
            assert!(row.is_some(), "table {table} missing");
        }
    }

    #[tokio::test]
    async fn insert_and_get_installation() {
        let s = SqlxStorage::in_memory().await.unwrap();
        let acct = sample_account(1, 100, "acme");
        s.insert_installation(&acct).await.unwrap();

        let by_id = s.get_installation(1).await.unwrap().unwrap();
        assert_eq!(by_id, acct);

        let by_acc = s
            .get_active_installation_by_account_id(100)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(by_acc.installation_id, 1);

        let by_login = s
            .get_active_installation_by_login("acme")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(by_login.installation_id, 1);
    }

    #[tokio::test]
    async fn duplicate_installation_id_conflict() {
        let s = SqlxStorage::in_memory().await.unwrap();
        let acct = sample_account(1, 100, "acme");
        s.insert_installation(&acct).await.unwrap();
        let err = s.insert_installation(&acct).await.unwrap_err();
        match err {
            Error::Conflict(ConflictKind::DuplicateId)
            | Error::Conflict(ConflictKind::DuplicateActiveInstallation) => (),
            other => panic!("expected DuplicateId/DuplicateActiveInstallation, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn duplicate_active_installation_for_same_account_conflict() {
        let s = SqlxStorage::in_memory().await.unwrap();
        s.insert_installation(&sample_account(1, 100, "acme"))
            .await
            .unwrap();
        // Different installation_id, same account_id, both active → partial unique index hits.
        let err = s
            .insert_installation(&sample_account(2, 100, "acme"))
            .await
            .unwrap_err();
        assert!(
            matches!(err, Error::Conflict(ConflictKind::DuplicateActiveInstallation)),
            "got {err:?}"
        );
    }

    #[tokio::test]
    async fn mark_uninstalled_hides_from_active_lookups() {
        let s = SqlxStorage::in_memory().await.unwrap();
        let acct = sample_account(1, 100, "acme");
        s.insert_installation(&acct).await.unwrap();
        s.mark_installation_uninstalled(1, dt("2026-05-05T00:00:00Z"))
            .await
            .unwrap();
        assert!(
            s.get_active_installation_by_account_id(100)
                .await
                .unwrap()
                .is_none()
        );
        // Direct lookup still finds it (kept for audit):
        let direct = s.get_installation(1).await.unwrap().unwrap();
        assert!(direct.uninstalled_at.is_some());
    }

    #[tokio::test]
    async fn second_uninstall_returns_not_found() {
        let s = SqlxStorage::in_memory().await.unwrap();
        s.insert_installation(&sample_account(1, 100, "acme"))
            .await
            .unwrap();
        s.mark_installation_uninstalled(1, dt("2026-05-05T00:00:00Z"))
            .await
            .unwrap();
        let err = s
            .mark_installation_uninstalled(1, dt("2026-05-06T00:00:00Z"))
            .await
            .unwrap_err();
        assert!(matches!(err, Error::NotFound));
    }

    #[tokio::test]
    async fn update_repos_swaps_subset() {
        let s = SqlxStorage::in_memory().await.unwrap();
        let acct = sample_account(1, 100, "acme");
        s.insert_installation(&acct).await.unwrap();

        s.update_installation_repos(1, &SelectedRepos::Subset(vec![1, 2, 3]))
            .await
            .unwrap();

        let got = s.get_installation(1).await.unwrap().unwrap();
        assert_eq!(got.selected_repos, SelectedRepos::Subset(vec![1, 2, 3]));
    }

    #[tokio::test]
    async fn list_active_omits_uninstalled() {
        let s = SqlxStorage::in_memory().await.unwrap();
        s.insert_installation(&sample_account(1, 100, "a"))
            .await
            .unwrap();
        s.insert_installation(&sample_account(2, 200, "b"))
            .await
            .unwrap();
        s.mark_installation_uninstalled(2, dt("2026-05-06T00:00:00Z"))
            .await
            .unwrap();

        let active = s.list_active_installations().await.unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].installation_id, 1);
    }
}
