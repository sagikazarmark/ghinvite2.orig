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
- No `CHECK (col IN (...))` on enum-shaped columns (see spec §7.2). Domain enums in `crates/domain` validate values before write.
- Timestamps are ISO-8601 strings stored in `TEXT` columns, with chrono's default `to_rfc3339()` format.

## Adding a new migration

1. Choose the next `NNNN`.
2. Write `NNNN_purpose.sql` containing only `CREATE TABLE`, `CREATE INDEX`, `ALTER TABLE ADD COLUMN`, and `DROP INDEX` statements that are SQLite-portable.
3. **Avoid altering existing CHECK constraints** — SQLite cannot do this without a 12-step `ALTER TABLE` recreate. If unavoidable, write the recreate dance in plain SQL inside the migration file.
4. Run `cargo test -p storage` locally to verify migrations apply cleanly to a fresh in-memory DB.
5. In Plan 7 (D1 deployment), `wrangler d1 migrations apply ghinvite --local` first; never go straight to `--remote`.
