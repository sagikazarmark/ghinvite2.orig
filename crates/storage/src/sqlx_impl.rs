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

    /// Test/debug-only: read all audit events for an account in occurrence order.
    /// Not on the `Storage` trait because audit reads are a v1.1 feature.
    #[cfg(any(test, feature = "test-suite"))]
    pub async fn debug_list_audit(&self, account_id: u64) -> Result<Vec<AuditEvent>> {
        let rows: Vec<crate::records::AuditEventRow> = sqlx::query_as(
            r#"SELECT id, account_id, occurred_at, event_type, actor_kind, actor_id,
                      target_kind, target_id, metadata, request_id
               FROM audit_events WHERE account_id = ?1 ORDER BY occurred_at, id"#,
        )
        .bind(u64_to_i64(account_id))
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(|r| r.try_into_domain()).collect()
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

/// Collapse a `LEFT JOIN share_links × share_link_repos` result set into at most
/// one [`ShareLink`]. Returns `None` if the join produced zero rows.
fn group_one_share_link(
    rows: Vec<(crate::records::ShareLinkRow, Option<i64>, Option<String>)>,
) -> Result<Option<ShareLink>> {
    let mut iter = rows.into_iter();
    let Some((row, first_repo_id, first_repo_name)) = iter.next() else {
        return Ok(None);
    };
    let mut repos = Vec::new();
    if let (Some(rid), Some(name)) = (first_repo_id, first_repo_name) {
        repos.push(domain::ShareLinkRepo {
            repo_id: rid as u64,
            repo_full_name: name,
        });
    }
    for (_, repo_id, repo_full_name) in iter {
        if let (Some(rid), Some(name)) = (repo_id, repo_full_name) {
            repos.push(domain::ShareLinkRepo {
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
            Err(sqlx::Error::Database(db)) if db.is_foreign_key_violation() => {
                Err(Error::Conflict(ConflictKind::ForeignKey))
            }
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
        .await?;
        Ok(())
    }

    async fn get_user(&self, user_id: u64) -> Result<Option<User>> {
        let row: Option<crate::records::UserRow> = sqlx::query_as(
            r#"SELECT user_id, login, avatar_url, last_seen_at FROM users WHERE user_id = ?1"#,
        )
        .bind(u64_to_i64(user_id))
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| r.into_domain()))
    }
    async fn insert_share_link(&self, link: &ShareLink) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            r#"
            INSERT INTO share_links
              (id, slug, installation_id, account_id, created_by, created_at, expires_at,
               max_uses, uses_count, permission, approval_required, internal_note,
               revoked_at, revoked_by)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
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
            other => Error::Database(other),
        })?;

        for repo in &link.repos {
            sqlx::query(
                r#"INSERT INTO share_link_repos (share_link_id, repo_id, repo_full_name)
                   VALUES (?1, ?2, ?3)"#,
            )
            .bind(link.id.to_string())
            .bind(u64_to_i64(repo.repo_id))
            .bind(&repo.repo_full_name)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    async fn mark_share_link_revoked(
        &self,
        id: ShareLinkId,
        by_user: u64,
        when: DateTime<Utc>,
    ) -> Result<()> {
        let res = sqlx::query(
            r#"UPDATE share_links SET revoked_at = ?1, revoked_by = ?2
               WHERE id = ?3 AND revoked_at IS NULL"#,
        )
        .bind(when)
        .bind(u64_to_i64(by_user))
        .bind(id.to_string())
        .execute(&self.pool)
        .await?;
        if res.rows_affected() == 0 {
            return Err(Error::NotFound);
        }
        Ok(())
    }

    async fn get_share_link_by_id(&self, id: ShareLinkId) -> Result<Option<ShareLink>> {
        let rows: Vec<crate::records::ShareLinkJoinRow> = sqlx::query_as(
            r#"
                SELECT l.id, l.slug, l.installation_id, l.account_id, l.created_by, l.created_at,
                       l.expires_at, l.max_uses, l.uses_count, l.permission, l.approval_required,
                       l.internal_note, l.revoked_at, l.revoked_by,
                       r.repo_id, r.repo_full_name
                FROM share_links l
                LEFT JOIN share_link_repos r ON r.share_link_id = l.id
                WHERE l.id = ?1
                ORDER BY r.repo_id
                "#,
        )
        .bind(id.to_string())
        .fetch_all(&self.pool)
        .await?;
        group_one_share_link(rows.into_iter().map(|j| j.split()).collect())
    }

    async fn get_share_link_by_slug(&self, slug: &str) -> Result<Option<ShareLink>> {
        let rows: Vec<crate::records::ShareLinkJoinRow> = sqlx::query_as(
            r#"
                SELECT l.id, l.slug, l.installation_id, l.account_id, l.created_by, l.created_at,
                       l.expires_at, l.max_uses, l.uses_count, l.permission, l.approval_required,
                       l.internal_note, l.revoked_at, l.revoked_by,
                       r.repo_id, r.repo_full_name
                FROM share_links l
                LEFT JOIN share_link_repos r ON r.share_link_id = l.id
                WHERE l.slug = ?1
                ORDER BY r.repo_id
                "#,
        )
        .bind(slug)
        .fetch_all(&self.pool)
        .await?;
        group_one_share_link(rows.into_iter().map(|j| j.split()).collect())
    }

    async fn list_share_links_for_account(&self, account_id: u64) -> Result<Vec<ShareLink>> {
        use std::collections::BTreeMap;

        let rows: Vec<crate::records::ShareLinkJoinRow> = sqlx::query_as(
            r#"
                SELECT l.id, l.slug, l.installation_id, l.account_id, l.created_by, l.created_at,
                       l.expires_at, l.max_uses, l.uses_count, l.permission, l.approval_required,
                       l.internal_note, l.revoked_at, l.revoked_by,
                       r.repo_id, r.repo_full_name
                FROM share_links l
                LEFT JOIN share_link_repos r ON r.share_link_id = l.id
                WHERE l.account_id = ?1
                ORDER BY l.created_at DESC, l.id, r.repo_id
                "#,
        )
        .bind(u64_to_i64(account_id))
        .fetch_all(&self.pool)
        .await?;
        let rows: Vec<(crate::records::ShareLinkRow, Option<i64>, Option<String>)> =
            rows.into_iter().map(|j| j.split()).collect();

        // Preserve the SQL ordering (created_at DESC, id) by tracking insertion order.
        let mut order: Vec<String> = Vec::new();
        let mut by_link: BTreeMap<
            String,
            (crate::records::ShareLinkRow, Vec<domain::ShareLinkRepo>),
        > = BTreeMap::new();
        for (link_row, repo_id, repo_full_name) in rows {
            let key = link_row.id.clone();
            let entry = by_link.entry(key.clone()).or_insert_with(|| {
                order.push(key.clone());
                (link_row, Vec::new())
            });
            if let (Some(rid), Some(name)) = (repo_id, repo_full_name) {
                entry.1.push(domain::ShareLinkRepo {
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
        let mut tx = self.pool.begin().await?;

        let res = sqlx::query(
            r#"
            INSERT INTO invitation_requests
              (id, share_link_id, requester_id, justification, state,
               decided_by, decided_at, decline_reason, created_at)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
            "#,
        )
        .bind(request.id.to_string())
        .bind(request.share_link_id.to_string())
        .bind(u64_to_i64(request.requester_id))
        .bind(request.justification.as_deref())
        .bind(request.state.to_string())
        .bind(request.decided_by.map(u64_to_i64))
        .bind(request.decided_at)
        .bind(request.decline_reason.as_deref())
        .bind(request.created_at)
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
            Err(e) => return Err(Error::Database(e)),
        }

        // Increment uses_count atomically. Caller has already verified is_active(now);
        // here we trust that and do the bump.
        let updated =
            sqlx::query(r#"UPDATE share_links SET uses_count = uses_count + 1 WHERE id = ?1"#)
                .bind(request.share_link_id.to_string())
                .execute(&mut *tx)
                .await?;
        if updated.rows_affected() == 0 {
            return Err(Error::NotFound);
        }

        tx.commit().await?;
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
        .await?;
        if res.rows_affected() == 0 {
            return Err(Error::NotFound);
        }
        Ok(())
    }

    async fn get_invitation_request(&self, id: RequestId) -> Result<Option<InvitationRequest>> {
        let row: Option<crate::records::InvitationRequestRow> = sqlx::query_as(
            r#"SELECT id, share_link_id, requester_id, justification, state,
                      decided_by, decided_at, decline_reason, created_at
               FROM invitation_requests WHERE id = ?1"#,
        )
        .bind(id.to_string())
        .fetch_optional(&self.pool)
        .await?;
        row.map(|r| r.try_into_domain()).transpose()
    }

    async fn list_pending_requests_for_account(
        &self,
        account_id: u64,
    ) -> Result<Vec<InvitationRequest>> {
        let rows: Vec<crate::records::InvitationRequestRow> = sqlx::query_as(
            r#"SELECT r.id, r.share_link_id, r.requester_id, r.justification, r.state,
                      r.decided_by, r.decided_at, r.decline_reason, r.created_at
               FROM invitation_requests r
               JOIN share_links l ON l.id = r.share_link_id
               WHERE l.account_id = ?1 AND r.state = 'pending'
               ORDER BY r.created_at"#,
        )
        .bind(u64_to_i64(account_id))
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(|r| r.try_into_domain()).collect()
    }

    async fn list_requests_for_link(&self, link_id: ShareLinkId) -> Result<Vec<InvitationRequest>> {
        let rows: Vec<crate::records::InvitationRequestRow> = sqlx::query_as(
            r#"SELECT id, share_link_id, requester_id, justification, state,
                      decided_by, decided_at, decline_reason, created_at
               FROM invitation_requests WHERE share_link_id = ?1
               ORDER BY created_at DESC"#,
        )
        .bind(link_id.to_string())
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(|r| r.try_into_domain()).collect()
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
            other => Error::Database(other),
        })?;
        Ok(())
    }

    async fn update_github_invitation(&self, update: &GithubInvitationUpdate) -> Result<()> {
        let res = sqlx::query(
            r#"UPDATE github_invitations
               SET state = ?1, github_invitation_id = COALESCE(?2, github_invitation_id),
                   error_message = ?3, updated_at = ?4
               WHERE id = ?5"#,
        )
        .bind(update.state.to_string())
        .bind(update.github_invitation_id.map(u64_to_i64))
        .bind(update.error_message.as_deref())
        .bind(update.updated_at)
        .bind(update.id.to_string())
        .execute(&self.pool)
        .await?;
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
        .await?;
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
        .await?;
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
               JOIN share_links l ON l.id = r.share_link_id
               WHERE l.installation_id = ?1
                 AND g.state IN ('sending', 'sent')
               ORDER BY g.created_at"#,
        )
        .bind(u64_to_i64(installation_id))
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(|r| r.try_into_domain()).collect()
    }
    async fn audit(&self, event: &AuditEvent) -> Result<()> {
        let metadata_json = if event.metadata.is_null() {
            None
        } else {
            Some(serde_json::to_string(&event.metadata).expect("audit metadata serializes"))
        };

        sqlx::query(
            r#"
            INSERT INTO audit_events
              (id, account_id, occurred_at, event_type, actor_kind, actor_id,
               target_kind, target_id, metadata, request_id)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
            "#,
        )
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
        .await?;
        Ok(())
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

    pub(crate) fn slug_with_seed(seed: u64) -> domain::Slug {
        use rand::SeedableRng;
        let mut rng = rand_chacha::ChaCha8Rng::seed_from_u64(seed);
        domain::Slug::generate(&mut rng)
    }

    pub(crate) fn sample_link(
        account_id: u64,
        installation_id: u64,
        created_by: u64,
        slug_seed: u64,
    ) -> ShareLink {
        use domain::{Permission, ShareLinkRepo};
        ShareLink {
            id: ShareLinkId::new(),
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
            internal_note: Some("for the contractor".into()),
            revoked_at: None,
            revoked_by: None,
            repos: vec![
                ShareLinkRepo {
                    repo_id: 10,
                    repo_full_name: "acme/api".into(),
                },
                ShareLinkRepo {
                    repo_id: 11,
                    repo_full_name: "acme/web".into(),
                },
            ],
        }
    }

    #[tokio::test]
    async fn insert_and_get_share_link_round_trips_with_repos() {
        let s = SqlxStorage::in_memory().await.unwrap();
        s.insert_installation(&sample_account(1, 100, "acme"))
            .await
            .unwrap();
        s.upsert_user(&sample_user(7, "octocat")).await.unwrap();

        let link = sample_link(100, 1, 7, 1);
        s.insert_share_link(&link).await.unwrap();

        let got = s.get_share_link_by_id(link.id).await.unwrap().unwrap();
        assert_eq!(got, link);

        let by_slug = s
            .get_share_link_by_slug(link.slug.as_str())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(by_slug.id, link.id);
    }

    #[tokio::test]
    async fn share_link_slug_is_unique() {
        let s = SqlxStorage::in_memory().await.unwrap();
        s.insert_installation(&sample_account(1, 100, "acme"))
            .await
            .unwrap();
        s.upsert_user(&sample_user(7, "octocat")).await.unwrap();

        let a = sample_link(100, 1, 7, 1);
        let mut b = sample_link(100, 1, 7, 1); // same seed → same slug
        b.id = ShareLinkId::new();
        s.insert_share_link(&a).await.unwrap();
        let err = s.insert_share_link(&b).await.unwrap_err();
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
        s.insert_share_link(&link).await.unwrap();

        s.mark_share_link_revoked(link.id, 7, dt("2026-05-04T13:00:00Z"))
            .await
            .unwrap();

        let got = s.get_share_link_by_id(link.id).await.unwrap().unwrap();
        assert_eq!(got.revoked_by, Some(7));
        assert_eq!(got.revoked_at, Some(dt("2026-05-04T13:00:00Z")));

        // Idempotent revoke returns NotFound second time:
        let err = s
            .mark_share_link_revoked(link.id, 7, dt("2026-05-04T14:00:00Z"))
            .await
            .unwrap_err();
        assert!(matches!(err, Error::NotFound));
    }

    #[tokio::test]
    async fn list_share_links_for_account_returns_in_descending_created_at() {
        let s = SqlxStorage::in_memory().await.unwrap();
        s.insert_installation(&sample_account(1, 100, "acme"))
            .await
            .unwrap();
        s.upsert_user(&sample_user(7, "octocat")).await.unwrap();

        let mut older = sample_link(100, 1, 7, 3);
        older.created_at = dt("2026-05-01T00:00:00Z");

        let mut newer = sample_link(100, 1, 7, 4);
        newer.created_at = dt("2026-05-04T00:00:00Z");

        s.insert_share_link(&older).await.unwrap();
        s.insert_share_link(&newer).await.unwrap();

        let list = s.list_share_links_for_account(100).await.unwrap();
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
        s.insert_share_link(&link).await.unwrap();

        let got = s.get_share_link_by_id(link.id).await.unwrap().unwrap();
        assert!(got.repos.is_empty());

        let list = s.list_share_links_for_account(100).await.unwrap();
        assert_eq!(list.len(), 1);
        assert!(list[0].repos.is_empty());
    }

    #[tokio::test]
    async fn share_link_with_unknown_installation_fails() {
        let s = SqlxStorage::in_memory().await.unwrap();
        s.upsert_user(&sample_user(7, "octocat")).await.unwrap();
        // installation_id 999 does not exist → FK violation
        let link = sample_link(100, 999, 7, 6);
        let err = s.insert_share_link(&link).await.unwrap_err();
        assert!(
            matches!(err, Error::Conflict(ConflictKind::ForeignKey)),
            "got {err:?}"
        );
    }

    pub(crate) fn sample_request(link_id: ShareLinkId, requester: u64) -> InvitationRequest {
        use domain::RequestState;
        InvitationRequest {
            id: RequestId::new(),
            share_link_id: link_id,
            requester_id: requester,
            justification: Some("contractor for q2".into()),
            state: RequestState::Pending,
            decided_by: None,
            decided_at: None,
            decline_reason: None,
            created_at: dt("2026-05-04T12:30:00Z"),
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
        s.insert_share_link(&link).await.unwrap();

        let req = sample_request(link.id, 8);
        s.insert_invitation_request_and_increment_uses(&req)
            .await
            .unwrap();

        let got = s.get_invitation_request(req.id).await.unwrap().unwrap();
        assert_eq!(got, req);

        let updated_link = s.get_share_link_by_id(link.id).await.unwrap().unwrap();
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
        s.insert_share_link(&link).await.unwrap();

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
        use domain::RequestState;
        let s = SqlxStorage::in_memory().await.unwrap();
        s.insert_installation(&sample_account(1, 100, "acme"))
            .await
            .unwrap();
        s.upsert_user(&sample_user(7, "octocat")).await.unwrap();
        s.upsert_user(&sample_user(8, "alice")).await.unwrap();
        let link = sample_link(100, 1, 7, 12);
        s.insert_share_link(&link).await.unwrap();

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
        use domain::RequestState;
        let s = SqlxStorage::in_memory().await.unwrap();
        s.insert_installation(&sample_account(1, 100, "acme"))
            .await
            .unwrap();
        s.upsert_user(&sample_user(7, "octocat")).await.unwrap();
        s.upsert_user(&sample_user(8, "alice")).await.unwrap();
        let link = sample_link(100, 1, 7, 13);
        s.insert_share_link(&link).await.unwrap();
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

        s.insert_share_link(&link_a).await.unwrap();
        s.insert_share_link(&link_b).await.unwrap();

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
        use domain::InvitationState;
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
        use domain::InvitationState;
        let s = SqlxStorage::in_memory().await.unwrap();
        s.insert_installation(&sample_account(1, 100, "acme"))
            .await
            .unwrap();
        s.upsert_user(&sample_user(7, "octocat")).await.unwrap();
        s.upsert_user(&sample_user(8, "alice")).await.unwrap();
        let link = sample_link(100, 1, 7, 20);
        s.insert_share_link(&link).await.unwrap();
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
        use domain::InvitationState;
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
        s.insert_share_link(&link_a).await.unwrap();
        let link_b = sample_link(200, 2, 7, 22);
        s.insert_share_link(&link_b).await.unwrap();

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
        event_type: audit::EventType,
        target: &str,
    ) -> AuditEvent {
        use audit::{ActorKind, TargetKind};
        use domain::AuditEventId;
        AuditEvent {
            id: AuditEventId::new(),
            account_id,
            occurred_at: dt("2026-05-04T13:00:00Z"),
            event_type,
            actor_kind: ActorKind::User,
            actor_id: Some(7),
            target_kind: TargetKind::ShareLink,
            target_id: target.into(),
            metadata: serde_json::json!({"k": "v"}),
            request_id: Some("inv-abc".into()),
        }
    }

    #[tokio::test]
    async fn audit_append_then_read_back() {
        use audit::EventType;
        let s = SqlxStorage::in_memory().await.unwrap();
        let e1 = sample_audit(100, EventType::ShareLinkCreated, "01HFLINK1");
        let e2 = AuditEvent {
            occurred_at: dt("2026-05-04T13:01:00Z"),
            ..sample_audit(100, EventType::ShareLinkRevoked, "01HFLINK1")
        };
        s.audit(&e1).await.unwrap();
        s.audit(&e2).await.unwrap();

        let got = s.debug_list_audit(100).await.unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(got[0], e1);
        assert_eq!(got[1], e2);
    }

    #[tokio::test]
    async fn audit_scoped_per_account() {
        use audit::EventType;
        let s = SqlxStorage::in_memory().await.unwrap();
        s.audit(&sample_audit(100, EventType::ShareLinkCreated, "x"))
            .await
            .unwrap();
        s.audit(&sample_audit(200, EventType::ShareLinkCreated, "y"))
            .await
            .unwrap();

        let a = s.debug_list_audit(100).await.unwrap();
        let b = s.debug_list_audit(200).await.unwrap();
        assert_eq!(a.len(), 1);
        assert_eq!(b.len(), 1);
    }

    #[tokio::test]
    async fn audit_metadata_null_is_preserved() {
        use audit::EventType;
        let s = SqlxStorage::in_memory().await.unwrap();
        let mut e = sample_audit(100, EventType::ShareLinkCreated, "x");
        e.metadata = serde_json::Value::Null;
        s.audit(&e).await.unwrap();
        let got = s.debug_list_audit(100).await.unwrap();
        assert_eq!(got[0].metadata, serde_json::Value::Null);
    }

    #[tokio::test]
    async fn invitation_request_with_unknown_link_fails() {
        let s = SqlxStorage::in_memory().await.unwrap();
        s.upsert_user(&sample_user(8, "alice")).await.unwrap();
        // bogus share_link_id (no row in share_links) → FK violation
        let req = sample_request(ShareLinkId::new(), 8);
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
