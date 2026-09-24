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
projection batch. `Storage` combines the seven through a blanket impl, and the
conformance suite is what names it; the adapters implement the parts
individually and receive `Storage` without asking for it. Callers depend on
bundles of the parts they use: `WebStorage`
(`crates/ghinvite-web/src/state.rs`) and `WorkflowStorage`
(`crates/ghinvite-workflows/src/state.rs`), and those are the bundles the
composition roots name. The web bundle omits delivery, installation and
audit writes, which belong to workflows.

## Rejected: a generic executor port

A port of `execute` / `query` / `batch` taking JSON parameters everywhere would
shrink each adapter to a few functions. We rejected it:

- Every statement would read its parameters through `json_extract`, which makes
  the queries harder to read and review. This is an objection to JSON
  parameters, not to a generic executor as such: a port taking a closed typed
  parameter vector, bound positionally to the `?1..?n` the statements already
  carry, would leave the SQL exactly as it is. That variant was not evaluated.
- JSON parameters move type errors from bind time to runtime.

A third reason stood here and has been struck: that no duplication was left to
justify the port. There is duplication, it is measurable, and the amendment
below measures it.

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
the adapters is a suite failure, not a code-review finding — for the behaviour
the suite reaches. That qualification was missing when this was written, and
what the suite reaches was smaller than the sentence implied; the amendment
below records what it covers now and what it still does not.

## Amendment (2026-09-23): four claims corrected, and the parity guard's reach

The decision stands. Core owns the SQL; the adapters bind and execute. Four
statements made in support of it were wrong or overstated and are corrected
above. They are recorded here rather than quietly edited away, because a
document whose whole argument is that the compiler checks none of this has no
business being unchecked itself.

**JSON rows were adopted and rejected in the same document.** The Decision
section has each driver present a row as a JSON object and hand it to
`decode_row`; the second rejection bullet then cited `json_object` rows as a
reason against the executor port. Both adapters do what the Decision says:
`crates/ghinvite-storage-d1/src/wasm_impl.rs:99-112` reads
`serde_json::Value` and decodes afterwards, and
`crates/ghinvite-storage-sqlx/src/lib.rs:31-58` assembles the same JSON object
column by column so that SQLx presents rows the way D1 does. Core therefore
deserializes at runtime either way, and on *type safety* the two designs are
level: that half of the bullet argued against this ADR's own decision. It now
claims only the parameters half. The distinction between constructing JSON in
SQL with `json_object` and presenting a driver's row as JSON is real, and is
why the adopted form is cheap — it is simply not a type-safety distinction.

**The first bullet described a design nobody proposed.** `json_extract`
follows from JSON parameters, not from a generic executor. A port taking a
closed typed parameter vector bound positionally would leave every statement
untouched, and no one weighed it. The bullet now says which port it rejects.

**There is duplication, and it is the largest thing this ADR left unsaid.**
"With statements and decoding already shared in core, no duplication is left to
justify it" is false. Measured at this commit: the SQLx adapter holds 84
`.bind(` calls in its production impl — excluding its `#[cfg(test)]` module
and its `test-util` `debug_*` helpers — against the D1 adapter's 83
top-level `int`/`text`/`time`/`nullable` parameter constructions. The same
values, in the same order, for the same statements, written twice.
`AuditStorage::audit` binds the same ten
values in the same order in both
(`crates/ghinvite-storage-sqlx/src/sqlx_impl.rs:598-614`,
`crates/ghinvite-storage-d1/src/wasm_impl.rs:646-665`).
`SqlxStorage::transaction` and `D1Storage::batch` have identical signatures,
down to `classify: fn(String) -> Error`, and differ only in name
(`sqlx_impl.rs:194-199`, `wasm_impl.rs:122-127`). `list_audit_events` repeats
the same 31-line two-probe control flow in both (`sqlx_impl.rs:341-371`,
`wasm_impl.rs:347-377`). The bullet is struck rather than rewritten: the
duplication it denied cannot be turned into a reason to keep it.

The rejection still holds on the two remaining reasons plus cost. The
migration would rewrite every bind list in both adapters, and until the gap
`bd59b78` closed — see below — it could not have been verified, because the
suite did not assert the failure classification such a rewrite would most
easily break. A
typed-parameter port therefore remains conceivable future work rather than a
closed question. What would have to be true first: the parity guard covering
classification and the not-found paths, which it now does; and a concrete
accounting of what a typed vector costs at the two places the adapters are
genuinely not the same shape — D1's refusal to bind a `u64` above 2^53 − 1,
and the projection batch's single JSON parameter.

**`Storage` serves one caller, not three.** `dyn Storage` and `: Storage` have
no production use anywhere in the workspace. The composition roots name
`Arc<dyn WebStorage>` and `Arc<dyn WorkflowStorage>`
(`crates/ghinvite-web-server/src/main.rs`,
`crates/ghinvite-web-worker/src/lib.rs`,
`crates/ghinvite-workflows-server/src/main.rs`,
`crates/ghinvite-workflows-worker/src/lib.rs`). Adapters implement the seven
parts individually and receive `Storage` from the blanket impl without ever
naming it. Only the conformance suite and the projection fixture, both behind
the `test-suite` feature, take it as a bound. The doc comment on the trait
(`crates/ghinvite-core/src/storage/mod.rs:102-103`) still carries the old
wording; this ADR no longer does.

**What the parity guard reaches now.** `bd59b78` added
`scenario_insert_conflict_kinds` and `scenario_installation_update_not_found`
to the suite. Before it, the suite asserted only `Error::Database` and
`Error::ProjectionInvariant` — never `Conflict`, `NotFound` or `Corrupt` — so
`classify_insert`, the piece of failure classification this ADR puts in core
precisely because both drivers depend on it, sat outside the guard entirely.
"A behaviour difference between the adapters is a suite failure" was overstated
when it was written. Both new scenarios pass against SQLx and against real D1
in workerd with no divergence found, so agreement that was assumed is now
recorded. `92b6d17` renamed
`insert_failures_classify_by_sqlite_text_whatever_the_driver_prefix` to
`a_failed_insert_classifies_by_the_constraint_named_in_its_message`: it feeds
the classifier strings rather than a driver, so it pins the message text, and
driver independence is what the new scenario pins.

`Error::Corrupt` remains uncovered, and deliberately so. It is raised by
`decode_row` on a row that does not decode, and every trait writer encodes
through `encode_selected_repos`, so no sequence of trait calls can plant an
undecodable row. The D1 constraint above — that a corrupt row becomes an error
rather than a Worker panic — is held by the adapter's structure and by review,
not by the suite.
