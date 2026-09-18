# ghinvite migrations

Plain SQL files, one schema-change per file, applied in lexicographic order.

The same files are applied by:
- **Local dev (sqlx):** `sqlx migrate run --source migrations/ --database-url sqlite:./dev.sqlite`
  - sqlx tracks applied migrations in a table called `_sqlx_migrations`.
- **Production (Cloudflare D1):** `wrangler d1 migrations apply ghinvite --remote`
  - wrangler tracks applied migrations in a table called `d1_migrations`.

**These are different tables.** Migrations applied via one tool are invisible to the other.
Treat the migration history as advisory only across backends; the schema itself is the source of truth.

## Foreign keys

SQLite (and therefore D1) requires `PRAGMA foreign_keys = ON` per-connection to actually
enforce `REFERENCES` constraints. The crate sets this in `SqliteConnectOptions::foreign_keys(true)`
for sqlx, but **D1 connections set `foreign_keys = OFF` by default**. Plan 7 (D1 implementation)
must issue `PRAGMA foreign_keys = ON;` as the first statement on every connection.

Without this PRAGMA, FK violations silently succeed and orphan rows accumulate.

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
domain timestamp. Reads exclude expired records; expired rows can be deleted
without affecting authoritative Restate receipts or granting fresh authority.

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
5. In Plan 7 (D1 deployment), `wrangler d1 migrations apply ghinvite --local` first; never go straight to `--remote`.
