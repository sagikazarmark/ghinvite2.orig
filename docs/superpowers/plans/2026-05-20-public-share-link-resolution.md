# Public Share Link Resolution Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Centralize recipient-facing Share Link resolution behind one Web module with explicit outcomes and tests.

**Architecture:** Move resolution from `crates/web/src/routes/share_link_resolution.rs` to `crates/web/src/share_link_resolution.rs`. The resolver parses the raw slug, performs storage lookup, confirms the stored slug in constant time, applies `ShareLink::is_active`, and optionally detects a duplicate pending Invitation Request for a recipient. Routes convert resolver results into HTTP responses while the resolver owns the policy decisions.

**Tech Stack:** Rust, axum, chrono, `storage::Storage`, `domain::{Slug, ShareLink, InvitationRequest, RequestState}`.

---

## File Structure

- Create: `crates/web/src/share_link_resolution.rs` as the canonical Web resolver module.
- Modify: `crates/web/src/lib.rs` to declare the top-level module.
- Modify: `crates/web/src/routes/mod.rs` to remove the route-local module declaration.
- Modify: `crates/web/src/routes/invitation.rs` to import the top-level resolver and handle distinct outcomes.
- Modify: `crates/web/tests/invitation_resolution.rs` to keep HTTP-level behavior coverage and add missing duplicate-pending edge cases.

---

### Task 1: Resolver Outcomes And Errors

**Files:**
- Create: `crates/web/src/share_link_resolution.rs`
- Modify: `crates/web/src/lib.rs`
- Modify: `crates/web/src/routes/mod.rs`

- [ ] **Step 1: Write failing resolver unit tests**

Add tests in `crates/web/src/share_link_resolution.rs` for these concrete cases:

```rust
#[tokio::test]
async fn malformed_slug_returns_invalid_slug_without_storage_lookup() { /* assert ResolutionError::InvalidSlug */ }

#[tokio::test]
async fn unknown_slug_returns_unknown_slug() { /* assert ResolutionError::UnknownSlug */ }

#[tokio::test]
async fn stored_slug_mismatch_returns_slug_mismatch() { /* assert ResolutionError::SlugMismatch */ }

#[tokio::test]
async fn revoked_expired_and_exhausted_links_return_inactive() { /* assert ResolutionError::Inactive */ }

#[tokio::test]
async fn active_link_returns_available() { /* assert PublicShareLinkResolution::Available */ }
```

- [ ] **Step 2: Run failing tests**

Run: `cargo test -p web share_link_resolution`

Expected: FAIL because the top-level module and result types do not exist.

- [ ] **Step 3: Implement minimal resolver types and no-pending resolution**

Implement:

```rust
pub(crate) enum PendingRequestPolicy {
    Ignore,
    RedirectForRecipient { recipient_id: u64 },
}

pub(crate) enum PublicShareLinkResolution {
    Available { slug: Slug, link: ShareLink },
    PendingRequest { slug: Slug, request_id: RequestId },
}

pub(crate) enum ResolutionError {
    InvalidSlug,
    UnknownSlug,
    SlugMismatch,
    Inactive,
    Storage(storage::Error),
}
```

Map every `ResolutionError` to `WebError::NotFound` through a small helper.

- [ ] **Step 4: Run tests green**

Run: `cargo test -p web share_link_resolution`

Expected: PASS.

---

### Task 2: Duplicate Pending Policy

**Files:**
- Modify: `crates/web/src/share_link_resolution.rs`

- [ ] **Step 1: Write failing duplicate-pending tests**

Add resolver tests for these cases:

```rust
#[tokio::test]
async fn same_recipient_pending_request_returns_pending_request_outcome() { /* assert PendingRequest */ }

#[tokio::test]
async fn other_recipient_pending_request_does_not_redirect() { /* assert Available */ }

#[tokio::test]
async fn same_recipient_non_pending_request_does_not_redirect() { /* assert Available */ }

#[tokio::test]
async fn pending_lookup_storage_error_returns_storage_error() { /* assert ResolutionError::Storage */ }
```

- [ ] **Step 2: Run failing tests**

Run: `cargo test -p web share_link_resolution`

Expected: FAIL because duplicate-pending handling still returns `Available` or ignores storage errors.

- [ ] **Step 3: Implement duplicate-pending scan**

Use `Storage::list_requests_for_link(link.id)` only when policy is `RedirectForRecipient { recipient_id }`. Return `PendingRequest { slug, request_id }` when a request has the same `requester_id` and `RequestState::Pending`; return `ResolutionError::Storage` on lookup failure.

- [ ] **Step 4: Run tests green**

Run: `cargo test -p web share_link_resolution`

Expected: PASS.

---

### Task 3: Route Integration

**Files:**
- Modify: `crates/web/src/routes/invitation.rs`
- Modify: `crates/web/tests/invitation_resolution.rs`

- [ ] **Step 1: Write failing HTTP-level route tests**

Add route tests for:

```rust
#[tokio::test]
async fn request_form_ignores_other_recipient_pending_request() { /* assert 200 */ }

#[tokio::test]
async fn request_form_ignores_same_recipient_declined_request() { /* assert 200 */ }

#[tokio::test]
async fn submit_ignores_other_recipient_pending_request_and_sends_command() { /* assert command recorded */ }
```

- [ ] **Step 2: Run failing route tests**

Run: `cargo test -p web --test invitation_resolution`

Expected: FAIL until routes use the top-level resolver result API and the tests compile against the updated behavior.

- [ ] **Step 3: Update routes**

Use `crate::share_link_resolution::{resolve_public_share_link, PendingRequestPolicy, PublicShareLinkResolution}`. Landing handles only `Available`; request form and submit handle both `Available` and `PendingRequest`, converting pending data to `Redirect::to(&format!("/i/{}/pending/{}", slug.as_str(), request_id))`.

- [ ] **Step 4: Run route tests green**

Run: `cargo test -p web --test invitation_resolution`

Expected: PASS.

---

### Task 4: Final Verification

**Files:**
- All changed files

- [ ] **Step 1: Format**

Run: `cargo fmt`

Expected: no formatting errors.

- [ ] **Step 2: Focused tests**

Run: `cargo test -p web share_link_resolution && cargo test -p web --test invitation_resolution`

Expected: PASS.

- [ ] **Step 3: Workspace tests**

Run: `cargo test --workspace`

Expected: PASS.
