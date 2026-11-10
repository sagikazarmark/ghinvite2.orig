# ghinvite migrations

Plain SQL files, one schema-change per file, applied in lexicographic order.

The same files are applied by:
- **Local dev (sqlx):** `sqlx migrate run --source migrations/ --database-url sqlite:./dev.sqlite`
  - sqlx tracks applied migrations in a table called `_sqlx_migrations`.
- **Local D1 simulation:** `wrangler d1 migrations apply DB --local --config wrangler/web.toml`
- **Remote (Cloudflare D1):** `wrangler d1 migrations apply DB --remote --config wrangler/web.toml`
  - wrangler tracks applied migrations in a table called `d1_migrations`.

**These are different tables.** Migrations applied via one tool are invisible to the other.
Treat the migration history as advisory only across backends; the schema itself is the source of truth.
Follow the [remote migration and binding verification procedure](../docs/deploy.md#3-apply-and-verify-remote-database-migrations).
Both Workers must bind the intended shared database UUID. Local success and a
passing `/health` do not establish that the remote schema exists. Production
rollout remains blocked on the operator-owned #61 gates.

## Foreign keys

Native SQLite sets `SqliteConnectOptions::foreign_keys(true)` to enforce
`REFERENCES` constraints. [D1 enforces foreign keys by default](https://developers.cloudflare.com/d1/sql-api/foreign-keys/),
equivalent to `PRAGMA foreign_keys = ON`; queries/migrations run in implicit
transactions and cannot toggle that enforcement. If a migration needs temporary
deferral, use `PRAGMA defer_foreign_keys = ON` and resolve violations before the
transaction ends. Verify deployed data with `PRAGMA foreign_key_check`.

## Conventions

- Files named `NNNN_short_description.sql` where `NNNN` is a zero-padded sequence number starting at `0001`.
- SQLite-portable SQL only: no Postgres-isms, no `WITHOUT ROWID`, no `STRICT`.
- No `CHECK (col IN (...))` on enum-shaped columns (see spec §7.2). Domain enums in `crates/ghinvite-core` validate values before write.
- Timestamps are ISO-8601 strings stored in `TEXT` columns, with chrono's default `to_rfc3339()` format. Projection batches use Chrono's equivalent UTC Serde encoding (`Z`), consistently for both writes and content checks.

### Audit ordering

The account/order and account/event/order audit indexes use an expression key.
Both audit writers serialize `DateTime<Utc>` with variable fractional precision
and a `+00:00` suffix. The comparison key also handles equivalent `Z` timestamps:
it right-pads the fractional seconds to nine digits without rounding. This avoids
a stored-data normalization migration and preserves original event timestamps.
Do not replace it with SQLite `datetime`/`julianday` (millisecond precision), or
raw text sorting. Keep the expression in `storage::audit_read` and the indexes identical.
Non-UTC offsets are not a format emitted by either audit writer.

`idx_request_history` applies the same timestamp normalization to request admission
history, indexed by `(invitation_link_id, normalized-time/ID DESC)`. The single
fixed-width key seeks past large equal-timestamp groups without scanning their
earlier IDs.
Keep its expression identical to `storage::request_history`. Both storage
adapters use that shared query with an exclusive cursor and a 26-row limit (25
displayed plus one lookahead); account authorization is independent of the cursor.
Requester logins are optional enrichment in that same statement, with indexed
user-ID lookups for the bounded page rather than separate per-request queries.

The scalar seek bound plus exclusive `(time key, id)` boundary uses these indexes
without a temporary sort. SQLite tests assert query-plan index/seek use; the D1 storage gate executes the
same query builder through the adapter, including mixed encodings, and asserts
D1's plans:

```sh
npm run test:storage --prefix tests/worker
```

### Pending queue

`invitation_requests.queue_account_id` is the pending queue's derived key, taken
from the owning invitation link. Portable SQLite triggers maintain it for every
projection write (including link/request relationship
updates and restoration of missing links). This is an indexing key, never an authorization source: the read also
joins the current owning link and checks its account. An orphan link cannot
establish account ownership and is excluded, as with the previous queue query.
The partial index excludes terminal history and orders pending requests by an
exact composite UTC-time/request-ID text key. Using one expression lets SQLite
seek past a large equal-timestamp pending prefix, rather than filtering its IDs
after seeking the timestamp alone. Keep its expression identical to
`storage::pending_queue`; it follows the audit normalization rules above.

Each queue read executes one statement, materializing at most 26 candidate
requests (25 displayed plus one lookahead) before loading context. Repository
scopes are loaded once per distinct page link. The final bounded page may use a
temporary sort; candidate selection must use the account-local seek index.
SQLx tests count actual query events and inspect both initial/continuation plans.
`npm run test:queue --prefix tests/worker` observes actual D1 statements, plans,
and rows-read metadata through the authenticated Worker routes. Queue cursors
are value boundaries, so a decided boundary row need not still exist; new or
late-projected earlier requests appear on returning to the oldest page.

The console overview's pending count (`storage::pending_queue::COUNT_QUERY`) is
one `COUNT(*)` statement with the same scoping (pending state, the
`queue_account_id` seek, and the current owning link's account), so it counts
exactly the rows the queue pages through without loading any of them.

### Attempt continuations

`attempt_continuations` stores encrypted browser continuations of in-flight
console mutations and invitation requests separately from authentication
sessions. A unique session-derived scope plus logical binding atomically retains
the first submitted input, including on D1. A scope lists in retention (rowid)
order. These are
recovery records, not authoritative business receipts. Their `expires_at`
integer is a Unix-second browser-session deadline (like session storage), not a
domain timestamp. Reads exclude expired records. Each retention call deletes at
most 100 expired continuations across all sessions, using the `attempt_continuations_expiry`
index, in both SQLite and D1. Cleanup runs with mutation traffic and
does not affect authoritative Restate receipts or extend live session deadlines.

### Admission projection

Links and requests are written only by the projector. `projection_revision` is
the snapshot revision it last applied, so a stale snapshot cannot regress a row;
there is no SQL uniqueness on requests, because Restate alone owns admission.
Link `projection_content` retains the full snapshot: equal-revision content must
match, and creation input (including exact repository scope) must match at every
revision. Missing repository child rows may be repaired by a matching snapshot,
including a stale one; replay cannot expand scope or regress current link values.
Audit events keep their logical ID and content for immutability checks. The
`projection_assertions` table is transient within each SQLx transaction/D1
batch: named CHECK failures abort the entire application, and successful batches
remove the assertion rows before commit.
See [projection repair](../docs/admission-v1.md#inspection-repair-and-redrive).

### Approved request completion

Migration `0010_request_lapsed.sql` permits an approved request to become
`lapsed` without replacing its approval decision. It rejects direct raw
pending/missing-to-lapsed writes, reopening, approval actor/time changes, and
revision regression. The owner supplies its retained original approved
projection as `RequestProjectionEnvelope.approval`; both adapters apply that
predecessor and the completion in one atomic batch. Completion-first delivery
therefore repairs projection order without waiting for a message queued behind
an exclusive projector invocation. A late SQL failure rolls back both snapshots
and audit writes. Original approval replay after Lapsed cannot reopen the row.

Delivery `final_result` columns use the same typed completion classifier as the
request owner. They distinguish `declined`, `lapsed_invite`, `unfulfillable` and
`satisfied`; unfinished targets remain null. Existing deployment data from an
older classifier needs a separately verified derived-column backfill before
upgrade: fresh-schema tests do not constitute that migration evidence, and an
unchanged organization receipt is not automatically reprojected.

## Adding a new migration

1. Choose the next `NNNN`.
2. Write `NNNN_purpose.sql` using SQLite-portable schema statements. Derived indexing keys may use a data backfill and `CREATE TRIGGER` maintenance, as for `queue_account_id`; verify these on both SQLx and D1.
3. **Avoid altering existing CHECK constraints** — SQLite cannot do this without a 12-step `ALTER TABLE` recreate. If unavoidable, write the recreate dance in plain SQL inside the migration file.
4. Run `cargo test -p ghinvite-storage-sqlx` locally to verify migrations apply cleanly to a fresh in-memory DB.
5. Rehearse `wrangler d1 migrations apply DB --local --config wrangler/web.toml`, then the disposable remote D1 gate before the operator-owned production procedure.


`0011_email_domains.sql` validates and freezes the existing nullable domain-policy
and evidence columns. Earlier unrestricted rows keep NULL policy (equivalent to
`[]`) and no evidence. Restricted requests require matched-domain evidence bound
to the original admission time; later approval/completion never refreshes it.
The typed core validates IDNA and exact timestamps; raw SQL guards add bounded
JSON shape, parent-rule matching and immutability. This local fresh-schema proof
is not a verified upgrade of arbitrary previously populated domain columns.

`0012_delivery_audit_version.sql` adds validation and immutability triggers to
`delivery_outcomes`. Missing versions remain Legacy; existing rows are neither
rewritten nor reinterpreted. Valid explicit versions are `legacy` and
`canonical_v1`; null, unknown and non-string values are rejected. The typed
projection also rejects version changes in stale envelopes that would otherwise
skip an SQL update. New owners select the canonical version only before their
first retained state. This local migration does not replace endpoint draining or
a rehearsal against a populated deployed database.

0013_organization_telemetry.sql adds per-organization/team observation storage
without changing repository telemetry or receipt JSON. Its operational view
exposes each independent step. Historical blocked-digest items are copied
unchanged into the new (day,delivery_key,step) identity as repo steps, including
their exact original day/account/reason. The native upgrade fixture applies all
prior migrations before this change; fresh SQLite and actual D1 run the same
schema and unified report/digest scenarios. Apply the migration before deploying
the new reader or scheduled recorder. This is local upgrade evidence, not a
rehearsal against a populated deployed database.

### Retired repository member-event matching

`0022_remove_member_webhook_facade.sql` drops only the unused `webhook_receipts`
table. Current repository webhook ingress ignores member and repository-invitation
events; retained owner reconciliation supplies settlement evidence. The independent
`organization_webhook_receipts` table, delivery ULID bridge, fences, and audit
identities are unchanged. Existing migration contents/checksums are preserved;
this additive local migration does not claim a deployed rollout.

Before applying0022 to any deployed database, operators must inventory every
serving and pinned endpoint version sharing that database. Older web/workflow
artifacts that still issue member-receipt SQL must be drained or routed away
before the table is dropped; absence of callers in this checkout does not prove
absence in deployed versions. Endpoint pinning alone does not make a shared
schema drop safe. This removes obsolete routing/deduplication receipts, not Audit
Log history: `audit_events` and organization webhook receipts are preserved.
Legacy private delivery handler wires remain a separate compatibility surface.
No remote migration, drain, reset, or deployment was executed for this change.

### Canonical delivery identity

`0023_canonical_delivery_identity.sql` rebuilds `deliveries` without its
transitional invitation-ULID column and removes the old ULID expression indexes
on write fences. It copies every other delivery column directly, preserves
child rows and receipt/audit/fence JSON, and reinstates the existing delivery
guards, organization parent guard and operational view. The stored physical
delivery key and upstream numeric GitHub invitation ID keep their meanings.
Legacy audit targets that already contain a physical delivery key remain valid;
old ULID targets resolve through the exact retained receipt compatibility field.

The preflight refuses a bridge-only legacy audit binding without that exact
receipt. Recover the original receipt through its compatible retained owner
before retrying; do not invent or backfill an invitation identity. This refusal
runs before the source table is dropped. Apply the complete migration in one
transaction, as in SQLx migrations or a D1 transactional batch, with deferred
foreign-key enforcement. The final guard checks all foreign keys before closing
deferral. See [SQLite rebuild ordering](https://www.sqlite.org/lang_altertable.html)
and [D1 foreign keys](https://developers.cloudflare.com/d1/sql-api/foreign-keys/).

ADR0011 requires inventorying and draining or routing away every older web,
workflow, delayed producer and pinned endpoint that still accesses the shared
column or old handlers before applying the cutover. Immutable endpoint pinning
alone cannot protect an old shared-schema reader. New readers/producers, fence
bindings, audit-version guards and the migration are one coordinated boundary.
The local raw D1 proof is included in `test:storage`; typed shared-adapter and
owner replay gates remain separate. No deployed migration or drain is implied.

`0024_delivery_audit_v2.sql` adds the new retained receipt contract after the
physical-key cutover. `canonical_v2` requires the old `invitation_id` property to
be absent; Legacy and CanonicalV1 require its original valid ULID. A receipt's
version remains immutable. Historical receipt identity uniqueness is retained
through an index over the exact immutable JSON compatibility field, rather than
an operational identity column. New repository audit targets use
`repository_delivery` and the physical key. Legacy/V1 payload IDs, targets and
metadata are not rewritten. This migration belongs to the same coordinated
producer/endpoint/shared-schema boundary as 0023; it does not authorize replaying
old invocation histories through changed handlers.

### Continuation rotation epoch (0028)

The additive singleton epoch and INSERT/UPDATE/DELETE triggers invalidate operator scan cursors on every continuation mutation. Bounded scans include expired retained rows. Exact old-ciphertext/context/retention CAS rewrites only payload; successful updates advance the epoch. Direct-row `RETURNING rowid` establishes CAS success because D1 change metadata also counts trigger writes. Read-only verification must finish at a stable epoch with zero old and zero bad rows. The integer overflow guard rolls back the original write atomically. D1 scan/epoch reads explicitly use primary sessions. See [the rotation runbook](../docs/continuation-keyring-rotation.md); do not reset the epoch or disable triggers during a pass.
