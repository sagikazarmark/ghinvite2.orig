# Account Admin Request Queue Reads Design

## Goal

Deepen Account Admin dashboard reads so route handlers ask for Account Admin read models instead of rebuilding Invitation Request, Share Link, requester, and ownership-sensitive authorization chains inline.

This preserves current dashboard behavior while localizing the lookup rules for pending request queues, request decisions, share-link detail pages, and share-link revocation.

## Current State

`crates/web/src/routes/dashboard.rs` currently assembles Account Admin request data directly in route handlers.

The pending request queue calls `Storage::list_pending_requests_for_account`, then performs one Share Link lookup and one GitHub User lookup per row to produce `PendingRequestRow` values. It also owns the display fallbacks for missing related rows:

- Missing Share Link displays `(deleted link)` and uses an empty link id.
- Missing GitHub User displays `user-{requester_id}`.

Approve and decline handlers parse the request id, load the Invitation Request, load the Share Link, confirm the Share Link belongs to the current account, and return generic `404 Not Found` for every missing, malformed, or wrong-account path.

Share-link detail and revoke handlers repeat the same ownership-sensitive Share Link lookup pattern for `ShareLinkId` path parameters.

The behavior is correct, but the Account Admin read behavior is shallow: callers must remember the lookup order, fallback behavior, and generic not-found policy each time.

## Chosen Approach

Add a focused read module inside `crates/web`, likely `crates/web/src/account_admin_reads.rs`.

This is the smallest useful boundary for issue #5 because the read models are currently consumed by Web Worker dashboard routes. It avoids widening `storage::Storage`, avoids touching the D1 adapter contract, and keeps product behavior unchanged.

The module should expose Account Admin terms rather than storage-table terms:

- `pending_request_queue(storage, account_id) -> Result<Vec<AccountAdminPendingRequestRow>>`
- `find_account_admin_request(storage, account_id, request_id) -> Result<AccountAdminInvitationRequest>`
- `find_account_admin_share_link(storage, account_id, link_id) -> Result<ShareLink>`

Exact names can be adjusted during implementation, but route handlers should no longer reconstruct these chains themselves.

## Detailed Design

The new read module depends on `storage::Storage` and domain types. It does not write state, call Restate, call GitHub, or render HTML.

`AccountAdminPendingRequestRow` should carry the route-ready queue fields currently needed by `views::requests::PendingRequestRow`:

- `request_id: RequestId`
- `link_slug: String`
- `link_id: Option<ShareLinkId>`
- `requester_login: String`
- `justification: Option<String>`
- `created_at: DateTime<Utc>`

Using `Option<ShareLinkId>` inside the read model keeps missing related Share Links explicit. The route can convert `None` to the existing empty string when building the view row, preserving the current UI contract.

`AccountAdminInvitationRequest` should carry the loaded `InvitationRequest` and its owning `ShareLink`. Approve and decline routes only need the request id after lookup, but returning both records makes the authorization proof visible at the read boundary and gives tests a useful successful lookup assertion.

`pending_request_queue` should:

- List pending Invitation Requests for the Account.
- Hydrate each row with Share Link and GitHub User display data.
- Preserve the existing explicit missing-record fallbacks.
- Return rows oldest-first.

`find_account_admin_request` should:

- Load an Invitation Request by id.
- Load its Share Link.
- Confirm the Share Link belongs to the Account Admin's current Account.
- Return generic `WebError::NotFound` for missing request, missing Share Link, or wrong-account access.

`find_account_admin_share_link` should:

- Load a Share Link by id.
- Confirm the Share Link belongs to the Account Admin's current Account.
- Return generic `WebError::NotFound` for missing link or wrong-account access.

`dashboard.rs` should delegate to this module from:

- `link_detail`
- `revoke_link`
- `requests_queue`
- `approve_request`
- `decline_request`

The command calls for approve, decline, and revoke remain unchanged. The module only centralizes read-side lookup and display model construction.

## Error Handling And Security

Ownership-sensitive reads must preserve the existing generic not-found policy. Routes must not reveal whether a request or Share Link exists in another account.

Malformed path ids should continue to return `WebError::NotFound` at the route boundary before calling read functions.

Storage errors should continue to surface as internal errors except where current behavior intentionally tolerated missing related display data in the queue. The pending queue already treats missing Share Link and GitHub User records as displayable fallback cases; that behavior remains explicit in the read module.

The read module must not add fallback authorization behavior. For detail, revoke, approve, and decline flows, missing related rows are not display concerns; they remain generic not-found outcomes.

## Testing

Add tests at the Account Admin read-module seam first. Use in-memory `SqlxStorage` fixtures for normal queue assembly and ownership-sensitive lookups so tests exercise real storage behavior without going through HTTP for every case. Use a small fake `Storage` implementation for missing related-record fallback cases that the current SQLite schema and pending-request join do not naturally produce.

Cover:

- Queue rows include request id, Share Link slug/id, requester login, justification, and created timestamp.
- Queue rows are oldest-first.
- Missing Share Link in a queue row uses `(deleted link)` and no link id.
- Missing GitHub User in a queue row uses `user-{requester_id}`.
- `find_account_admin_request` returns the request for the owning account.
- `find_account_admin_request` returns `NotFound` for wrong-account access.
- `find_account_admin_request` returns `NotFound` when the related Share Link is missing.
- `find_account_admin_share_link` returns the Share Link for the owning account.
- `find_account_admin_share_link` returns `NotFound` for wrong-account access.

Update route tests only where useful to confirm handlers still map unauthorized Account Admin flows to `404 Not Found` and still invoke the expected command after a successful read. Avoid duplicating every read-module assertion at the HTTP layer.

Run:

```bash
cargo test -p web account_admin
cargo test -p web dashboard
cargo fmt --check
```

## Non-Goals

This change does not modify database schemas, storage trait methods, D1 adapter contracts, Restate payloads, command facade behavior, dashboard UI layout, GitHub API calls, or product-visible authorization policy.

It also does not attempt to remove all dashboard storage reads. Overview link counts, new-link repository listing, settings, and audit stubs remain outside this issue.
