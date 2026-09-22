---
status: accepted
date: 2026-09-22
---

# Core owns every SQL statement; storage adapters only bind and execute

ghinvite runs on two databases: SQLx over SQLite natively
(`crates/ghinvite-storage-sqlx`) and Cloudflare D1 in production
(`crates/ghinvite-storage-d1`). Both are SQLite, so the query text, row shapes
and failure classification are the same problem twice. We put all of it on the
core side of the seam.

## Decision

`crates/ghinvite-core/src/storage/` owns every SQL statement, every row type,
paging assembly (audit log and request history pages) and constraint
classification (`classify_insert`, projection `classify`). Each driver presents
a result row as a JSON object of its column values and hands it to core's
`decode_row`; an undecodable row is `Error::Corrupt`. The adapters bind
parameters, run the statement and return rows or rows-changed. They contain no
SQL of their own and no domain decisions.

The port is split by caller role: `RecordStorage`, `ConsoleStorage`,
`ContinuationStorage`, `WebhookStorage`, `InstallationStorage`,
`DeliveryStorage` and `AuditStorage`, plus `ProjectionStorage` for the
projection batch. `Storage` combines them for adapters, the conformance suite
and composition roots. Callers depend on bundles of the parts they use:
`WebStorage` (`crates/ghinvite-web/src/state.rs`) and `WorkflowStorage`
(`crates/ghinvite-workflows/src/state.rs`). The web bundle omits delivery, installation and
audit writes, which belong to workflows.

## Rejected: a generic executor port

A port of `execute` / `query` / `batch` taking JSON parameters everywhere would
shrink each adapter to a few functions. We rejected it:

- Every statement would read parameters through `json_extract`, which makes
  queries harder to read and review.
- `json_object` rows and JSON parameters move type errors from bind time to
  runtime.
- With statements and decoding already shared in core, no duplication is left
  to justify it.

The projection batch keeps its single JSON parameter
(`crates/ghinvite-core/src/storage/projection/sql.rs`): it is one fixed
multi-statement transaction, and one parameter avoids lossy JavaScript number
binding there. That exception does not extend to other statements.

## D1 constraints the adapter must keep

- `worker`'s `D1Result::results::<T>()` panics when a row fails to
  deserialize. The D1 adapter reads `serde_json::Value` first and decodes
  through `decode_row`, so a corrupt row becomes an error, not a Worker panic.
- D1 binds numbers as JavaScript doubles. The adapter refuses to bind a `u64`
  above 2^53 − 1 rather than store a rounded value.

## Parity guard

The conformance suite (`crates/ghinvite-core/src/storage/test_suite.rs`) runs
against both adapters: natively through
`crates/ghinvite-storage-sqlx/tests/sqlx_suite.rs`, and against D1 in workerd
through `npm run test:storage` in `tests/worker`. A behaviour difference between
the adapters is a suite failure, not a code-review finding.
