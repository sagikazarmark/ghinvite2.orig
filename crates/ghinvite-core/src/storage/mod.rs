//! Persistence boundary for ghinvite. The storage traits, one per caller role
//! and combined as [`Storage`], are the seam between domain logic and the
//! database. Two impls: `SqlxStorage`
//! (`ghinvite-storage-sqlx`, native dev & tests) and `D1Storage`
//! (`ghinvite-storage-d1`, production on Cloudflare Workers).

use crate::audit::AuditEvent;
use crate::{InvitationLink, InvitationLinkId, InvitationRequest, RequestId, User};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use thiserror::Error;

/// Conformance suite every `Storage` impl must pass — see
/// [`test_suite::run_suite`].
#[cfg(feature = "test-suite")]
pub mod test_suite;

pub mod access_grants;
pub mod attempt_continuations;
pub mod audit_read;
pub mod audit_write;
pub mod blocked_digest;
pub mod bulk;
pub mod console_scope;
pub mod continuation_rotation;
pub mod delivery_attempts;
pub mod delivery_projection;

pub mod installation_projection;
pub mod installations;
pub mod integer;
pub mod invitation_links;
pub mod invitation_requests;
pub mod invite_budget;
pub mod link_secrets;
pub mod link_targets;
pub mod operational;
pub mod org_delivery;
pub mod organization_telemetry;
pub use org_delivery::OrgDeliveryStorage;
pub mod org_delivery_reads;
pub mod org_webhooks;
pub use org_delivery_reads::OrgDeliveryReadStorage;
pub use org_webhooks::OrgWebhookStorage;
pub mod pending_queue;
pub mod pending_signin;
pub mod projection;
pub mod request_history;
pub mod settlement;
pub mod signin_budget;
pub mod sweep_digest;
pub mod users;
pub use audit_read::{AUDIT_PAGE_SIZE, AuditBoundary, AuditPage, AuditPosition};

pub type Result<T> = std::result::Result<T, Error>;

/// Reasons a write may fail with [`Error::Conflict`]. Each variant pinpoints a
/// specific unique-constraint or invariant the storage layer enforced.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ConflictKind {
    #[error("primary key already exists")]
    DuplicateId,
    #[error("active installation already exists for this account")]
    DuplicateActiveInstallation,
    /// Insert references a parent row that does not exist.
    #[error("foreign-key violation: referenced row does not exist")]
    ForeignKey,
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("projection dependency missing")]
    ProjectionDependency,

    #[error("projection invariant violated: {0}")]
    ProjectionInvariant(String),

    #[error("database error: {0}")]
    Database(String),

    #[error("not found")]
    NotFound,

    #[error("conflict: {0}")]
    Conflict(ConflictKind),

    #[error("data corruption: {0}")]
    Corrupt(String),
}

/// Classify a failed constraint-checked insert by SQLite's error text, which both
/// drivers carry (D1 surfaces nothing else). Anything else is a database error.
pub fn classify_insert(message: String) -> Error {
    if unique_violation(&message) {
        // Only the partial unique index covers `installations.account_id`;
        // primary-key hits name `installations.installation_id`.
        Error::Conflict(if message.contains("installations.account_id") {
            ConflictKind::DuplicateActiveInstallation
        } else {
            ConflictKind::DuplicateId
        })
    } else if message.contains("FOREIGN KEY constraint failed") {
        Error::Conflict(ConflictKind::ForeignKey)
    } else {
        Error::Database(message)
    }
}

/// Decode one result row, presented by either driver as a JSON object of its
/// column values, into a shared row type. Undecodable rows are corrupt.
pub fn decode_row<T: serde::de::DeserializeOwned>(row: serde_json::Value) -> Result<T> {
    integer::validate_json(&row)
        .map_err(|_| Error::Corrupt("integer outside portable storage range".into()))?;
    serde_json::from_value(row).map_err(|e| Error::Corrupt(e.to_string()))
}

/// SQLite names the violated columns, not the index, after this prefix.
pub(crate) fn unique_violation(message: &str) -> bool {
    message.contains("UNIQUE constraint failed")
}

/// Every storage port. Adapters implement each part and receive this through
/// the blanket impl below; the conformance suite is what names the whole.
/// Callers depend on the parts they use, bundled as `WebStorage` and
/// `WorkflowStorage`, which is what the composition roots name.
pub trait Storage:
    RecordStorage
    + ConsoleStorage
    + ContinuationStorage
    + AppInstallationStorage
    + LinkCapabilityStorage
    + SigninBudgetStorage
    + DeliveryStorage
    + OrgDeliveryStorage
    + OrgDeliveryReadStorage
    + OrgWebhookStorage
    + OperationalReadStorage
    + AuditStorage
{
}

impl<T> Storage for T where
    T: RecordStorage
        + ConsoleStorage
        + ContinuationStorage
        + AppInstallationStorage
        + LinkCapabilityStorage
        + SigninBudgetStorage
        + DeliveryStorage
        + OrgDeliveryStorage
        + OrgDeliveryReadStorage
        + OrgWebhookStorage
        + OperationalReadStorage
        + AuditStorage
{
}

/// Point lookups both the web app and the workflows read, plus the user
/// profile refreshed at sign-in.
#[async_trait]
pub trait RecordStorage: Send + Sync + 'static {
    /// Insert-or-replace a user row keyed on `user_id`. Updates `login`,
    /// `avatar_url`, `last_seen_at` on conflict. Used on every successful sign-in.
    ///
    /// **Errors:** [`Error::Database`] only.
    /// **Idempotency:** safe to call repeatedly; always lands the supplied state.
    async fn upsert_user(&self, user: &User) -> Result<()>;

    /// Look up a user by GitHub user id.
    ///
    /// **Errors:** [`Error::Database`] only.
    async fn get_user(&self, user_id: u64) -> Result<Option<User>>;

    /// Read an invitation link plus its repo set by primary key.
    ///
    /// **Errors:**
    /// - [`Error::Corrupt`] if the row contains an unparseable enum/ULID.
    /// - [`Error::Database`] otherwise.
    async fn get_invitation_link_by_id(
        &self,
        id: InvitationLinkId,
    ) -> Result<Option<InvitationLink>>;

    /// Look up a single invitation request by id.
    ///
    /// **Errors:** [`Error::Corrupt`] / [`Error::Database`].
    async fn get_invitation_request(&self, id: RequestId) -> Result<Option<InvitationRequest>>;
}

/// Bounded, eventually consistent reads behind Console pages, Console sign-in
/// and the Invitation Request Flow's status. Never authority.
#[async_trait]
pub trait ConsoleStorage: Send + Sync + 'static {
    async fn revoke_preview(
        &self,
        account_id: u64,
        link_id: InvitationLinkId,
    ) -> Result<Option<crate::revocation::RevokePreview>>;
    /// Latest immutable daily snapshot at or before the requested UTC day.
    /// None is missing evidence; an empty snapshot is a scoped healthy zero.
    async fn scoped_blocked_digest(
        &self,
        scope: &console_scope::ConsoleScope,
        day: chrono::NaiveDate,
    ) -> Result<Option<blocked_digest::BlockedDigest>>;
    async fn scoped_list_invitation_links_for_account(
        &self,
        scope: &console_scope::ConsoleScope,
    ) -> Result<Vec<InvitationLink>>;
    async fn scoped_count_pending_requests_for_account(
        &self,
        scope: &console_scope::ConsoleScope,
    ) -> Result<u64>;
    async fn scoped_list_pending_requests_page(
        &self,
        scope: &console_scope::ConsoleScope,
        after: Option<pending_queue::PendingBoundary>,
    ) -> Result<pending_queue::PendingPage>;
    async fn scoped_list_audit_events(
        &self,
        scope: &console_scope::ConsoleScope,
        event: Option<crate::audit::EventType>,
        position: AuditPosition,
    ) -> Result<AuditPage>;
    /// Complete bounded discovery across retained invitation-link and Bulk
    /// manifests. Display names are hints; current GitHub evidence grants access.
    async fn distinct_console_repo_targets(
        &self,
        account_id: u64,
        after_repo_id: Option<u64>,
        limit: u16,
    ) -> Result<console_scope::RepoTargetPage>;

    /// Bounded identity-only lookup for safe Console resource links. Does not
    /// load private metadata or the link's (potentially large) repository scope.
    async fn invitation_link_belongs_to_account(
        &self,
        account_id: u64,
        id: InvitationLinkId,
    ) -> Result<bool>;

    /// List every invitation link belonging to an account, newest first. Each link
    /// includes its repo set (single LEFT JOIN — no N+1).
    ///
    /// **Errors:** [`Error::Corrupt`] / [`Error::Database`] as above.
    async fn list_invitation_links_for_account(
        &self,
        account_id: u64,
    ) -> Result<Vec<InvitationLink>>;

    /// At most 25 requests, ordered by admission time then ID descending. Account
    /// scope is checked independently of the exclusive cursor. Never loads all
    /// requests to paginate in memory. Missing projections are not absence proof.
    async fn request_history(
        &self,
        account_id: u64,
        link_id: InvitationLinkId,
        before: Option<request_history::Boundary>,
    ) -> Result<request_history::Page>;

    /// Number of pending requests in the account's decision queue: exactly the
    /// rows [`ConsoleStorage::pending_request_page`] pages through, counted by
    /// one statement without loading them.
    ///
    /// **Errors:** [`Error::Corrupt`] / [`Error::Database`].
    async fn count_pending_requests_for_account(&self, account_id: u64) -> Result<u64>;

    /// Oldest-first, account-authorized, bounded decision queue with joined
    /// context. A cursor is a value boundary, not a reference to a live row;
    /// terminal transitions between reads cannot shift subsequent pages.
    /// Pending rows are not filtered by deadline or link expiration/revocation.
    async fn pending_request_page(
        &self,
        account_id: u64,
        after: Option<pending_queue::PendingBoundary>,
    ) -> Result<pending_queue::PendingPage>;

    /// Read an exact kind-qualified repository delivery projection. Never authority.
    async fn get_delivery(&self, key: &str) -> Result<Option<crate::delivery::DeliverySnapshot>>;

    /// Query projection of the receiving object's receipts, separate from lifecycle.
    async fn list_delivery_for_request(
        &self,
        id: RequestId,
    ) -> Result<Vec<crate::delivery::DeliverySnapshot>>;

    /// Read at most 25 account events in (occurred_at DESC, id DESC) order.
    /// Exact event filtering precedes limiting. Boundaries are exclusive; After
    /// returns the nearest newer rows. Navigation uses bounded existence probes.
    /// Account scope is independent of cursors and includes all installations.
    /// Malformed rows fail the read rather than silently producing partial pages.
    async fn list_audit_events(
        &self,
        account_id: u64,
        event: Option<crate::audit::EventType>,
        position: AuditPosition,
    ) -> Result<AuditPage>;
}

/// Browser attempt continuations, retained until the authority answers.
#[async_trait]
pub trait ContinuationStorage: Send + Sync + 'static {
    /// Primary-consistent bounded rowid page, including expired records; one
    /// sentinel row carries the epoch even when the continuation table is empty.
    async fn continuation_rotation_page(
        &self,
        after: Option<i64>,
    ) -> Result<Vec<continuation_rotation::ScanRow>>;
    /// Primary-consistent mutation counter. Missing/corrupt state is unavailable.
    async fn continuation_rotation_epoch(&self) -> Result<i64>;
    /// Only ciphertext may change; exact old context, identity, expiry and global
    /// mutation epoch are compared atomically. Unknown ACK is an error, not false.
    async fn rewrap_attempt_continuation(
        &self,
        old: &continuation_rotation::ScanRow,
        replacement: &str,
        expected_epoch: i64,
    ) -> Result<bool>;

    /// First writer wins atomically by ID and logical binding; return that record.
    async fn retain_attempt_continuation(
        &self,
        scope: &str,
        id: &str,
        binding: &str,
        payload: &str,
        expires_at: i64,
        now: i64,
    ) -> Result<attempt_continuations::StoredContinuation>;
    async fn get_attempt_continuation(
        &self,
        scope: &str,
        id: &str,
        now: i64,
    ) -> Result<Option<attempt_continuations::StoredContinuation>>;
    /// Live continuations of one scope, oldest retained first.
    async fn list_attempt_continuations(
        &self,
        scope: &str,
        now: i64,
    ) -> Result<Vec<attempt_continuations::StoredContinuation>>;
    /// Forget a continuation the authority definitively rejected (nothing was
    /// applied). Releasing a missing record succeeds.
    async fn release_attempt_continuation(&self, scope: &str, id: &str) -> Result<()>;
}

/// GitHub invitation delivery and settlement, driven by the workflows.
#[async_trait]
pub trait DeliveryStorage: Send + Sync + 'static {
    /// Atomic input-bound HTTP attempt fence. Returns a generation authorizing
    /// one PUT; None means uncertain/confirmed prior effect. Only an explicitly
    /// rejected generation may permit a new attempt; acknowledgement loss fences it.
    async fn claim_delivery_attempt(
        &self,
        command: &crate::delivery::CreateCommand,
    ) -> Result<Option<u64>>;
    async fn reject_delivery_attempt(
        &self,
        command: &crate::delivery::CreateCommand,
        generation: u64,
    ) -> Result<()>;
    async fn delivery_attempt_exists(
        &self,
        command: &crate::delivery::CreateCommand,
    ) -> Result<bool>;

    /// Separately input-bound DELETE authorization. A missing generation means
    /// the prior effect is uncertain/confirmed, never permission to retry it.
    async fn claim_cancellation_attempt(
        &self,
        command: &delivery_attempts::CancelCommand,
    ) -> Result<Option<u64>>;
    /// Conservative read for run retries; unknown fence state never permits a write.
    async fn cancellation_attempt_exists(
        &self,
        command: &delivery_attempts::CancelCommand,
    ) -> Result<bool>;
    /// Only a positively rejected DELETE may release its exact generation.
    async fn reject_cancellation_attempt(
        &self,
        command: &delivery_attempts::CancelCommand,
        generation: u64,
    ) -> Result<()>;

    /// Atomically establish/validate invitation identity, create the initial row,
    /// project the receipt and permitted Sending advance, and publish immutable
    /// audit facts. Missing parents are ProjectionDependency; conflicting input
    /// or equal-revision content is ProjectionInvariant. Stale receipts cannot
    /// regress lifecycle but still publish their audit; identical replay is safe.
    async fn project_delivery(&self, receipt: &crate::delivery::DeliverySnapshot) -> Result<()> {
        self.project_delivery_with_events(&receipt.clone().into())
            .await
    }

    /// Retained transition facts are inserted even for stale snapshots. The
    /// snapshot and all facts commit atomically; changed event identities fail.
    async fn project_delivery_with_events(
        &self,
        projection: &crate::delivery_audit::RepositoryDeliveryProjection,
    ) -> Result<()>;

    /// Monotonic telemetry only; cannot rewrite retained facts or lifecycle.
    async fn project_repository_telemetry(
        &self,
        telemetry: &operational::RepositoryTelemetry,
    ) -> Result<()>;
}

/// Audit Log appends. No update/delete surface is exposed.
#[async_trait]
pub trait AuditStorage: Send + Sync + 'static {
    /// Append an audit event.
    ///
    /// **Errors:** [`Error::Database`] only.
    /// **Idempotency:** safe to retry on transient failure as long as the caller
    /// reuses the same `event.id` (collisions surface as a SQLite unique-violation
    /// inside the [`Error::Database`] branch).
    /// Metadata-update events are the exception: their journaled ID is an
    /// idempotency key, so a duplicate primary key succeeds without another row.
    /// Other constraints and database errors must still surface.
    async fn audit(&self, event: &AuditEvent) -> Result<()>;
}

/// App-scoped reads available to the web without granting installation writes.
#[async_trait]
pub trait AppInstallationReadStorage: Send + Sync + 'static {
    async fn app_installation(
        &self,
        app: crate::account::App,
        installation_id: u64,
    ) -> Result<Option<crate::account::Installation>>;
    async fn active_app_installation(
        &self,
        app: crate::account::App,
        account_id: u64,
    ) -> Result<Option<crate::account::Installation>>;
    async fn active_app_installation_by_login(
        &self,
        app: crate::account::App,
        login: &str,
    ) -> Result<Option<crate::account::Installation>>;
    async fn latest_app_installation_by_login(
        &self,
        app: crate::account::App,
        login: &str,
    ) -> Result<Option<crate::account::Installation>>;
    async fn list_active_app_installations(
        &self,
        app: crate::account::App,
    ) -> Result<Vec<crate::account::Installation>>;
    async fn account_settings(
        &self,
        account_id: u64,
    ) -> Result<Option<crate::account::AccountSettings>>;
}

/// Authoritative snapshots are revisioned; stale projection cannot restore old
/// permissions, an installer or a delegation flag. All writers use this app-scoped port.
#[async_trait]
pub trait AppInstallationStorage: AppInstallationReadStorage {
    async fn project_installation_change(
        &self,
        owner: crate::account::AccountInstallationRef,
        change: &crate::installation_audit::InstallationChange,
    ) -> Result<()>;
    async fn put_app_installation(&self, installation: &crate::account::Installation)
    -> Result<()>;
    async fn put_account_settings(&self, settings: &crate::account::AccountSettings) -> Result<()>;
}

#[async_trait]
pub trait LinkCapabilityStorage: Send + Sync + 'static {
    async fn link_targets(&self, id: InvitationLinkId) -> Result<crate::targets::Targets>;
    async fn register_link_secret(&self, secret: &link_secrets::LinkSecret) -> Result<()>;
    async fn resolve_link_secret(
        &self,
        sha256: &crate::SecretHash,
    ) -> Result<Option<link_secrets::LinkSecret>>;
    async fn retire_link_secrets(
        &self,
        id: InvitationLinkId,
        through_generation: u32,
        at: DateTime<Utc>,
    ) -> Result<()>;
}

/// Shared atomic admission before a browser starts OAuth. An ambiguous failure
/// spends the reservation conservatively and never starts an exchange.
#[async_trait]
pub trait SigninBudgetStorage: Send + Sync + 'static {
    async fn reserve_signin(
        &self,
        key: signin_budget::SigninBudgetKey,
        reservation: RequestId,
        now_ms: i64,
    ) -> Result<bool>;
}

/// Operator-only aggregate reads; this port grants no account Console authority.
#[async_trait]
pub trait OperationalReadStorage: Send + Sync + 'static {
    async fn delivery_signals(&self, now: DateTime<Utc>) -> Result<operational::DeliverySignals>;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Message text only: both drivers wrap SQLite's wording in a prefix of
    /// their own, and the constraint is read out from under it. That either
    /// driver really reports these failures this way is pinned against a live
    /// database by `test_suite::scenario_insert_conflict_kinds`.
    #[test]
    fn a_failed_insert_classifies_by_the_constraint_named_in_its_message() {
        let classified = |message: &str| classify_insert(message.into());
        assert!(matches!(
            classified(
                "D1_ERROR: UNIQUE constraint failed: installations.account_id: SQLITE_CONSTRAINT"
            ),
            Error::Conflict(ConflictKind::DuplicateActiveInstallation)
        ));
        assert!(matches!(
            classified(
                "error returned from database: (code: 1555) UNIQUE constraint failed: installations.installation_id"
            ),
            Error::Conflict(ConflictKind::DuplicateId)
        ));
        assert!(matches!(
            classified("D1_ERROR: FOREIGN KEY constraint failed: SQLITE_CONSTRAINT"),
            Error::Conflict(ConflictKind::ForeignKey)
        ));
        assert!(matches!(
            classified("NOT NULL constraint failed: audit_events.account_id"),
            Error::Database(_)
        ));
    }
}

pub mod email_review;
pub mod email_vault;

pub mod email_delivery;
