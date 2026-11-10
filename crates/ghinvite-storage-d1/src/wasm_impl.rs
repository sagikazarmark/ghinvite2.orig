//! D1Storage: implements the full Storage trait using worker::D1Database.
//! Statements and row mapping are core's; this adapter binds values and runs them.

#[path = "access_grants.rs"]
mod access_grants;
#[path = "blocked_digest.rs"]
mod blocked_digest;
#[path = "bulk.rs"]
mod bulk;
#[path = "invite_budget.rs"]
mod invite_budget;
#[path = "org_delivery.rs"]
mod org_delivery;
#[path = "org_webhooks.rs"]
mod org_webhooks;
#[path = "pending_signin.rs"]
mod pending_signin;

#[cfg(not(target_arch = "wasm32"))]
use crate::native::{self as worker, JsValue};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use ghinvite_core::audit::{AuditEvent, EventType};
use ghinvite_core::delivery::{CreateCommand, DeliverySnapshot};
use ghinvite_core::storage::AppInstallationReadStorage;
use ghinvite_core::storage::attempt_continuations::StoredContinuation;
use ghinvite_core::storage::projection::{ProjectionEnvelope, ProjectionStorage};
use ghinvite_core::storage::{
    AuditPage, AuditPosition, AuditStorage, ConsoleStorage, ContinuationStorage, DeliveryStorage,
    Error, RecordStorage, Result, attempt_continuations, audit_read, audit_write, classify_insert,
    decode_row, delivery_attempts, delivery_projection, installations, invitation_links,
    invitation_requests, pending_queue, projection, request_history, users,
};
use ghinvite_core::{InvitationLink, InvitationLinkId, InvitationRequest, RequestId, User};
use serde::de::{DeserializeOwned, IgnoredAny};
use std::future::Future;
#[cfg(target_arch = "wasm32")]
use std::pin::Pin;
#[cfg(target_arch = "wasm32")]
use std::task::{Context, Poll};
#[cfg(target_arch = "wasm32")]
use wasm_bindgen::JsValue;
use worker::{D1Database, D1PreparedStatement};

/// Wraps a `Future` and unsafely implements `Send`.
///
/// SAFETY: Cloudflare Workers (wasm32) are single-threaded: there is no
/// preemptive threading and `wasm32-unknown-unknown` has no `std::thread::spawn`.
/// No value ever crosses a thread boundary, so implementing `Send` here is sound.
#[cfg(target_arch = "wasm32")]
struct WasmSend<F>(F);
#[cfg(target_arch = "wasm32")]
unsafe impl<F> Send for WasmSend<F> {}
#[cfg(target_arch = "wasm32")]
impl<F: Future> Future for WasmSend<F> {
    type Output = F::Output;
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        // SAFETY: we only project to the inner field, which was already pinned.
        unsafe { self.map_unchecked_mut(|s| &mut s.0).poll(cx) }
    }
}

/// Wrap an async block so that the resulting future is `Send`.
/// Used to satisfy the `async_trait` `+ Send` requirement on wasm32.
#[cfg(target_arch = "wasm32")]
fn wasm_send<F: Future>(f: F) -> WasmSend<F> {
    WasmSend(f)
}

#[cfg(not(target_arch = "wasm32"))]
fn wasm_send<F: Future>(f: F) -> F {
    f
}

/// Map a `worker::Error` to an unclassified, retryable database error, as the
/// SQLx adapter reports every failure it does not classify as a conflict.
fn database(e: worker::Error) -> Error {
    Error::Database(e.to_string())
}

/// A row this call just wrote could not be read back.
fn unreadable_write(what: &str) -> Error {
    Error::Database(format!("{what} missing after write"))
}

/// D1 and native adapters share one checked integer contract.
fn int(value: u64) -> Result<JsValue> {
    Ok(JsValue::from_f64(
        ghinvite_core::storage::integer::unsigned(value)? as f64,
    ))
}
fn signed_int(value: i64) -> Result<JsValue> {
    Ok(JsValue::from_f64(
        ghinvite_core::storage::integer::signed(value)? as f64,
    ))
}

fn text(value: &str) -> JsValue {
    JsValue::from_str(value)
}

fn time(value: DateTime<Utc>) -> JsValue {
    text(&value.to_rfc3339())
}

fn nullable(value: Option<JsValue>) -> JsValue {
    value.unwrap_or(JsValue::NULL)
}

pub struct D1Storage {
    pub db: D1Database,
}

impl D1Storage {
    pub fn new(db: D1Database) -> Self {
        Self { db }
    }

    fn prepare(&self, sql: &str, values: &[JsValue]) -> Result<D1PreparedStatement> {
        self.db.prepare(sql).bind(values).map_err(database)
    }

    // Rows are read as plain JSON values and decoded afterwards: worker panics
    // when `results::<T>()` cannot deserialize a row, so corruption must not
    // reach its deserializer.
    async fn all<T: DeserializeOwned>(&self, sql: &str, values: &[JsValue]) -> Result<Vec<T>> {
        let result = self.prepare(sql, values)?.all().await.map_err(database)?;
        let rows = result.results::<serde_json::Value>().map_err(database)?;
        rows.into_iter().map(decode_row).collect()
    }

    async fn first<T: DeserializeOwned>(&self, sql: &str, values: &[JsValue]) -> Result<Option<T>> {
        let statement = self.prepare(sql, values)?;
        let row = statement.first::<serde_json::Value>(None).await;
        row.map_err(database)?.map(decode_row).transpose()
    }

    /// Rows changed.
    async fn run(&self, sql: &str, values: &[JsValue]) -> Result<usize> {
        let result = self.prepare(sql, values)?.run().await.map_err(database)?;
        let meta = result.meta().map_err(database)?;
        Ok(meta.and_then(|m| m.changes).unwrap_or(0))
    }

    /// Run `statements` as one D1 batch, each bound to the same `input`.
    async fn batch<S: AsRef<str>>(
        &self,
        statements: &[S],
        input: &str,
        classify: fn(String) -> Error,
    ) -> Result<()> {
        let statements = statements
            .iter()
            .map(|sql| self.prepare(sql.as_ref(), &[text(input)]))
            .collect::<Result<Vec<_>>>()?;
        self.db
            .batch(statements)
            .await
            .map_err(|e| classify(e.to_string()))?;
        Ok(())
    }
}

// SAFETY: D1Database holds a JS object reference (JsValue) which is !Send + !Sync upstream.
// This is sound only under the single-threaded Cloudflare Workers execution model where
// no OS threads exist and wasm32 has no thread-spawn capability. If SharedArrayBuffer-based
// threads are ever added to the build, these impls must be revisited.
#[cfg(target_arch = "wasm32")]
unsafe impl Send for D1Storage {}
#[cfg(target_arch = "wasm32")]
unsafe impl Sync for D1Storage {}

#[async_trait]
impl ProjectionStorage for D1Storage {
    async fn apply_reissue(
        &self,
        envelope: &ghinvite_core::reissue_projection::RequestReissueProjection,
    ) -> Result<()> {
        let input = projection::reissue_sql::encode(envelope)?;
        wasm_send(self.batch(
            &projection::reissue_sql::statements(),
            &input,
            projection::sql::classify,
        ))
        .await
    }

    async fn apply_revocation(
        &self,
        envelope: &ghinvite_core::revocation::RevocationProjection,
    ) -> Result<()> {
        let input = projection::revocation_sql::encode(envelope)?;
        wasm_send(self.batch(
            projection::revocation_sql::STATEMENTS,
            &input,
            projection::sql::classify,
        ))
        .await
    }

    async fn apply_request(&self, envelope: &projection::RequestProjectionEnvelope) -> Result<()> {
        let input = projection::request_sql::encode(envelope)?;
        wasm_send(self.batch(
            &projection::request_sql::statements(envelope),
            &input,
            projection::sql::classify,
        ))
        .await
    }
    async fn apply_transition(&self, envelope: &ProjectionEnvelope) -> Result<()> {
        let input = projection::sql::encode(envelope)?;
        wasm_send(self.batch(
            projection::sql::STATEMENTS,
            &input,
            projection::sql::classify,
        ))
        .await
    }
}

#[async_trait]
impl RecordStorage for D1Storage {
    async fn upsert_user(&self, user: &User) -> Result<()> {
        wasm_send(async {
            let values = [
                int(user.user_id)?,
                text(&user.login),
                nullable(user.avatar_url.as_deref().map(text)),
                time(user.last_seen_at),
                nullable(user.github_created_at.map(time)),
            ];
            self.run(users::UPSERT, &values).await?;
            Ok(())
        })
        .await
    }

    async fn get_user(&self, user_id: u64) -> Result<Option<User>> {
        wasm_send(async { self.first(users::GET, &[int(user_id)?]).await }).await
    }

    async fn get_invitation_link_by_id(
        &self,
        id: InvitationLinkId,
    ) -> Result<Option<InvitationLink>> {
        wasm_send(async {
            let values = [text(&id.to_string())];
            let rows = self.all(invitation_links::GET, &values).await?;
            Ok(invitation_links::fold(rows)?.pop())
        })
        .await
    }

    async fn get_invitation_request(&self, id: RequestId) -> Result<Option<InvitationRequest>> {
        wasm_send(async {
            let values = [text(&id.to_string())];
            self.first::<invitation_requests::Row>(invitation_requests::GET, &values)
                .await?
                .map(invitation_requests::Row::into_request)
                .transpose()
        })
        .await
    }
}

#[async_trait]
impl ConsoleStorage for D1Storage {
    async fn revoke_preview(
        &self,
        account_id: u64,
        link_id: ghinvite_core::InvitationLinkId,
    ) -> Result<Option<ghinvite_core::revocation::RevokePreview>> {
        wasm_send(async {
            self.first(
                projection::revocation_sql::PREVIEW,
                &[text(&link_id.to_string()), int(account_id)?],
            )
            .await
        })
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
        wasm_send(async {
            invitation_links::fold(
                self.all(
                    &cs::links_query(),
                    &[int(scope.account_id())?, text(&repos)],
                )
                .await?,
            )
        })
        .await
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
        wasm_send(async {
            let row: Option<pending_queue::CountRow> = self
                .first(
                    &cs::count_query(),
                    &[int(scope.account_id())?, text(&repos)],
                )
                .await?;
            row.map(|r| r.pending)
                .ok_or_else(|| Error::Corrupt("pending count returned no row".into()))
        })
        .await
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
        wasm_send(async {
            pending_queue::PendingPage::from_json(
                self.all(
                    &cs::pending_query(after.is_some()),
                    &[
                        int(scope.account_id())?,
                        nullable(after.map(|b| text(&b.seek_key()))),
                        text(&repos),
                    ],
                )
                .await?,
            )
        })
        .await
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
        wasm_send(async {
            let account = int(scope.account_id())?;
            let values = |position: AuditPosition| {
                let boundary = position.boundary();
                [
                    account.clone(),
                    nullable(event.map(|e| text(e.as_str()))),
                    nullable(boundary.map(|b| text(&audit_read::boundary_time(b)))),
                    nullable(boundary.map(|b| text(&b.id.to_string()))),
                    text(&repos),
                ]
            };
            let mut page = AuditPage::from_rows(
                self.all(&cs::audit_query(event, position, false), &values(position))
                    .await?,
                position,
            )?;
            if let Some([newer, older]) = page.probes() {
                for (seek, flag) in [(newer, &mut page.has_newer), (older, &mut page.has_older)] {
                    let row: Option<IgnoredAny> = self
                        .first(&cs::audit_query(event, seek, true), &values(seek))
                        .await?;
                    *flag = row.is_some();
                }
            }
            Ok(page)
        })
        .await
    }
    async fn distinct_console_repo_targets(
        &self,
        account_id: u64,
        after_repo_id: Option<u64>,
        limit: u16,
    ) -> Result<ghinvite_core::storage::console_scope::RepoTargetPage> {
        use ghinvite_core::storage::console_scope as cs;
        let (account, after, count) = cs::target_query_args(account_id, after_repo_id, limit)?;
        wasm_send(async {
            cs::target_page(
                self.all(cs::REPO_TARGETS, &[int(account)?, int(after)?, int(count)?])
                    .await?,
                limit,
            )
        })
        .await
    }

    async fn invitation_link_belongs_to_account(
        &self,
        account_id: u64,
        id: InvitationLinkId,
    ) -> Result<bool> {
        wasm_send(async {
            let values = [text(&id.to_string()), int(account_id)?];
            let row: Option<IgnoredAny> = self
                .first(invitation_links::BELONGS_TO_ACCOUNT, &values)
                .await?;
            Ok(row.is_some())
        })
        .await
    }

    async fn list_invitation_links_for_account(
        &self,
        account_id: u64,
    ) -> Result<Vec<InvitationLink>> {
        wasm_send(async {
            let values = [int(account_id)?];
            invitation_links::fold(self.all(invitation_links::FOR_ACCOUNT, &values).await?)
        })
        .await
    }

    async fn request_history(
        &self,
        account_id: u64,
        link_id: InvitationLinkId,
        before: Option<request_history::Boundary>,
    ) -> Result<request_history::Page> {
        wasm_send(async {
            let values = [
                int(account_id)?,
                text(&link_id.to_string()),
                nullable(before.map(|b| text(&request_history::boundary_key(b)))),
            ];
            let sql = request_history::query(before);
            request_history::page(self.all(&sql, &values).await?)
        })
        .await
    }

    async fn count_pending_requests_for_account(&self, account_id: u64) -> Result<u64> {
        wasm_send(async {
            let values = [int(account_id)?];
            let row: Option<pending_queue::CountRow> =
                self.first(pending_queue::COUNT_QUERY, &values).await?;
            row.map(|r| r.pending)
                .ok_or_else(|| Error::Corrupt("pending count returned no row".into()))
        })
        .await
    }

    async fn pending_request_page(
        &self,
        account_id: u64,
        after: Option<pending_queue::PendingBoundary>,
    ) -> Result<pending_queue::PendingPage> {
        wasm_send(async {
            let values = [
                int(account_id)?,
                nullable(after.map(|b| text(&b.seek_key()))),
            ];
            let sql = pending_queue::query(after.is_some());
            pending_queue::PendingPage::from_json(self.all(&sql, &values).await?)
        })
        .await
    }

    async fn get_delivery(&self, key: &str) -> Result<Option<DeliverySnapshot>> {
        wasm_send(async {
            self.first::<delivery_projection::ReceiptRow>(delivery_projection::GET, &[text(key)])
                .await?
                .map(|row| row.decode())
                .transpose()
        })
        .await
    }

    async fn list_delivery_for_request(&self, id: RequestId) -> Result<Vec<DeliverySnapshot>> {
        wasm_send(async {
            let values = [text(&id.to_string())];
            let rows: Vec<delivery_projection::ReceiptRow> =
                self.all(delivery_projection::FOR_REQUEST, &values).await?;
            rows.into_iter().map(|row| row.decode()).collect()
        })
        .await
    }

    async fn list_audit_events(
        &self,
        account_id: u64,
        event: Option<EventType>,
        position: AuditPosition,
    ) -> Result<AuditPage> {
        wasm_send(async {
            let account = int(account_id)?;
            let values = |position: AuditPosition| {
                let boundary = position.boundary();
                [
                    account.clone(),
                    nullable(event.map(|e| text(e.as_str()))),
                    nullable(boundary.map(|b| text(&audit_read::boundary_time(b)))),
                    nullable(boundary.map(|b| text(&b.id.to_string()))),
                ]
            };
            let sql = audit_read::query(event, position, false);
            let rows = self.all(&sql, &values(position)).await?;
            let mut page = AuditPage::from_rows(rows, position)?;
            if let Some([newer, older]) = page.probes() {
                for (seek, flag) in [(newer, &mut page.has_newer), (older, &mut page.has_older)] {
                    let sql = audit_read::query(event, seek, true);
                    let row: Option<IgnoredAny> = self.first(&sql, &values(seek)).await?;
                    *flag = row.is_some();
                }
            }
            Ok(page)
        })
        .await
    }
}

#[async_trait]
impl ContinuationStorage for D1Storage {
    async fn continuation_rotation_page(
        &self,
        after: Option<i64>,
    ) -> Result<Vec<ghinvite_core::storage::continuation_rotation::ScanRow>> {
        use ghinvite_core::storage::continuation_rotation as sql;
        wasm_send(async {
            let session = self
                .db
                .with_session_constraint(worker::D1SessionConstraint::FirstPrimary)
                .map_err(database)?;
            let values = [after.map(signed_int).transpose()?.unwrap_or(JsValue::NULL)];
            let result = session
                .prepare(sql::PAGE)
                .bind(&values)
                .map_err(database)?
                .all()
                .await
                .map_err(database)?;
            let rows = result
                .results::<serde_json::Value>()
                .map_err(database)?
                .into_iter()
                .map(decode_row)
                .collect::<Result<Vec<sql::ScanRow>>>()?;
            if rows.is_empty() {
                return Err(Error::Corrupt("continuation rotation epoch missing".into()));
            }
            for row in &rows {
                row.validate()?;
            }
            Ok(rows)
        })
        .await
    }
    async fn continuation_rotation_epoch(&self) -> Result<i64> {
        use ghinvite_core::storage::{continuation_rotation as sql, integer};
        wasm_send(async {
            let session = self
                .db
                .with_session_constraint(worker::D1SessionConstraint::FirstPrimary)
                .map_err(database)?;
            let value = session
                .prepare(sql::EPOCH)
                .first::<serde_json::Value>(None)
                .await
                .map_err(database)?
                .ok_or_else(|| Error::Corrupt("continuation rotation epoch missing".into()))?;
            let row: sql::Epoch = decode_row(value)?;
            integer::unsigned(
                u64::try_from(row.revision)
                    .map_err(|_| Error::Corrupt("continuation rotation epoch".into()))?,
            )?;
            Ok(row.revision)
        })
        .await
    }
    async fn rewrap_attempt_continuation(
        &self,
        old: &ghinvite_core::storage::continuation_rotation::ScanRow,
        replacement: &str,
        expected_epoch: i64,
    ) -> Result<bool> {
        use ghinvite_core::storage::continuation_rotation as sql;
        old.validate()?;
        if old.row_id.is_none()
            || old.oversized != 0
            || replacement.len() > sql::MAX_CIPHERTEXT_BYTES
        {
            return Err(Error::Corrupt("continuation rotation CAS input".into()));
        }
        wasm_send(async {
            let returned: Vec<sql::CasReceipt> = self
                .all(
                    sql::CAS,
                    &[
                        text(replacement),
                        signed_int(old.row_id.unwrap())?,
                        text(old.scope.as_deref().unwrap()),
                        text(old.id.as_deref().unwrap()),
                        text(old.binding.as_deref().unwrap()),
                        text(old.payload.as_deref().unwrap()),
                        signed_int(old.expires_at.unwrap())?,
                        signed_int(expected_epoch)?,
                    ],
                )
                .await?;
            sql::CasReceipt::applied(&returned, old.row_id.unwrap())
        })
        .await
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
        wasm_send(async {
            let now = signed_int(now)?;
            self.run(CLEANUP, std::slice::from_ref(&now)).await?;
            let values = [
                text(scope),
                text(id),
                text(binding),
                text(payload),
                signed_int(expires_at)?,
            ];
            self.run(INSERT, &values).await?;
            let values = [text(scope), text(binding), text(id), now];
            self.first(BY_BINDING, &values)
                .await?
                .ok_or_else(|| unreadable_write("attempt continuation"))
        })
        .await
    }

    async fn get_attempt_continuation(
        &self,
        scope: &str,
        id: &str,
        now: i64,
    ) -> Result<Option<StoredContinuation>> {
        wasm_send(async {
            let values = [text(scope), text(id), signed_int(now)?];
            self.first(attempt_continuations::GET, &values).await
        })
        .await
    }

    async fn list_attempt_continuations(
        &self,
        scope: &str,
        now: i64,
    ) -> Result<Vec<StoredContinuation>> {
        wasm_send(async {
            let values = [text(scope), signed_int(now)?];
            self.all(attempt_continuations::LIST, &values).await
        })
        .await
    }

    async fn release_attempt_continuation(&self, scope: &str, id: &str) -> Result<()> {
        wasm_send(async {
            let values = [text(scope), text(id)];
            self.run(attempt_continuations::RELEASE, &values).await?;
            Ok(())
        })
        .await
    }
}

#[async_trait]
impl DeliveryStorage for D1Storage {
    async fn cancellation_attempt_exists(
        &self,
        command: &delivery_attempts::CancelCommand,
    ) -> Result<bool> {
        let encoded = delivery_attempts::encode_cancel(command)?;
        wasm_send(async {
            let row = self
                .first(
                    delivery_attempts::CANCEL_FENCED,
                    &[text(&command.create.delivery_key())],
                )
                .await?;
            delivery_attempts::fenced(row, &encoded)
        })
        .await
    }

    async fn claim_cancellation_attempt(
        &self,
        command: &delivery_attempts::CancelCommand,
    ) -> Result<Option<u64>> {
        let encoded = delivery_attempts::encode_cancel(command)?;
        wasm_send(async {
            let key = text(&command.create.delivery_key());
            let generation = self
                .first(
                    delivery_attempts::CANCEL_CLAIM,
                    &[key.clone(), text(&encoded)],
                )
                .await?;
            let retained = self
                .first(delivery_attempts::CANCEL_COMMAND, &[key, text(&encoded)])
                .await?
                .ok_or_else(|| unreadable_write("cancellation attempt"))?;
            delivery_attempts::claimed(generation, retained, &encoded)
        })
        .await
    }
    async fn reject_cancellation_attempt(
        &self,
        command: &delivery_attempts::CancelCommand,
        generation: u64,
    ) -> Result<()> {
        let encoded = delivery_attempts::encode_cancel(command)?;
        wasm_send(async {
            self.run(
                delivery_attempts::CANCEL_REJECT,
                &[
                    text(&command.create.delivery_key()),
                    text(&encoded),
                    int(generation)?,
                ],
            )
            .await?;
            Ok(())
        })
        .await
    }

    async fn claim_delivery_attempt(&self, command: &CreateCommand) -> Result<Option<u64>> {
        let encoded = delivery_attempts::encode(command)?;
        wasm_send(async {
            let key = text(&command.delivery_key());
            let generation = self
                .first(delivery_attempts::CLAIM, &[key.clone(), text(&encoded)])
                .await?;
            let retained = self
                .first(delivery_attempts::COMMAND, &[key, text(&encoded)])
                .await?
                .ok_or_else(|| unreadable_write("delivery attempt"))?;
            delivery_attempts::claimed(generation, retained, &encoded)
        })
        .await
    }
    async fn reject_delivery_attempt(
        &self,
        command: &CreateCommand,
        generation: u64,
    ) -> Result<()> {
        let encoded = delivery_attempts::encode(command)?;
        wasm_send(async {
            self.run(
                delivery_attempts::REJECT,
                &[
                    text(&command.delivery_key()),
                    text(&encoded),
                    int(generation)?,
                ],
            )
            .await?;
            Ok(())
        })
        .await
    }
    async fn delivery_attempt_exists(&self, command: &CreateCommand) -> Result<bool> {
        let encoded = delivery_attempts::encode(command)?;
        wasm_send(async {
            let row = self
                .first(delivery_attempts::FENCED, &[text(&command.delivery_key())])
                .await?;
            delivery_attempts::fenced(row, &encoded)
        })
        .await
    }

    async fn project_delivery_with_events(
        &self,
        projection: &ghinvite_core::delivery_audit::RepositoryDeliveryProjection,
    ) -> Result<()> {
        let encoded = delivery_projection::encode_with_events(projection)?;
        let statements = delivery_projection::statements();
        wasm_send(self.batch(&statements, &encoded, delivery_projection::classify)).await
    }

    async fn project_repository_telemetry(
        &self,
        telemetry: &ghinvite_core::storage::operational::RepositoryTelemetry,
    ) -> Result<()> {
        use ghinvite_core::storage::operational;
        let encoded = operational::encode(telemetry)?;
        let statements = operational::statements();
        wasm_send(self.batch(&statements, &encoded, delivery_projection::classify)).await
    }
}

#[async_trait]
impl AuditStorage for D1Storage {
    async fn audit(&self, event: &AuditEvent) -> Result<()> {
        ghinvite_core::storage::integer::validate(event)?;
        wasm_send(async {
            let values = [
                text(&event.id.to_string()),
                int(event.account_id)?,
                time(event.occurred_at),
                text(event.event_type.as_str()),
                text(&event.actor_kind.to_string()),
                nullable(event.actor_id.map(int).transpose()?),
                text(&event.target_kind.to_string()),
                text(&event.target_id),
                nullable(audit_write::metadata(event).as_deref().map(text)),
                nullable(event.request_id.as_deref().map(text)),
            ];
            self.run(&audit_write::insert(event.event_type), &values)
                .await?;
            Ok(())
        })
        .await
    }
}

#[async_trait]
impl ghinvite_core::storage::AppInstallationReadStorage for D1Storage {
    async fn app_installation(
        &self,
        app: ghinvite_core::account::App,
        id: u64,
    ) -> Result<Option<ghinvite_core::account::Installation>> {
        wasm_send(async {
            self.first::<installations::AppRow>(
                installations::GET_APP,
                &[text(app.as_str()), int(id)?],
            )
            .await?
            .map(installations::AppRow::decode)
            .transpose()
        })
        .await
    }
    async fn active_app_installation(
        &self,
        app: ghinvite_core::account::App,
        id: u64,
    ) -> Result<Option<ghinvite_core::account::Installation>> {
        wasm_send(async {
            self.first::<installations::AppRow>(
                installations::ACTIVE_APP,
                &[text(app.as_str()), int(id)?],
            )
            .await?
            .map(installations::AppRow::decode)
            .transpose()
        })
        .await
    }
    async fn account_settings(
        &self,
        id: u64,
    ) -> Result<Option<ghinvite_core::account::AccountSettings>> {
        wasm_send(async {
            self.first::<installations::SettingsRow>(installations::GET_SETTINGS, &[int(id)?])
                .await?
                .map(installations::SettingsRow::decode)
                .transpose()
        })
        .await
    }
    async fn active_app_installation_by_login(
        &self,
        app: ghinvite_core::account::App,
        login: &str,
    ) -> Result<Option<ghinvite_core::account::Installation>> {
        wasm_send(async {
            self.first::<installations::AppRow>(
                installations::ACTIVE_APP_LOGIN,
                &[text(app.as_str()), text(login)],
            )
            .await?
            .map(installations::AppRow::decode)
            .transpose()
        })
        .await
    }
    async fn latest_app_installation_by_login(
        &self,
        app: ghinvite_core::account::App,
        login: &str,
    ) -> Result<Option<ghinvite_core::account::Installation>> {
        wasm_send(async {
            self.first::<installations::AppRow>(
                installations::LATEST_APP_LOGIN,
                &[text(app.as_str()), text(login)],
            )
            .await?
            .map(installations::AppRow::decode)
            .transpose()
        })
        .await
    }
    async fn list_active_app_installations(
        &self,
        app: ghinvite_core::account::App,
    ) -> Result<Vec<ghinvite_core::account::Installation>> {
        wasm_send(async {
            self.all::<installations::AppRow>(installations::LIST_ACTIVE_APP, &[text(app.as_str())])
                .await?
                .into_iter()
                .map(installations::AppRow::decode)
                .collect()
        })
        .await
    }
}
#[async_trait]
impl ghinvite_core::storage::AppInstallationStorage for D1Storage {
    async fn project_installation_change(
        &self,
        owner: ghinvite_core::account::AccountInstallationRef,
        change: &ghinvite_core::installation_audit::InstallationChange,
    ) -> Result<()> {
        use ghinvite_core::storage::installation_projection as sql;
        let input = sql::encode(owner, change)?;
        wasm_send(async {
            self.batch(
                &sql::statements(),
                &input,
                ghinvite_core::storage::delivery_projection::classify,
            )
            .await
        })
        .await
    }

    async fn put_app_installation(
        &self,
        value: &ghinvite_core::account::Installation,
    ) -> Result<()> {
        let encoded = installations::encode_app(value)?;
        wasm_send(async {
            let a = &value.account;
            self.run(
                installations::PUT_APP,
                &[
                    text(value.app.as_str()),
                    int(a.installation_id)?,
                    int(a.account_id)?,
                    text(&a.account_login),
                    text(&a.account_type.to_string()),
                    time(a.installed_at),
                    nullable(a.uninstalled_at.map(time)),
                    text(&installations::encode_selected_repos(&a.selected_repos)),
                    nullable(value.suspended_at.map(time)),
                    text(&serde_json::to_string(&value.permissions).expect("typed permissions")),
                    nullable(value.installed_by_user_id.map(int).transpose()?),
                    nullable(value.installed_by_login.as_deref().map(text)),
                    int(value.revision)?,
                    text(&encoded),
                ],
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
        })
        .await
    }
    async fn put_account_settings(
        &self,
        value: &ghinvite_core::account::AccountSettings,
    ) -> Result<()> {
        let encoded = installations::encode_settings(value)?;
        wasm_send(async {
            self.run(
                installations::PUT_SETTINGS,
                &[
                    int(value.account_id)?,
                    int(u64::from(value.repo_admin_delegation))?,
                    int(value.changed_by)?,
                    time(value.changed_at),
                    int(value.revision)?,
                    text(&encoded),
                ],
            )
            .await?;
            installations::applied_settings(value, self.account_settings(value.account_id).await?)
        })
        .await
    }
}

#[async_trait]
impl ghinvite_core::storage::LinkCapabilityStorage for D1Storage {
    async fn link_targets(&self, id: InvitationLinkId) -> Result<ghinvite_core::targets::Targets> {
        use ghinvite_core::storage::link_targets as t;
        wasm_send(async { t::decode(self.all(t::GET, &[text(&id.to_string())]).await?) }).await
    }
    async fn register_link_secret(
        &self,
        secret: &ghinvite_core::storage::link_secrets::LinkSecret,
    ) -> Result<()> {
        use ghinvite_core::storage::link_secrets as s;
        secret.validate()?;
        wasm_send(async {
            self.run(
                s::REGISTER,
                &[
                    text(&secret.secret_sha256.as_hex()),
                    text(&secret.link_id.to_string()),
                    int(u64::from(secret.secret_generation))?,
                    time(secret.created_at),
                ],
            )
            .await?;
            s::registered(
                secret,
                self.first(s::EXACT, &[text(&secret.secret_sha256.as_hex())])
                    .await?,
            )
        })
        .await
    }
    async fn resolve_link_secret(
        &self,
        sha256: &ghinvite_core::SecretHash,
    ) -> Result<Option<ghinvite_core::storage::link_secrets::LinkSecret>> {
        wasm_send(self.first(
            ghinvite_core::storage::link_secrets::GET,
            &[text(&sha256.as_hex())],
        ))
        .await
    }
    async fn retire_link_secrets(
        &self,
        id: InvitationLinkId,
        through_generation: u32,
        at: DateTime<Utc>,
    ) -> Result<()> {
        wasm_send(async {
            self.run(
                ghinvite_core::storage::link_secrets::RETIRE,
                &[
                    text(&id.to_string()),
                    int(u64::from(through_generation))?,
                    time(at),
                ],
            )
            .await?;
            Ok(())
        })
        .await
    }
}

#[async_trait]
impl ghinvite_core::storage::SigninBudgetStorage for D1Storage {
    async fn reserve_signin(
        &self,
        key: ghinvite_core::storage::signin_budget::SigninBudgetKey,
        reservation: RequestId,
        now_ms: i64,
    ) -> Result<bool> {
        use ghinvite_core::storage::signin_budget as s;
        s::validate(key, reservation, now_ms)?;
        wasm_send(async {
            self.run(s::CLEANUP, &[signed_int(now_ms)?]).await?;
            let row: Option<s::ReservationRow> = self
                .first(
                    s::RESERVE,
                    &[
                        text(&key.as_hex()),
                        text(&reservation.to_string()),
                        signed_int(now_ms)?,
                    ],
                )
                .await?;
            Ok(row.is_some_and(|row| row.started_ms == now_ms))
        })
        .await
    }
}

#[async_trait]
impl ghinvite_core::storage::OperationalReadStorage for D1Storage {
    async fn delivery_signals(
        &self,
        now: DateTime<Utc>,
    ) -> Result<ghinvite_core::storage::operational::DeliverySignals> {
        use ghinvite_core::storage::operational;
        wasm_send(async {
            let rows = self.all(operational::SIGNALS, &[]).await?;
            operational::report(rows, now)
        })
        .await
    }
}

#[path = "email_review.rs"]
mod email_review;
#[path = "email_vault.rs"]
mod email_vault;

#[path = "email_delivery.rs"]
mod email_delivery;
