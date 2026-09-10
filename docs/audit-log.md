# Console Audit Log

`GET /console/accounts/{login}/audit` shows recorded events for the authorized
account ID, including previous installations. Current Console authorization is
required on every request. Audit responses use `Cache-Control: private, no-store`.

## URLs and live history

- No `event` means All events. Otherwise use one exact persisted event type.
  Unknown values normalize to All events.
- `before` seeks older than a row; `after` seeks the nearest newer rows. Both
  use exclusive timestamp/ID boundaries. Pages contain at most 25 rows and are
  displayed newest first. Navigation is based on bounded existence probes.
- Cursors are version-1 URL-safe base64 JSON containing the full UTC timestamp,
  audit event ID, and normalized event filter. They carry no account authority.
  Invalid, unsupported, over-512-byte, filter-mismatched, or conflicting cursors
  normalize to latest. Duplicate recognized keys use the last value; unrelated
  parameters are ignored. Applying the native GET filter drops both cursors.
- Back to latest and retry preserve the normalized filter. An empty cursor range
  is distinct from empty account history, no filter matches, and a load error.

This is live operational history, not a snapshot or a completeness guarantee.
Front arrivals do not shift older pages; delayed events can appear behind a
cursor. Names are locally last known, not historical identity snapshots.

## Read and presentation boundaries

The storage port owns bounded reads and indexed timestamp/ID pagination on both
SQLx and D1 (see `migrations/README.md`). No debug full-history reader is enabled
for production. Up to 25 distinct actors and 25 distinct invitation-link targets
are enriched through local point reads, deduplicated within the visible page.
Target enrichment reads only identity/account membership, not repository lists.
Optional enrichment failures fall back to IDs; core read/decoding failures fail
the whole page with HTTP 500.

Only safe event-specific metadata summaries enter UI rows. Private metadata
values, arbitrary JSON, invitation codes, upstream errors, and correlation IDs
are not rendered. Browsing calls no workflow commands and does not emit events.
GitHub calls are limited to existing authorization. Controls, table, and
navigation are server-rendered and work without JavaScript.

## Verification

- Shared storage conformance covers account isolation, exact filtering, bounded
  pages, exclusive forward/backward seeks, equal-time IDs, nanoseconds, and new
  arrivals. SQLite fixtures cover historical mixed encodings and corrupt rows.
- Console HTTP tests cover authorization, reinstall history, URL normalization,
  navigation, safe summaries/resources, privacy, enrichment failures, and errors.
- The opt-in local D1 suite executes the shared production SQL builder against
  Wrangler D1, including mixed encodings, both directions, exact page boundaries,
  filtering/account isolation, and expression-index seek plans. It does not run
  the Rust Worker decoder end-to-end; Workers/Wasm checks verify that integration
  compiles. Run instructions are in `migrations/README.md`.
