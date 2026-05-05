<!-- /autoplan restore point: /home/laborant/.gstack/projects/sagikazarmark-ghinvite2.orig/main-autoplan-restore-20260504-222550.md -->
# ghinvite — Plan 7: Workers + D1 Deployment

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Produce the production-ready Cloudflare Workers deployment: a wasm-compatible `D1Storage` backend that mirrors the full `Storage` trait, Workers entry points for both the `web` and `restate-svc` binaries, `wrangler.toml` configuration for both workers, a secrets scaffolding guide, and a verified end-to-end `wrangler deploy` run. After this plan ships the entire application can run on Cloudflare Workers + D1 with no native binary dependencies.

**Architecture:** Plans 1–6 produced a fully native application: axum + tokio, SQLite via sqlx, native session store. Plan 7 adds a wasm32 deployment path via two new thin crates: `crates/web-worker` and `crates/restate-svc-worker`. These crates depend on the existing `web` and `restate-svc` libs respectively, provide the `#[event(fetch)]` Workers entry point, and wire in the wasm32-specific glue (`D1Storage`, `KvSessionStore`, secrets loading). The existing `crates/web` and `crates/restate-svc` remain pure native Rust — no `#[cfg(wasm32)]` in their source, no `crate-type` changes. The only existing crate that gains wasm32-conditional code is `crates/storage`, where `sqlx` must be gated to native-only so the `Storage` trait compiles on wasm32 for `storage-d1`. The native path (local dev, CI, `SqlxStorage`) is preserved throughout — only new code is added; existing native tests must not regress.

**Tech stack additions:**
- `worker = "0.8"` (Cloudflare Workers Rust SDK, `wasm32-unknown-unknown` only) — brings `D1Database`, `Env`, `KvStore`, `#[event(fetch)]`
- `worker-build` CLI tool (`cargo install worker-build`) — wraps `cargo build --target wasm32-unknown-unknown` + `wasm-bindgen` for wrangler consumption
- `wasm-bindgen = "0.2"` (transitive via `worker`, but pinned for reproducibility)
- `console_error_panic_hook = "0.1.7"` — converts wasm panics to `console.error` calls visible in `wrangler tail`
- `serde-wasm-bindgen = "0.6"` — for D1 result deserialization
- `tracing-wasm = "0.2"` — routes `tracing` events to the browser/Workers console on wasm32 (replaces `tracing-subscriber` which does not compile to wasm32)

---

## Spec coverage

This plan implements:
- **§3 tech stack §Q5** — "Two wasm-targetable binaries; pluggable Storage trait (sqlx + D1)"
- **§7.3 D1Storage** — production impl of the full `Storage` trait using `worker::D1Database`
- **§20 open items** — validates Workers + Restate SDK compatibility; implements fallback path if incompatible
- **§17 testing strategy** — D1Storage compilation verified; wrangler dev smoke test noted (full D1 test suite deferred to Plan 8 per §17)

It does NOT implement:
- Automated CI deployment — Plan 8
- D1Storage parameterized test suite (same-harness run against `wrangler dev`) — Plan 8
- Custom domain setup — post-ship ops task

## File structure

| Path | Plan 7 action | Responsibility |
|---|---|---|
| `crates/storage/src/lib.rs` | Modify | Change `Error::Database(sqlx::Error)` → `Error::Database(String)` |
| `crates/storage/src/sqlx_impl.rs` | Modify | Adjust `From<sqlx::Error>` callsites to `.map_err(to_db_err)` |
| `crates/storage-d1/Cargo.toml` | Create | New crate, wasm32-only `worker` dep |
| `crates/storage-d1/src/lib.rs` | Create | `D1Storage` struct + `impl Storage` — all 20 methods |
| `crates/storage-d1/src/bind.rs` | Create | Helper module: D1 parameter binding + row deserialization |
| `Cargo.toml` (workspace) | Modify | Add `storage-d1`, `web-worker`, `restate-svc-worker` to members; add `worker`, `serde-wasm-bindgen` to workspace deps |
| `crates/restate-svc/Cargo.toml` | Modify | Add explicit `[lib]` section so `restate-svc-worker` can depend on it as a library |
| `crates/web-worker/Cargo.toml` | Create | Thin wasm32-only crate; `crate-type = ["cdylib"]`; depends on `web`, `storage-d1`, `worker` |
| `crates/web-worker/src/lib.rs` | Create | `#[event(fetch)]` + `KvSessionStore` + `config_from_env` |
| `crates/restate-svc-worker/Cargo.toml` | Create | Thin wasm32-only crate; `crate-type = ["cdylib"]`; depends on `restate-svc`, `storage-d1`, `worker` |
| `crates/restate-svc-worker/src/lib.rs` | Create | `#[event(fetch)]` + service wiring following PoC pattern |
| `wrangler/web.toml` | Create | Web Worker config: D1 binding, KV namespace, routes |
| `wrangler/restate-svc.toml` | Create | Restate-svc Worker config: D1 binding, Restate routes |
| `.dev.vars` | Create | Local wrangler dev env overrides (gitignored) |
| `.gitignore` | Modify | Add `.dev.vars` entry |
| `sqlx.toml` | Create | `version-format = "sequential"` |
| `docs/superpowers/plans/README.md` | Modify | Mark Plan 7 Implemented |

---

## Critical architectural decisions

### A1: `storage::Error::Database` variant type

**Current:** `Database(#[from] sqlx::Error)` — couples the error enum to sqlx, preventing `storage` from compiling on wasm32.

**Required change:** `Database(String)` — the error becomes an opaque description. `SqlxStorage` wraps sqlx errors via `.map_err(|e| Error::Database(e.to_string()))`. `D1Storage` wraps `worker::Error` the same way.

Impact is isolated to `storage/src/lib.rs` and `storage/src/sqlx_impl.rs`. All existing error matching in callers uses the variant shape (`Error::Database(_)`, `Error::NotFound`, `Error::Conflict(_)`) which is unchanged. Run `cargo test` after this change to confirm no regressions.

### A2: D1Storage in a new crate

`crates/storage-d1` is a separate workspace member rather than a feature flag inside `storage`. Rationale: `worker::D1Database` only exists on wasm32; mixing it into the existing crate via `#[cfg]` creates complicated compilation paths for every consumer. A separate crate cleanly isolates the wasm32 dependency — native crates depend on `storage` only, while the wasm32 web binary depends on both.

The `storage-d1` crate declares `worker` under `[target.'cfg(target_arch = "wasm32")'.dependencies]` so `cargo check` from a native host does not try to load the wasm-only SDK. Guard the entire `D1Storage` impl with `#[cfg(target_arch = "wasm32")]`.

### A3: D1 query style

`worker::D1Database::prepare(sql).bind(&[..]).first::<T>(None).await?` is the D1 query pattern. Parameters bind as `JsValue` via `wasm_bindgen::JsValue::from_*`. Results deserialize via serde (JSON bridge). There are no compile-time query macros.

For **atomic multi-statement operations** (`insert_share_link` needs link + repos rows together; `insert_invitation_request_and_increment_uses` needs request insert + uses_count increment): D1 GA batch API runs statements in a transaction (Cloudflare's current documentation states batch calls are "automatically wrapped in a transaction"). Use `d1.batch(vec![stmt1, stmt2, ...]).await?`. If at implementation time batch is found NOT to be atomic (verify against current Cloudflare docs when implementing), fall back to explicit SQLite transaction statements: run `"BEGIN"`, then the individual statements, then `"COMMIT"` — on error run `"ROLLBACK"`. The `D1Database::exec()` method accepts raw SQL strings for this purpose.

**Conflict detection** in D1: run/batch errors surface as JavaScript errors with messages like `UNIQUE constraint failed: share_links.slug`. Parse `worker::Error`'s message string to map to `ConflictKind`. **Limitation:** This approach is brittle against D1 error-message format changes. A helper `classify_d1_error(err: &worker::Error) -> storage::Error` lives in `bind.rs` and must be tested against each conflict scenario (covered by the parameterized test suite in Plan 8). Match on the most specific substring available (table.column name from the index definition, e.g. `"share_links.slug"`, `"invitation_requests"`, `"FOREIGN KEY"`), and default to `Error::Database(msg)` for anything unrecognized.

### A4: KV session store for Workers

`tower-sessions-sqlx-store` (used in the native binary) depends on `sqlx::SqlitePool` and cannot compile to wasm32. For Workers, implement `KvSessionStore` in `crates/web-worker/src/lib.rs` using `worker::kv::KvStore`. Because this lives in the dedicated worker crate, `crates/web` needs no changes — `tower-sessions-sqlx-store` stays as a plain dep there.

```rust
// Minimal interface needed by tower-sessions:
impl SessionStore for KvSessionStore {
    async fn load(&self, session_id: &Id) -> session::Result<Option<Record>>;
    async fn save(&self, record: &Record) -> session::Result<()>;
    async fn delete(&self, session_id: &Id) -> session::Result<()>;
}
```

KV key: `"session:{session_id}"`. Value: JSON-serialized `Record`. Expiry set from `record.expiry_date` using `KvStore::put(key, value).expiration(unix_ts)`.

### A5: Restate SDK wasm compatibility

The Restate Rust SDK (`restate-sdk = "0.10"`) wasm32 compatibility has been validated in a PoC by the project author: https://github.com/sagikazarmark/poc-restate-cloudflare-worker. Task 6 follows that PoC pattern to wire up the full service rather than exploring compatibility from scratch.

Spec §20's open question is resolved. The Workers path is the primary deployment target for both `web` and `restate-svc`. No VM fallback is needed.

---

## Task ordering

Tasks 1–2 are structural prerequisites (error type + D1 crate skeleton). Tasks 3–4 fill in all 20 Storage methods. Task 5 builds the web Workers entry point. Task 6 resolves the Restate wasm question. Tasks 7–8 write wrangler configuration. Task 9 sets up secrets and applies migrations. Task 10 does the end-to-end deploy.

---

### Task 1: Make `storage::Error` wasm-compatible

**Files:**
- Modify: `crates/storage/src/lib.rs`
- Modify: `crates/storage/src/sqlx_impl.rs`

**Why first:** Every subsequent task depends on `storage` compiling to wasm32. This is the smallest possible change that enables that.

- [ ] **Step 1: Change the variant**

In `storage/src/lib.rs`, change:
```rust
#[error("database error: {0}")]
Database(#[from] sqlx::Error),
```
to:
```rust
#[error("database error: {0}")]
Database(String),
```

Remove the `#[from]` attribute (no more blanket `From<sqlx::Error> for Error` impl).

- [ ] **Step 2: Add a free function helper**

Below the `Error` enum in `lib.rs`, add:
```rust
pub(crate) fn to_db_err(e: sqlx::Error) -> Error {
    Error::Database(e.to_string())
}
```
This is private to the crate; callers outside never construct `Error::Database` directly.

- [ ] **Step 3: Update SqlxStorage**

In `storage/src/sqlx_impl.rs`, there are three distinct patterns that must all be updated:

1. `.map_err(Error::Database)` → `.map_err(crate::to_db_err)`
2. Direct construction `Error::Database(e)` where `e: sqlx::Error` → `Error::Database(e.to_string())`
3. `?` on sqlx calls that relied on `From<sqlx::Error>` → add `.map_err(crate::to_db_err)?`

A migration error variant like `Error::Database(sqlx::Error::Migrate(Box::new(e)))` must become `Error::Database(format!("migrate: {e}"))` or `.map_err(|e| Error::Database(e.to_string()))` on the outer call. Use `cargo check -p storage` to locate every unresolved callsite — the compiler will point to each one.

- [ ] **Step 4: Gate sqlx behind native-only**

This step is mandatory before the wasm32 compilation can proceed. In `crates/storage/Cargo.toml`, move `sqlx` from `[dependencies]` to `[target.'cfg(not(target_arch = "wasm32"))'.dependencies]`:

```toml
# sqlx is native-only — wasm32 uses D1Storage (crates/storage-d1)
[target.'cfg(not(target_arch = "wasm32"))'.dependencies]
sqlx.workspace = true
```

In `crates/storage/src/lib.rs`, gate the sqlx-dependent items:
```rust
#[cfg(not(target_arch = "wasm32"))]
pub mod sqlx_impl;
#[cfg(not(target_arch = "wasm32"))]
pub use sqlx_impl::SqlxStorage;
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn to_db_err(e: sqlx::Error) -> Error {
    Error::Database(e.to_string())
}
```

- [ ] **Step 5: Verify**

```bash
cargo test -p storage
cargo test -p web
cargo test -p restate-svc
```
All tests must pass. Then:
```bash
cargo check -p storage --target wasm32-unknown-unknown
```
Expect: no errors. The storage crate now compiles on both targets: native (with sqlx) and wasm32 (trait + Error only).

---

### Task 2: `crates/storage-d1` skeleton + wasm32 smoke test

**Files:**
- Create: `crates/storage-d1/Cargo.toml`
- Create: `crates/storage-d1/src/lib.rs`
- Create: `crates/storage-d1/src/bind.rs`
- Modify: `Cargo.toml` (workspace)

**Goal:** Get a wasm32-compilable stub that implements all `Storage` trait methods with `unimplemented!()` bodies. Verify the crate compiles to wasm32 before writing any real SQL.

- [ ] **Step 1: Create `crates/storage-d1/Cargo.toml`**

```toml
[package]
name = "storage-d1"
version.workspace = true
edition.workspace = true
license.workspace = true
publish.workspace = true

[dependencies]
async-trait.workspace = true
audit.workspace = true
chrono.workspace = true
domain.workspace = true
serde.workspace = true
serde_json.workspace = true
storage.workspace = true
thiserror.workspace = true
ulid.workspace = true

[target.'cfg(target_arch = "wasm32")'.dependencies]
worker = { version = "0.8", features = ["d1", "kv"] }
serde-wasm-bindgen = "0.6"
wasm-bindgen = "0.2"
```

- [ ] **Step 2: Create `crates/storage-d1/src/bind.rs`**

Placeholder module with:
```rust
//! D1 parameter binding helpers + conflict-error classification.
#[cfg(target_arch = "wasm32")]
pub use wasm_impl::*;

#[cfg(target_arch = "wasm32")]
mod wasm_impl {
    use storage::{ConflictKind, Error};

    /// Convert a worker::Error to a storage::Error by inspecting the message string.
    pub fn classify_d1_error(e: worker::Error) -> Error {
        let msg = e.to_string();
        if msg.contains("UNIQUE constraint failed") {
            if msg.contains("share_links.slug") {
                return Error::Conflict(ConflictKind::DuplicateSlug);
            }
            // DuplicatePendingRequest: partial unique index covers BOTH columns.
            // Check both to avoid misclassifying PK collisions on invitation_requests.id.
            if msg.contains("invitation_requests.share_link_id")
                && msg.contains("invitation_requests.requester_id")
            {
                return Error::Conflict(ConflictKind::DuplicatePendingRequest);
            }
            // DuplicateActiveInstallation: partial unique index on installations.account_id
            if msg.contains("installations.account_id") {
                return Error::Conflict(ConflictKind::DuplicateActiveInstallation);
            }
            return Error::Conflict(ConflictKind::DuplicateId);
        }
        if msg.contains("FOREIGN KEY constraint failed") {
            return Error::Conflict(ConflictKind::ForeignKey);
        }
        Error::Database(msg)
    }
}
```

- [ ] **Step 3: Create `crates/storage-d1/src/lib.rs`**

```rust
//! Cloudflare D1 implementation of the Storage trait.
//! Only compiles on wasm32-unknown-unknown.

#[cfg(target_arch = "wasm32")]
mod wasm_impl;

#[cfg(target_arch = "wasm32")]
pub use wasm_impl::D1Storage;

pub mod bind;
```

Then create `crates/storage-d1/src/wasm_impl.rs`:
```rust
//! D1Storage: implements the full Storage trait using worker::D1Database.

use async_trait::async_trait;
use storage::Storage;
use worker::D1Database;

pub struct D1Storage {
    db: D1Database,
}

impl D1Storage {
    pub fn new(db: D1Database) -> Self {
        Self { db }
    }
}

#[async_trait(?Send)]  // wasm32 is single-threaded; use ?Send variant
impl Storage for D1Storage {
    // All 20 methods — stubs for now, filled in Tasks 3–4
    async fn insert_installation(&self, _account: &domain::Account) -> storage::Result<()> {
        unimplemented!("D1Storage::insert_installation")
    }
    // ... (repeat for all 20 methods)
}
```

**Note on `Send + Sync` bounds:** The `Storage` trait is defined as `pub trait Storage: Send + Sync + 'static`. `D1Database` wraps a JavaScript object and is not `Send` in Rust's type system. However, wasm32 is single-threaded and there is no actual thread-safety concern. The resolution is:

```rust
// wasm32 is single-threaded: no actual threads exist, so Send + Sync are vacuously true.
unsafe impl Send for D1Storage {}
unsafe impl Sync for D1Storage {}
```

Add these impls in `wasm_impl.rs`. The same pattern is required for `KvSessionStore` in `crates/web-worker/src/lib.rs` — `tower_sessions::SessionStore` has `Send + Sync + 'static` as supertraits, and `worker::kv::KvStore` is also `!Send + !Sync` as a JS wrapper:

```rust
// In crates/web-worker/src/lib.rs, after KvSessionStore definition:
unsafe impl Send for KvSessionStore {}
unsafe impl Sync for KvSessionStore {}
```

**Do not use `Rc` or restructure `AppState` — that would require pervasive changes to callers.** Apply the `D1Storage` unsafe impls first in Task 2 before writing Tasks 3–5, and the `KvSessionStore` impls in Task 5 Step 3, to confirm the full wasm32 compile path before the final binary check.

- [ ] **Step 4: Add to workspace**

In root `Cargo.toml`:
- Add `"crates/storage-d1"` to `[workspace] members`
- Add `storage-d1 = { path = "crates/storage-d1" }` to `[workspace.dependencies]`

- [ ] **Step 5: Smoke compile**

```bash
cargo check -p storage-d1 --target wasm32-unknown-unknown
```

Must compile without errors before proceeding to Task 3. Fix any wasm32 incompatibilities before moving on.

---

### Task 3: D1Storage — installations, users, share_links

**Files:**
- Modify: `crates/storage-d1/src/wasm_impl.rs`
- Modify: `crates/storage-d1/src/bind.rs`

**Implements:** 9 of the 20 Storage methods.

**D1 query pattern reference:**
```rust
// Single row:
let row: Option<MyRecord> = self.db
    .prepare("SELECT * FROM table WHERE id = ?1")
    .bind(&[JsValue::from_f64(id as f64)])?
    .first::<MyRecord>(None)
    .await
    .map_err(classify_d1_error)?;

// Multi-row:
let rows: Vec<MyRecord> = self.db
    .prepare("SELECT * FROM table WHERE account_id = ?1 ORDER BY created_at ASC")
    .bind(&[JsValue::from_f64(account_id as f64)])?
    .all()
    .await
    .map_err(classify_d1_error)?
    .results::<MyRecord>()
    .map_err(|e| Error::Corrupt(e.to_string()))?;

// Mutation (expecting 1 affected row):
let meta = self.db
    .prepare("UPDATE table SET col = ?1 WHERE id = ?2 AND condition IS NULL")
    .bind(&[JsValue::from_str(&val), JsValue::from_f64(id as f64)])?
    .run()
    .await
    .map_err(classify_d1_error)?;
if meta.rows_changed().unwrap_or(0) == 0 {
    return Err(Error::NotFound);
}

// Atomic batch (two statements, both succeed or both fail):
self.db
    .batch(vec![
        self.db.prepare("INSERT INTO ...").bind(&[...])?,
        self.db.prepare("UPDATE ... SET uses_count = uses_count + 1 WHERE id = ?1")
               .bind(&[...])?,
    ])
    .await
    .map_err(classify_d1_error)?;
```

**Row deserialization:** D1 returns JSON rows. Create serde-deserializable structs in `bind.rs` for each table (`InstallationRow`, `UserRow`, `ShareLinkRow`, `ShareLinkRepoRow`, etc.) and implement `From<InstallationRow> for Account`, etc. Match the column names exactly as defined in `migrations/0001_initial.sql`.

**DateTime serialization:** The schema stores timestamps as TEXT (`TEXT NOT NULL`). Use ISO8601 format (`chrono::DateTime<Utc>::to_rfc3339()`). Deserialize with `chrono::DateTime::parse_from_rfc3339`.

- [ ] **Step 1: Row types in `bind.rs`**

For each table, add a `#[derive(Deserialize)] struct <Table>Row { ... }` with field names matching the SQL column names exactly. Add `From<XRow> for X` / `TryFrom<XRow> for X` conversions that construct domain types and return `Error::Corrupt` if enum parsing fails.

- [ ] **Step 2: implement `insert_installation`**

Single `INSERT OR IGNORE` for the installation row. The unique index `idx_installations_active_account` will surface a conflict if another active row exists for the same `account_id`.

- [ ] **Step 3: implement `mark_installation_uninstalled`**

`UPDATE installations SET uninstalled_at = ?1 WHERE installation_id = ?2 AND uninstalled_at IS NULL`. Check `rows_changed == 0` → `Error::NotFound`.

- [ ] **Step 4: implement `update_installation_repos`**

`UPDATE installations SET selected_repos = ?1 WHERE installation_id = ?2 AND uninstalled_at IS NULL`. Serialize `SelectedRepos` to JSON string. Check `rows_changed == 0` → `Error::NotFound`.

- [ ] **Step 5: implement `get_installation`, `get_active_installation_by_account_id`, `get_active_installation_by_login`, `list_active_installations`**

Standard SELECT queries. Use the `InstallationRow` → `Account` conversion from bind.rs.

- [ ] **Step 6: implement `upsert_user`, `get_user`**

`INSERT INTO users ... ON CONFLICT(user_id) DO UPDATE SET login = excluded.login, avatar_url = excluded.avatar_url, last_seen_at = excluded.last_seen_at`.

- [ ] **Step 7: implement `insert_share_link`**

Requires atomicity: the share_link row and all share_link_repos rows must succeed or fail together. Use explicit `BEGIN`/`COMMIT`/`ROLLBACK` via `D1Database::exec()`:

```rust
self.db.exec("BEGIN").await.map_err(classify_d1_error)?;
// run INSERT INTO share_links + all INSERT INTO share_link_repos statements
// if any fails: self.db.exec("ROLLBACK").await?; return Err(...)
self.db.exec("COMMIT").await.map_err(classify_d1_error)?;
```

Build the individual statements using `self.db.prepare(sql).bind(&[...])?.run().await?` for each. Do NOT rely on `batch()` for atomicity — D1's `batch()` does not roll back on partial failure.

The `D1Database::exec()` method accepts a raw SQL string. Use it only for the `BEGIN`/`COMMIT`/`ROLLBACK` control flow statements (no parameters needed).

- [ ] **Step 8: implement `mark_share_link_revoked`**

`UPDATE share_links SET revoked_at = ?1, revoked_by = ?2 WHERE id = ?3 AND revoked_at IS NULL`. Check rows_changed.

- [ ] **Step 9: implement `get_share_link_by_id`, `get_share_link_by_slug`, `list_share_links_for_account`**

Each requires a JOIN with `share_link_repos`:
```sql
SELECT sl.*, slr.repo_id, slr.repo_full_name
FROM share_links sl
LEFT JOIN share_link_repos slr ON sl.id = slr.share_link_id
WHERE sl.id = ?1
```

D1 returns one row per repo. `ShareLinkJoinRow` must have nullable repo fields:
```rust
#[derive(Deserialize)]
struct ShareLinkJoinRow {
    // share_links columns...
    id: String,
    slug: String,
    // ... other share_link fields ...
    repo_id: Option<i64>,        // NULL when link has zero repos (LEFT JOIN)
    repo_full_name: Option<String>,
}
```

A helper `fn collect_share_links(rows: Vec<ShareLinkJoinRow>) -> Result<Vec<ShareLink>>` in `bind.rs` groups rows by `id` and reconstructs each `ShareLink`. When both `repo_id` and `repo_full_name` are `None` (zero-repo link produces one null-sentinel row from the LEFT JOIN), the row is skipped — do not push a phantom `ShareLinkRepo` entry. This mirrors the null-handling in `sqlx_impl.rs` around `if let (Some(rid), Some(name)) = (row.repo_id, row.repo_full_name)`. Reference that existing function when implementing.

- [ ] **Step 10: compile check**

```bash
cargo check -p storage-d1 --target wasm32-unknown-unknown
```

---

### Task 4: D1Storage — invitation_requests, github_invitations, audit

**Files:**
- Modify: `crates/storage-d1/src/wasm_impl.rs`
- Modify: `crates/storage-d1/src/bind.rs`

**Implements:** remaining 11 of 20 Storage methods.

- [ ] **Step 1: implement `insert_invitation_request_and_increment_uses`**

Requires atomicity: the request row insert and the uses_count increment must both succeed or both fail. Use explicit `BEGIN`/`COMMIT`/`ROLLBACK` (same pattern as `insert_share_link`):

```sql
-- Inside BEGIN/COMMIT block:
-- statement 1:
INSERT INTO invitation_requests (id, share_link_id, requester_id, justification, state, created_at)
VALUES (?1, ?2, ?3, ?4, 'pending', ?5)

-- statement 2:
UPDATE share_links SET uses_count = uses_count + 1 WHERE id = ?2
```

The `(share_link_id, requester_id) WHERE state = 'pending'` partial unique index will surface `DuplicatePendingRequest` if a pending request exists — this error bubbles out through the `ROLLBACK` path. Do NOT use `d1.batch()` here — it does not roll back on partial failure.

- [ ] **Step 2: implement `record_request_decision`**

```sql
UPDATE invitation_requests SET state = ?1, decided_by = ?2, decided_at = ?3, decline_reason = ?4
WHERE id = ?5 AND state = 'pending'
```

Check `rows_changed == 0` → `Error::NotFound`.

- [ ] **Step 3: implement `get_invitation_request`, `list_pending_requests_for_account`, `list_requests_for_link`**

Standard SELECT queries. `list_pending_requests_for_account` joins `share_links` on `account_id`. Deserialize via `InvitationRequestRow` → `InvitationRequest` conversion in `bind.rs`.

**D1 query latency note:** Each D1 call is an HTTP round-trip (~10–50ms). The dashboard handler calls `list_share_links_for_account` and `list_pending_requests_for_account` for every page load. These two queries are independent and should be fired concurrently. In the dashboard route handler (in `crates/web`), use `futures::join!` or `tokio::join!` (both are wasm32-compatible via `wasm-bindgen-futures`):
```rust
let (links, requests) = tokio::join!(
    storage.list_share_links_for_account(account_id),
    storage.list_pending_requests_for_account(account_id),
);
```
Add this as a note for the dashboard handler implementer (Plan 5 code in `crates/web/src/routes/dashboard.rs`).

- [ ] **Step 4: implement `insert_github_invitation`**

Single INSERT for `github_invitations` row in `sending` state.

- [ ] **Step 5: implement `update_github_invitation`**

```sql
UPDATE github_invitations
SET state = ?1,
    github_invitation_id = COALESCE(?2, github_invitation_id),
    error_message = ?3,
    updated_at = ?4
WHERE id = ?5
```

Check `rows_changed == 0` → `Error::NotFound`.

- [ ] **Step 6: implement `get_github_invitation`, `get_github_invitation_by_github_id`, `list_pending_github_invitations_for_installation`**

Standard SELECT queries. The installation-scoped list joins through `invitation_requests → share_links → installations`.

- [ ] **Step 7: implement `audit`**

Single INSERT into `audit_events`. Serialize `AuditEvent` fields. The `id` uniqueness guarantees idempotency per the trait contract.

- [ ] **Step 8: full compile + method coverage check**

```bash
cargo check -p storage-d1 --target wasm32-unknown-unknown
```

Verify no `unimplemented!()` stubs remain — search:
```bash
grep -r "unimplemented!" crates/storage-d1/
```
Expect zero results.

---

### Task 5: `crates/web-worker` — Workers entry point + KV session store

**Files:**
- Create: `crates/web-worker/Cargo.toml`
- Create: `crates/web-worker/src/lib.rs`
- Modify: `Cargo.toml` (workspace — add `web-worker` member)

**Goal:** Create a thin wasm32-only crate that depends on the existing `web` library and provides the `#[event(fetch)]` Workers entry point, `KvSessionStore`, and Workers-specific config loading. `crates/web` itself is NOT modified — it remains pure native Rust with no wasm32 entanglement.

- [ ] **Step 1: Create `crates/web-worker/Cargo.toml`**

```toml
[package]
name = "web-worker"
version.workspace = true
edition.workspace = true
license.workspace = true
publish.workspace = true

[lib]
path = "src/lib.rs"
crate-type = ["cdylib"]

[dependencies]
async-trait.workspace = true
github.workspace = true
serde_json.workspace = true
storage.workspace = true
storage-d1.workspace = true
tower-sessions.workspace = true
web.workspace = true
worker = { version = "0.8", features = ["d1", "kv", "axum"] }
console_error_panic_hook = "0.1.7"
tracing-wasm = "0.2"
```

No `[target.'cfg(...)'.dependencies]` needed — this crate is wasm32 only by construction. `wrangler` / `worker-build` always invokes it with `--target wasm32-unknown-unknown`.

- [ ] **Step 2: Add `web-worker` to workspace**

In root `Cargo.toml`, add `"crates/web-worker"` to `[workspace] members`.

- [ ] **Step 3: Create `crates/web-worker/src/lib.rs`** — KvSessionStore

```rust
use std::sync::Arc;
use tower_sessions::{session::{Id, Record}, session_store, SessionStore};
use worker::{event, Context, Env, Request, Response, Result};

pub struct KvSessionStore {
    kv: worker::kv::KvStore,
}

// wasm32 is single-threaded: no actual threads exist.
unsafe impl Send for KvSessionStore {}
unsafe impl Sync for KvSessionStore {}

impl KvSessionStore {
    fn from_env(env: &Env) -> Result<Self> {
        Ok(Self { kv: env.kv("SESSIONS")? })
    }
}

#[async_trait::async_trait]
impl SessionStore for KvSessionStore {
    async fn create(&self, record: &mut Record) -> session_store::Result<()> {
        self.save(record).await
    }

    async fn save(&self, record: &Record) -> session_store::Result<()> {
        let key = format!("session:{}", record.id);
        let value = serde_json::to_string(record)
            .map_err(|e| session_store::Error::Encode(e.to_string()))?;
        let mut put = self.kv.put(&key, value)
            .map_err(|e| session_store::Error::Backend(e.to_string()))?;
        if let Some(expiry) = record.expiry_date {
            let unix = expiry.unix_timestamp() as u64;
            put = put.expiration(unix);
        }
        put.execute().await
            .map_err(|e| session_store::Error::Backend(e.to_string()))
    }

    async fn load(&self, session_id: &Id) -> session_store::Result<Option<Record>> {
        let key = format!("session:{session_id}");
        let raw = self.kv.get(&key).text().await
            .map_err(|e| session_store::Error::Backend(e.to_string()))?;
        match raw {
            None => Ok(None),
            Some(text) => {
                let record: Record = serde_json::from_str(&text)
                    .map_err(|e| session_store::Error::Decode(e.to_string()))?;
                Ok(Some(record))
            }
        }
    }

    async fn delete(&self, session_id: &Id) -> session_store::Result<()> {
        let key = format!("session:{session_id}");
        self.kv.delete(&key).await
            .map_err(|e| session_store::Error::Backend(e.to_string()))
    }
}
```

- [ ] **Step 4: Add `config_from_env` + `#[event(fetch)]` handler**

Append to `crates/web-worker/src/lib.rs`:

```rust
fn config_from_env(env: &Env) -> Result<web::WebConfig> {
    use github::oauth::OAuthConfig;

    let base_url = env.var("GHINVITE_BASE_URL")?.to_string();

    // Session secret: 32-byte key derived from hex string.
    // Secret must be generated with `openssl rand -hex 32` (64 hex chars = 32 bytes).
    let secret_str = env.secret("GHINVITE_SESSION_SECRET")?.to_string();
    let mut session_secret = [0u8; 32];
    let bytes = secret_str.as_bytes();
    session_secret[..bytes.len().min(32)].copy_from_slice(&bytes[..bytes.len().min(32)]);

    Ok(web::WebConfig {
        base_url: base_url.clone(),
        session_secret,
        restate_ingress: env.var("GHINVITE_RESTATE_INGRESS")?.to_string(),
        oauth: OAuthConfig {
            client_id: env.secret("GHINVITE_GITHUB_CLIENT_ID")?.to_string(),
            client_secret: env.secret("GHINVITE_GITHUB_CLIENT_SECRET")?.to_string(),
            redirect_uri: format!("{base_url}/oauth/callback"),
        },
        github_install_url: env.var("GHINVITE_GITHUB_INSTALL_URL")?.to_string(),
        cookie_secure: true,
        webhook_secret: env.secret("GHINVITE_WEBHOOK_SECRET")?.to_string().into_bytes(),
    })
}

#[event(fetch)]
async fn fetch(req: Request, env: Env, _ctx: Context) -> Result<Response> {
    console_error_panic_hook::set_once();
    tracing_wasm::set_as_global_default();

    let config = config_from_env(&env)?;
    let db = env.d1("DB")?;
    let storage: Arc<dyn storage::Storage> = Arc::new(storage_d1::D1Storage::new(db));
    let transport: Arc<dyn github::HttpTransport> =
        Arc::new(github::reqwest_transport::ReqwestTransport::new());
    let restate = Arc::new(web::RestateClient::new(&config.restate_ingress)?);
    let state = web::AppState::new(storage, transport, restate, config);
    let session_store = KvSessionStore::from_env(&env)?;
    let app = web::build_app(state, session_store);

    worker::axum::run(app, req).await
}
```

**Note:** `web::WebConfig`, `web::AppState`, `web::RestateClient`, and `web::build_app` must all be `pub` in `crates/web/src/lib.rs`. Check current visibility and add `pub` where missing — these are already pub from Plan 4 work, but verify.

- [ ] **Step 5: Compile check**

```bash
cargo check -p web-worker --target wasm32-unknown-unknown
```

Native tests must still pass (no changes to `crates/web`):
```bash
cargo test -p web
```

---

### Task 6: `crates/restate-svc-worker` — Workers entry point

**Files:**
- Modify: `crates/restate-svc/Cargo.toml` (add `[lib]` section only)
- Modify: `crates/restate-svc/src/lib.rs` (expose `serve_workers` as `pub`)
- Create: `crates/restate-svc-worker/Cargo.toml`
- Create: `crates/restate-svc-worker/src/lib.rs`
- Modify: `Cargo.toml` (workspace — add `restate-svc-worker` member)

**Goal:** Create a thin wasm32-only crate that depends on `restate-svc` as a library and provides the `#[event(fetch)]` Workers entry point. `crates/restate-svc` itself gains only a `[lib]` section and one `pub` function — no wasm32 deps, no cfg gates in source.

- [ ] **Step 1: Add `[lib]` to `crates/restate-svc/Cargo.toml`**

`crates/restate-svc` currently has no `[lib]` section. Add one so `restate-svc-worker` can depend on it:

```toml
[lib]
path = "src/lib.rs"
```

No `crate-type` change needed — the default (`["lib"]` = rlib) is correct. `crates/restate-svc` remains native only.

- [ ] **Step 2: Read the PoC source**

Study https://github.com/sagikazarmark/poc-restate-cloudflare-worker/blob/main/src/lib.rs to understand the exact entry point pattern: how the Restate SDK wires up service handlers for Workers, what function signature `serve_workers` needs to have, and how `D1Database` is threaded in.

- [ ] **Step 3: Expose `serve_workers` in `crates/restate-svc/src/lib.rs`**

Add a `pub` function that registers all five services with the Restate SDK router, following the PoC pattern. The exact signature depends on the Restate SDK's Workers integration API — read Step 2 first:

```rust
// In crates/restate-svc/src/lib.rs
// Exact signature — adapt from PoC:
pub async fn serve_workers(
    storage: std::sync::Arc<dyn storage::Storage>,
    req: worker::Request,
    env: worker::Env,
    ctx: worker::Context,
) -> worker::Result<worker::Response> {
    // Register Installation, ShareLink, InvitationRequest,
    // GithubInvitation, Reconcile services with the Restate SDK router.
    // Delegate to the SDK's Workers fetch handler.
    todo!("adapt from PoC — exact wiring TBD after reading Step 2")
}
```

**Do NOT add `worker` as a dependency to `crates/restate-svc/Cargo.toml`**. Instead, keep `serve_workers` gated behind a `#[cfg(feature = "workers")]` feature OR accept that `crates/restate-svc` will grow a `worker` dep. The cleaner choice: keep `serve_workers` in `crates/restate-svc-worker/src/lib.rs` and have it call into the service-registration logic from `restate-svc` directly. Resolve this after reading the PoC — the PoC will dictate whether service registration code needs to live in the lib or the worker crate.

- [ ] **Step 4: Create `crates/restate-svc-worker/Cargo.toml`**

```toml
[package]
name = "restate-svc-worker"
version.workspace = true
edition.workspace = true
license.workspace = true
publish.workspace = true

[lib]
path = "src/lib.rs"
crate-type = ["cdylib"]

[dependencies]
restate-svc.workspace = true
storage-d1.workspace = true
storage.workspace = true
worker = { version = "0.8", features = ["d1"] }
console_error_panic_hook = "0.1.7"
tracing-wasm = "0.2"
```

- [ ] **Step 5: Create `crates/restate-svc-worker/src/lib.rs`**

```rust
//! Cloudflare Workers entry point for the Restate service binary.
//! Pattern from: https://github.com/sagikazarmark/poc-restate-cloudflare-worker

use std::sync::Arc;
use worker::{event, Context, Env, Request, Response, Result};

#[event(fetch)]
async fn fetch(req: Request, env: Env, ctx: Context) -> Result<Response> {
    console_error_panic_hook::set_once();
    tracing_wasm::set_as_global_default();

    let db = env.d1("DB")?;
    let storage: Arc<dyn storage::Storage> = Arc::new(storage_d1::D1Storage::new(db));

    // Delegate to restate-svc's serve_workers — registers all five services
    // and hands off to the Restate SDK's Workers fetch handler.
    restate_svc::serve_workers(storage, req, env, ctx).await
}
```

- [ ] **Step 6: Add `restate-svc-worker` to workspace**

In root `Cargo.toml`, add `"crates/restate-svc-worker"` to `[workspace] members`.

- [ ] **Step 7: Compile check**

```bash
cargo check -p restate-svc-worker --target wasm32-unknown-unknown
```

Fix any issues. The PoC has already proven the approach works; any errors here are wiring issues in our specific service registration, not fundamental incompatibility.

---

### Task 7: `wrangler/web.toml`

**Files:**
- Create: `wrangler/web.toml`

**Note:** The `wrangler/` directory does not yet exist. Create it.

- [ ] **Step 1: Create `wrangler/web.toml`**

```toml
# Cloudflare Worker — Web binary
# Deploy: wrangler deploy --config wrangler/web.toml
# Build: worker-build --release -p web-worker

name = "ghinvite-web"
main = "build/worker/shim.mjs"
compatibility_date = "2024-09-23"
compatibility_flags = ["nodejs_compat"]

[build]
command = "worker-build --release -p web-worker"

# D1 database binding (matches env.d1("DB") in web-worker/src/lib.rs)
[[d1_databases]]
binding = "DB"
database_name = "ghinvite"
database_id = "REPLACE_WITH_WRANGLER_D1_CREATE_OUTPUT"

# KV namespace for session storage (matches env.kv("SESSIONS"))
[[kv_namespaces]]
binding = "SESSIONS"
id = "REPLACE_WITH_WRANGLER_KV_CREATE_OUTPUT"

# Non-secret config vars (set in wrangler.toml [vars] section — not secrets)
[vars]
GHINVITE_BASE_URL         = "https://ghinvite.workers.dev"   # replace with actual domain
GHINVITE_RESTATE_INGRESS  = "https://ACCOUNT_ID.restate.cloud"  # from Restate Cloud console
GHINVITE_GITHUB_INSTALL_URL = "https://github.com/apps/YOUR_APP_NAME/installations/new"

# Secrets (set via `wrangler secret put`; never commit these values):
# GHINVITE_SESSION_SECRET      (32 bytes, any format — first 32 bytes used as key)
# GHINVITE_GITHUB_CLIENT_ID    (GitHub App OAuth client ID)
# GHINVITE_GITHUB_CLIENT_SECRET (GitHub App OAuth client secret)
# GHINVITE_WEBHOOK_SECRET      (GitHub App webhook secret)
```

- [ ] **Step 2: Verify directory structure**

```bash
ls wrangler/
```

Expect: `web.toml` (and `restate-svc.toml` once Task 8 is done).

---

### Task 8: `wrangler/restate-svc.toml`

**Files:**
- Create: `wrangler/restate-svc.toml`

- [ ] **Step 1: Create `wrangler/restate-svc.toml`**

```toml
# Cloudflare Worker — Restate Service
# Deploy: wrangler deploy --config wrangler/restate-svc.toml
# Build: worker-build --release -p restate-svc-worker

name = "ghinvite-restate-svc"
main = "build/worker/shim.mjs"
compatibility_date = "2024-09-23"
compatibility_flags = ["nodejs_compat"]

[build]
command = "worker-build --release -p restate-svc-worker"

[[d1_databases]]
binding = "DB"
database_name = "ghinvite"
database_id = "REPLACE_WITH_SAME_ID_AS_WEB_TOML"  # same D1 database as web worker

# Secrets (set via `wrangler secret put`):
# GHINVITE_GITHUB_APP_ID           (GitHub App numeric id)
# GHINVITE_GITHUB_APP_PRIVATE_KEY  (PEM-encoded RSA private key, base64-encoded for safe storage)
# RESTATE_IDENTITY_KEY             (Restate Cloud identity verification key, if required)
```

**Note on private key storage:** The GitHub App private key is multi-line PEM. Encode as base64 before storing as a secret (`base64 -w0 private-key.pem`). The `github` crate's key loading code must decode from base64 before PEM parsing — add a code comment in the relevant loader when implementing.

---

### Task 9: D1 migrations + secrets management

**Files:**
- Create: `docs/deploy.md` (deployment runbook)

**Goal:** Document the one-time setup steps so anyone with the right credentials can deploy from scratch.

- [ ] **Step 1: Create the D1 database**

```bash
# Run once to create the D1 database:
wrangler d1 create ghinvite
# → Outputs database_id; copy it into wrangler/web.toml and wrangler/restate-svc.toml
```

- [ ] **Step 2: Apply migrations**

```bash
# Local dev (wrangler dev uses a local SQLite simulation of D1):
wrangler d1 migrations apply ghinvite --local --config wrangler/web.toml

# Production:
wrangler d1 migrations apply ghinvite --config wrangler/web.toml
```

Add to `wrangler/web.toml`:
```toml
[migrations]
directory = "../migrations"
```

**Important — migration tracking conflict:** Wrangler tracks applied migrations in a `d1_migrations` table that it creates in the D1 database. sqlx tracks applied migrations in a `_sqlx_migrations` table in the local SQLite file. These are completely separate tracking mechanisms on separate databases (D1 in production, local SQLite in dev). **Do not run `wrangler d1 migrations apply` against the local dev database, and do not run `sqlx migrate run` against D1.** The split is: wrangler manages D1 (production + `--local` wrangler dev simulation); sqlx manages the local SQLite file (native dev + tests). The same SQL files in `migrations/` are shared, but the tracking tables are separate and non-conflicting.

- [ ] **Step 2b: Lock sqlx to sequential migration naming**

Add `sqlx.toml` to the workspace root to ensure `sqlx migrate add` always produces sequential filenames matching wrangler's expected format:

```toml
[migrations]
version-format = "sequential"
```

Without this, running `sqlx migrate add <name>` without `--sequential` defaults to timestamp-based names (e.g. `20260504120000_name.sql`) that could cause ordering confusion if mixed with the existing `0001_initial.sql`.

- [ ] **Step 3: Create KV namespace**

```bash
wrangler kv:namespace create "SESSIONS" --config wrangler/web.toml
# → Outputs KV namespace id; copy into wrangler/web.toml
```

- [ ] **Step 4: Generate + set secrets**

```bash
# Session secret — generate 32 bytes (64 hex chars) so the first 32 bytes of the
# decoded string fill the [u8; 32] key. Using -hex 16 produces only 16 bytes, which
# leaves the upper half of the key zeroed and halves the entropy.
openssl rand -hex 32 | wrangler secret put GHINVITE_SESSION_SECRET --config wrangler/web.toml

# GitHub OAuth (from GitHub App settings page):
wrangler secret put GHINVITE_GITHUB_CLIENT_ID --config wrangler/web.toml
wrangler secret put GHINVITE_GITHUB_CLIENT_SECRET --config wrangler/web.toml

# GitHub webhook secret (from GitHub App settings page):
wrangler secret put GHINVITE_WEBHOOK_SECRET --config wrangler/web.toml

# For restate-svc (GitHub App private key):
# The private key is multi-line PEM; base64-encode for safe secret storage:
base64 -w0 private-key.pem | wrangler secret put GHINVITE_GITHUB_APP_PRIVATE_KEY --config wrangler/restate-svc.toml
wrangler secret put GHINVITE_GITHUB_APP_ID --config wrangler/restate-svc.toml
```

**Note on private key:** Add a comment in `crates/github/` wherever the private key is loaded from env that it is base64-encoded in Workers secrets and must be decoded before PEM parsing. The `for_local_dev()` path reads it as a raw file path — the Workers path reads the base64-encoded string from `env.secret(...)`.

**Note on non-secrets:** `GHINVITE_BASE_URL`, `GHINVITE_RESTATE_INGRESS`, and `GHINVITE_GITHUB_INSTALL_URL` are set in `[vars]` in `wrangler/web.toml`, not as secrets — they are not sensitive. Update them in the toml file directly.

- [ ] **Step 5: Write `docs/deploy.md`**

Concise runbook covering: prerequisites (Cloudflare account, Wrangler CLI, `worker-build`), D1 setup, KV setup, secrets, first deploy command, smoke test URLs, secrets rotation procedure.

---

### Task 9.5: Local wrangler dev workflow

**Goal:** Document how to run and test the Workers binaries locally before deploying to production. This is the primary dev loop for Plan 7 work after Tasks 1–9 are complete.

`wrangler dev --local` runs both Workers in a local miniflare process with a SQLite-backed D1 simulation and an in-memory KV store. No Cloudflare account credentials or real D1 database are required.

- [ ] **Step 1: Create `.dev.vars` file**

The `wrangler dev` local process reads non-secret vars from `.dev.vars` in the project root. Create this file (and add it to `.gitignore` — it may contain dev secrets):

```bash
# .dev.vars — local overrides for wrangler dev (never commit secrets)
GHINVITE_BASE_URL=http://localhost:8787
GHINVITE_RESTATE_INGRESS=http://localhost:8080
GHINVITE_GITHUB_INSTALL_URL=https://github.com/apps/ghinvite-local/installations/new
GHINVITE_SESSION_SECRET=insecure-local-dev-session-key!!-pad-to-64-hex
GHINVITE_GITHUB_CLIENT_ID=Iv1.local-dev-client-id
GHINVITE_GITHUB_CLIENT_SECRET=local-dev-client-secret
GHINVITE_WEBHOOK_SECRET=
```

Add to `.gitignore`:
```
.dev.vars
```

**Note on local Restate integration:** `GHINVITE_RESTATE_INGRESS=http://localhost:8080` assumes a local Restate server is running (e.g. via `docker compose up` from the existing `compose.yaml`). The web Worker's `/webhooks/github` handler and the Restate service calls will target `localhost:8080`. If you only want to test the web Worker UI without Restate workflows, set `GHINVITE_RESTATE_INGRESS` to a non-existent URL — workflow calls will fail at the Restate step but the SSR pages still render.

- [ ] **Step 2: Apply migrations for wrangler dev**

The local `wrangler dev` D1 simulation requires migrations to be applied before the app can serve requests:

```bash
wrangler d1 migrations apply ghinvite --local --config wrangler/web.toml
```

This is separate from the native dev SQLite database (which sqlx manages). Each time you run this command it's idempotent (already-applied migrations are skipped).

- [ ] **Step 3: D1 table round-trip smoke tests (wrangler dev)**

Before running the full Worker, verify each of the 7 D1 tables accepts writes and reads correctly against the local simulation. This catches JSON deserialization mismatches and UNIQUE constraint classification issues before touching production D1.

Start `wrangler dev --local --config wrangler/web.toml` in the background, then hit routes or use `wrangler d1 execute` directly to probe each table. The preferred approach is `wrangler d1 execute` (no running Worker needed) using the `--local` flag:

```bash
# Verify each table exists and can accept a synthetic row:
wrangler d1 execute ghinvite --local --config wrangler/web.toml \
  --command "INSERT INTO installations (installation_id, account_id, account_login, account_type, account_avatar_url, selected_repos, installed_at) VALUES (1, 1, 'test-org', 'Organization', 'https://avatars.github.com/u/1', '\"all\"', datetime('now'))"
wrangler d1 execute ghinvite --local --config wrangler/web.toml \
  --command "SELECT COUNT(*) as cnt FROM installations"
# Expected: cnt = 1

wrangler d1 execute ghinvite --local --config wrangler/web.toml \
  --command "INSERT INTO users (user_id, login, avatar_url, last_seen_at) VALUES (99, 'testuser', 'https://avatars.github.com/u/99', datetime('now'))"
wrangler d1 execute ghinvite --local --config wrangler/web.toml \
  --command "SELECT COUNT(*) as cnt FROM users"
# Expected: cnt = 1

# Verify share_links and share_link_repos foreign key + LEFT JOIN work:
wrangler d1 execute ghinvite --local --config wrangler/web.toml \
  --command "INSERT INTO share_links (id, installation_id, created_by, slug, permission, approval_required, max_uses, uses_count, created_at) VALUES ('01JTEST000000000000000001', 1, 99, 'test-slug', 'read', 0, 10, 0, datetime('now'))"
wrangler d1 execute ghinvite --local --config wrangler/web.toml \
  --command "SELECT sl.id, slr.repo_id FROM share_links sl LEFT JOIN share_link_repos slr ON sl.id = slr.share_link_id WHERE sl.slug = 'test-slug'"
# Expected: one row, repo_id = NULL (zero repos) — this exercises the null-sentinel path

# Verify UNIQUE constraint fires correctly (duplicate slug):
wrangler d1 execute ghinvite --local --config wrangler/web.toml \
  --command "INSERT INTO share_links (id, installation_id, created_by, slug, permission, approval_required, max_uses, uses_count, created_at) VALUES ('01JTEST000000000000000002', 1, 99, 'test-slug', 'read', 0, 10, 0, datetime('now'))" \
  2>&1 | grep -i "UNIQUE constraint failed: share_links.slug" || echo "ERROR: duplicate slug not rejected"
# Expected: grep matches (constraint fired on share_links.slug)

# Repeat for invitation_requests, github_invitations, audit_events with similarly minimal synthetic rows.
```

If any command fails or returns unexpected output, fix the row struct (field names, types, serialization format) in `crates/storage-d1/src/bind.rs` before proceeding.

**Clean up after smoke tests:**

```bash
wrangler d1 execute ghinvite --local --config wrangler/web.toml \
  --command "DELETE FROM installations WHERE installation_id = 1"
# (or re-apply migrations to reset the local db to a clean state)
```

- [ ] **Step 4: Start the web Worker locally**

```bash
wrangler dev --local --config wrangler/web.toml
```

The web Worker is now available at `http://localhost:8787`. Test the golden path:

```bash
curl http://localhost:8787/health          # → "ok"
curl -s http://localhost:8787/ | grep ghinvite  # → HTML with "ghinvite"
curl -s -o /dev/null -w "%{http_code}" \
  -X POST http://localhost:8787/webhooks/github \
  -d '{}' 2>&1                            # → 401 (missing HMAC)
```

- [ ] **Step 5: Start the restate-svc Worker locally**

In a separate terminal:

```bash
wrangler dev --local --config wrangler/restate-svc.toml
```

Default port for the second worker will be `8788` (wrangler auto-increments). Register it with the local Restate server:

```bash
restate deployments register http://localhost:8788
```

(Requires Restate CLI and a local Restate server running on `localhost:8080`.)

- [ ] **Step 6: Document in `docs/deploy.md`**

Add a "Local dev" section to `docs/deploy.md` covering Steps 1–4 above, with emphasis on the `.dev.vars` setup and the `wrangler d1 migrations apply --local` prerequisite.

---

### Task 10: End-to-end deploy run

**Goal:** Deploy both workers, run smoke tests confirming the application is live.

- [ ] **Step 0: Binary size check + artifact path verify**

Before the production deploy, verify the wasm binary fits within Cloudflare's limits (10 MB compressed for free plan, 25 MB for paid). Run `worker-build --release -p web` and check:

```bash
ls -lh build/worker/
```

If the binary exceeds the limit, add to `Cargo.toml`'s `[profile.release]` section: `opt-level = "z"`, `lto = true`, `codegen-units = 1`.

Also confirm the actual output file extension. `worker-build` may produce `build/worker/shim.js` rather than `build/worker/shim.mjs`. If the extension differs, update `main = "..."` in both `wrangler/web.toml` and `wrangler/restate-svc.toml` to match before deploying.

Also confirm `worker-build` locates the correct crate when passed `-p web-worker` from the workspace root. If it needs explicit invocation, the `wrangler.toml` `[build]` command may need to be `cd crates/web-worker && worker-build --release` — resolve this at implementation time.

- [ ] **Step 1: Build + deploy web worker**

```bash
wrangler deploy --config wrangler/web.toml
```

Expect: wrangler outputs a `*.workers.dev` URL or the configured custom domain.

- [ ] **Step 2: Deploy restate-svc**

```bash
wrangler deploy --config wrangler/restate-svc.toml
```

- [ ] **Step 2b: Register services with Restate Cloud**

After deploying the restate-svc Worker, the Restate Cloud control plane must be told where to find it. Run (using the Restate CLI or `curl` against the Restate Cloud API):

```bash
restate deployments register https://ghinvite-restate-svc.YOUR_SUBDOMAIN.workers.dev
```

Restate will introspect the Worker, discover the five services (`Installation`, `ShareLink`, `InvitationRequest`, `GithubInvitation`, `Reconcile`), and begin routing invocations to it.

**`RESTATE_IDENTITY_KEY` is always required for Restate Cloud deployments** — it is the key Restate Cloud uses to sign requests to the Worker so the Worker can verify they are genuine. Set it in `wrangler/restate-svc.toml` as a secret:

```bash
wrangler secret put RESTATE_IDENTITY_KEY --config wrangler/restate-svc.toml
```

The Restate SDK reads this env var automatically — no code change needed; it is a convention of the SDK. Obtain the key from the Restate Cloud console under "Deployments → Identity key".

Re-registration is required every time the service interface changes (new handler, renamed service, changed input/output types). The URL stays stable across redeployments as long as the Worker name does not change, but re-registering after each deploy is safe and idempotent.

- [ ] **Step 3: Smoke tests**

```bash
# Health endpoint:
curl -s https://ghinvite.workers.dev/health
# Expected: "ok"

# Home page:
curl -s https://ghinvite.workers.dev/ | grep "ghinvite"
# Expected: HTML containing "ghinvite"

# Webhook HMAC rejection (missing header):
curl -s -o /dev/null -w "%{http_code}" \
  -X POST https://ghinvite.workers.dev/webhooks/github \
  -d '{"action":"ping"}'
# Expected: 401

# Invalid slug:
curl -s -o /dev/null -w "%{http_code}" \
  https://ghinvite.workers.dev/i/AAAAAAAAAAAAAAAA
# Expected: 404
```

- [ ] **Step 4: Verify rollback capability**

Before marking deploy complete:
```bash
# Confirm wrangler rollback is available (returns worker version list):
wrangler versions list --config wrangler/web.toml
wrangler versions list --config wrangler/restate-svc.toml
```
Document the `wrangler rollback` command in `docs/deploy.md` for both workers. Note that D1 schema migrations are not reversible — document "take a D1 backup snapshot before applying migrations" as a pre-deploy step. A rollback reverts the Worker code but not any schema changes applied during this deploy.

- [ ] **Step 5: Update README.md plan table**

In `docs/superpowers/plans/README.md`, mark Plan 7 status as **Implemented**.

---

## Security notes

- `GHINVITE_GITHUB_APP_PRIVATE_KEY` must be set as a secret in `restate-svc` only — never in the web worker. This enforces the spec §2.1 requirement that the private key never enters the web binary.
- `GHINVITE_SESSION_SECRET` rotation: this secret is used for session cookie encryption. Rotating it invalidates all existing sessions (all users are logged out). A rolling rotation strategy (dual-key window) is deferred to v1.1; in v1, rotation = forced re-login for all users.
- Workers `compatibility_flags = ["nodejs_compat"]` is required for some crypto operations. The exact flag set should be validated against the actual wasm output.

---

## Audit trail

### CEO phase (auto-decided)
1. **F1 Restate wasm** — User provided PoC confirming Restate+Workers works (https://github.com/sagikazarmark/poc-restate-cloudflare-worker). Task 6 restructured to follow PoC pattern directly; VM fallback removed. Decision: RESOLVED BY USER EVIDENCE.
2. **F3 D1 batch atomicity** — D1 GA batch is documented as transactional; plan updated to use batch but added explicit BEGIN/COMMIT fallback guidance and verification note. Decision: APPLY (add verification note + fallback).
3. **F5 Conflict string parsing** — Added explicit limitation warning to `classify_d1_error` spec; noted it must be covered by Plan 8 parameterized test suite. Decision: APPLY (document limitation, defer test coverage to Plan 8).
4. **F6 Send bounds** — Added explicit `unsafe impl Send/Sync for D1Storage {}` with wasm32 single-threaded justification; do this in Task 2 before writing Tasks 3–5. Decision: APPLY.
5. **F7 Migration tracking** — Added explicit note: wrangler manages D1, sqlx manages local SQLite, same SQL files but separate tracking tables — no conflict. Decision: APPLY (clarify in Task 9).
6. **F8 Rollback** — Added Task 10 Step 4: verify `wrangler versions list`, document `wrangler rollback` and D1 backup pre-deploy in `docs/deploy.md`. Decision: APPLY.

### Eng phase (auto-decided)
7. **1a.1+1a.2 sqlx wasm32 compilation** — `sqlx` and `to_db_err` must be gated behind `cfg(not(wasm32))` in `storage/Cargo.toml` and `lib.rs`. Task 1 Step 4 made mandatory (not optional). Decision: APPLY.
8. **1b+1c KvSessionStore Send/Sync** — `tower_sessions::SessionStore` requires `Send + Sync`; `KvStore` is `!Send`. Add `unsafe impl Send/Sync for KvSessionStore {}` to Task 5 Step 3, same pattern as D1Storage. Decision: APPLY.
9. **1e.1 worker crate version** — Updated `"0.4"` → `"0.8"` throughout. Current release is v0.8.x; v0.4 is 4 major versions behind with breaking API changes. Decision: APPLY.
10. **1e.2 build artifact path** — Added Task 10 Step 0 to verify `build/worker/shim.js` vs `.mjs` extension before deploy. Decision: APPLY.
11. **1f D1 batch not atomic (reinforced)** — Promoted explicit `BEGIN`/`COMMIT`/`ROLLBACK` to primary path for `insert_share_link` and `insert_invitation_request_and_increment_uses`. Removed batch() from atomicity-critical paths. Decision: APPLY.
12. **2a Error::Database direct construction** — Task 1 Step 3 now enumerates three distinct patterns (`.map_err(Error::Database)`, direct construction, migration error wrapping). Decision: APPLY.
13. **2b classify_d1_error misclassification** — Fixed to two-column check for DuplicatePendingRequest; added DuplicateActiveInstallation case. Decision: APPLY.
14. **2c collect_share_links null-repo handling** — Specified `Option<i64>` fields, null-sentinel skip logic, reference to sqlx_impl.rs equivalent. Decision: APPLY.
15. **2d WebConfig::from_env wrong fields** — Rewrote to match actual struct (base_url, session_secret [u8;32], restate_ingress, oauth: OAuthConfig, github_install_url). Updated wrangler/web.toml vars and secrets lists to match. Decision: APPLY.
16. **2e sqlx sequential migrations** — Added `sqlx.toml` with `version-format = "sequential"` as Task 9 Step 2b. Decision: APPLY.
17. **3b test gate too narrow** — Task 1 Step 5 now includes `cargo test -p web -p restate-svc`. Decision: APPLY.
18. **4a binary size** — Added Task 10 Step 0: binary size check + `opt-level = "z"` / `lto = true` profile guidance. Decision: APPLY.
19. **4b sequential D1 queries** — Added `tokio::join!` guidance for dashboard concurrent queries in Task 4 Step 3 note. Decision: APPLY.

### Eng phase (taste — deferred to Final Gate)
- **3a/3c D1 smoke tests before production deploy** — Overlaps CEO F2. Surface at Final Gate: should Plan 7 include a `wrangler dev` round-trip test for each D1 table before the production deploy, or is the 4-curl smoke test sufficient?

### DX phase (auto-decided)
20. **DX-5b crate-type** — Added `crate-type = ["cdylib", "rlib"]` to `[lib]` in Tasks 5 and 6. `worker-build` requires `cdylib` to produce a deployable wasm artifact; without it the deploy silently fails. For `restate-svc`, a new `[lib]` section must be added (crate currently has none). Decision: APPLY (critical — silently breaks deploy without it).
21. **DX-3a session secret entropy** — Fixed `openssl rand -hex 16` → `openssl rand -hex 32` in Task 9 Step 4. `rand -hex 16` produces a 32-char hex string = 16 bytes of entropy; the `[u8; 32]` key then has its upper half zeroed, halving security. Decision: APPLY.
22. **DX-3b GHINVITE_COOKIE_KEY phantom** — Fixed Security Notes `GHINVITE_COOKIE_KEY` → `GHINVITE_SESSION_SECRET`. The field is `session_secret` in the struct; `GHINVITE_COOKIE_KEY` does not exist. Decision: APPLY.
23. **DX-3c Restate registration** — Added Task 10 Step 2b: `restate deployments register <url>` after deploying restate-svc, with `RESTATE_IDENTITY_KEY` setup. Without this step the deploy is incomplete — Restate Cloud will not route invocations to the Worker. Decision: APPLY (missing step would leave deployed Worker unreachable from Restate).
24. **DX-4 tracing-wasm** — Added `tracing-wasm = "0.2"` as wasm32-only dep in Tasks 5 and 6; added `tracing_wasm::set_as_global_default()` call in both `#[event(fetch)]` handlers. Without this, `tracing::info!` etc. produce no output in Workers logs. Decision: APPLY.
25. **DX-5c worker version in snippets** — All remaining `version = "0.4"` code snippets updated to `"0.8"` (storage-d1 and restate-svc Task 6). Decision: APPLY.
26. **DX-2 wrangler dev local workflow** — Added Task 9.5 covering `.dev.vars`, local migration apply, and `wrangler dev --local` for both workers. Decision: APPLY.
27. **DX-6 local Restate compose.yaml** — `.dev.vars` notes that `GHINVITE_RESTATE_INGRESS=http://localhost:8080` assumes a local Restate server via `docker compose up`; includes guidance for testing web Worker in isolation. Decision: APPLY.

### Post-gate architectural revision
30. **Separate worker crates** — User requested separate `crates/web-worker` and `crates/restate-svc-worker` crates rather than adding wasm32 entry points to the existing `web` and `restate-svc` crates. Revised Tasks 5 and 6, file structure table, architecture description, A4, and wrangler build commands accordingly. Key benefit: existing crates remain pure native Rust — no `#[cfg(wasm32)]` in their source, no `crate-type` changes, no target-specific deps. Worker crates are thin wiring layers (`crate-type = ["cdylib"]` only). Decision: APPLY.

### Final Gate (user decisions)
28. **D1 D1Storage smoke tests** — User chose: Yes, add wrangler dev round-trip smoke tests per table before production deploy. Added Task 9.5 Step 3 with `wrangler d1 execute --local` commands covering all 7 tables, including UNIQUE constraint verification and null-sentinel LEFT JOIN check. Decision: APPLY.
29. **D2 crates/web README** — User chose: `docs/deploy.md` only. No update to `crates/web/README.md`. Decision: DISMISSED.

### DX phase (resolved)
- **DX-5a crates/web README** — Resolved at Final Gate: `docs/deploy.md` only, no `crates/web/README.md` update.

### CEO phase (resolved)
- **F2 D1Storage tests before deploy** — Resolved at Final Gate: wrangler dev round-trip smoke tests added as Task 9.5 Step 3.
- **F4 Fly.io-native premise** — User clarified they want Workers+D1 (misread premise question). Workers+D1 confirmed as primary target per spec Q5. Decision: DISMISSED (user confirmed original direction).

---

## Task status

- [ ] Task 1: Make `storage::Error` wasm-compatible
- [ ] Task 2: `crates/storage-d1` skeleton + wasm32 smoke test
- [ ] Task 3: D1Storage — installations, users, share_links
- [ ] Task 4: D1Storage — invitation_requests, github_invitations, audit
- [ ] Task 5: `crates/web` Workers entry point + KV session store
- [ ] Task 6: `crates/restate-svc` wasm compatibility + entry point
- [ ] Task 7: `wrangler/web.toml`
- [ ] Task 8: `wrangler/restate-svc.toml`
- [ ] Task 9: D1 migrations + secrets management
- [ ] Task 9.5: Local wrangler dev workflow
- [ ] Task 10: End-to-end deploy run
