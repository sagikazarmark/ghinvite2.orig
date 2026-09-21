//! `Storage` implementation backed by `sqlx::SqlitePool`.

use crate::records::{InstallationRow, encode_selected_repos, u64_to_i64};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use ghinvite_core::audit::AuditEvent;
use ghinvite_core::storage::{
    ConflictKind, Error, GithubInvitationUpdate, RequestDecision, Result, Storage,
};
use ghinvite_core::{
    Account, GithubInvitation, GithubInvitationId, InvitationLink, InvitationLinkId,
    InvitationRequest, RequestId, SelectedRepos, User,
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
            .await
            .map_err(crate::to_db_err)?;
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
            .await
            .map_err(crate::to_db_err)?;
        let s = Self { pool };
        s.run_migrations().await?;
        Ok(s)
    }

    /// Test/debug-only: read all audit events for an account in occurrence order.
    /// Production browsing uses the bounded `Storage::list_audit_events`.
    #[cfg(any(test, feature = "test-util"))]
    pub async fn debug_list_audit(&self, account_id: u64) -> Result<Vec<AuditEvent>> {
        let rows: Vec<crate::records::AuditEventRow> = sqlx::query_as(
            r#"SELECT id, account_id, occurred_at, event_type, actor_kind, actor_id,
                      target_kind, target_id, metadata, request_id
               FROM audit_events WHERE account_id = ?1 ORDER BY occurred_at, id"#,
        )
        .bind(u64_to_i64(account_id))
        .fetch_all(&self.pool)
        .await
        .map_err(crate::to_db_err)?;
        rows.into_iter().map(|r| r.try_into_domain()).collect()
    }

    /// Test-only fault injection for exercising durable audit retries against
    /// real storage. Use with `in_memory()`'s single-connection pool because
    /// SQLite temporary triggers are connection-local.
    #[cfg(feature = "test-util")]
    pub async fn debug_set_audit_failure(&self, fail: bool) -> Result<()> {
        let sql = if fail {
            "CREATE TEMP TRIGGER fail_audit BEFORE INSERT ON audit_events
             BEGIN SELECT RAISE(FAIL, 'injected audit failure'); END"
        } else {
            "DROP TRIGGER fail_audit"
        };
        sqlx::query(sql)
            .execute(&self.pool)
            .await
            .map_err(crate::to_db_err)?;
        Ok(())
    }

    /// Test-only invalid projection storage rule, repaired by removing it.
    #[cfg(feature = "test-util")]
    pub async fn debug_set_projection_invariant_failure(&self, fail: bool) -> Result<()> {
        let sql = if fail {
            "CREATE TEMP TRIGGER fail_projection BEFORE INSERT ON audit_events
             BEGIN SELECT RAISE(ABORT, 'projection_invariant'); END"
        } else {
            "DROP TRIGGER fail_projection"
        };
        sqlx::query(sql)
            .execute(&self.pool)
            .await
            .map_err(crate::to_db_err)?;
        Ok(())
    }

    /// Simulate a committed audit insert whose acknowledgement was lost.
    /// SQLite's AFTER-trigger RAISE(FAIL) leaves the inserted row intact.
    /// Like `debug_set_audit_failure`, requires the single-connection test pool.
    #[cfg(feature = "test-util")]
    pub async fn debug_set_audit_ack_loss(&self, fail: bool) -> Result<()> {
        let sql = if fail {
            "CREATE TEMP TRIGGER lose_audit_ack AFTER INSERT ON audit_events
             BEGIN SELECT RAISE(FAIL, 'injected audit acknowledgement loss'); END"
        } else {
            "DROP TRIGGER lose_audit_ack"
        };
        sqlx::query(sql)
            .execute(&self.pool)
            .await
            .map_err(crate::to_db_err)?;
        Ok(())
    }

    /// Run the embedded migrations.
    pub async fn run_migrations(&self) -> Result<()> {
        // sqlx::migrate! is a proc-macro that requires a string LITERAL — not a const.
        // The path is resolved relative to CARGO_MANIFEST_DIR (this crate's Cargo.toml),
        // so from `crates/ghinvite-storage-sqlx/` we reach `migrations/` at the workspace root.
        sqlx::migrate!("../../migrations")
            .run(&self.pool)
            .await
            .map_err(|e| Error::Database(e.to_string()))?;
        Ok(())
    }
}

/// Collapse a `LEFT JOIN invitation_links × invitation_link_repos` result set into at most
/// one [`InvitationLink`]. Returns `None` if the join produced zero rows.
fn group_one_invitation_link(
    rows: Vec<(
        crate::records::InvitationLinkRow,
        Option<i64>,
        Option<String>,
    )>,
) -> Result<Option<InvitationLink>> {
    let mut iter = rows.into_iter();
    let Some((row, first_repo_id, first_repo_name)) = iter.next() else {
        return Ok(None);
    };
    let mut repos = Vec::new();
    if let (Some(rid), Some(name)) = (first_repo_id, first_repo_name) {
        repos.push(ghinvite_core::InvitationLinkRepo {
            repo_id: rid as u64,
            repo_full_name: name,
        });
    }
    for (_, repo_id, repo_full_name) in iter {
        if let (Some(rid), Some(name)) = (repo_id, repo_full_name) {
            repos.push(ghinvite_core::InvitationLinkRepo {
                repo_id: rid as u64,
                repo_full_name: name,
            });
        }
    }
    Ok(Some(row.try_into_domain(repos)?))
}

/// Map a SQLite UNIQUE-violation error message to our typed [`ConflictKind`].
///
/// SQLite formats its UNIQUE constraint violations as
/// `"UNIQUE constraint failed: <table>.<column>[, <table>.<column>...]"`,
/// using the underlying *column*, not the index name. We pattern-match on
/// table.column pairs to identify which constraint fired and fall back to
/// `default` for primary-key collisions and other unique violations.
fn classify_unique(db: &dyn sqlx::error::DatabaseError, default: ConflictKind) -> ConflictKind {
    let m = db.message();
    if m.contains("invitation_links.slug") {
        ConflictKind::DuplicateSlug
    } else if m.contains("invitation_requests.invitation_link_id")
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
    async fn retain_admin_attempt(
        &self,
        scope: &str,
        id: &str,
        binding: &str,
        payload: &str,
        expires_at: i64,
        now: i64,
    ) -> Result<ghinvite_core::storage::admin_attempts::StoredAttempt> {
        use ghinvite_core::storage::admin_attempts::*;
        sqlx::query(CLEANUP)
            .bind(now)
            .execute(&self.pool)
            .await
            .map_err(crate::to_db_err)?;
        sqlx::query(INSERT)
            .bind(scope)
            .bind(id)
            .bind(binding)
            .bind(payload)
            .bind(expires_at)
            .execute(&self.pool)
            .await
            .map_err(crate::to_db_err)?;
        let (id, payload): (String, String) = sqlx::query_as(BY_BINDING)
            .bind(scope)
            .bind(binding)
            .bind(id)
            .bind(now)
            .fetch_one(&self.pool)
            .await
            .map_err(crate::to_db_err)?;
        Ok(StoredAttempt { id, payload })
    }
    async fn get_admin_attempt(
        &self,
        scope: &str,
        id: &str,
        now: i64,
    ) -> Result<Option<ghinvite_core::storage::admin_attempts::StoredAttempt>> {
        use ghinvite_core::storage::admin_attempts::*;
        let row: Option<(String, String)> = sqlx::query_as(GET)
            .bind(scope)
            .bind(id)
            .bind(now)
            .fetch_optional(&self.pool)
            .await
            .map_err(crate::to_db_err)?;
        Ok(row.map(|(id, payload)| StoredAttempt { id, payload }))
    }
    async fn list_admin_attempts(
        &self,
        scope: &str,
        now: i64,
    ) -> Result<Vec<ghinvite_core::storage::admin_attempts::StoredAttempt>> {
        use ghinvite_core::storage::admin_attempts::*;
        let rows: Vec<(String, String)> = sqlx::query_as(LIST)
            .bind(scope)
            .bind(now)
            .fetch_all(&self.pool)
            .await
            .map_err(crate::to_db_err)?;
        Ok(rows
            .into_iter()
            .map(|(id, payload)| StoredAttempt { id, payload })
            .collect())
    }
    async fn settle_github_invitation(
        &self,
        transition: &ghinvite_core::storage::settlement::Settlement,
    ) -> Result<()> {
        use ghinvite_core::storage::settlement;
        let input = settlement::encode(transition)?;
        let mut tx = self.pool.begin().await.map_err(crate::to_db_err)?;
        for statement in settlement::STATEMENTS {
            sqlx::query(statement)
                .bind(&input)
                .execute(&mut *tx)
                .await
                .map_err(crate::to_db_err)?;
        }
        tx.commit().await.map_err(crate::to_db_err)
    }
    async fn list_github_invitations_for_request(
        &self,
        id: RequestId,
    ) -> Result<Vec<GithubInvitation>> {
        let rows: Vec<crate::records::GithubInvitationRow> = sqlx::query_as(
            "SELECT * FROM github_invitations WHERE invitation_request_id = ? ORDER BY id",
        )
        .bind(id.to_string())
        .fetch_all(&self.pool)
        .await
        .map_err(crate::to_db_err)?;
        rows.into_iter().map(|row| row.try_into_domain()).collect()
    }
    async fn delivery_attempt_exists(&self, id: GithubInvitationId) -> Result<bool> {
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM delivery_attempts WHERE invitation_id = ? AND retryable = 0)")
            .bind(id.to_string())
            .fetch_one(&self.pool)
            .await
            .map_err(crate::to_db_err)
    }
    async fn claim_delivery_attempt(
        &self,
        command: &ghinvite_core::delivery::CreateCommand,
    ) -> Result<Option<u64>> {
        let encoded = serde_json::to_string(command).map_err(|e| Error::Corrupt(e.to_string()))?;
        let generation: Option<i64> = sqlx::query_scalar("INSERT INTO delivery_attempts(invitation_id, command) VALUES (?, ?) ON CONFLICT(invitation_id) DO UPDATE SET generation = generation + 1, retryable = 0 WHERE retryable = 1 AND command = excluded.command RETURNING generation")
            .bind(command.invitation_id.to_string()).bind(&encoded).fetch_optional(&self.pool).await.map_err(crate::to_db_err)?;
        let old: String =
            sqlx::query_scalar("SELECT command FROM delivery_attempts WHERE invitation_id = ?")
                .bind(command.invitation_id.to_string())
                .fetch_one(&self.pool)
                .await
                .map_err(crate::to_db_err)?;
        if old != encoded {
            return Err(Error::ProjectionInvariant(
                "delivery attempt conflict".into(),
            ));
        }
        Ok(generation.map(|n| n as u64))
    }

    async fn reject_delivery_attempt(&self, id: GithubInvitationId, generation: u64) -> Result<()> {
        sqlx::query(
            "UPDATE delivery_attempts SET retryable = 1 WHERE invitation_id = ? AND generation = ?",
        )
        .bind(id.to_string())
        .bind(generation as i64)
        .execute(&self.pool)
        .await
        .map_err(crate::to_db_err)?;
        Ok(())
    }

    async fn project_delivery(
        &self,
        receipt: &ghinvite_core::delivery::CreateReceipt,
    ) -> Result<()> {
        use ghinvite_core::storage::delivery_projection as sql;
        let encoded = sql::encode(receipt)?;
        let mut tx = self.pool.begin().await.map_err(crate::to_db_err)?;
        for statement in sql::statements() {
            sqlx::query(&statement)
                .bind(&encoded)
                .execute(&mut *tx)
                .await
                .map_err(crate::to_db_err)?;
        }
        tx.commit().await.map_err(crate::to_db_err)?;
        Ok(())
    }

    async fn list_delivery_for_request(
        &self,
        id: RequestId,
    ) -> Result<Vec<ghinvite_core::delivery::CreateReceipt>> {
        let rows: Vec<String> = sqlx::query_scalar(
            "SELECT receipt FROM delivery_outcomes WHERE request_id = ? ORDER BY invitation_id",
        )
        .bind(id.to_string())
        .fetch_all(&self.pool)
        .await
        .map_err(crate::to_db_err)?;
        rows.into_iter()
            .map(|row| serde_json::from_str(&row).map_err(|e| Error::Corrupt(e.to_string())))
            .collect()
    }

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
            Err(sqlx::Error::Database(db)) if db.is_foreign_key_violation() => {
                Err(Error::Conflict(ConflictKind::ForeignKey))
            }
            Err(e) => Err(Error::Database(e.to_string())),
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
        .await
        .map_err(crate::to_db_err)?;
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
        .await
        .map_err(crate::to_db_err)?;
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
        .await
        .map_err(crate::to_db_err)?;
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
        .await
        .map_err(crate::to_db_err)?;
        row.map(|r| r.try_into_domain()).transpose()
    }

    async fn get_active_installation_by_login(&self, login: &str) -> Result<Option<Account>> {
        let row: Option<InstallationRow> = sqlx::query_as(
            r#"SELECT installation_id, account_id, account_login, account_type, installed_at, uninstalled_at, selected_repos
               FROM installations WHERE account_login = ?1 AND uninstalled_at IS NULL"#,
        )
        .bind(login)
        .fetch_optional(&self.pool)
        .await
        .map_err(crate::to_db_err)?;
        row.map(|r| r.try_into_domain()).transpose()
    }

    async fn get_latest_installation_by_login(&self, login: &str) -> Result<Option<Account>> {
        let row: Option<InstallationRow> = sqlx::query_as(
            "SELECT installation_id, account_id, account_login, account_type, installed_at, uninstalled_at, selected_repos FROM installations WHERE account_login = ? ORDER BY installed_at DESC, installation_id DESC LIMIT 1",
        ).bind(login).fetch_optional(&self.pool).await.map_err(crate::to_db_err)?;
        row.map(|r| r.try_into_domain()).transpose()
    }

    async fn list_active_installations(&self) -> Result<Vec<Account>> {
        let rows: Vec<InstallationRow> = sqlx::query_as(
            r#"SELECT installation_id, account_id, account_login, account_type, installed_at, uninstalled_at, selected_repos
               FROM installations WHERE uninstalled_at IS NULL ORDER BY installed_at"#,
        )
        .fetch_all(&self.pool)
        .await
        .map_err(crate::to_db_err)?;
        rows.into_iter().map(|r| r.try_into_domain()).collect()
    }

    // --- stubs for the rest of the trait, filled in by later tasks ---
    async fn upsert_user(&self, user: &User) -> Result<()> {
        sqlx::query(
            r#"
            INSERT INTO users (user_id, login, avatar_url, last_seen_at)
            VALUES (?1, ?2, ?3, ?4)
            ON CONFLICT(user_id) DO UPDATE SET
                login = excluded.login,
                avatar_url = excluded.avatar_url,
                last_seen_at = excluded.last_seen_at
            "#,
        )
        .bind(u64_to_i64(user.user_id))
        .bind(&user.login)
        .bind(user.avatar_url.as_deref())
        .bind(user.last_seen_at)
        .execute(&self.pool)
        .await
        .map_err(crate::to_db_err)?;
        Ok(())
    }

    async fn get_user(&self, user_id: u64) -> Result<Option<User>> {
        let row: Option<crate::records::UserRow> = sqlx::query_as(
            r#"SELECT user_id, login, avatar_url, last_seen_at FROM users WHERE user_id = ?1"#,
        )
        .bind(u64_to_i64(user_id))
        .fetch_optional(&self.pool)
        .await
        .map_err(crate::to_db_err)?;
        Ok(row.map(|r| r.into_domain()))
    }
    async fn insert_invitation_link(&self, link: &InvitationLink) -> Result<()> {
        let mut tx = self.pool.begin().await.map_err(crate::to_db_err)?;
        sqlx::query(
            r#"
            INSERT INTO invitation_links
              (id, slug, installation_id, account_id, created_by, created_at, expires_at,
               max_uses, uses_count, permission, approval_required, description, internal_note,
               revoked_at, revoked_by)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
            "#,
        )
        .bind(link.id.to_string())
        .bind(link.slug.as_str())
        .bind(u64_to_i64(link.installation_id))
        .bind(u64_to_i64(link.account_id))
        .bind(u64_to_i64(link.created_by))
        .bind(link.created_at)
        .bind(link.expires_at)
        .bind(link.max_uses.map(i64::from))
        .bind(i64::from(link.uses_count))
        .bind(link.permission.to_string())
        .bind(if link.approval_required { 1_i64 } else { 0 })
        .bind(&link.description)
        .bind(link.internal_note.as_deref())
        .bind(link.revoked_at)
        .bind(link.revoked_by.map(u64_to_i64))
        .execute(&mut *tx)
        .await
        .map_err(|e| match e {
            sqlx::Error::Database(db) if db.is_unique_violation() => {
                Error::Conflict(classify_unique(&*db, ConflictKind::DuplicateId))
            }
            sqlx::Error::Database(db) if db.is_foreign_key_violation() => {
                Error::Conflict(ConflictKind::ForeignKey)
            }
            other => Error::Database(other.to_string()),
        })?;

        for repo in &link.repos {
            sqlx::query(
                r#"INSERT INTO invitation_link_repos (invitation_link_id, repo_id, repo_full_name)
                   VALUES (?1, ?2, ?3)"#,
            )
            .bind(link.id.to_string())
            .bind(u64_to_i64(repo.repo_id))
            .bind(&repo.repo_full_name)
            .execute(&mut *tx)
            .await
            .map_err(crate::to_db_err)?;
        }
        tx.commit().await.map_err(crate::to_db_err)?;
        Ok(())
    }

    async fn update_invitation_link_metadata(
        &self,
        account_id: u64,
        id: InvitationLinkId,
        description: &str,
        internal_note: Option<&str>,
    ) -> Result<()> {
        let res = sqlx::query(
            "UPDATE invitation_links SET description = ?1, internal_note = ?2
             WHERE account_id = ?3 AND id = ?4",
        )
        .bind(description)
        .bind(internal_note)
        .bind(u64_to_i64(account_id))
        .bind(id.to_string())
        .execute(&self.pool)
        .await
        .map_err(crate::to_db_err)?;
        if res.rows_affected() == 0 {
            return Err(Error::NotFound);
        }
        Ok(())
    }

    async fn mark_invitation_link_revoked(
        &self,
        id: InvitationLinkId,
        by_user: u64,
        when: DateTime<Utc>,
    ) -> Result<()> {
        let res = sqlx::query(
            r#"UPDATE invitation_links SET revoked_at = ?1, revoked_by = ?2
               WHERE id = ?3 AND revoked_at IS NULL"#,
        )
        .bind(when)
        .bind(u64_to_i64(by_user))
        .bind(id.to_string())
        .execute(&self.pool)
        .await
        .map_err(crate::to_db_err)?;
        if res.rows_affected() == 0 {
            return Err(Error::NotFound);
        }
        Ok(())
    }

    async fn get_invitation_link_by_id(
        &self,
        id: InvitationLinkId,
    ) -> Result<Option<InvitationLink>> {
        let rows: Vec<crate::records::InvitationLinkJoinRow> = sqlx::query_as(
            r#"
                SELECT l.id, l.slug, l.installation_id, l.account_id, l.created_by, l.created_at,
                       l.expires_at, l.max_uses, l.uses_count, l.permission, l.approval_required,
                       l.description, l.internal_note, l.revoked_at, l.revoked_by,
                       r.repo_id, r.repo_full_name
                FROM invitation_links l
                LEFT JOIN invitation_link_repos r ON r.invitation_link_id = l.id
                WHERE l.id = ?1
                ORDER BY r.repo_id
                "#,
        )
        .bind(id.to_string())
        .fetch_all(&self.pool)
        .await
        .map_err(crate::to_db_err)?;
        group_one_invitation_link(rows.into_iter().map(|j| j.split()).collect())
    }

    async fn get_invitation_link_by_slug(&self, slug: &str) -> Result<Option<InvitationLink>> {
        let rows: Vec<crate::records::InvitationLinkJoinRow> = sqlx::query_as(
            r#"
                SELECT l.id, l.slug, l.installation_id, l.account_id, l.created_by, l.created_at,
                       l.expires_at, l.max_uses, l.uses_count, l.permission, l.approval_required,
                       l.description, l.internal_note, l.revoked_at, l.revoked_by,
                       r.repo_id, r.repo_full_name
                FROM invitation_links l
                LEFT JOIN invitation_link_repos r ON r.invitation_link_id = l.id
                WHERE l.slug = ?1
                ORDER BY r.repo_id
                "#,
        )
        .bind(slug)
        .fetch_all(&self.pool)
        .await
        .map_err(crate::to_db_err)?;
        group_one_invitation_link(rows.into_iter().map(|j| j.split()).collect())
    }

    async fn list_invitation_links_for_account(
        &self,
        account_id: u64,
    ) -> Result<Vec<InvitationLink>> {
        use std::collections::BTreeMap;

        let rows: Vec<crate::records::InvitationLinkJoinRow> = sqlx::query_as(
            r#"
                SELECT l.id, l.slug, l.installation_id, l.account_id, l.created_by, l.created_at,
                       l.expires_at, l.max_uses, l.uses_count, l.permission, l.approval_required,
                       l.description, l.internal_note, l.revoked_at, l.revoked_by,
                       r.repo_id, r.repo_full_name
                FROM invitation_links l
                LEFT JOIN invitation_link_repos r ON r.invitation_link_id = l.id
                WHERE l.account_id = ?1
                ORDER BY l.created_at DESC, l.id, r.repo_id
                "#,
        )
        .bind(u64_to_i64(account_id))
        .fetch_all(&self.pool)
        .await
        .map_err(crate::to_db_err)?;
        let rows: Vec<(
            crate::records::InvitationLinkRow,
            Option<i64>,
            Option<String>,
        )> = rows.into_iter().map(|j| j.split()).collect();

        // Preserve the SQL ordering (created_at DESC, id) by tracking insertion order.
        let mut order: Vec<String> = Vec::new();
        let mut by_link: BTreeMap<
            String,
            (
                crate::records::InvitationLinkRow,
                Vec<ghinvite_core::InvitationLinkRepo>,
            ),
        > = BTreeMap::new();
        for (link_row, repo_id, repo_full_name) in rows {
            let key = link_row.id.clone();
            let entry = by_link.entry(key.clone()).or_insert_with(|| {
                order.push(key.clone());
                (link_row, Vec::new())
            });
            if let (Some(rid), Some(name)) = (repo_id, repo_full_name) {
                entry.1.push(ghinvite_core::InvitationLinkRepo {
                    repo_id: rid as u64,
                    repo_full_name: name,
                });
            }
        }

        order
            .into_iter()
            .map(|k| {
                let (row, repos) = by_link.remove(&k).expect("inserted above");
                row.try_into_domain(repos)
            })
            .collect()
    }
    async fn insert_invitation_request_and_increment_uses(
        &self,
        request: &InvitationRequest,
    ) -> Result<()> {
        let mut tx = self.pool.begin().await.map_err(crate::to_db_err)?;

        let res = sqlx::query(
            r#"
            INSERT INTO invitation_requests
              (id, invitation_link_id, requester_id, justification, state,
               decided_by, decided_at, decline_reason, created_at, decision_deadline)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
            "#,
        )
        .bind(request.id.to_string())
        .bind(request.invitation_link_id.to_string())
        .bind(u64_to_i64(request.requester_id))
        .bind(request.justification.as_deref())
        .bind(request.state.to_string())
        .bind(request.decided_by.map(u64_to_i64))
        .bind(request.decided_at)
        .bind(request.decline_reason.as_deref())
        .bind(request.created_at)
        .bind(request.decision_deadline)
        .execute(&mut *tx)
        .await;

        match res {
            Ok(_) => (),
            Err(sqlx::Error::Database(db)) if db.is_unique_violation() => {
                return Err(Error::Conflict(classify_unique(
                    &*db,
                    ConflictKind::DuplicateId,
                )));
            }
            Err(sqlx::Error::Database(db)) if db.is_foreign_key_violation() => {
                return Err(Error::Conflict(ConflictKind::ForeignKey));
            }
            Err(e) => return Err(Error::Database(e.to_string())),
        }

        // Increment uses_count atomically. Caller has already verified is_active(now);
        // here we trust that and do the bump.
        let updated =
            sqlx::query(r#"UPDATE invitation_links SET uses_count = uses_count + 1 WHERE id = ?1"#)
                .bind(request.invitation_link_id.to_string())
                .execute(&mut *tx)
                .await
                .map_err(crate::to_db_err)?;
        if updated.rows_affected() == 0 {
            return Err(Error::NotFound);
        }

        tx.commit().await.map_err(crate::to_db_err)?;
        Ok(())
    }

    async fn record_request_decision(&self, decision: &RequestDecision) -> Result<()> {
        let res = sqlx::query(
            r#"UPDATE invitation_requests
               SET state = ?1, decided_by = ?2, decided_at = ?3, decline_reason = ?4
               WHERE id = ?5 AND state = 'pending'"#,
        )
        .bind(decision.state.to_string())
        .bind(decision.decided_by.map(u64_to_i64))
        .bind(decision.decided_at)
        .bind(decision.decline_reason.as_deref())
        .bind(decision.request_id.to_string())
        .execute(&self.pool)
        .await
        .map_err(crate::to_db_err)?;
        if res.rows_affected() == 0 {
            return Err(Error::NotFound);
        }
        Ok(())
    }

    async fn get_invitation_request(&self, id: RequestId) -> Result<Option<InvitationRequest>> {
        let row: Option<crate::records::InvitationRequestRow> = sqlx::query_as(
            r#"SELECT id, invitation_link_id, requester_id, justification, state,
                      decided_by, decided_at, decline_reason, created_at, decision_deadline
               FROM invitation_requests WHERE id = ?1"#,
        )
        .bind(id.to_string())
        .fetch_optional(&self.pool)
        .await
        .map_err(crate::to_db_err)?;
        row.map(|r| r.try_into_domain()).transpose()
    }

    async fn list_pending_requests_for_account(
        &self,
        account_id: u64,
    ) -> Result<Vec<InvitationRequest>> {
        let rows: Vec<crate::records::InvitationRequestRow> = sqlx::query_as(
            r#"SELECT r.id, r.invitation_link_id, r.requester_id, r.justification, r.state,
                      r.decided_by, r.decided_at, r.decline_reason, r.created_at, r.decision_deadline
               FROM invitation_requests r
               JOIN invitation_links l ON l.id = r.invitation_link_id
               WHERE l.account_id = ?1 AND r.state = 'pending'
               ORDER BY r.created_at"#,
        )
        .bind(u64_to_i64(account_id))
        .fetch_all(&self.pool)
        .await
        .map_err(crate::to_db_err)?;
        rows.into_iter().map(|r| r.try_into_domain()).collect()
    }

    async fn pending_request_page(
        &self,
        account_id: u64,
        after: Option<ghinvite_core::storage::pending_queue::PendingBoundary>,
    ) -> Result<ghinvite_core::storage::pending_queue::PendingPage> {
        use ghinvite_core::storage::pending_queue::{self, PendingPage};
        let rows: Vec<String> = sqlx::query_scalar(&pending_queue::query(after.is_some()))
            .bind(u64_to_i64(account_id))
            .bind(after.map(|b| b.seek_key()))
            .fetch_all(&self.pool)
            .await
            .map_err(crate::to_db_err)?;
        PendingPage::from_json(rows)
    }

    async fn list_requests_for_link(
        &self,
        link_id: InvitationLinkId,
    ) -> Result<Vec<InvitationRequest>> {
        let rows: Vec<crate::records::InvitationRequestRow> = sqlx::query_as(
            r#"SELECT id, invitation_link_id, requester_id, justification, state,
                      decided_by, decided_at, decline_reason, created_at, decision_deadline
               FROM invitation_requests WHERE invitation_link_id = ?1
               ORDER BY created_at DESC"#,
        )
        .bind(link_id.to_string())
        .fetch_all(&self.pool)
        .await
        .map_err(crate::to_db_err)?;
        rows.into_iter().map(|r| r.try_into_domain()).collect()
    }
    async fn request_history(
        &self,
        account_id: u64,
        link_id: InvitationLinkId,
        before: Option<ghinvite_core::storage::request_history::Boundary>,
    ) -> Result<ghinvite_core::storage::request_history::Page> {
        use ghinvite_core::storage::request_history as history;
        #[derive(sqlx::FromRow)]
        struct Row {
            #[sqlx(flatten)]
            request: crate::records::InvitationRequestRow,
            requester_login: Option<String>,
        }
        let rows: Vec<Row> = sqlx::query_as(&history::query(before))
            .bind(u64_to_i64(account_id))
            .bind(link_id.to_string())
            .bind(before.map(history::boundary_key))
            .fetch_all(&self.pool)
            .await
            .map_err(crate::to_db_err)?;
        Ok(history::page(
            rows.into_iter()
                .map(|r| Ok((r.request.try_into_domain()?, r.requester_login)))
                .collect::<Result<_>>()?,
        ))
    }
    async fn insert_github_invitation(&self, invitation: &GithubInvitation) -> Result<()> {
        sqlx::query(
            r#"
            INSERT INTO github_invitations
              (id, invitation_request_id, repo_id, github_invitation_id, state, error_message, created_at, updated_at)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
            "#,
        )
        .bind(invitation.id.to_string())
        .bind(invitation.invitation_request_id.to_string())
        .bind(u64_to_i64(invitation.repo_id))
        .bind(invitation.github_invitation_id.map(u64_to_i64))
        .bind(invitation.state.to_string())
        .bind(invitation.error_message.as_deref())
        .bind(invitation.created_at)
        .bind(invitation.updated_at)
        .execute(&self.pool)
        .await
        .map_err(|e| match e {
            sqlx::Error::Database(db) if db.is_unique_violation() => {
                Error::Conflict(classify_unique(&*db, ConflictKind::DuplicateId))
            }
            sqlx::Error::Database(db) if db.is_foreign_key_violation() => {
                Error::Conflict(ConflictKind::ForeignKey)
            }
            other => Error::Database(other.to_string()),
        })?;
        Ok(())
    }

    async fn update_github_invitation(&self, update: &GithubInvitationUpdate) -> Result<()> {
        let res = sqlx::query(
            r#"UPDATE github_invitations
               SET state = ?1, github_invitation_id = ?2,
                   error_message = ?3, updated_at = ?4
               WHERE id = ?5"#,
        )
        .bind(update.state.to_string())
        .bind(update.github_invitation_id.map(u64_to_i64))
        .bind(update.error_message.as_deref())
        .bind(update.updated_at)
        .bind(update.id.to_string())
        .execute(&self.pool)
        .await
        .map_err(crate::to_db_err)?;
        if res.rows_affected() == 0 {
            return Err(Error::NotFound);
        }
        Ok(())
    }

    async fn get_github_invitation(
        &self,
        id: GithubInvitationId,
    ) -> Result<Option<GithubInvitation>> {
        let row: Option<crate::records::GithubInvitationRow> = sqlx::query_as(
            r#"SELECT id, invitation_request_id, repo_id, github_invitation_id, state,
                      error_message, created_at, updated_at
               FROM github_invitations WHERE id = ?1"#,
        )
        .bind(id.to_string())
        .fetch_optional(&self.pool)
        .await
        .map_err(crate::to_db_err)?;
        row.map(|r| r.try_into_domain()).transpose()
    }

    async fn get_github_invitation_by_github_id(
        &self,
        github_id: u64,
    ) -> Result<Option<GithubInvitation>> {
        let row: Option<crate::records::GithubInvitationRow> = sqlx::query_as(
            r#"SELECT id, invitation_request_id, repo_id, github_invitation_id, state,
                      error_message, created_at, updated_at
               FROM github_invitations WHERE github_invitation_id = ?1"#,
        )
        .bind(u64_to_i64(github_id))
        .fetch_optional(&self.pool)
        .await
        .map_err(crate::to_db_err)?;
        row.map(|r| r.try_into_domain()).transpose()
    }

    async fn list_pending_github_invitations_for_installation(
        &self,
        installation_id: u64,
    ) -> Result<Vec<GithubInvitation>> {
        // Cross-join via the request → link → installation_id chain.
        let rows: Vec<crate::records::GithubInvitationRow> = sqlx::query_as(
            r#"SELECT g.id, g.invitation_request_id, g.repo_id, g.github_invitation_id, g.state,
                      g.error_message, g.created_at, g.updated_at
               FROM github_invitations g
               JOIN invitation_requests r ON r.id = g.invitation_request_id
               JOIN invitation_links l ON l.id = r.invitation_link_id
               WHERE l.installation_id = ?1
                 AND g.state IN ('sending', 'sent')
               ORDER BY g.created_at"#,
        )
        .bind(u64_to_i64(installation_id))
        .fetch_all(&self.pool)
        .await
        .map_err(crate::to_db_err)?;
        rows.into_iter().map(|r| r.try_into_domain()).collect()
    }
    async fn member_invitation_candidates(
        &self,
        account_id: u64,
        repo_id: u64,
        requester_id: u64,
    ) -> Result<Vec<GithubInvitation>> {
        let rows: Vec<crate::records::GithubInvitationRow> =
            sqlx::query_as(ghinvite_core::storage::MEMBER_INVITATION_CANDIDATES)
                .bind(u64_to_i64(account_id))
                .bind(u64_to_i64(repo_id))
                .bind(u64_to_i64(requester_id))
                .fetch_all(&self.pool)
                .await
                .map_err(crate::to_db_err)?;
        rows.into_iter().map(|r| r.try_into_domain()).collect()
    }

    async fn bind_member_webhook(
        &self,
        payload_sha256: &str,
        invitation_id: Option<GithubInvitationId>,
    ) -> Result<Option<GithubInvitationId>> {
        sqlx::query("INSERT INTO member_webhook_receipts(payload_sha256, invitation_id) VALUES (?1, ?2) ON CONFLICT DO NOTHING")
            .bind(payload_sha256).bind(invitation_id.map(|id| id.to_string()))
            .execute(&self.pool).await.map_err(crate::to_db_err)?;
        let id: Option<String> = sqlx::query_scalar(
            "SELECT invitation_id FROM member_webhook_receipts WHERE payload_sha256 = ?1",
        )
        .bind(payload_sha256)
        .fetch_one(&self.pool)
        .await
        .map_err(crate::to_db_err)?;
        id.map(|id| {
            id.parse()
                .map_err(|_| Error::Corrupt("member webhook invitation ID".into()))
        })
        .transpose()
    }

    async fn list_pending_github_invitations_for_account(
        &self,
        account_id: u64,
    ) -> Result<Vec<GithubInvitation>> {
        let rows: Vec<crate::records::GithubInvitationRow> = sqlx::query_as(
            r#"SELECT g.id, g.invitation_request_id, g.repo_id, g.github_invitation_id, g.state,
                      g.error_message, g.created_at, g.updated_at
               FROM github_invitations g
               JOIN invitation_requests r ON r.id = g.invitation_request_id
               JOIN invitation_links l ON l.id = r.invitation_link_id
               WHERE l.account_id = ?1 AND g.state IN ('sending', 'sent')
               ORDER BY g.created_at"#,
        )
        .bind(u64_to_i64(account_id))
        .fetch_all(&self.pool)
        .await
        .map_err(crate::to_db_err)?;
        rows.into_iter().map(|r| r.try_into_domain()).collect()
    }

    async fn invitation_link_belongs_to_account(
        &self,
        account_id: u64,
        id: InvitationLinkId,
    ) -> Result<bool> {
        Ok(
            sqlx::query("SELECT 1 FROM invitation_links WHERE id = ?1 AND account_id = ?2 LIMIT 1")
                .bind(id.to_string())
                .bind(u64_to_i64(account_id))
                .fetch_optional(&self.pool)
                .await
                .map_err(crate::to_db_err)?
                .is_some(),
        )
    }

    async fn list_audit_events(
        &self,
        account_id: u64,
        event: Option<ghinvite_core::audit::EventType>,
        position: ghinvite_core::storage::AuditPosition,
    ) -> Result<ghinvite_core::storage::AuditPage> {
        use ghinvite_core::storage::{AuditBoundary, AuditPage, AuditPosition, audit_read};
        let boundary = position.boundary();
        let rows: Vec<crate::records::AuditEventRow> =
            sqlx::query_as(&audit_read::query(event, position, false))
                .bind(u64_to_i64(account_id))
                .bind(event.map(|e| e.as_str()))
                .bind(boundary.map(audit_read::boundary_time))
                .bind(boundary.map(|b| b.id.to_string()))
                .fetch_all(&self.pool)
                .await
                .map_err(crate::to_db_err)?;
        let mut events = rows
            .into_iter()
            .map(|r| r.try_into_domain())
            .collect::<Result<Vec<_>>>()?;
        if matches!(position, AuditPosition::After(_)) {
            events.reverse();
        }
        let mut page = AuditPage {
            events,
            has_older: false,
            has_newer: false,
        };
        if let (Some(first), Some(last)) = (page.events.first(), page.events.last()) {
            for (seek, flag) in [
                (
                    AuditPosition::After(AuditBoundary::from(first)),
                    &mut page.has_newer,
                ),
                (
                    AuditPosition::Before(AuditBoundary::from(last)),
                    &mut page.has_older,
                ),
            ] {
                let b = seek.boundary().unwrap();
                *flag = sqlx::query(&audit_read::query(event, seek, true))
                    .bind(u64_to_i64(account_id))
                    .bind(event.map(|e| e.as_str()))
                    .bind(audit_read::boundary_time(b))
                    .bind(b.id.to_string())
                    .fetch_optional(&self.pool)
                    .await
                    .map_err(crate::to_db_err)?
                    .is_some();
            }
        }
        Ok(page)
    }

    async fn audit(&self, event: &AuditEvent) -> Result<()> {
        let metadata_json = if event.metadata.is_null() {
            None
        } else {
            Some(serde_json::to_string(&event.metadata).expect("audit metadata serializes"))
        };

        let mut sql = String::from(
            r#"
            INSERT INTO audit_events
              (id, account_id, occurred_at, event_type, actor_kind, actor_id,
               target_kind, target_id, metadata, request_id)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
            "#,
        );
        // Metadata commands journal their event ID before writing. Ignore only
        // that primary-key replay, not other constraints or database failures.
        if event.event_type == ghinvite_core::audit::EventType::InvitationLinkMetadataUpdated {
            sql.push_str(" ON CONFLICT(id) DO NOTHING");
        } else if matches!(
            event.event_type,
            ghinvite_core::audit::EventType::InstallationCreated
                | ghinvite_core::audit::EventType::InstallationReposChanged
                | ghinvite_core::audit::EventType::InstallationUninstalled
        ) {
            sql.push_str(ghinvite_core::storage::INSTALLATION_AUDIT_REPLAY);
        }
        sqlx::query(&sql)
            .bind(event.id.to_string())
            .bind(u64_to_i64(event.account_id))
            .bind(event.occurred_at)
            .bind(event.event_type.as_str())
            .bind(event.actor_kind.to_string())
            .bind(event.actor_id.map(u64_to_i64))
            .bind(event.target_kind.to_string())
            .bind(&event.target_id)
            .bind(metadata_json)
            .bind(event.request_id.as_deref())
            .execute(&self.pool)
            .await
            .map_err(crate::to_db_err)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ghinvite_core::{AccountType, SelectedRepos};

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
            "invitation_links",
            "invitation_link_repos",
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

        for old_table in ["share_links", "share_link_repos"] {
            let row: Option<(String,)> =
                sqlx::query_as("SELECT name FROM sqlite_master WHERE type='table' AND name=?1")
                    .bind(old_table)
                    .fetch_optional(&s.pool)
                    .await
                    .unwrap();
            assert!(row.is_none(), "old table {old_table} should not exist");
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
            matches!(
                err,
                Error::Conflict(ConflictKind::DuplicateActiveInstallation)
            ),
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

    pub(crate) fn sample_user(user_id: u64, login: &str) -> User {
        User {
            user_id,
            login: login.into(),
            avatar_url: Some(format!("https://example.test/{login}.png")),
            last_seen_at: dt("2026-05-04T12:00:00Z"),
        }
    }

    pub(crate) fn slug_with_seed(seed: u64) -> ghinvite_core::Slug {
        use rand::SeedableRng;
        let mut rng = rand_chacha::ChaCha8Rng::seed_from_u64(seed);
        ghinvite_core::Slug::generate(&mut rng)
    }

    pub(crate) fn sample_link(
        account_id: u64,
        installation_id: u64,
        created_by: u64,
        slug_seed: u64,
    ) -> InvitationLink {
        use ghinvite_core::{InvitationLinkRepo, Permission};
        InvitationLink {
            id: InvitationLinkId::new(),
            slug: slug_with_seed(slug_seed),
            installation_id,
            account_id,
            created_by,
            created_at: dt("2026-05-04T12:00:00Z"),
            expires_at: None,
            max_uses: Some(5),
            uses_count: 0,
            permission: Permission::Push,
            approval_required: true,
            description: "Contractor onboarding".into(),
            internal_note: Some("for the contractor".into()),
            revoked_at: None,
            revoked_by: None,
            repos: vec![
                InvitationLinkRepo {
                    repo_id: 10,
                    repo_full_name: "acme/api".into(),
                },
                InvitationLinkRepo {
                    repo_id: 11,
                    repo_full_name: "acme/web".into(),
                },
            ],
        }
    }

    #[tokio::test]
    async fn insert_and_get_invitation_link_round_trips_with_repos() {
        let s = SqlxStorage::in_memory().await.unwrap();
        s.insert_installation(&sample_account(1, 100, "acme"))
            .await
            .unwrap();
        s.upsert_user(&sample_user(7, "octocat")).await.unwrap();

        let link = sample_link(100, 1, 7, 1);
        s.insert_invitation_link(&link).await.unwrap();

        let got = s.get_invitation_link_by_id(link.id).await.unwrap().unwrap();
        assert_eq!(got, link);

        let by_slug = s
            .get_invitation_link_by_slug(link.slug.as_str())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(by_slug.id, link.id);
    }

    #[tokio::test]
    async fn invitation_link_slug_is_unique() {
        let s = SqlxStorage::in_memory().await.unwrap();
        s.insert_installation(&sample_account(1, 100, "acme"))
            .await
            .unwrap();
        s.upsert_user(&sample_user(7, "octocat")).await.unwrap();

        let a = sample_link(100, 1, 7, 1);
        let mut b = sample_link(100, 1, 7, 1); // same seed → same slug
        b.id = InvitationLinkId::new();
        s.insert_invitation_link(&a).await.unwrap();
        let err = s.insert_invitation_link(&b).await.unwrap_err();
        assert!(
            matches!(err, Error::Conflict(ConflictKind::DuplicateSlug)),
            "got {err:?}"
        );
    }

    #[tokio::test]
    async fn revoke_marks_revoked_at_and_by() {
        let s = SqlxStorage::in_memory().await.unwrap();
        s.insert_installation(&sample_account(1, 100, "acme"))
            .await
            .unwrap();
        s.upsert_user(&sample_user(7, "octocat")).await.unwrap();
        let link = sample_link(100, 1, 7, 2);
        s.insert_invitation_link(&link).await.unwrap();

        s.mark_invitation_link_revoked(link.id, 7, dt("2026-05-04T13:00:00Z"))
            .await
            .unwrap();

        let got = s.get_invitation_link_by_id(link.id).await.unwrap().unwrap();
        assert_eq!(got.revoked_by, Some(7));
        assert_eq!(got.revoked_at, Some(dt("2026-05-04T13:00:00Z")));

        // Idempotent revoke returns NotFound second time:
        let err = s
            .mark_invitation_link_revoked(link.id, 7, dt("2026-05-04T14:00:00Z"))
            .await
            .unwrap_err();
        assert!(matches!(err, Error::NotFound));
    }

    #[tokio::test]
    async fn list_invitation_links_for_account_returns_in_descending_created_at() {
        let s = SqlxStorage::in_memory().await.unwrap();
        s.insert_installation(&sample_account(1, 100, "acme"))
            .await
            .unwrap();
        s.upsert_user(&sample_user(7, "octocat")).await.unwrap();

        let mut older = sample_link(100, 1, 7, 3);
        older.created_at = dt("2026-05-01T00:00:00Z");

        let mut newer = sample_link(100, 1, 7, 4);
        newer.created_at = dt("2026-05-04T00:00:00Z");

        s.insert_invitation_link(&older).await.unwrap();
        s.insert_invitation_link(&newer).await.unwrap();

        let list = s.list_invitation_links_for_account(100).await.unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].id, newer.id);
        assert_eq!(list[1].id, older.id);
    }

    #[tokio::test]
    async fn list_returns_link_with_no_repos_as_empty_vec() {
        let s = SqlxStorage::in_memory().await.unwrap();
        s.insert_installation(&sample_account(1, 100, "acme"))
            .await
            .unwrap();
        s.upsert_user(&sample_user(7, "octocat")).await.unwrap();

        let mut link = sample_link(100, 1, 7, 5);
        link.repos.clear();
        s.insert_invitation_link(&link).await.unwrap();

        let got = s.get_invitation_link_by_id(link.id).await.unwrap().unwrap();
        assert!(got.repos.is_empty());

        let list = s.list_invitation_links_for_account(100).await.unwrap();
        assert_eq!(list.len(), 1);
        assert!(list[0].repos.is_empty());
    }

    #[tokio::test]
    async fn invitation_link_with_unknown_installation_fails() {
        let s = SqlxStorage::in_memory().await.unwrap();
        s.upsert_user(&sample_user(7, "octocat")).await.unwrap();
        // installation_id 999 does not exist → FK violation
        let link = sample_link(100, 999, 7, 6);
        let err = s.insert_invitation_link(&link).await.unwrap_err();
        assert!(
            matches!(err, Error::Conflict(ConflictKind::ForeignKey)),
            "got {err:?}"
        );
    }

    pub(crate) fn sample_request(link_id: InvitationLinkId, requester: u64) -> InvitationRequest {
        use ghinvite_core::RequestState;
        InvitationRequest {
            id: RequestId::new(),
            invitation_link_id: link_id,
            requester_id: requester,
            justification: Some("contractor for q2".into()),
            state: RequestState::Pending,
            decided_by: None,
            decided_at: None,
            decline_reason: None,
            created_at: dt("2026-05-04T12:30:00Z"),
            decision_deadline: None,
        }
    }

    #[tokio::test]
    async fn insert_request_increments_uses_count() {
        let s = SqlxStorage::in_memory().await.unwrap();
        s.insert_installation(&sample_account(1, 100, "acme"))
            .await
            .unwrap();
        s.upsert_user(&sample_user(7, "octocat")).await.unwrap();
        s.upsert_user(&sample_user(8, "alice")).await.unwrap();
        let link = sample_link(100, 1, 7, 10);
        s.insert_invitation_link(&link).await.unwrap();

        let req = sample_request(link.id, 8);
        s.insert_invitation_request_and_increment_uses(&req)
            .await
            .unwrap();

        let got = s.get_invitation_request(req.id).await.unwrap().unwrap();
        assert_eq!(got, req);

        let updated_link = s.get_invitation_link_by_id(link.id).await.unwrap().unwrap();
        assert_eq!(updated_link.uses_count, 1);
    }

    #[tokio::test]
    async fn second_pending_request_for_same_user_conflicts() {
        let s = SqlxStorage::in_memory().await.unwrap();
        s.insert_installation(&sample_account(1, 100, "acme"))
            .await
            .unwrap();
        s.upsert_user(&sample_user(7, "octocat")).await.unwrap();
        s.upsert_user(&sample_user(8, "alice")).await.unwrap();
        let link = sample_link(100, 1, 7, 11);
        s.insert_invitation_link(&link).await.unwrap();

        let req_a = sample_request(link.id, 8);
        s.insert_invitation_request_and_increment_uses(&req_a)
            .await
            .unwrap();

        let req_b = sample_request(link.id, 8); // same requester, distinct id
        let err = s
            .insert_invitation_request_and_increment_uses(&req_b)
            .await
            .unwrap_err();
        assert!(
            matches!(err, Error::Conflict(ConflictKind::DuplicatePendingRequest)),
            "got {err:?}"
        );
    }

    #[tokio::test]
    async fn record_decision_to_approved() {
        use ghinvite_core::RequestState;
        let s = SqlxStorage::in_memory().await.unwrap();
        s.insert_installation(&sample_account(1, 100, "acme"))
            .await
            .unwrap();
        s.upsert_user(&sample_user(7, "octocat")).await.unwrap();
        s.upsert_user(&sample_user(8, "alice")).await.unwrap();
        let link = sample_link(100, 1, 7, 12);
        s.insert_invitation_link(&link).await.unwrap();

        let req = sample_request(link.id, 8);
        s.insert_invitation_request_and_increment_uses(&req)
            .await
            .unwrap();

        s.record_request_decision(&RequestDecision {
            request_id: req.id,
            state: RequestState::Approved,
            decided_by: Some(7),
            decided_at: dt("2026-05-04T13:00:00Z"),
            decline_reason: None,
        })
        .await
        .unwrap();

        let got = s.get_invitation_request(req.id).await.unwrap().unwrap();
        assert_eq!(got.state, RequestState::Approved);
        assert_eq!(got.decided_by, Some(7));
        assert_eq!(got.decided_at, Some(dt("2026-05-04T13:00:00Z")));
    }

    #[tokio::test]
    async fn second_decision_returns_not_found() {
        use ghinvite_core::RequestState;
        let s = SqlxStorage::in_memory().await.unwrap();
        s.insert_installation(&sample_account(1, 100, "acme"))
            .await
            .unwrap();
        s.upsert_user(&sample_user(7, "octocat")).await.unwrap();
        s.upsert_user(&sample_user(8, "alice")).await.unwrap();
        let link = sample_link(100, 1, 7, 13);
        s.insert_invitation_link(&link).await.unwrap();
        let req = sample_request(link.id, 8);
        s.insert_invitation_request_and_increment_uses(&req)
            .await
            .unwrap();
        s.record_request_decision(&RequestDecision {
            request_id: req.id,
            state: RequestState::Approved,
            decided_by: Some(7),
            decided_at: dt("2026-05-04T13:00:00Z"),
            decline_reason: None,
        })
        .await
        .unwrap();

        let err = s
            .record_request_decision(&RequestDecision {
                request_id: req.id,
                state: RequestState::Declined,
                decided_by: Some(7),
                decided_at: dt("2026-05-04T14:00:00Z"),
                decline_reason: Some("wrong person".into()),
            })
            .await
            .unwrap_err();
        assert!(matches!(err, Error::NotFound));
    }

    #[tokio::test]
    async fn list_pending_for_account_filters_by_account() {
        let s = SqlxStorage::in_memory().await.unwrap();
        s.insert_installation(&sample_account(1, 100, "acme"))
            .await
            .unwrap();
        s.insert_installation(&sample_account(2, 200, "other"))
            .await
            .unwrap();
        s.upsert_user(&sample_user(7, "octocat")).await.unwrap();
        s.upsert_user(&sample_user(8, "alice")).await.unwrap();

        let link_a = sample_link(100, 1, 7, 14);
        let link_b = sample_link(200, 2, 7, 15);

        s.insert_invitation_link(&link_a).await.unwrap();
        s.insert_invitation_link(&link_b).await.unwrap();

        let r_a = sample_request(link_a.id, 8);
        let r_b = sample_request(link_b.id, 8);
        s.insert_invitation_request_and_increment_uses(&r_a)
            .await
            .unwrap();
        s.insert_invitation_request_and_increment_uses(&r_b)
            .await
            .unwrap();

        let pending_a = s.list_pending_requests_for_account(100).await.unwrap();
        assert_eq!(pending_a.len(), 1);
        assert_eq!(pending_a[0].id, r_a.id);

        let pending_b = s.list_pending_requests_for_account(200).await.unwrap();
        assert_eq!(pending_b.len(), 1);
        assert_eq!(pending_b[0].id, r_b.id);
    }

    pub(crate) fn sample_ginv(req_id: RequestId, repo_id: u64) -> GithubInvitation {
        use ghinvite_core::InvitationState;
        GithubInvitation {
            id: GithubInvitationId::new(),
            invitation_request_id: req_id,
            repo_id,
            github_invitation_id: None,
            state: InvitationState::Sending,
            error_message: None,
            created_at: dt("2026-05-04T13:00:00Z"),
            updated_at: dt("2026-05-04T13:00:00Z"),
        }
    }

    #[tokio::test]
    async fn insert_and_update_github_invitation() {
        use ghinvite_core::InvitationState;
        let s = SqlxStorage::in_memory().await.unwrap();
        s.insert_installation(&sample_account(1, 100, "acme"))
            .await
            .unwrap();
        s.upsert_user(&sample_user(7, "octocat")).await.unwrap();
        s.upsert_user(&sample_user(8, "alice")).await.unwrap();
        let link = sample_link(100, 1, 7, 20);
        s.insert_invitation_link(&link).await.unwrap();
        let req = sample_request(link.id, 8);
        s.insert_invitation_request_and_increment_uses(&req)
            .await
            .unwrap();

        let g = sample_ginv(req.id, 10);
        s.insert_github_invitation(&g).await.unwrap();

        s.update_github_invitation(&GithubInvitationUpdate {
            id: g.id,
            state: InvitationState::Sent,
            github_invitation_id: Some(99999),
            error_message: None,
            updated_at: dt("2026-05-04T13:01:00Z"),
        })
        .await
        .unwrap();

        let got = s.get_github_invitation(g.id).await.unwrap().unwrap();
        assert_eq!(got.state, InvitationState::Sent);
        assert_eq!(got.github_invitation_id, Some(99999));
        assert_eq!(got.updated_at, dt("2026-05-04T13:01:00Z"));

        let by_gid = s
            .get_github_invitation_by_github_id(99999)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(by_gid.id, g.id);
    }

    #[tokio::test]
    async fn list_pending_for_installation_filters_state_and_installation() {
        use ghinvite_core::InvitationState;
        let s = SqlxStorage::in_memory().await.unwrap();
        s.insert_installation(&sample_account(1, 100, "acme"))
            .await
            .unwrap();
        s.insert_installation(&sample_account(2, 200, "other"))
            .await
            .unwrap();
        s.upsert_user(&sample_user(7, "octocat")).await.unwrap();
        s.upsert_user(&sample_user(8, "alice")).await.unwrap();

        let link_a = sample_link(100, 1, 7, 21);
        s.insert_invitation_link(&link_a).await.unwrap();
        let link_b = sample_link(200, 2, 7, 22);
        s.insert_invitation_link(&link_b).await.unwrap();

        let req_a = sample_request(link_a.id, 8);
        s.insert_invitation_request_and_increment_uses(&req_a)
            .await
            .unwrap();
        let req_b = sample_request(link_b.id, 8);
        s.insert_invitation_request_and_increment_uses(&req_b)
            .await
            .unwrap();

        let g_a_pending = sample_ginv(req_a.id, 10);
        let mut g_a_done = sample_ginv(req_a.id, 11);
        g_a_done.state = InvitationState::Accepted;
        let g_b_pending = sample_ginv(req_b.id, 12);

        s.insert_github_invitation(&g_a_pending).await.unwrap();
        s.insert_github_invitation(&g_a_done).await.unwrap();
        s.insert_github_invitation(&g_b_pending).await.unwrap();

        let pending_inst_1 = s
            .list_pending_github_invitations_for_installation(1)
            .await
            .unwrap();
        assert_eq!(pending_inst_1.len(), 1);
        assert_eq!(pending_inst_1[0].id, g_a_pending.id);

        let pending_inst_2 = s
            .list_pending_github_invitations_for_installation(2)
            .await
            .unwrap();
        assert_eq!(pending_inst_2.len(), 1);
        assert_eq!(pending_inst_2[0].id, g_b_pending.id);
    }

    #[tokio::test]
    async fn github_invitation_with_unknown_request_fails() {
        let s = SqlxStorage::in_memory().await.unwrap();
        // No request exists → FK violation
        let g = sample_ginv(RequestId::new(), 10);
        let err = s.insert_github_invitation(&g).await.unwrap_err();
        assert!(
            matches!(err, Error::Conflict(ConflictKind::ForeignKey)),
            "got {err:?}"
        );
    }

    pub(crate) fn sample_audit(
        account_id: u64,
        event_type: ghinvite_core::audit::EventType,
        target: &str,
    ) -> AuditEvent {
        use ghinvite_core::AuditEventId;
        use ghinvite_core::audit::{ActorKind, TargetKind};
        AuditEvent {
            id: AuditEventId::new(),
            account_id,
            occurred_at: dt("2026-05-04T13:00:00Z"),
            event_type,
            actor_kind: ActorKind::User,
            actor_id: Some(7),
            target_kind: TargetKind::InvitationLink,
            target_id: target.into(),
            metadata: serde_json::json!({"k": "v"}),
            request_id: Some("inv-abc".into()),
        }
    }

    #[tokio::test]
    async fn audit_mixed_timestamp_encodings_seek_exactly_using_indexes() {
        use ghinvite_core::audit::EventType;
        use ghinvite_core::storage::{AuditBoundary, AuditPosition, audit_read};
        let s = SqlxStorage::in_memory().await.unwrap();
        let fixtures = [
            "2026-05-04T12:00:00Z",
            "2026-05-04T12:00:00+00:00",
            "2026-05-04T12:00:00.000000001Z",
            "2026-05-04T12:00:00.001+00:00",
            "2026-05-04T12:00:00.100Z",
            "2026-05-04T12:00:00.100000+00:00",
            "2026-05-04T12:00:00.100000001+00:00",
            "2026-05-04T12:00:01Z",
        ];
        let mut expected = Vec::new();
        for (n, timestamp) in fixtures.iter().enumerate() {
            let mut event = sample_audit(100, EventType::RequestCreated, "missing");
            event.id = ghinvite_core::AuditEventId::from_ulid(ulid::Ulid::from(n as u128 + 1));
            event.occurred_at = dt(timestamp);
            s.audit(&event).await.unwrap();
            sqlx::query("UPDATE audit_events SET occurred_at = ? WHERE id = ?")
                .bind(timestamp)
                .bind(event.id.to_string())
                .execute(&s.pool)
                .await
                .unwrap();
            expected.push(event);
        }
        expected.reverse();
        let page = s
            .list_audit_events(100, None, AuditPosition::Latest)
            .await
            .unwrap();
        assert_eq!(page.events, expected);
        for (i, event) in expected.iter().enumerate() {
            let b = AuditBoundary::from(event);
            assert_eq!(
                s.list_audit_events(100, None, AuditPosition::Before(b))
                    .await
                    .unwrap()
                    .events,
                expected[i + 1..]
            );
            assert_eq!(
                s.list_audit_events(100, None, AuditPosition::After(b))
                    .await
                    .unwrap()
                    .events,
                expected[..i]
            );
            for filter in [None, Some(EventType::RequestCreated)] {
                for position in [
                    AuditPosition::Latest,
                    AuditPosition::Before(b),
                    AuditPosition::After(b),
                ] {
                    let plan: Vec<(i64, i64, i64, String)> = sqlx::query_as(&format!(
                        "EXPLAIN QUERY PLAN {}",
                        audit_read::query(filter, position, false)
                    ))
                    .bind(100)
                    .bind(filter.map(|e| e.as_str()))
                    .bind(position.boundary().map(audit_read::boundary_time))
                    .bind(position.boundary().map(|b| b.id.to_string()))
                    .fetch_all(&s.pool)
                    .await
                    .unwrap();
                    let plan = format!("{plan:?}");
                    assert!(
                        plan.contains(if filter.is_some() {
                            "idx_audit_account_event_order"
                        } else {
                            "idx_audit_account_order"
                        }),
                        "{plan}"
                    );
                    assert!(!plan.contains("TEMP B-TREE"), "{plan}");
                    if position != AuditPosition::Latest {
                        assert!(plan.contains("<expr>"), "{plan}");
                    }
                }
            }
        }
    }

    #[tokio::test]
    async fn request_history_seeks_index_without_sorting_history() {
        use ghinvite_core::storage::request_history::{self, Boundary};
        let s = SqlxStorage::in_memory().await.unwrap();
        for before in [
            None,
            Some(Boundary {
                admitted_at: dt("2026-01-01T00:00:00Z"),
                id: RequestId::new(),
            }),
        ] {
            let plan: Vec<(i64, i64, i64, String)> = sqlx::query_as(&format!(
                "EXPLAIN QUERY PLAN {}",
                request_history::query(before)
            ))
            .bind(42)
            .bind(InvitationLinkId::new().to_string())
            .bind(before.map(request_history::boundary_key))
            .fetch_all(&s.pool)
            .await
            .unwrap();
            let plan = format!("{plan:?}");
            assert!(plan.contains("idx_request_history"), "{plan}");
            assert!(!plan.contains("TEMP B-TREE"), "{plan}");
            if before.is_some() {
                assert!(plan.contains("<expr><?"), "{plan}");
            }
        }
    }

    #[tokio::test]
    async fn request_history_deep_equal_timestamp_page_has_bounded_database_work() {
        use ghinvite_core::storage::request_history::Boundary;
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        let s = SqlxStorage::in_memory().await.unwrap();
        s.insert_installation(&sample_account(1, 42, "acme"))
            .await
            .unwrap();
        s.upsert_user(&sample_user(7, "requester")).await.unwrap();
        let link = sample_link(42, 1, 7, 1);
        s.insert_invitation_link(&link).await.unwrap();
        // A large imported history can share one admission timestamp. The deep
        // cursor must seek past that prefix, not examine it row by row.
        sqlx::query("WITH RECURSIVE n(i) AS (VALUES(1) UNION ALL SELECT i+1 FROM n WHERE i<10000) INSERT INTO invitation_requests(id,invitation_link_id,requester_id,state,created_at) SELECT printf('%026d',i),?,7,'declined','2026-01-01T00:00:00Z' FROM n")
            .bind(link.id.to_string()).execute(&s.pool).await.unwrap();
        let work = Arc::new(AtomicUsize::new(0));
        let count = work.clone();
        let mut connection = s.pool.acquire().await.unwrap();
        connection
            .lock_handle()
            .await
            .unwrap()
            .set_progress_handler(100, move || {
                count.fetch_add(100, Ordering::Relaxed);
                true
            });
        drop(connection);
        let page = s
            .request_history(
                42,
                link.id,
                Some(Boundary {
                    admitted_at: dt("2026-01-01T00:00:00Z"),
                    id: "00000000000000000000000100".parse().unwrap(),
                }),
            )
            .await
            .unwrap();
        assert_eq!(page.requests.len(), 25);
        assert_eq!(
            page.requests[0].id.to_string(),
            "00000000000000000000000099"
        );
        assert!(
            work.load(Ordering::Relaxed) < 10000,
            "page scanned the timestamp prefix: {} VM operations",
            work.load(Ordering::Relaxed)
        );
        assert_eq!(page.requester_logins.len(), 1);
        assert_eq!(
            page.requester_logins.get(&7).map(String::as_str),
            Some("requester")
        );
        let mut connection = s.pool.acquire().await.unwrap();
        connection
            .lock_handle()
            .await
            .unwrap()
            .remove_progress_handler();
        sqlx::raw_sql("PRAGMA foreign_keys=OFF; DELETE FROM users;")
            .execute(&mut *connection)
            .await
            .unwrap();
        drop(connection);
        let missing = s.request_history(42, link.id, None).await.unwrap();
        assert_eq!(missing.requests.len(), 25);
        assert!(missing.requester_logins.is_empty());
    }

    #[tokio::test]
    async fn audit_corruption_fails_the_page_instead_of_dropping_rows() {
        use ghinvite_core::audit::EventType;
        use ghinvite_core::storage::AuditPosition;
        for (column, value) in [
            ("id", "bad"),
            ("occurred_at", "bad"),
            ("event_type", "unknown"),
            ("actor_kind", "unknown"),
            ("actor_id", "-1"),
            ("target_kind", "unknown"),
            ("metadata", "{bad"),
        ] {
            let s = SqlxStorage::in_memory().await.unwrap();
            s.audit(&sample_audit(100, EventType::RequestCreated, "missing"))
                .await
                .unwrap();
            sqlx::query(&format!("UPDATE audit_events SET {column} = ?"))
                .bind(value)
                .execute(&s.pool)
                .await
                .unwrap();
            assert!(
                s.list_audit_events(100, None, AuditPosition::Latest)
                    .await
                    .is_err(),
                "{column}"
            );
        }
    }

    #[tokio::test]
    async fn audit_append_then_read_back() {
        use ghinvite_core::audit::EventType;
        let s = SqlxStorage::in_memory().await.unwrap();
        let e1 = sample_audit(100, EventType::InvitationLinkCreated, "01HFLINK1");
        let e2 = AuditEvent {
            occurred_at: dt("2026-05-04T13:01:00Z"),
            ..sample_audit(100, EventType::InvitationLinkRevoked, "01HFLINK1")
        };
        s.audit(&e1).await.unwrap();
        s.audit(&e2).await.unwrap();

        let got = s.debug_list_audit(100).await.unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(got[0], e1);
        assert_eq!(got[1], e2);
    }

    #[tokio::test]
    async fn audit_metadata_deduplicates_only_primary_key_and_leaves_other_events_unchanged() {
        use ghinvite_core::audit::EventType;
        let s = SqlxStorage::in_memory().await.unwrap();
        let event = sample_audit(100, EventType::InvitationLinkMetadataUpdated, "link");
        s.audit(&event).await.unwrap();
        s.audit(&event).await.unwrap();
        assert_eq!(s.debug_list_audit(100).await.unwrap(), vec![event.clone()]);

        // A test-only constraint exercises an unrelated unique failure rather
        // than relying on today's production schema having another unique key.
        sqlx::query("CREATE UNIQUE INDEX test_audit_request_id ON audit_events(request_id)")
            .execute(&s.pool)
            .await
            .unwrap();
        let conflicting = AuditEvent {
            id: ghinvite_core::AuditEventId::new(),
            ..event.clone()
        };
        let err = s.audit(&conflicting).await.unwrap_err();
        assert!(matches!(err, Error::Database(_)));
        assert_eq!(s.debug_list_audit(100).await.unwrap(), vec![event]);

        let mut other = sample_audit(100, EventType::InvitationLinkCreated, "link");
        other.request_id = None;
        s.audit(&other).await.unwrap();
        assert!(matches!(s.audit(&other).await, Err(Error::Database(_))));
    }

    #[tokio::test]
    async fn audit_scoped_per_account() {
        use ghinvite_core::audit::EventType;
        let s = SqlxStorage::in_memory().await.unwrap();
        s.audit(&sample_audit(100, EventType::InvitationLinkCreated, "x"))
            .await
            .unwrap();
        s.audit(&sample_audit(200, EventType::InvitationLinkCreated, "y"))
            .await
            .unwrap();

        let a = s.debug_list_audit(100).await.unwrap();
        let b = s.debug_list_audit(200).await.unwrap();
        assert_eq!(a.len(), 1);
        assert_eq!(b.len(), 1);
    }

    #[tokio::test]
    async fn audit_metadata_null_is_preserved() {
        use ghinvite_core::audit::EventType;
        let s = SqlxStorage::in_memory().await.unwrap();
        let mut e = sample_audit(100, EventType::InvitationLinkCreated, "x");
        e.metadata = serde_json::Value::Null;
        s.audit(&e).await.unwrap();
        let got = s.debug_list_audit(100).await.unwrap();
        assert_eq!(got[0].metadata, serde_json::Value::Null);
    }

    #[tokio::test]
    async fn invitation_request_with_unknown_link_fails() {
        let s = SqlxStorage::in_memory().await.unwrap();
        s.upsert_user(&sample_user(8, "alice")).await.unwrap();
        // bogus invitation_link_id (no row in invitation_links) → FK violation
        let req = sample_request(InvitationLinkId::new(), 8);
        let err = s
            .insert_invitation_request_and_increment_uses(&req)
            .await
            .unwrap_err();
        assert!(
            matches!(err, Error::Conflict(ConflictKind::ForeignKey)),
            "got {err:?}"
        );
    }

    #[tokio::test]
    async fn upsert_user_inserts_then_updates() {
        let s = SqlxStorage::in_memory().await.unwrap();
        let mut u = sample_user(7, "octocat");
        s.upsert_user(&u).await.unwrap();
        assert_eq!(s.get_user(7).await.unwrap().unwrap(), u);

        u.login = "octorenamed".into();
        u.last_seen_at = dt("2026-05-05T12:00:00Z");
        s.upsert_user(&u).await.unwrap();
        assert_eq!(s.get_user(7).await.unwrap().unwrap(), u);
    }

    #[tokio::test]
    async fn get_user_missing_returns_none() {
        let s = SqlxStorage::in_memory().await.unwrap();
        assert!(s.get_user(123).await.unwrap().is_none());
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
