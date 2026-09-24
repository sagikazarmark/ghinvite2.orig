//! D1Storage: implements the full Storage trait using worker::D1Database.
//! Statements and row mapping are core's; this adapter binds values and runs them.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use ghinvite_core::audit::{AuditEvent, EventType};
use ghinvite_core::delivery::{CreateCommand, CreateReceipt};
use ghinvite_core::storage::attempt_continuations::StoredContinuation;
use ghinvite_core::storage::projection::{ProjectionEnvelope, ProjectionStorage};
use ghinvite_core::storage::{
    AuditPage, AuditPosition, AuditStorage, ConsoleStorage, ContinuationStorage, DeliveryStorage,
    Error, InstallationStorage, RecordStorage, Result, WebhookStorage, attempt_continuations,
    audit_read, audit_write, classify_insert, decode_row, delivery_attempts, delivery_projection,
    github_invitations, installations, invitation_links, invitation_requests, pending_queue,
    projection, request_history, settlement, users,
};
use ghinvite_core::{
    Account, GithubInvitation, GithubInvitationId, InvitationLink, InvitationLinkId,
    InvitationRequest, RequestId, SelectedRepos, User,
};
use serde::de::{DeserializeOwned, IgnoredAny};
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use wasm_bindgen::JsValue;
use worker::{D1Database, D1PreparedStatement};

/// Wraps a `Future` and unsafely implements `Send`.
///
/// SAFETY: Cloudflare Workers (wasm32) are single-threaded: there is no
/// preemptive threading and `wasm32-unknown-unknown` has no `std::thread::spawn`.
/// No value ever crosses a thread boundary, so implementing `Send` here is sound.
struct WasmSend<F>(F);
unsafe impl<F> Send for WasmSend<F> {}
impl<F: Future> Future for WasmSend<F> {
    type Output = F::Output;
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        // SAFETY: we only project to the inner field, which was already pinned.
        unsafe { self.map_unchecked_mut(|s| &mut s.0).poll(cx) }
    }
}

/// Wrap an async block so that the resulting future is `Send`.
/// Used to satisfy the `async_trait` `+ Send` requirement on wasm32.
fn wasm_send<F: Future>(f: F) -> WasmSend<F> {
    WasmSend(f)
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

/// Largest integer a JavaScript number binds exactly.
const MAX_EXACT_INTEGER: u64 = (1 << 53) - 1;

/// D1 binds numbers as JavaScript doubles: refuse an integer that would be
/// stored rounded rather than silently writing a different value.
fn int(value: u64) -> Result<JsValue> {
    if value > MAX_EXACT_INTEGER {
        return Err(Error::Database(format!(
            "integer {value} exceeds D1's exact binding range"
        )));
    }
    Ok(JsValue::from_f64(value as f64))
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

    async fn installation(&self, sql: &str, values: &[JsValue]) -> Result<Option<Account>> {
        let row: Option<installations::InstallationRow> = self.first(sql, values).await?;
        row.map(|row| row.into_account()).transpose()
    }
}

// SAFETY: D1Database holds a JS object reference (JsValue) which is !Send + !Sync upstream.
// This is sound only under the single-threaded Cloudflare Workers execution model where
// no OS threads exist and wasm32 has no thread-spawn capability. If SharedArrayBuffer-based
// threads are ever added to the build, these impls must be revisited.
unsafe impl Send for D1Storage {}
unsafe impl Sync for D1Storage {}

#[async_trait]
impl ProjectionStorage for D1Storage {
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
    async fn get_installation(&self, installation_id: u64) -> Result<Option<Account>> {
        wasm_send(async {
            let values = [int(installation_id)?];
            self.installation(installations::GET, &values).await
        })
        .await
    }

    async fn get_active_installation_by_account_id(
        &self,
        account_id: u64,
    ) -> Result<Option<Account>> {
        wasm_send(async {
            let values = [int(account_id)?];
            self.installation(installations::ACTIVE_BY_ACCOUNT, &values)
                .await
        })
        .await
    }

    async fn upsert_user(&self, user: &User) -> Result<()> {
        wasm_send(async {
            let values = [
                int(user.user_id)?,
                text(&user.login),
                nullable(user.avatar_url.as_deref().map(text)),
                time(user.last_seen_at),
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
            self.first(invitation_requests::GET, &values).await
        })
        .await
    }

    async fn get_github_invitation(
        &self,
        id: GithubInvitationId,
    ) -> Result<Option<GithubInvitation>> {
        wasm_send(async {
            let values = [text(&id.to_string())];
            self.first(github_invitations::GET, &values).await
        })
        .await
    }
}

#[async_trait]
impl ConsoleStorage for D1Storage {
    async fn get_active_installation_by_login(&self, login: &str) -> Result<Option<Account>> {
        wasm_send(async {
            self.installation(installations::ACTIVE_BY_LOGIN, &[text(login)])
                .await
        })
        .await
    }

    async fn get_latest_installation_by_login(&self, login: &str) -> Result<Option<Account>> {
        wasm_send(async {
            self.installation(installations::LATEST_BY_LOGIN, &[text(login)])
                .await
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
            Ok(request_history::page(self.all(&sql, &values).await?))
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

    async fn list_delivery_for_request(&self, id: RequestId) -> Result<Vec<CreateReceipt>> {
        wasm_send(async {
            let values = [text(&id.to_string())];
            let rows: Vec<delivery_projection::ReceiptRow> =
                self.all(delivery_projection::FOR_REQUEST, &values).await?;
            rows.into_iter().map(|row| row.decode()).collect()
        })
        .await
    }

    async fn list_github_invitations_for_request(
        &self,
        id: RequestId,
    ) -> Result<Vec<GithubInvitation>> {
        wasm_send(async {
            let values = [text(&id.to_string())];
            self.all(github_invitations::FOR_REQUEST, &values).await
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
        wasm_send(async {
            let now = JsValue::from_f64(now as f64);
            self.run(CLEANUP, std::slice::from_ref(&now)).await?;
            let values = [
                text(scope),
                text(id),
                text(binding),
                text(payload),
                JsValue::from_f64(expires_at as f64),
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
            let values = [text(scope), text(id), JsValue::from_f64(now as f64)];
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
            let values = [text(scope), JsValue::from_f64(now as f64)];
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
impl WebhookStorage for D1Storage {
    async fn get_github_invitation_by_github_id(
        &self,
        github_id: u64,
    ) -> Result<Option<GithubInvitation>> {
        wasm_send(async {
            let values = [int(github_id)?];
            self.first(github_invitations::BY_GITHUB_ID, &values).await
        })
        .await
    }

    async fn member_invitation_candidates(
        &self,
        account_id: u64,
        repo_id: u64,
        requester_id: u64,
    ) -> Result<Vec<GithubInvitation>> {
        wasm_send(async {
            let values = [int(account_id)?, int(repo_id)?, int(requester_id)?];
            self.all(github_invitations::MEMBER_CANDIDATES, &values)
                .await
        })
        .await
    }

    async fn bind_member_webhook(
        &self,
        payload_sha256: &str,
        invitation_id: Option<GithubInvitationId>,
    ) -> Result<Option<GithubInvitationId>> {
        wasm_send(async {
            let values = [
                text(payload_sha256),
                nullable(invitation_id.map(|id| text(&id.to_string()))),
            ];
            self.run(github_invitations::BIND_MEMBER_WEBHOOK, &values)
                .await?;
            let binding: github_invitations::MemberWebhookBinding = self
                .first(
                    github_invitations::MEMBER_WEBHOOK_BINDING,
                    &[text(payload_sha256)],
                )
                .await?
                .ok_or_else(|| unreadable_write("member webhook binding"))?;
            Ok(binding.invitation_id)
        })
        .await
    }
}

#[async_trait]
impl InstallationStorage for D1Storage {
    async fn insert_installation(&self, account: &Account) -> Result<()> {
        wasm_send(async {
            let values = [
                int(account.installation_id)?,
                int(account.account_id)?,
                text(&account.account_login),
                text(&account.account_type.to_string()),
                time(account.installed_at),
                nullable(account.uninstalled_at.map(time)),
                text(&installations::encode_selected_repos(
                    &account.selected_repos,
                )),
            ];
            self.prepare(installations::INSERT, &values)?
                .run()
                .await
                .map_err(|e| classify_insert(e.to_string()))?;
            Ok(())
        })
        .await
    }

    async fn mark_installation_uninstalled(
        &self,
        installation_id: u64,
        when: DateTime<Utc>,
    ) -> Result<()> {
        wasm_send(async {
            let values = [time(when), int(installation_id)?];
            match self.run(installations::MARK_UNINSTALLED, &values).await? {
                0 => Err(Error::NotFound),
                _ => Ok(()),
            }
        })
        .await
    }

    async fn update_installation_repos(
        &self,
        installation_id: u64,
        selected: &SelectedRepos,
    ) -> Result<()> {
        wasm_send(async {
            let values = [
                text(&installations::encode_selected_repos(selected)),
                int(installation_id)?,
            ];
            match self.run(installations::UPDATE_REPOS, &values).await? {
                0 => Err(Error::NotFound),
                _ => Ok(()),
            }
        })
        .await
    }

    async fn list_active_installations(&self) -> Result<Vec<Account>> {
        wasm_send(async {
            let rows: Vec<installations::InstallationRow> =
                self.all(installations::LIST_ACTIVE, &[]).await?;
            rows.into_iter().map(|row| row.into_account()).collect()
        })
        .await
    }
}

#[async_trait]
impl DeliveryStorage for D1Storage {
    async fn claim_delivery_attempt(&self, command: &CreateCommand) -> Result<Option<u64>> {
        let encoded = delivery_attempts::encode(command)?;
        wasm_send(async {
            let id = text(&command.invitation_id.to_string());
            let values = [id.clone(), text(&encoded)];
            let generation = self.first(delivery_attempts::CLAIM, &values).await?;
            let retained = self
                .first(delivery_attempts::COMMAND, &[id])
                .await?
                .ok_or_else(|| unreadable_write("delivery attempt"))?;
            delivery_attempts::claimed(generation, retained, &encoded)
        })
        .await
    }

    async fn reject_delivery_attempt(&self, id: GithubInvitationId, generation: u64) -> Result<()> {
        wasm_send(async {
            let values = [text(&id.to_string()), int(generation)?];
            self.run(delivery_attempts::REJECT, &values).await?;
            Ok(())
        })
        .await
    }

    async fn delivery_attempt_exists(&self, id: GithubInvitationId) -> Result<bool> {
        wasm_send(async {
            let values = [text(&id.to_string())];
            let row: Option<IgnoredAny> = self.first(delivery_attempts::FENCED, &values).await?;
            Ok(row.is_some())
        })
        .await
    }

    async fn project_delivery(&self, receipt: &CreateReceipt) -> Result<()> {
        let encoded = delivery_projection::encode(receipt)?;
        let statements = delivery_projection::statements();
        wasm_send(self.batch(&statements, &encoded, delivery_projection::classify)).await
    }

    async fn insert_github_invitation(&self, invitation: &GithubInvitation) -> Result<()> {
        wasm_send(async {
            let values = [
                text(&invitation.id.to_string()),
                text(&invitation.invitation_request_id.to_string()),
                int(invitation.repo_id)?,
                nullable(invitation.github_invitation_id.map(int).transpose()?),
                text(&invitation.state.to_string()),
                nullable(invitation.error_message.as_deref().map(text)),
                time(invitation.created_at),
                time(invitation.updated_at),
            ];
            self.prepare(github_invitations::INSERT, &values)?
                .run()
                .await
                .map_err(|e| classify_insert(e.to_string()))?;
            Ok(())
        })
        .await
    }

    async fn settle_github_invitation(&self, transition: &settlement::Settlement) -> Result<()> {
        let input = settlement::encode(transition)?;
        wasm_send(self.batch(settlement::STATEMENTS, &input, Error::Database)).await
    }

    async fn list_pending_github_invitations_for_account(
        &self,
        account_id: u64,
    ) -> Result<Vec<GithubInvitation>> {
        wasm_send(async {
            let values = [int(account_id)?];
            self.all(github_invitations::PENDING_FOR_ACCOUNT, &values)
                .await
        })
        .await
    }
}

#[async_trait]
impl AuditStorage for D1Storage {
    async fn audit(&self, event: &AuditEvent) -> Result<()> {
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
