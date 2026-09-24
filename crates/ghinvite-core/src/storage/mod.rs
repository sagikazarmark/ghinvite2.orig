//! Persistence boundary for ghinvite. The storage traits, one per caller role
//! and combined as [`Storage`], are the seam between domain logic and the
//! database. Two impls: `SqlxStorage`
//! (`ghinvite-storage-sqlx`, native dev & tests) and `D1Storage`
//! (`ghinvite-storage-d1`, production on Cloudflare Workers).

use crate::audit::AuditEvent;
use crate::{
    Account, GithubInvitation, GithubInvitationId, InvitationLink, InvitationLinkId,
    InvitationRequest, RequestId, SelectedRepos, User,
};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use thiserror::Error;

/// Conformance suite every `Storage` impl must pass — see
/// [`test_suite::run_suite`].
#[cfg(feature = "test-suite")]
pub mod test_suite;

pub mod attempt_continuations;
pub mod audit_read;
pub mod audit_write;
pub mod delivery_attempts;
pub mod delivery_projection;
pub mod github_invitations;
pub mod installations;
pub mod invitation_links;
pub mod invitation_requests;
pub mod pending_queue;
pub mod projection;
pub mod request_history;
pub mod settlement;
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
    + WebhookStorage
    + InstallationStorage
    + DeliveryStorage
    + AuditStorage
{
}

impl<T> Storage for T where
    T: RecordStorage
        + ConsoleStorage
        + ContinuationStorage
        + WebhookStorage
        + InstallationStorage
        + DeliveryStorage
        + AuditStorage
{
}

/// Point lookups both the web app and the workflows read, plus the user
/// profile refreshed at sign-in.
#[async_trait]
pub trait RecordStorage: Send + Sync + 'static {
    /// Look up an installation by primary key. Returns the row whether or not it's
    /// active — callers needing only active rows should filter with `uninstalled_at`.
    ///
    /// **Errors:** [`Error::Database`] only.
    async fn get_installation(&self, installation_id: u64) -> Result<Option<Account>>;

    /// Look up the *active* installation for a GitHub account id, if any.
    /// Equivalent to `WHERE account_id = ? AND uninstalled_at IS NULL`.
    ///
    /// **Errors:** [`Error::Database`] only.
    async fn get_active_installation_by_account_id(
        &self,
        account_id: u64,
    ) -> Result<Option<Account>>;

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

    /// Look up a github_invitations row by primary key.
    ///
    /// **Errors:** [`Error::Corrupt`] / [`Error::Database`].
    async fn get_github_invitation(
        &self,
        id: GithubInvitationId,
    ) -> Result<Option<GithubInvitation>>;
}

/// Bounded, eventually consistent reads behind Console pages, Console sign-in
/// and the Invitation Request Flow's status. Never authority.
#[async_trait]
pub trait ConsoleStorage: Send + Sync + 'static {
    /// Look up the *active* installation by GitHub login (org or user name).
    /// Logins are mutable upstream — fall back to id-based lookup whenever possible.
    ///
    /// **Errors:** [`Error::Database`] only.
    async fn get_active_installation_by_login(&self, login: &str) -> Result<Option<Account>>;

    /// Console history lookup after uninstall. Never grants authority: callers
    /// must validate the returned numeric account against current GitHub identity.
    async fn get_latest_installation_by_login(&self, login: &str) -> Result<Option<Account>>;

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

    /// Query projection of the receiving object's receipts, separate from lifecycle.
    async fn list_delivery_for_request(
        &self,
        id: RequestId,
    ) -> Result<Vec<crate::delivery::CreateReceipt>>;

    async fn list_github_invitations_for_request(
        &self,
        id: RequestId,
    ) -> Result<Vec<GithubInvitation>>;

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

/// Webhook ingress routing: which GitHub invitation an upstream event names.
#[async_trait]
pub trait WebhookStorage: Send + Sync + 'static {
    /// Look up a github_invitations row by GitHub's invitation id (the integer
    /// surfaced in webhook payloads). Returns `None` if we never persisted such
    /// an id (e.g. row never reached `Sent`).
    ///
    /// **Errors:** [`Error::Corrupt`] / [`Error::Database`].
    async fn get_github_invitation_by_github_id(
        &self,
        github_id: u64,
    ) -> Result<Option<GithubInvitation>>;

    /// At most two historical matches for verified member-event identities.
    /// Exactly one Sent row with an upstream ID permits webhook settlement;
    /// ambiguity requires invitation-specific reconciliation instead.
    async fn member_invitation_candidates(
        &self,
        account_id: u64,
        repo_id: u64,
        requester_id: u64,
    ) -> Result<Vec<GithubInvitation>>;

    /// Retain the first matching result (including None) for a verified body hash.
    /// Returns the retained binding on replay/concurrency, never a new match.
    /// This is ingress routing identity; lifecycle settlement remains owner-only.
    async fn bind_member_webhook(
        &self,
        payload_sha256: &str,
        invitation_id: Option<GithubInvitationId>,
    ) -> Result<Option<GithubInvitationId>>;
}

/// Installation writers, driven by the installation workflows.
#[async_trait]
pub trait InstallationStorage: Send + Sync + 'static {
    /// Insert a brand-new installation row.
    ///
    /// **Errors:**
    /// - [`Error::Conflict`] with [`ConflictKind::DuplicateId`] if `installation_id`
    ///   already exists.
    /// - [`Error::Conflict`] with [`ConflictKind::DuplicateActiveInstallation`] if
    ///   another row with the same `account_id` is still active (partial unique index).
    /// - [`Error::Database`] for any other SQLite/D1 failure.
    ///
    /// **Idempotency:** not idempotent — caller must distinguish "first install"
    /// from "reinstall" before calling.
    async fn insert_installation(&self, account: &Account) -> Result<()>;

    /// Mark an installation as uninstalled by stamping `uninstalled_at`.
    ///
    /// **Preconditions:** caller decides "when" (typically `Utc::now()`).
    ///
    /// **Errors:**
    /// - [`Error::NotFound`] if no row matches `installation_id` *or* the row was
    ///   already marked uninstalled (idempotent guard via `WHERE uninstalled_at IS NULL`).
    /// - [`Error::Database`] otherwise.
    async fn mark_installation_uninstalled(
        &self,
        installation_id: u64,
        when: DateTime<Utc>,
    ) -> Result<()>;

    /// Update the `selected_repos` field on an active installation.
    ///
    /// **Errors:**
    /// - [`Error::NotFound`] if `installation_id` is unknown or already uninstalled.
    /// - [`Error::Database`] otherwise.
    async fn update_installation_repos(
        &self,
        installation_id: u64,
        selected: &SelectedRepos,
    ) -> Result<()>;

    /// List every active installation, ordered by `installed_at` ascending.
    /// Used by the daily reconcile sweep.
    ///
    /// **Errors:** [`Error::Database`] only.
    async fn list_active_installations(&self) -> Result<Vec<Account>>;
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
    async fn reject_delivery_attempt(&self, id: GithubInvitationId, generation: u64) -> Result<()>;
    async fn delivery_attempt_exists(&self, id: GithubInvitationId) -> Result<bool>;

    /// Query projection of the receiving object's receipt, separate from lifecycle.
    async fn project_delivery(&self, receipt: &crate::delivery::CreateReceipt) -> Result<()>;

    /// Insert a new github_invitations row in `Sending` state. Caller is the
    /// Restate handler that's about to call GitHub.
    ///
    /// **Errors:**
    /// - [`Error::Conflict`] with [`ConflictKind::DuplicateId`] on `id` collision.
    /// - [`Error::Conflict`] with [`ConflictKind::ForeignKey`] if
    ///   `invitation_request_id` references a row that does not exist.
    /// - [`Error::Database`] for any other failure.
    async fn insert_github_invitation(&self, invitation: &GithubInvitation) -> Result<()>;

    /// Atomically settle the observed Sent invitation and publish its audit.
    /// Stale evidence is a no-op; replay after acknowledgement loss is safe.
    async fn settle_github_invitation(&self, transition: &settlement::Settlement) -> Result<()>;

    /// In-flight GitHub invitations across all historical installations of an
    /// immutable account. Installation IDs on links remain original provenance.
    async fn list_pending_github_invitations_for_account(
        &self,
        account_id: u64,
    ) -> Result<Vec<GithubInvitation>>;
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
