//! `Storage` implementation backed by `sqlx::SqlitePool`. Statements and row
//! mapping are core's; this adapter binds values and runs them.

use crate::{decode, to_db_err, u64_to_i64};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use ghinvite_core::audit::{AuditEvent, EventType};
use ghinvite_core::delivery::{CreateCommand, DeliverySnapshot};
use ghinvite_core::storage::AppInstallationReadStorage;
use ghinvite_core::storage::attempt_continuations::StoredContinuation;
use ghinvite_core::storage::{
    AuditPage, AuditPosition, AuditStorage, ConsoleStorage, ContinuationStorage, DeliveryStorage,
    Error, RecordStorage, Result, attempt_continuations, audit_read, audit_write, classify_insert,
    delivery_attempts, delivery_projection, installations, invitation_links, invitation_requests,
    pending_queue, request_history, users,
};
use ghinvite_core::{InvitationLink, InvitationLinkId, InvitationRequest, RequestId, User};
use serde::de::{DeserializeOwned, IgnoredAny};
use sqlx::sqlite::{SqliteArguments, SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{Sqlite, SqlitePool};
use std::path::Path;

type Query<'q> = sqlx::query::Query<'q, Sqlite, SqliteArguments<'q>>;

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
            .map_err(to_db_err)?;
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
            .map_err(to_db_err)?;
        let s = Self { pool };
        s.run_migrations().await?;
        Ok(s)
    }

    /// Test/debug-only: read all audit events for an account in occurrence order.
    /// Production browsing uses the bounded `Storage::list_audit_events`.
    #[cfg(any(test, feature = "test-util"))]
    pub async fn debug_list_audit(&self, account_id: u64) -> Result<Vec<AuditEvent>> {
        let rows: Vec<audit_read::AuditEventRow> = self
            .all(
                sqlx::query(
                    r#"SELECT id, account_id, occurred_at, event_type, actor_kind, actor_id,
                      target_kind, target_id, metadata, request_id
               FROM audit_events WHERE account_id = ?1 ORDER BY occurred_at, id"#,
                )
                .bind(u64_to_i64(account_id)?),
            )
            .await?;
        rows.into_iter()
            .map(audit_read::AuditEventRow::into_event)
            .collect()
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
            .map_err(to_db_err)?;
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
            .map_err(to_db_err)?;
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
            .map_err(to_db_err)?;
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

    async fn all<T: DeserializeOwned>(&self, query: Query<'_>) -> Result<Vec<T>> {
        let rows = query.fetch_all(&self.pool).await.map_err(to_db_err)?;
        rows.iter().map(decode).collect()
    }

    async fn optional<T: DeserializeOwned>(&self, query: Query<'_>) -> Result<Option<T>> {
        let row = query.fetch_optional(&self.pool).await.map_err(to_db_err)?;
        row.as_ref().map(decode).transpose()
    }

    async fn one<T: DeserializeOwned>(&self, query: Query<'_>) -> Result<T> {
        decode(&query.fetch_one(&self.pool).await.map_err(to_db_err)?)
    }

    /// Rows changed.
    async fn execute(&self, query: Query<'_>) -> Result<u64> {
        Ok(query
            .execute(&self.pool)
            .await
            .map_err(to_db_err)?
            .rows_affected())
    }

    /// Run `statements` in one transaction, each bound to the same `input`.
    pub(crate) async fn transaction<S: AsRef<str>>(
        &self,
        statements: &[S],
        input: &str,
        classify: fn(String) -> Error,
    ) -> Result<()> {
        let mut tx = self.pool.begin().await.map_err(to_db_err)?;
        for statement in statements {
            sqlx::query(statement.as_ref())
                .bind(input)
                .execute(&mut *tx)
                .await
                .map_err(|e| classify(e.to_string()))?;
        }
        tx.commit().await.map_err(to_db_err)
    }
}

#[async_trait]
impl RecordStorage for SqlxStorage {
    async fn upsert_user(&self, user: &User) -> Result<()> {
        let query = sqlx::query(users::UPSERT)
            .bind(u64_to_i64(user.user_id)?)
            .bind(&user.login)
            .bind(user.avatar_url.as_deref())
            .bind(user.last_seen_at)
            .bind(user.github_created_at);
        self.execute(query).await?;
        Ok(())
    }

    async fn get_user(&self, user_id: u64) -> Result<Option<User>> {
        self.optional(sqlx::query(users::GET).bind(u64_to_i64(user_id)?))
            .await
    }

    async fn get_invitation_link_by_id(
        &self,
        id: InvitationLinkId,
    ) -> Result<Option<InvitationLink>> {
        let query = sqlx::query(invitation_links::GET).bind(id.to_string());
        Ok(invitation_links::fold(self.all(query).await?)?.pop())
    }

    async fn get_invitation_request(&self, id: RequestId) -> Result<Option<InvitationRequest>> {
        self.optional::<invitation_requests::Row>(
            sqlx::query(invitation_requests::GET).bind(id.to_string()),
        )
        .await?
        .map(invitation_requests::Row::into_request)
        .transpose()
    }
}

#[async_trait]
impl ConsoleStorage for SqlxStorage {
    async fn revoke_preview(
        &self,
        account_id: u64,
        link_id: ghinvite_core::InvitationLinkId,
    ) -> Result<Option<ghinvite_core::revocation::RevokePreview>> {
        self.optional(
            sqlx::query(ghinvite_core::storage::projection::revocation_sql::PREVIEW)
                .bind(link_id.to_string())
                .bind(ghinvite_core::storage::integer::unsigned(account_id)?),
        )
        .await
    }

    async fn scoped_blocked_digest(
        &self,
        scope: &ghinvite_core::storage::console_scope::ConsoleScope,
        day: chrono::NaiveDate,
    ) -> Result<Option<ghinvite_core::storage::blocked_digest::BlockedDigest>> {
        self.read_blocked_digest(scope, day).await
    }

    async fn scoped_list_invitation_links_for_account(
        &self,
        scope: &ghinvite_core::storage::console_scope::ConsoleScope,
    ) -> Result<Vec<InvitationLink>> {
        use ghinvite_core::storage::console_scope as cs;
        let Some(repos) = scope.repo_ids_json() else {
            return self
                .list_invitation_links_for_account(scope.account_id())
                .await;
        };
        let sql = cs::links_query();
        invitation_links::fold(
            self.all(
                sqlx::query(&sql)
                    .bind(u64_to_i64(scope.account_id())?)
                    .bind(repos),
            )
            .await?,
        )
    }
    async fn scoped_count_pending_requests_for_account(
        &self,
        scope: &ghinvite_core::storage::console_scope::ConsoleScope,
    ) -> Result<u64> {
        use ghinvite_core::storage::console_scope as cs;
        let Some(repos) = scope.repo_ids_json() else {
            return self
                .count_pending_requests_for_account(scope.account_id())
                .await;
        };
        let sql = cs::count_query();
        let row: pending_queue::CountRow = self
            .one(
                sqlx::query(&sql)
                    .bind(u64_to_i64(scope.account_id())?)
                    .bind(repos),
            )
            .await?;
        Ok(row.pending)
    }
    async fn scoped_list_pending_requests_page(
        &self,
        scope: &ghinvite_core::storage::console_scope::ConsoleScope,
        after: Option<pending_queue::PendingBoundary>,
    ) -> Result<pending_queue::PendingPage> {
        use ghinvite_core::storage::console_scope as cs;
        let Some(repos) = scope.repo_ids_json() else {
            return self.pending_request_page(scope.account_id(), after).await;
        };
        let sql = cs::pending_query(after.is_some());
        pending_queue::PendingPage::from_json(
            self.all(
                sqlx::query(&sql)
                    .bind(u64_to_i64(scope.account_id())?)
                    .bind(after.map(|b| b.seek_key()))
                    .bind(repos),
            )
            .await?,
        )
    }
    async fn scoped_list_audit_events(
        &self,
        scope: &ghinvite_core::storage::console_scope::ConsoleScope,
        event: Option<EventType>,
        position: AuditPosition,
    ) -> Result<AuditPage> {
        use ghinvite_core::storage::console_scope as cs;
        let Some(repos) = scope.repo_ids_json() else {
            return self
                .list_audit_events(scope.account_id(), event, position)
                .await;
        };
        fn query<'a>(
            sql: &'a str,
            account: u64,
            repos: &'a str,
            event: Option<EventType>,
            position: AuditPosition,
        ) -> Result<Query<'a>> {
            let boundary = position.boundary();
            Ok(sqlx::query(sql)
                .bind(u64_to_i64(account)?)
                .bind(event.map(|e| e.as_str()))
                .bind(boundary.map(audit_read::boundary_time))
                .bind(boundary.map(|b| b.id.to_string()))
                .bind(repos))
        }
        let sql = cs::audit_query(event, position, false);
        let mut page = AuditPage::from_rows(
            self.all(query(&sql, scope.account_id(), &repos, event, position)?)
                .await?,
            position,
        )?;
        if let Some([newer, older]) = page.probes() {
            for (seek, flag) in [(newer, &mut page.has_newer), (older, &mut page.has_older)] {
                let sql = cs::audit_query(event, seek, true);
                *flag = self
                    .optional::<IgnoredAny>(query(&sql, scope.account_id(), &repos, event, seek)?)
                    .await?
                    .is_some();
            }
        }
        Ok(page)
    }
    async fn distinct_console_repo_targets(
        &self,
        account_id: u64,
        after_repo_id: Option<u64>,
        limit: u16,
    ) -> Result<ghinvite_core::storage::console_scope::RepoTargetPage> {
        use ghinvite_core::storage::console_scope as cs;
        let (account, after, count) = cs::target_query_args(account_id, after_repo_id, limit)?;
        cs::target_page(
            self.all(
                sqlx::query(cs::REPO_TARGETS)
                    .bind(u64_to_i64(account)?)
                    .bind(u64_to_i64(after)?)
                    .bind(u64_to_i64(count)?),
            )
            .await?,
            limit,
        )
    }

    async fn invitation_link_belongs_to_account(
        &self,
        account_id: u64,
        id: InvitationLinkId,
    ) -> Result<bool> {
        let query = sqlx::query(invitation_links::BELONGS_TO_ACCOUNT)
            .bind(id.to_string())
            .bind(u64_to_i64(account_id)?);
        Ok(self.optional::<IgnoredAny>(query).await?.is_some())
    }

    async fn list_invitation_links_for_account(
        &self,
        account_id: u64,
    ) -> Result<Vec<InvitationLink>> {
        let query = sqlx::query(invitation_links::FOR_ACCOUNT).bind(u64_to_i64(account_id)?);
        invitation_links::fold(self.all(query).await?)
    }

    async fn request_history(
        &self,
        account_id: u64,
        link_id: InvitationLinkId,
        before: Option<request_history::Boundary>,
    ) -> Result<request_history::Page> {
        let sql = request_history::query(before);
        let query = sqlx::query(&sql)
            .bind(u64_to_i64(account_id)?)
            .bind(link_id.to_string())
            .bind(before.map(request_history::boundary_key));
        request_history::page(self.all(query).await?)
    }

    async fn count_pending_requests_for_account(&self, account_id: u64) -> Result<u64> {
        let query = sqlx::query(pending_queue::COUNT_QUERY).bind(u64_to_i64(account_id)?);
        let row: pending_queue::CountRow = self.one(query).await?;
        Ok(row.pending)
    }

    async fn pending_request_page(
        &self,
        account_id: u64,
        after: Option<pending_queue::PendingBoundary>,
    ) -> Result<pending_queue::PendingPage> {
        let sql = pending_queue::query(after.is_some());
        let query = sqlx::query(&sql)
            .bind(u64_to_i64(account_id)?)
            .bind(after.map(|b| b.seek_key()));
        pending_queue::PendingPage::from_json(self.all(query).await?)
    }

    async fn get_delivery(&self, key: &str) -> Result<Option<DeliverySnapshot>> {
        self.optional::<delivery_projection::ReceiptRow>(
            sqlx::query(delivery_projection::GET).bind(key),
        )
        .await?
        .map(|row| row.decode())
        .transpose()
    }

    async fn list_delivery_for_request(&self, id: RequestId) -> Result<Vec<DeliverySnapshot>> {
        let query = sqlx::query(delivery_projection::FOR_REQUEST).bind(id.to_string());
        let rows: Vec<delivery_projection::ReceiptRow> = self.all(query).await?;
        rows.into_iter().map(|row| row.decode()).collect()
    }

    async fn list_audit_events(
        &self,
        account_id: u64,
        event: Option<EventType>,
        position: AuditPosition,
    ) -> Result<AuditPage> {
        fn query(
            sql: &str,
            account_id: u64,
            event: Option<EventType>,
            position: AuditPosition,
        ) -> Result<Query<'_>> {
            let boundary = position.boundary();
            Ok(sqlx::query(sql)
                .bind(u64_to_i64(account_id)?)
                .bind(event.map(|e| e.as_str()))
                .bind(boundary.map(audit_read::boundary_time))
                .bind(boundary.map(|b| b.id.to_string())))
        }
        let sql = audit_read::query(event, position, false);
        let rows = self.all(query(&sql, account_id, event, position)?).await?;
        let mut page = AuditPage::from_rows(rows, position)?;
        if let Some([newer, older]) = page.probes() {
            for (seek, flag) in [(newer, &mut page.has_newer), (older, &mut page.has_older)] {
                let sql = audit_read::query(event, seek, true);
                let probe = query(&sql, account_id, event, seek)?;
                *flag = self.optional::<IgnoredAny>(probe).await?.is_some();
            }
        }
        Ok(page)
    }
}

#[async_trait]
impl ContinuationStorage for SqlxStorage {
    async fn continuation_rotation_page(
        &self,
        after: Option<i64>,
    ) -> Result<Vec<ghinvite_core::storage::continuation_rotation::ScanRow>> {
        use ghinvite_core::storage::{continuation_rotation as sql, integer};
        if let Some(value) = after {
            integer::signed(value)?;
        }
        let rows: Vec<sql::ScanRow> = self.all(sqlx::query(sql::PAGE).bind(after)).await?;
        if rows.is_empty() {
            return Err(Error::Corrupt("continuation rotation epoch missing".into()));
        }
        for row in &rows {
            row.validate()?;
        }
        Ok(rows)
    }
    async fn continuation_rotation_epoch(&self) -> Result<i64> {
        use ghinvite_core::storage::{continuation_rotation as sql, integer};
        let row: sql::Epoch = self.one(sqlx::query(sql::EPOCH)).await?;
        integer::unsigned(
            u64::try_from(row.revision)
                .map_err(|_| Error::Corrupt("continuation rotation epoch".into()))?,
        )?;
        Ok(row.revision)
    }
    async fn rewrap_attempt_continuation(
        &self,
        old: &ghinvite_core::storage::continuation_rotation::ScanRow,
        replacement: &str,
        expected_epoch: i64,
    ) -> Result<bool> {
        use ghinvite_core::storage::{continuation_rotation as sql, integer};
        old.validate()?;
        integer::signed(expected_epoch)?;
        if old.row_id.is_none()
            || old.oversized != 0
            || replacement.len() > sql::MAX_CIPHERTEXT_BYTES
        {
            return Err(Error::Corrupt("continuation rotation CAS input".into()));
        }
        let returned: Vec<sql::CasReceipt> = self
            .all(
                sqlx::query(sql::CAS)
                    .bind(replacement)
                    .bind(old.row_id)
                    .bind(&old.scope)
                    .bind(&old.id)
                    .bind(&old.binding)
                    .bind(&old.payload)
                    .bind(old.expires_at)
                    .bind(expected_epoch),
            )
            .await?;
        sql::CasReceipt::applied(&returned, old.row_id.unwrap())
    }

    async fn retain_attempt_continuation(
        &self,
        scope: &str,
        id: &str,
        binding: &str,
        payload: &str,
        expires_at: i64,
        now: i64,
    ) -> Result<StoredContinuation> {
        use attempt_continuations::*;
        ghinvite_core::storage::integer::signed(expires_at)?;
        ghinvite_core::storage::integer::signed(now)?;
        self.execute(sqlx::query(CLEANUP).bind(ghinvite_core::storage::integer::signed(now)?))
            .await?;
        self.execute(
            sqlx::query(INSERT)
                .bind(scope)
                .bind(id)
                .bind(binding)
                .bind(payload)
                .bind(ghinvite_core::storage::integer::signed(expires_at)?),
        )
        .await?;
        self.one(
            sqlx::query(BY_BINDING)
                .bind(scope)
                .bind(binding)
                .bind(id)
                .bind(ghinvite_core::storage::integer::signed(now)?),
        )
        .await
    }

    async fn get_attempt_continuation(
        &self,
        scope: &str,
        id: &str,
        now: i64,
    ) -> Result<Option<StoredContinuation>> {
        let query = sqlx::query(attempt_continuations::GET);
        self.optional(
            query
                .bind(scope)
                .bind(id)
                .bind(ghinvite_core::storage::integer::signed(now)?),
        )
        .await
    }

    async fn list_attempt_continuations(
        &self,
        scope: &str,
        now: i64,
    ) -> Result<Vec<StoredContinuation>> {
        let query = sqlx::query(attempt_continuations::LIST);
        self.all(
            query
                .bind(scope)
                .bind(ghinvite_core::storage::integer::signed(now)?),
        )
        .await
    }

    async fn release_attempt_continuation(&self, scope: &str, id: &str) -> Result<()> {
        let query = sqlx::query(attempt_continuations::RELEASE);
        self.execute(query.bind(scope).bind(id)).await?;
        Ok(())
    }
}

#[async_trait]
impl DeliveryStorage for SqlxStorage {
    async fn cancellation_attempt_exists(
        &self,
        command: &delivery_attempts::CancelCommand,
    ) -> Result<bool> {
        let encoded = delivery_attempts::encode_cancel(command)?;
        let row = self
            .optional(
                sqlx::query(delivery_attempts::CANCEL_FENCED).bind(command.create.delivery_key()),
            )
            .await?;
        delivery_attempts::fenced(row, &encoded)
    }

    async fn claim_cancellation_attempt(
        &self,
        command: &delivery_attempts::CancelCommand,
    ) -> Result<Option<u64>> {
        let encoded = delivery_attempts::encode_cancel(command)?;
        let key = command.create.delivery_key();
        let generation = self
            .optional(
                sqlx::query(delivery_attempts::CANCEL_CLAIM)
                    .bind(&key)
                    .bind(&encoded),
            )
            .await?;
        let retained = self
            .one(
                sqlx::query(delivery_attempts::CANCEL_COMMAND)
                    .bind(&key)
                    .bind(&encoded),
            )
            .await?;
        delivery_attempts::claimed(generation, retained, &encoded)
    }
    async fn reject_cancellation_attempt(
        &self,
        command: &delivery_attempts::CancelCommand,
        generation: u64,
    ) -> Result<()> {
        let encoded = delivery_attempts::encode_cancel(command)?;
        self.execute(
            sqlx::query(delivery_attempts::CANCEL_REJECT)
                .bind(command.create.delivery_key())
                .bind(encoded)
                .bind(u64_to_i64(generation)?),
        )
        .await?;
        Ok(())
    }

    async fn claim_delivery_attempt(&self, command: &CreateCommand) -> Result<Option<u64>> {
        let encoded = delivery_attempts::encode(command)?;
        let key = command.delivery_key();
        let generation = self
            .optional(
                sqlx::query(delivery_attempts::CLAIM)
                    .bind(&key)
                    .bind(&encoded),
            )
            .await?;
        let retained = self
            .one(
                sqlx::query(delivery_attempts::COMMAND)
                    .bind(&key)
                    .bind(&encoded),
            )
            .await?;
        delivery_attempts::claimed(generation, retained, &encoded)
    }
    async fn reject_delivery_attempt(
        &self,
        command: &CreateCommand,
        generation: u64,
    ) -> Result<()> {
        let encoded = delivery_attempts::encode(command)?;
        self.execute(
            sqlx::query(delivery_attempts::REJECT)
                .bind(command.delivery_key())
                .bind(encoded)
                .bind(u64_to_i64(generation)?),
        )
        .await?;
        Ok(())
    }
    async fn delivery_attempt_exists(&self, command: &CreateCommand) -> Result<bool> {
        let encoded = delivery_attempts::encode(command)?;
        let row = self
            .optional(sqlx::query(delivery_attempts::FENCED).bind(command.delivery_key()))
            .await?;
        delivery_attempts::fenced(row, &encoded)
    }

    async fn project_delivery_with_events(
        &self,
        projection: &ghinvite_core::delivery_audit::RepositoryDeliveryProjection,
    ) -> Result<()> {
        let encoded = delivery_projection::encode_with_events(projection)?;
        self.transaction(
            &delivery_projection::statements(),
            &encoded,
            delivery_projection::classify,
        )
        .await
    }

    async fn project_repository_telemetry(
        &self,
        telemetry: &ghinvite_core::storage::operational::RepositoryTelemetry,
    ) -> Result<()> {
        use ghinvite_core::storage::operational;
        let encoded = operational::encode(telemetry)?;
        self.transaction(
            &operational::statements(),
            &encoded,
            delivery_projection::classify,
        )
        .await
    }
}

#[async_trait]
impl AuditStorage for SqlxStorage {
    async fn audit(&self, event: &AuditEvent) -> Result<()> {
        ghinvite_core::storage::integer::validate(event)?;
        let sql = audit_write::insert(event.event_type);
        let query = sqlx::query(&sql)
            .bind(event.id.to_string())
            .bind(u64_to_i64(event.account_id)?)
            .bind(event.occurred_at)
            .bind(event.event_type.as_str())
            .bind(event.actor_kind.to_string())
            .bind(event.actor_id.map(u64_to_i64).transpose()?)
            .bind(event.target_kind.to_string())
            .bind(&event.target_id)
            .bind(audit_write::metadata(event))
            .bind(event.request_id.as_deref());
        self.execute(query).await?;
        Ok(())
    }
}

impl SqlxStorage {}

#[async_trait]
impl ghinvite_core::storage::AppInstallationReadStorage for SqlxStorage {
    async fn app_installation(
        &self,
        app: ghinvite_core::account::App,
        id: u64,
    ) -> Result<Option<ghinvite_core::account::Installation>> {
        self.optional::<installations::AppRow>(
            sqlx::query(installations::GET_APP)
                .bind(app.as_str())
                .bind(u64_to_i64(id)?),
        )
        .await?
        .map(installations::AppRow::decode)
        .transpose()
    }
    async fn active_app_installation(
        &self,
        app: ghinvite_core::account::App,
        id: u64,
    ) -> Result<Option<ghinvite_core::account::Installation>> {
        self.optional::<installations::AppRow>(
            sqlx::query(installations::ACTIVE_APP)
                .bind(app.as_str())
                .bind(u64_to_i64(id)?),
        )
        .await?
        .map(installations::AppRow::decode)
        .transpose()
    }
    async fn account_settings(
        &self,
        id: u64,
    ) -> Result<Option<ghinvite_core::account::AccountSettings>> {
        self.optional::<installations::SettingsRow>(
            sqlx::query(installations::GET_SETTINGS).bind(u64_to_i64(id)?),
        )
        .await?
        .map(installations::SettingsRow::decode)
        .transpose()
    }
    async fn active_app_installation_by_login(
        &self,
        app: ghinvite_core::account::App,
        login: &str,
    ) -> Result<Option<ghinvite_core::account::Installation>> {
        self.optional::<installations::AppRow>(
            sqlx::query(installations::ACTIVE_APP_LOGIN)
                .bind(app.as_str())
                .bind(login),
        )
        .await?
        .map(installations::AppRow::decode)
        .transpose()
    }
    async fn latest_app_installation_by_login(
        &self,
        app: ghinvite_core::account::App,
        login: &str,
    ) -> Result<Option<ghinvite_core::account::Installation>> {
        self.optional::<installations::AppRow>(
            sqlx::query(installations::LATEST_APP_LOGIN)
                .bind(app.as_str())
                .bind(login),
        )
        .await?
        .map(installations::AppRow::decode)
        .transpose()
    }
    async fn list_active_app_installations(
        &self,
        app: ghinvite_core::account::App,
    ) -> Result<Vec<ghinvite_core::account::Installation>> {
        self.all::<installations::AppRow>(
            sqlx::query(installations::LIST_ACTIVE_APP).bind(app.as_str()),
        )
        .await?
        .into_iter()
        .map(installations::AppRow::decode)
        .collect()
    }
}
#[async_trait]
impl ghinvite_core::storage::AppInstallationStorage for SqlxStorage {
    async fn project_installation_change(
        &self,
        owner: ghinvite_core::account::AccountInstallationRef,
        change: &ghinvite_core::installation_audit::InstallationChange,
    ) -> Result<()> {
        use ghinvite_core::storage::installation_projection as sql;
        let input = sql::encode(owner, change)?;
        let mut tx = self.pool.begin().await.map_err(to_db_err)?;
        for statement in sql::statements() {
            sqlx::query(&statement)
                .bind(&input)
                .execute(&mut *tx)
                .await
                .map_err(|e| {
                    ghinvite_core::storage::delivery_projection::classify(e.to_string())
                })?;
        }
        tx.commit().await.map_err(to_db_err)
    }

    async fn put_app_installation(
        &self,
        value: &ghinvite_core::account::Installation,
    ) -> Result<()> {
        let encoded = installations::encode_app(value)?;
        let a = &value.account;
        self.execute(
            sqlx::query(installations::PUT_APP)
                .bind(value.app.as_str())
                .bind(u64_to_i64(a.installation_id)?)
                .bind(u64_to_i64(a.account_id)?)
                .bind(&a.account_login)
                .bind(a.account_type.to_string())
                .bind(a.installed_at.to_rfc3339())
                .bind(a.uninstalled_at.map(|v| v.to_rfc3339()))
                .bind(installations::encode_selected_repos(&a.selected_repos))
                .bind(value.suspended_at.map(|v| v.to_rfc3339()))
                .bind(serde_json::to_string(&value.permissions).expect("typed permissions"))
                .bind(value.installed_by_user_id.map(u64_to_i64).transpose()?)
                .bind(&value.installed_by_login)
                .bind(u64_to_i64(value.revision)?)
                .bind(encoded),
        )
        .await
        .map_err(|e| match e {
            Error::Database(message) => classify_insert(message),
            other => other,
        })?;
        installations::applied_app(
            value,
            self.app_installation(value.app, a.installation_id).await?,
        )
    }
    async fn put_account_settings(
        &self,
        value: &ghinvite_core::account::AccountSettings,
    ) -> Result<()> {
        let encoded = installations::encode_settings(value)?;
        self.execute(
            sqlx::query(installations::PUT_SETTINGS)
                .bind(u64_to_i64(value.account_id)?)
                .bind(i64::from(value.repo_admin_delegation))
                .bind(u64_to_i64(value.changed_by)?)
                .bind(value.changed_at.to_rfc3339())
                .bind(u64_to_i64(value.revision)?)
                .bind(encoded),
        )
        .await?;
        installations::applied_settings(value, self.account_settings(value.account_id).await?)
    }
}

#[async_trait]
impl ghinvite_core::storage::LinkCapabilityStorage for SqlxStorage {
    async fn link_targets(&self, id: InvitationLinkId) -> Result<ghinvite_core::targets::Targets> {
        use ghinvite_core::storage::link_targets as t;
        t::decode(self.all(sqlx::query(t::GET).bind(id.to_string())).await?)
    }
    async fn register_link_secret(
        &self,
        secret: &ghinvite_core::storage::link_secrets::LinkSecret,
    ) -> Result<()> {
        use ghinvite_core::storage::link_secrets as s;
        secret.validate()?;
        self.execute(
            sqlx::query(s::REGISTER)
                .bind(secret.secret_sha256.as_hex())
                .bind(secret.link_id.to_string())
                .bind(i64::from(secret.secret_generation))
                .bind(secret.created_at.to_rfc3339()),
        )
        .await?;
        s::registered(
            secret,
            self.optional(sqlx::query(s::EXACT).bind(secret.secret_sha256.as_hex()))
                .await?,
        )
    }
    async fn resolve_link_secret(
        &self,
        sha256: &ghinvite_core::SecretHash,
    ) -> Result<Option<ghinvite_core::storage::link_secrets::LinkSecret>> {
        self.optional(sqlx::query(ghinvite_core::storage::link_secrets::GET).bind(sha256.as_hex()))
            .await
    }
    async fn retire_link_secrets(
        &self,
        id: InvitationLinkId,
        through_generation: u32,
        at: DateTime<Utc>,
    ) -> Result<()> {
        self.execute(
            sqlx::query(ghinvite_core::storage::link_secrets::RETIRE)
                .bind(id.to_string())
                .bind(i64::from(through_generation))
                .bind(at.to_rfc3339()),
        )
        .await?;
        Ok(())
    }
}

#[async_trait]
impl ghinvite_core::storage::SigninBudgetStorage for SqlxStorage {
    async fn reserve_signin(
        &self,
        key: ghinvite_core::storage::signin_budget::SigninBudgetKey,
        reservation: RequestId,
        now_ms: i64,
    ) -> Result<bool> {
        use ghinvite_core::storage::signin_budget as s;
        s::validate(key, reservation, now_ms)?;
        sqlx::query(s::CLEANUP)
            .bind(now_ms)
            .execute(&self.pool)
            .await
            .map_err(to_db_err)?;
        let row: Option<s::ReservationRow> = self
            .optional(
                sqlx::query(s::RESERVE)
                    .bind(key.as_hex())
                    .bind(reservation.to_string())
                    .bind(now_ms),
            )
            .await?;
        Ok(row.is_some_and(|row| row.started_ms == now_ms))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ghinvite_core::storage::AppInstallationStorage;
    use ghinvite_core::storage::ConflictKind;
    use ghinvite_core::{Account, AccountType, SelectedRepos};

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
            "invitation_link_targets",
            "invitation_requests",
            "deliveries",
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
        s.put_app_installation(&ghinvite_core::account::Installation::repo((acct).clone()))
            .await
            .unwrap();

        let by_id = s
            .app_installation(ghinvite_core::account::App::Repo, 1)
            .await
            .map(|value| value.map(|installation| installation.account))
            .unwrap()
            .unwrap();
        assert_eq!(by_id, acct);

        let by_acc = s
            .active_app_installation(ghinvite_core::account::App::Repo, 100)
            .await
            .map(|value| value.map(|installation| installation.account))
            .unwrap()
            .unwrap();
        assert_eq!(by_acc.installation_id, 1);

        let by_login = s
            .active_app_installation_by_login(ghinvite_core::account::App::Repo, "acme")
            .await
            .map(|value| value.map(|installation| installation.account))
            .unwrap()
            .unwrap();
        assert_eq!(by_login.installation_id, 1);
    }

    #[tokio::test]
    async fn same_installation_replay_preserves_account_binding() {
        let s = SqlxStorage::in_memory().await.unwrap();
        let value = ghinvite_core::account::Installation::repo(sample_account(1, 100, "acme"));
        s.put_app_installation(&value).await.unwrap();
        s.put_app_installation(&value).await.unwrap();
        let mut changed = value;
        changed.account.account_id = 101;
        assert!(matches!(
            s.put_app_installation(&changed).await,
            Err(Error::ProjectionInvariant(_))
        ));
    }

    #[tokio::test]
    async fn duplicate_active_installation_for_same_account_conflict() {
        let s = SqlxStorage::in_memory().await.unwrap();
        s.put_app_installation(&ghinvite_core::account::Installation::repo(
            (sample_account(1, 100, "acme")).clone(),
        ))
        .await
        .unwrap();
        // Different installation_id, same account_id, both active → partial unique index hits.
        let err = s
            .put_app_installation(&ghinvite_core::account::Installation::repo(
                (sample_account(2, 100, "acme")).clone(),
            ))
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
        s.put_app_installation(&ghinvite_core::account::Installation::repo((acct).clone()))
            .await
            .unwrap();
        let mut value = s
            .app_installation(ghinvite_core::account::App::Repo, 1)
            .await
            .unwrap()
            .unwrap();
        value.revision += 1;
        value.account.uninstalled_at = Some(dt("2026-05-05T00:00:00Z"));
        s.put_app_installation(&value).await.unwrap();
        assert!(
            s.active_app_installation(ghinvite_core::account::App::Repo, 100)
                .await
                .map(|value| value.map(|installation| installation.account))
                .unwrap()
                .is_none()
        );
        // Direct lookup still finds it (kept for audit):
        let direct = s
            .app_installation(ghinvite_core::account::App::Repo, 1)
            .await
            .map(|value| value.map(|installation| installation.account))
            .unwrap()
            .unwrap();
        assert!(direct.uninstalled_at.is_some());
    }

    #[tokio::test]
    async fn uninstall_replay_preserves_original_tombstone() {
        let s = SqlxStorage::in_memory().await.unwrap();
        let mut value = ghinvite_core::account::Installation::repo(sample_account(1, 100, "acme"));
        s.put_app_installation(&value).await.unwrap();
        value.revision += 1;
        value.account.uninstalled_at = Some(dt("2026-05-05T00:00:00Z"));
        s.put_app_installation(&value).await.unwrap();
        s.put_app_installation(&value).await.unwrap();
        value.revision += 1;
        value.account.uninstalled_at = Some(dt("2026-05-06T00:00:00Z"));
        assert!(matches!(
            s.put_app_installation(&value).await,
            Err(Error::ProjectionInvariant(_))
        ));
    }

    #[tokio::test]
    async fn update_repos_swaps_subset() {
        let s = SqlxStorage::in_memory().await.unwrap();
        let mut value = ghinvite_core::account::Installation::repo(sample_account(1, 100, "acme"));
        s.put_app_installation(&value).await.unwrap();
        value.revision += 1;
        value.account.selected_repos = SelectedRepos::Subset(vec![1, 2, 3]);
        s.put_app_installation(&value).await.unwrap();
        assert_eq!(
            s.app_installation(ghinvite_core::account::App::Repo, 1)
                .await
                .unwrap(),
            Some(value)
        );
    }

    pub(crate) fn sample_user(user_id: u64, login: &str) -> User {
        User {
            user_id,
            login: login.into(),
            avatar_url: Some(format!("https://example.test/{login}.png")),
            github_created_at: None,
            last_seen_at: dt("2026-05-04T12:00:00Z"),
        }
    }

    pub(crate) fn sample_link(
        account_id: u64,
        installation_id: u64,
        created_by: u64,
    ) -> InvitationLink {
        InvitationLink {
            batch_source: None,
            access_lifetime_seconds: None,
            email_domains: vec![],
            id: InvitationLinkId::new(),
            organization_installation_id: None,
            installation_id,
            account_id,
            created_by,
            created_at: dt("2026-05-04T12:00:00Z"),
            expires_at: None,
            max_uses: Some(5),
            invitation_lifetime_seconds: 604800,
            abuse_threshold_per_hour: None,
            budget_share_pct: 50,
            uses_count: 0,

            approval_required: true,
            description: "Contractor onboarding".into(),
            internal_note: Some("for the contractor".into()),
            revoked_at: None,
            revoked_by: None,
            pause: None,
            targets: ghinvite_core::targets::Targets {
                repos: vec![
                    ghinvite_core::targets::RepoTarget {
                        repo_id: 10,
                        repo_full_name: "acme/api".into(),
                        grant: ghinvite_core::targets::RepoRole::Write,
                    },
                    ghinvite_core::targets::RepoTarget {
                        repo_id: 11,
                        repo_full_name: "acme/web".into(),
                        grant: ghinvite_core::targets::RepoRole::Write,
                    },
                ],
                ..Default::default()
            },

            secret: ghinvite_core::LinkSecretState {
                secret_generation: 1,
                sha256: ghinvite_core::SecretHash::from_bytes([1; 32]),
            },
        }
    }

    /// Seed links and requests through the projector, their production writer.
    pub(crate) async fn seed(
        s: &SqlxStorage,
        link: &InvitationLink,
        requests: &[InvitationRequest],
        revision: u64,
    ) {
        use ghinvite_core::storage::projection::fixture;
        fixture::seed(s, link, requests, revision).await.unwrap();
    }

    #[tokio::test]
    async fn projected_invitation_link_round_trips_with_repos() {
        let s = SqlxStorage::in_memory().await.unwrap();
        s.put_app_installation(&ghinvite_core::account::Installation::repo(
            (sample_account(1, 100, "acme")).clone(),
        ))
        .await
        .unwrap();
        s.upsert_user(&sample_user(7, "octocat")).await.unwrap();

        let link = sample_link(100, 1, 7);
        seed(&s, &link, &[], 1).await;

        let got = s.get_invitation_link_by_id(link.id).await.unwrap().unwrap();
        assert_eq!(got, link);
    }

    #[tokio::test]
    async fn list_invitation_links_for_account_returns_in_descending_created_at() {
        let s = SqlxStorage::in_memory().await.unwrap();
        s.put_app_installation(&ghinvite_core::account::Installation::repo(
            (sample_account(1, 100, "acme")).clone(),
        ))
        .await
        .unwrap();
        s.upsert_user(&sample_user(7, "octocat")).await.unwrap();

        let mut older = sample_link(100, 1, 7);
        older.created_at = dt("2026-05-01T00:00:00Z");

        let mut newer = sample_link(100, 1, 7);
        newer.created_at = dt("2026-05-04T00:00:00Z");

        seed(&s, &older, &[], 1).await;
        seed(&s, &newer, &[], 1).await;

        let list = s.list_invitation_links_for_account(100).await.unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].id, newer.id);
        assert_eq!(list[1].id, older.id);
    }

    pub(crate) fn sample_request(link_id: InvitationLinkId, requester: u64) -> InvitationRequest {
        use ghinvite_core::RequestState;
        InvitationRequest {
            batch: None,
            domain_evidence: None,
            id: RequestId::new(),
            invitation_link_id: Some(link_id),
            requester_id: requester,
            justification: Some("contractor for q2".into()),
            state: RequestState::Pending,
            decided_by: None,
            decided_at: None,
            cancellation_reason: None,
            decline_reason: None,
            requester_message: None,
            created_at: dt("2026-05-04T12:30:00Z"),
            decision_deadline: None,
        }
    }

    #[tokio::test]
    async fn pending_count_and_queue_filter_by_account() {
        let s = SqlxStorage::in_memory().await.unwrap();
        s.put_app_installation(&ghinvite_core::account::Installation::repo(
            (sample_account(1, 100, "acme")).clone(),
        ))
        .await
        .unwrap();
        s.put_app_installation(&ghinvite_core::account::Installation::repo(
            (sample_account(2, 200, "other")).clone(),
        ))
        .await
        .unwrap();
        s.upsert_user(&sample_user(7, "octocat")).await.unwrap();
        s.upsert_user(&sample_user(8, "alice")).await.unwrap();

        let link_a = sample_link(100, 1, 7);
        let link_b = sample_link(200, 2, 7);
        let r_a = sample_request(link_a.id, 8);
        let r_b = sample_request(link_b.id, 8);
        seed(&s, &link_a, std::slice::from_ref(&r_a), 1).await;
        seed(&s, &link_b, std::slice::from_ref(&r_b), 1).await;

        for (account_id, request) in [(100, &r_a), (200, &r_b)] {
            assert_eq!(
                s.count_pending_requests_for_account(account_id)
                    .await
                    .unwrap(),
                1
            );
            let page = s.pending_request_page(account_id, None).await.unwrap();
            assert_eq!(page.rows.len(), 1);
            assert_eq!(page.rows[0].request_id, request.id);
        }
        assert_eq!(s.count_pending_requests_for_account(300).await.unwrap(), 0);
    }

    fn sample_delivery(link: &InvitationLink, request: &InvitationRequest) -> DeliverySnapshot {
        ghinvite_core::delivery::CreateReceipt {
            audit_version: ghinvite_core::delivery_audit::DeliveryAuditVersion::CanonicalV2,
            command: CreateCommand {
                invitation_id: None,
                delivery_generation: 1,
                link_id: Some(link.id),
                batch: None,
                request_id: request.id,
                approval_id: "fixture-approval".into(),
                account_id: link.account_id,
                installation_id: link.installation_id,
                requester_id: request.requester_id,
                repo_id: 10,
                repo_full_name: "acme/api".into(),
                permission: ghinvite_core::Permission::Pull,
                approved_at: dt("2026-05-04T13:00:00Z"),
                budget_attribution: Default::default(),
                access_lifetime_seconds: None,
                budget_share_pct: 50,
            },
            outcome: ghinvite_core::delivery::CreateOutcome::Planned,
            revision: 1,
            confirmed_at: None,
        }
        .into()
    }

    #[tokio::test]
    async fn canonical_delivery_keys_are_identical_for_real_number_bindings() {
        let s = SqlxStorage::in_memory().await.unwrap();
        s.put_app_installation(&ghinvite_core::account::Installation::repo(sample_account(
            1, 100, "acme",
        )))
        .await
        .unwrap();
        s.upsert_user(&sample_user(7, "admin")).await.unwrap();
        s.upsert_user(&sample_user(8, "alice")).await.unwrap();
        let link = sample_link(100, 1, 7);
        let request = sample_request(link.id, 8);
        seed(&s, &link, std::slice::from_ref(&request), 1).await;
        let delivery = sample_delivery(&link, &request);
        let key = delivery.create.command.delivery_key();
        // Real D1 numeric bindings receive INTEGER affinity before the key CHECK.
        sqlx::query("INSERT INTO deliveries(id,request_id,target_id,delivery_generation,created_at,updated_at) VALUES(?1,?2,?3,?4,?5,?5)")
            .bind(&key).bind(request.id.to_string()).bind(10.0f64).bind(1.0f64)
            .bind(delivery.create.command.approved_at).execute(&s.pool).await.unwrap();
        let actual: (String, String, String) = sqlx::query_as(
            "SELECT id,typeof(target_id),typeof(delivery_generation) FROM deliveries",
        )
        .fetch_one(&s.pool)
        .await
        .unwrap();
        assert_eq!(actual, (key.clone(), "integer".into(), "integer".into()));
        assert!(
            s.get_delivery(&key).await.unwrap().is_none(),
            "a read row cannot invent its owner's receipt"
        );
        s.project_delivery(&delivery).await.unwrap();
        assert_eq!(s.get_delivery(&key).await.unwrap(), Some(delivery));
    }

    #[tokio::test]
    async fn canonical_delivery_round_trips_with_upstream_identity() {
        let s = SqlxStorage::in_memory().await.unwrap();
        s.put_app_installation(&ghinvite_core::account::Installation::repo(sample_account(
            1, 100, "acme",
        )))
        .await
        .unwrap();
        s.upsert_user(&sample_user(7, "admin")).await.unwrap();
        s.upsert_user(&sample_user(8, "alice")).await.unwrap();
        let link = sample_link(100, 1, 7);
        let request = sample_request(link.id, 8);
        seed(&s, &link, std::slice::from_ref(&request), 1).await;
        let mut delivery = sample_delivery(&link, &request);
        delivery.create.outcome =
            ghinvite_core::delivery::CreateOutcome::Created { upstream_id: 99999 };
        delivery.create.confirmed_at = Some(delivery.create.command.approved_at);
        s.project_delivery(&delivery).await.unwrap();
        assert_eq!(
            s.get_delivery(&delivery.create.command.delivery_key())
                .await
                .unwrap(),
            Some(delivery)
        );
    }

    #[tokio::test]
    async fn canonical_delivery_requires_its_request_projection() {
        let s = SqlxStorage::in_memory().await.unwrap();
        let link = sample_link(100, 1, 7);
        let request = sample_request(link.id, 8);
        let delivery = sample_delivery(&link, &request);
        let error = s.project_delivery(&delivery).await.unwrap_err();
        assert!(matches!(error, Error::ProjectionDependency), "{error:?}");
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
        s.put_app_installation(&ghinvite_core::account::Installation::repo(
            (sample_account(1, 42, "acme")).clone(),
        ))
        .await
        .unwrap();
        s.upsert_user(&sample_user(7, "requester")).await.unwrap();
        let link = sample_link(42, 1, 7);
        seed(&s, &link, &[], 1).await;
        // A large imported history can share one admission timestamp. The deep
        // cursor must seek past that prefix, not examine it row by row.
        sqlx::query("WITH RECURSIVE n(i) AS (VALUES(1) UNION ALL SELECT i+1 FROM n WHERE i<10000) INSERT INTO invitation_requests(id,invitation_link_id,requester_id,state,created_at,projection_revision) SELECT printf('%026d',i),?,7,'declined','2026-01-01T00:00:00Z',1 FROM n")
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
        s.put_app_installation(&ghinvite_core::account::Installation::repo(
            (sample_account(1, 100, "a")).clone(),
        ))
        .await
        .unwrap();
        s.put_app_installation(&ghinvite_core::account::Installation::repo(
            (sample_account(2, 200, "b")).clone(),
        ))
        .await
        .unwrap();
        let mut value = s
            .app_installation(ghinvite_core::account::App::Repo, 2)
            .await
            .unwrap()
            .unwrap();
        value.revision += 1;
        value.account.uninstalled_at = Some(dt("2026-05-06T00:00:00Z"));
        s.put_app_installation(&value).await.unwrap();

        let active = s
            .list_active_app_installations(ghinvite_core::account::App::Repo)
            .await
            .map(|values| {
                values
                    .into_iter()
                    .map(|value| value.account)
                    .collect::<Vec<_>>()
            })
            .unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].installation_id, 1);
    }
}

#[async_trait]
impl ghinvite_core::storage::OperationalReadStorage for SqlxStorage {
    async fn delivery_signals(
        &self,
        now: DateTime<Utc>,
    ) -> Result<ghinvite_core::storage::operational::DeliverySignals> {
        use ghinvite_core::storage::operational;
        let rows = self.all(sqlx::query(operational::SIGNALS)).await?;
        operational::report(rows, now)
    }
}

#[cfg(test)]
mod operational_tests {
    use super::*;
    use ghinvite_core::storage::{OperationalReadStorage, operational::RepositoryTelemetry};
    use sqlx::Row;

    #[tokio::test]
    async fn telemetry_refuses_other_kinds_and_preserves_every_other_column() {
        let storage = SqlxStorage::in_memory().await.unwrap();
        ghinvite_core::storage::test_suite::scenario_repository_telemetry(storage.clone()).await;
        let row=sqlx::query("SELECT id,request_id,target_id,delivery_generation FROM deliveries WHERE create_outcome='outcome_unknown' ORDER BY id LIMIT 1").fetch_one(&storage.pool).await.unwrap();
        let key: String = row.get("id");
        let value = RepositoryTelemetry {
            request_id: row.get::<String, _>("request_id").parse().unwrap(),
            target_id: row.get::<i64, _>("target_id") as u64,
            delivery_generation: row.get::<i64, _>("delivery_generation") as u64,
            account_id: 100,
            unknown_since: None,
            last_observed_at: Some("2026-05-04T15:00:00Z".parse().unwrap()),
        };
        // Email retains the unprefixed key: this reaches the kind guard itself.
        sqlx::query("UPDATE deliveries SET kind='email' WHERE id=?1")
            .bind(&key)
            .execute(&storage.pool)
            .await
            .unwrap();
        assert!(matches!(
            storage.project_repository_telemetry(&value).await,
            Err(Error::ProjectionInvariant(_))
        ));
        let observed: Option<String> =
            sqlx::query_scalar("SELECT last_observed_at FROM deliveries WHERE id=?1")
                .bind(&key)
                .fetch_one(&storage.pool)
                .await
                .unwrap();
        assert!(observed.is_none());
        assert_eq!(
            storage
                .delivery_signals("2026-05-04T15:00:00Z".parse().unwrap())
                .await
                .unwrap()
                .counts
                .repository_deliveries,
            13
        );
        sqlx::query("UPDATE deliveries SET kind='repo' WHERE id=?1")
            .bind(&key)
            .execute(&storage.pool)
            .await
            .unwrap();
        let before: Vec<serde_json::Value> = sqlx::query("SELECT * FROM deliveries WHERE id=?1")
            .bind(&key)
            .fetch_all(&storage.pool)
            .await
            .unwrap()
            .iter()
            .map(|r| decode(r).unwrap())
            .collect();
        storage.project_repository_telemetry(&value).await.unwrap();
        let mut after: Vec<serde_json::Value> = sqlx::query("SELECT * FROM deliveries WHERE id=?1")
            .bind(&key)
            .fetch_all(&storage.pool)
            .await
            .unwrap()
            .iter()
            .map(|r| decode(r).unwrap())
            .collect();
        after[0]["last_observed_at"] = before[0]["last_observed_at"].clone();
        assert_eq!(after, before);
    }
}
