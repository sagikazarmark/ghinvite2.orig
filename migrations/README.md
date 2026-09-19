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
- Timestamps are ISO-8601 strings stored in `TEXT` columns, with chrono's default `to_rfc3339()` format. Versioned projection batches use Chrono's equivalent UTC Serde encoding (`Z`), consistently for both writes and content checks.

### Audit ordering

Migration 0003 adds account/order and account/event/order expression indexes.
Both audit writers serialize `DateTime<Utc>` with variable fractional precision
and a `+00:00` suffix. The comparison key also handles equivalent `Z` timestamps:
it right-pads the fractional seconds to nine digits without rounding. This avoids
a stored-data normalization migration and preserves original event timestamps.
Do not replace it with SQLite `datetime`/`julianday` (millisecond precision), or
raw text sorting. Keep the expression in `storage::audit_read` and 0003 identical.
Non-UTC offsets are not a format emitted by either audit writer.

The scalar seek bound plus exclusive `(time key, id)` boundary uses these indexes
without a temporary sort. SQLite tests assert query-plan index/seek use; targeted
D1 runtime tests execute the same query builder, including mixed encodings:

```sh
wrangler d1 migrations apply ghinvite --local --config wrangler/web.toml
cargo test -p ghinvite-storage-d1 --features d1-suite --test d1_suite -- --ignored
```

## Adding a new migration

Migration 0008 stores encrypted admin browser continuations separately from
authentication sessions. A unique session/account scope plus logical binding
atomically retains the first submitted input, including on D1. These are
recovery records, not authoritative business receipts. Their `expires_at`
integer is a Unix-second browser-session deadline (like session storage), not a
domain timestamp. Reads exclude expired records. Each retention call deletes at
most 100 expired continuations across all sessions, using the expiry index from
migration 0009, in both SQLite and D1. Cleanup runs with mutation traffic and
does not affect authoritative Restate receipts or extend live session deadlines.

Migration 0004 adds v1 projection revisions, content/identity checks, deadlines,
and audit identities. Its `projection_assertions` table is transient within each
SQLx transaction/D1 batch: named CHECK failures abort the entire application,
and successful batches remove the assertion rows before commit. NULL revisions
remain legacy-owned. The legacy pending-only uniqueness guard excludes versioned
rows so reordered projections can converge without becoming admission authority.
See [projection repair](../docs/admission-v1.md#inspection-repair-and-redrive).

1. Choose the next `NNNN`.
2. Write `NNNN_purpose.sql` containing only `CREATE TABLE`, `CREATE INDEX`, `ALTER TABLE ADD COLUMN`, and `DROP INDEX` statements that are SQLite-portable.
3. **Avoid altering existing CHECK constraints** — SQLite cannot do this without a 12-step `ALTER TABLE` recreate. If unavoidable, write the recreate dance in plain SQL inside the migration file.
4. Run `cargo test -p ghinvite-storage-sqlx` locally to verify migrations apply cleanly to a fresh in-memory DB.
5. Rehearse `wrangler d1 migrations apply DB --local --config wrangler/web.toml`, then the disposable remote D1 gate before the operator-owned production procedure.
