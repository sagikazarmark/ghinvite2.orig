# Console request history

From an invitation-link detail page, **Request history** opens
`/console/accounts/{login}/links/{link_id}/requests`. Each page contains at most
25 requests, newest admission first with request ID as the deterministic tie
breaker. **Older requests** uses an exclusive, link-bound keyset cursor;
**Latest requests** restarts browsing. Invalid cursors restart at the latest page.
New projections can arrive between page reads, so this is not a snapshot or a
claim that the history is complete.

`/console/accounts/{login}/requests/{request_id}` is the stable Console detail
route for pending and terminal requests. It uses the existing current,
immutable-GitHub-ID account-admin authorization. Foreign resources and requests
whose owning link cannot be established are concealed. Both pages are private,
non-cacheable reads. Requester profiles are optional enrichment; immutable user
IDs remain visible when a profile cannot be loaded.

History loads requester logins in its single bounded SQL statement. Its index
uses a fixed-width normalized-time/ID key, so deep pages seek past even large
equal-timestamp groups. A database-work regression checks a 10,000-row group;
query-count coverage checks that profile enrichment adds no round trips.

Details show projected admission/deadline/decision facts, admin-only justification
and decline reason, and the owning link's immutable repository scope and
permission. Delivery receipts and later GitHub invitation lifecycle records are
shown per repository. An absent outcome means that dispatch or projection may be
delayed, not that delivery failed. A failed delivery read is explicitly unavailable;
any retained facts from another successful read remain visible. These pages do
not query or claim authoritative dispatch progress.

Verification:

```sh
cargo test -p ghinvite-storage-sqlx --test sqlx_suite
cargo test -p ghinvite-storage-sqlx request_history_seeks_index
cargo test -p ghinvite-web --test console_flow request_history::
# From tests/worker (requires matching wasm-bindgen CLI):
npm run test:history
# From tests/browser:
npx playwright test --config request-history.config.mjs
```
