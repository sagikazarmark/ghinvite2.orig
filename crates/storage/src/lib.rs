//! Persistence boundary for ghinvite. The `Storage` trait is the single seam
//! between domain logic and the database. Two impls in v1: `SqlxStorage`
//! (native dev & tests, this crate) and `D1Storage` (production, future plan).

use async_trait::async_trait;
use audit::AuditEvent;
use chrono::{DateTime, Utc};
use domain::{
    Account, GithubInvitation, GithubInvitationId, InvitationRequest, InvitationState, RequestId,
    RequestState, SelectedRepos, ShareLink, ShareLinkId, User,
};
use thiserror::Error;

pub mod records;
pub mod sqlx_impl;

#[cfg(any(test, feature = "test-suite"))]
pub mod tests;

pub use sqlx_impl::SqlxStorage;

pub type Result<T> = std::result::Result<T, Error>;

/// Reasons a write may fail with [`Error::Conflict`]. Each variant pinpoints a
/// specific unique-constraint or invariant the storage layer enforced.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ConflictKind {
    #[error("share_link slug already in use")]
    DuplicateSlug,
    #[error("a pending request already exists for this (link, requester)")]
    DuplicatePendingRequest,
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
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),

    #[error("not found")]
    NotFound,

    #[error("conflict: {0}")]
    Conflict(ConflictKind),

    #[error("data corruption: {0}")]
    Corrupt(String),
}

/// Decision recorded against a previously-pending invitation request.
#[derive(Clone, Debug)]
pub struct RequestDecision {
    pub request_id: RequestId,
    pub state: RequestState,
    pub decided_by: Option<u64>,
    pub decided_at: DateTime<Utc>,
    pub decline_reason: Option<String>,
}

/// Update payload for github_invitations row state transitions.
#[derive(Clone, Debug)]
pub struct GithubInvitationUpdate {
    pub id: GithubInvitationId,
    pub state: InvitationState,
    pub github_invitation_id: Option<u64>,
    pub error_message: Option<String>,
    pub updated_at: DateTime<Utc>,
}

#[async_trait]
pub trait Storage: Send + Sync + 'static {
    // -------- installations --------

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

    /// Look up the *active* installation by GitHub login (org or user name).
    /// Logins are mutable upstream — fall back to id-based lookup whenever possible.
    ///
    /// **Errors:** [`Error::Database`] only.
    async fn get_active_installation_by_login(&self, login: &str) -> Result<Option<Account>>;

    /// List every active installation, ordered by `installed_at` ascending.
    /// Used by the daily reconcile sweep.
    ///
    /// **Errors:** [`Error::Database`] only.
    async fn list_active_installations(&self) -> Result<Vec<Account>>;

    // -------- users --------

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

    // -------- share links --------

    /// Insert a share link plus its repo set in one transaction.
    ///
    /// **Errors:**
    /// - [`Error::Conflict`] with [`ConflictKind::DuplicateSlug`] if `link.slug`
    ///   collides with an existing link.
    /// - [`Error::Conflict`] with [`ConflictKind::DuplicateId`] if `link.id` collides.
    /// - [`Error::Conflict`] with [`ConflictKind::ForeignKey`] if `installation_id`
    ///   or `created_by` reference rows that do not exist.
    /// - [`Error::Database`] for any other failure.
    async fn insert_share_link(&self, link: &ShareLink) -> Result<()>;

    /// Mark a share link as revoked (idempotent guard: only updates rows where
    /// `revoked_at IS NULL`).
    ///
    /// **Errors:**
    /// - [`Error::NotFound`] if the link is unknown *or* was already revoked.
    /// - [`Error::Database`] otherwise.
    async fn mark_share_link_revoked(
        &self,
        id: ShareLinkId,
        by_user: u64,
        when: DateTime<Utc>,
    ) -> Result<()>;

    /// Read a share link plus its repo set by primary key.
    ///
    /// **Errors:**
    /// - [`Error::Corrupt`] if the row contains an unparseable enum/slug/ulid.
    /// - [`Error::Database`] otherwise.
    async fn get_share_link_by_id(&self, id: ShareLinkId) -> Result<Option<ShareLink>>;

    /// Read a share link by its public slug. Slug is the URL-facing identifier;
    /// callers MUST compare in constant time against `Slug::ct_eq` *before* trusting
    /// the result, to defend against timing-based slug enumeration.
    ///
    /// **Errors:** as for [`Self::get_share_link_by_id`].
    async fn get_share_link_by_slug(&self, slug: &str) -> Result<Option<ShareLink>>;

    /// List every share link belonging to an account, newest first. Each link
    /// includes its repo set (single LEFT JOIN — no N+1).
    ///
    /// **Errors:** [`Error::Corrupt`] / [`Error::Database`] as above.
    async fn list_share_links_for_account(&self, account_id: u64) -> Result<Vec<ShareLink>>;

    // -------- invitation requests --------

    /// Insert a new `InvitationRequest` row and atomically increment
    /// `share_links.uses_count` for the link this request was filed against.
    ///
    /// **Precondition:** the caller must have just verified that the share link is
    /// `ShareLink::is_active(now)`. This method does *not* re-check active status —
    /// the partial unique index on `(share_link_id, requester_id) WHERE state = 'pending'`
    /// only defends against duplicate pending requests, not against exhaustion or expiry.
    ///
    /// **Errors:**
    /// - [`Error::Conflict`] with [`ConflictKind::DuplicatePendingRequest`] if a pending
    ///   request already exists for this `(link, requester)`.
    /// - [`Error::Conflict`] with [`ConflictKind::DuplicateId`] if `request.id` collides.
    /// - [`Error::NotFound`] if `share_link_id` doesn't reference an existing link.
    /// - [`Error::Database`] for any other SQLite/D1 failure.
    ///
    /// **Idempotency:** safe to retry on [`Error::Database`] (timeout etc.) — the
    /// unique constraints will surface a [`Error::Conflict`] if the prior attempt
    /// actually succeeded.
    async fn insert_invitation_request_and_increment_uses(
        &self,
        request: &InvitationRequest,
    ) -> Result<()>;

    /// Record a decision on a pending request. Only updates rows currently in
    /// state `pending` (so a second decision returns `NotFound` rather than
    /// silently overwriting).
    ///
    /// **Errors:**
    /// - [`Error::NotFound`] if the request is unknown *or* already decided.
    /// - [`Error::Database`] otherwise.
    async fn record_request_decision(&self, decision: &RequestDecision) -> Result<()>;

    /// Look up a single invitation request by id.
    ///
    /// **Errors:** [`Error::Corrupt`] / [`Error::Database`].
    async fn get_invitation_request(&self, id: RequestId) -> Result<Option<InvitationRequest>>;

    /// List all pending requests for any link belonging to the given account.
    /// Used by the admin approval queue.
    ///
    /// **Errors:** [`Error::Corrupt`] / [`Error::Database`].
    async fn list_pending_requests_for_account(
        &self,
        account_id: u64,
    ) -> Result<Vec<InvitationRequest>>;

    /// List every request (any state) for a given share link, newest first.
    ///
    /// **Errors:** [`Error::Corrupt`] / [`Error::Database`].
    async fn list_requests_for_link(&self, link_id: ShareLinkId) -> Result<Vec<InvitationRequest>>;

    // -------- github invitations --------

    /// Insert a new github_invitations row in `Sending` state. Caller is the
    /// Restate handler that's about to call GitHub.
    ///
    /// **Errors:**
    /// - [`Error::Conflict`] with [`ConflictKind::DuplicateId`] on `id` collision.
    /// - [`Error::Conflict`] with [`ConflictKind::ForeignKey`] if
    ///   `invitation_request_id` references a row that does not exist.
    /// - [`Error::Database`] for any other failure.
    async fn insert_github_invitation(&self, invitation: &GithubInvitation) -> Result<()>;

    /// Update an existing github_invitations row to a new state. The
    /// `github_invitation_id` field is `COALESCE`d so passing `None` preserves
    /// the previously stored id.
    ///
    /// **Errors:**
    /// - [`Error::NotFound`] if `update.id` doesn't match any row.
    /// - [`Error::Database`] otherwise.
    async fn update_github_invitation(&self, update: &GithubInvitationUpdate) -> Result<()>;

    /// Look up a github_invitations row by primary key.
    ///
    /// **Errors:** [`Error::Corrupt`] / [`Error::Database`].
    async fn get_github_invitation(
        &self,
        id: GithubInvitationId,
    ) -> Result<Option<GithubInvitation>>;

    /// Look up a github_invitations row by GitHub's invitation id (the integer
    /// surfaced in webhook payloads). Returns `None` if we never persisted such
    /// an id (e.g. row never reached `Sent`).
    ///
    /// **Errors:** [`Error::Corrupt`] / [`Error::Database`].
    async fn get_github_invitation_by_github_id(
        &self,
        github_id: u64,
    ) -> Result<Option<GithubInvitation>>;

    /// List github_invitations rows for a given installation that are still
    /// in flight (state `sending` or `sent`). Used by the daily reconcile sweep.
    ///
    /// **Errors:** [`Error::Corrupt`] / [`Error::Database`].
    async fn list_pending_github_invitations_for_installation(
        &self,
        installation_id: u64,
    ) -> Result<Vec<GithubInvitation>>;

    // -------- audit (write-only) --------

    /// Append an audit event. The trait deliberately exposes no read or mutate
    /// surface for audit data — that's a v1.1 feature handled by per-impl debug
    /// helpers.
    ///
    /// **Errors:** [`Error::Database`] only.
    /// **Idempotency:** safe to retry on transient failure as long as the caller
    /// reuses the same `event.id` (collisions surface as a SQLite unique-violation
    /// inside the [`Error::Database`] branch).
    async fn audit(&self, event: &AuditEvent) -> Result<()>;
}
