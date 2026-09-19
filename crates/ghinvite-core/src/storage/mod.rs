//! Persistence boundary for ghinvite. The `Storage` trait is the single seam
//! between domain logic and the database. Two impls in v1: `SqlxStorage`
//! (`ghinvite-storage-sqlx`, native dev & tests) and `D1Storage`
//! (`ghinvite-storage-d1`, production on Cloudflare Workers).

use crate::audit::AuditEvent;
use crate::{
    Account, GithubInvitation, GithubInvitationId, InvitationLink, InvitationLinkId,
    InvitationRequest, InvitationState, RequestId, RequestState, SelectedRepos, User,
};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use thiserror::Error;

/// Conformance suite every `Storage` impl must pass — see
/// [`test_suite::run_suite`].
#[cfg(feature = "test-suite")]
pub mod test_suite;

pub mod admin_attempts;
pub mod audit_read;
pub mod delivery_projection;
pub mod pending_queue;
pub mod projection;
pub mod settlement;
pub use audit_read::{AUDIT_PAGE_SIZE, AuditBoundary, AuditPage, AuditPosition};

pub type Result<T> = std::result::Result<T, Error>;

/// Installation projectors retain complete audit events before insertion. An
/// identical primary-key replay succeeds; differing content violates NOT NULL
/// and leaves the stored event intact. Shared SQLite/D1 statement semantics.
pub const INSTALLATION_AUDIT_REPLAY: &str = " ON CONFLICT(id) DO UPDATE SET account_id = CASE WHEN audit_events.account_id IS excluded.account_id AND audit_events.occurred_at IS excluded.occurred_at AND audit_events.event_type IS excluded.event_type AND audit_events.actor_kind IS excluded.actor_kind AND audit_events.actor_id IS excluded.actor_id AND audit_events.target_kind IS excluded.target_kind AND audit_events.target_id IS excluded.target_id AND audit_events.metadata IS excluded.metadata AND audit_events.request_id IS excluded.request_id THEN audit_events.account_id ELSE NULL END";

/// Include terminal history: member events carry no invitation ID or event time,
/// so a historical duplicate must not be reassigned to a later request. Two rows
/// suffice to detect ambiguity without loading an account's invitation history.
pub const MEMBER_INVITATION_CANDIDATES: &str = "SELECT g.id, g.invitation_request_id,
    g.repo_id, g.github_invitation_id, g.state, g.error_message, g.created_at, g.updated_at
    FROM github_invitations g
    JOIN invitation_requests r ON r.id = g.invitation_request_id
    JOIN invitation_links l ON l.id = r.invitation_link_id
    WHERE l.account_id = ?1 AND g.repo_id = ?2 AND r.requester_id = ?3 LIMIT 2";

/// Reasons a write may fail with [`Error::Conflict`]. Each variant pinpoints a
/// specific unique-constraint or invariant the storage layer enforced.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ConflictKind {
    #[error("invitation_link slug already in use")]
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
    /// First writer wins atomically by ID and logical binding; return that record.
    async fn retain_admin_attempt(
        &self,
        _scope: &str,
        _id: &str,
        _binding: &str,
        _payload: &str,
        _expires_at: i64,
        _now: i64,
    ) -> Result<admin_attempts::StoredAttempt> {
        Err(Error::Database("attempt storage unavailable".into()))
    }
    async fn get_admin_attempt(
        &self,
        _scope: &str,
        _id: &str,
        _now: i64,
    ) -> Result<Option<admin_attempts::StoredAttempt>> {
        Err(Error::Database("attempt storage unavailable".into()))
    }
    async fn list_admin_attempts(
        &self,
        _scope: &str,
        _now: i64,
    ) -> Result<Vec<admin_attempts::StoredAttempt>> {
        Err(Error::Database("attempt storage unavailable".into()))
    }
    /// Atomically settle the observed Sent invitation and publish its audit.
    /// Stale evidence is a no-op; replay after acknowledgement loss is safe.
    async fn settle_github_invitation(&self, _transition: &settlement::Settlement) -> Result<()> {
        Err(Error::Database("settlement storage unavailable".into()))
    }
    /// Atomic input-bound HTTP attempt fence. Returns a generation authorizing
    /// one PUT; None means uncertain/confirmed prior effect. Only an explicitly
    /// rejected generation may permit a new attempt; acknowledgement loss fences it.
    async fn claim_delivery_attempt(
        &self,
        _command: &crate::delivery::CreateCommand,
    ) -> Result<Option<u64>> {
        Err(Error::Database("delivery storage unavailable".into()))
    }
    async fn reject_delivery_attempt(
        &self,
        _id: GithubInvitationId,
        _generation: u64,
    ) -> Result<()> {
        Err(Error::Database("delivery storage unavailable".into()))
    }
    async fn delivery_attempt_exists(&self, _id: GithubInvitationId) -> Result<bool> {
        Err(Error::Database("delivery storage unavailable".into()))
    }

    /// Query projection of the receiving object's receipt, separate from lifecycle.
    async fn project_delivery(&self, _receipt: &crate::delivery::CreateReceipt) -> Result<()> {
        Err(Error::Database("delivery storage unavailable".into()))
    }

    async fn list_delivery_for_request(
        &self,
        _id: RequestId,
    ) -> Result<Vec<crate::delivery::CreateReceipt>> {
        Err(Error::Database("delivery storage unavailable".into()))
    }
    async fn list_github_invitations_for_request(
        &self,
        _id: RequestId,
    ) -> Result<Vec<GithubInvitation>> {
        Err(Error::Database(
            "invitation history read unavailable".into(),
        ))
    }

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

    /// Console history lookup after uninstall. Never grants authority: callers
    /// must validate the returned numeric account against current GitHub identity.
    async fn get_latest_installation_by_login(&self, _login: &str) -> Result<Option<Account>> {
        Ok(None)
    }

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

    // -------- invitation links --------

    /// Insert an invitation link plus its repo set in one transaction.
    ///
    /// **Errors:**
    /// - [`Error::Conflict`] with [`ConflictKind::DuplicateSlug`] if `link.slug`
    ///   collides with an existing link.
    /// - [`Error::Conflict`] with [`ConflictKind::DuplicateId`] if `link.id` collides.
    /// - [`Error::Conflict`] with [`ConflictKind::ForeignKey`] if `installation_id`
    ///   or `created_by` reference rows that do not exist.
    /// - [`Error::Database`] for any other failure.
    async fn insert_invitation_link(&self, link: &InvitationLink) -> Result<()>;

    /// Replace only the description and internal note of an account's link.
    /// `None` clears the note. Inactive links and unchanged values are accepted;
    /// all guardrails, identity, usage counters and revocation fields are preserved.
    /// The caller supplies validated, normalized metadata.
    ///
    /// **Errors:** [`Error::NotFound`] for an unknown or foreign-account link;
    /// [`Error::Database`] otherwise. Safe to retry with the same values.
    async fn update_invitation_link_metadata(
        &self,
        account_id: u64,
        id: InvitationLinkId,
        description: &str,
        internal_note: Option<&str>,
    ) -> Result<()>;

    /// Mark an invitation link as revoked (idempotent guard: only updates rows where
    /// `revoked_at IS NULL`).
    ///
    /// **Errors:**
    /// - [`Error::NotFound`] if the link is unknown *or* was already revoked.
    /// - [`Error::Database`] otherwise.
    async fn mark_invitation_link_revoked(
        &self,
        id: InvitationLinkId,
        by_user: u64,
        when: DateTime<Utc>,
    ) -> Result<()>;

    /// Read an invitation link plus its repo set by primary key.
    ///
    /// **Errors:**
    /// - [`Error::Corrupt`] if the row contains an unparseable enum/slug/ulid.
    /// - [`Error::Database`] otherwise.
    async fn get_invitation_link_by_id(
        &self,
        id: InvitationLinkId,
    ) -> Result<Option<InvitationLink>>;

    /// Bounded identity-only lookup for safe Console resource links. Does not
    /// load private metadata or the link's (potentially large) repository scope.
    async fn invitation_link_belongs_to_account(
        &self,
        account_id: u64,
        id: InvitationLinkId,
    ) -> Result<bool>;

    /// Read an invitation link by its public slug. Slug is the URL-facing identifier;
    /// callers MUST compare in constant time against `Slug::ct_eq` *before* trusting
    /// the result, to defend against timing-based slug enumeration.
    ///
    /// **Errors:** as for [`Self::get_invitation_link_by_id`].
    async fn get_invitation_link_by_slug(&self, slug: &str) -> Result<Option<InvitationLink>>;

    /// List every invitation link belonging to an account, newest first. Each link
    /// includes its repo set (single LEFT JOIN — no N+1).
    ///
    /// **Errors:** [`Error::Corrupt`] / [`Error::Database`] as above.
    async fn list_invitation_links_for_account(
        &self,
        account_id: u64,
    ) -> Result<Vec<InvitationLink>>;

    // -------- invitation requests --------

    /// Insert a new `InvitationRequest` row and atomically increment
    /// `invitation_links.uses_count` for the link this request was filed against.
    ///
    /// **Precondition:** the caller must have just verified that the invitation link is
    /// `InvitationLink::is_active(now)`. This method does *not* re-check active status —
    /// the partial unique index on `(invitation_link_id, requester_id) WHERE state = 'pending'`
    /// only defends against duplicate pending requests, not against exhaustion or expiry.
    ///
    /// **Errors:**
    /// - [`Error::Conflict`] with [`ConflictKind::DuplicatePendingRequest`] if a pending
    ///   request already exists for this `(link, requester)`.
    /// - [`Error::Conflict`] with [`ConflictKind::DuplicateId`] if `request.id` collides.
    /// - [`Error::NotFound`] if `invitation_link_id` doesn't reference an existing link.
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
    /// Legacy unbounded read (overview count and compatibility callers).
    ///
    /// **Errors:** [`Error::Corrupt`] / [`Error::Database`].
    async fn list_pending_requests_for_account(
        &self,
        account_id: u64,
    ) -> Result<Vec<InvitationRequest>>;

    /// Oldest-first, account-authorized, bounded decision queue with joined
    /// context. A cursor is a value boundary, not a reference to a live row;
    /// terminal transitions between reads cannot shift subsequent pages.
    /// Pending rows are not filtered by deadline or link expiration/revocation.
    async fn pending_request_page(
        &self,
        _account_id: u64,
        _after: Option<pending_queue::PendingBoundary>,
    ) -> Result<pending_queue::PendingPage> {
        Err(Error::Database("pending queue read unsupported".into()))
    }

    /// List every request (any state) for a given invitation link, newest first.
    ///
    /// **Errors:** [`Error::Corrupt`] / [`Error::Database`].
    async fn list_requests_for_link(
        &self,
        link_id: InvitationLinkId,
    ) -> Result<Vec<InvitationRequest>>;

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
    /// `github_invitation_id` field is assigned exactly: passing `None` clears
    /// any previously stored upstream GitHub invitation id.
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

    /// At most two historical matches for verified member-event identities.
    /// Exactly one Sent row with an upstream ID permits webhook settlement;
    /// ambiguity requires invitation-specific reconciliation instead.
    async fn member_invitation_candidates(
        &self,
        _account_id: u64,
        _repo_id: u64,
        _requester_id: u64,
    ) -> Result<Vec<GithubInvitation>> {
        Err(Error::Database(
            "member invitation lookup unavailable".into(),
        ))
    }

    /// Retain the first matching result (including None) for a verified body hash.
    /// Returns the retained binding on replay/concurrency, never a new match.
    /// This is ingress routing identity; lifecycle settlement remains owner-only.
    async fn bind_member_webhook(
        &self,
        _payload_sha256: &str,
        _invitation_id: Option<GithubInvitationId>,
    ) -> Result<Option<GithubInvitationId>> {
        Err(Error::Database("member webhook binding unavailable".into()))
    }

    /// List github_invitations rows for a given installation that are still
    /// in flight (state `sending` or `sent`). Used by the daily reconcile sweep.
    ///
    /// **Errors:** [`Error::Corrupt`] / [`Error::Database`].
    async fn list_pending_github_invitations_for_installation(
        &self,
        installation_id: u64,
    ) -> Result<Vec<GithubInvitation>>;

    /// In-flight GitHub invitations across all historical installations of an
    /// immutable account. Installation IDs on links remain original provenance.
    async fn list_pending_github_invitations_for_account(
        &self,
        _account_id: u64,
    ) -> Result<Vec<GithubInvitation>> {
        Err(Error::Database(
            "account invitation history read unavailable".into(),
        ))
    }

    // -------- audit --------

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

    /// Append an audit event. No update/delete surface is exposed.
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
