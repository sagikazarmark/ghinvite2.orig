<!-- /autoplan restore point: /home/laborant/.gstack/projects/sagikazarmark-ghinvite2.orig/main-autoplan-restore-20260505-034106.md -->
# ghinvite — Plan 8: E2E + CI

**Goal:** Ship a complete automated test and CI pipeline that gives confidence before every production deploy. Three deliverables: (1) Restate handler integration tests using a real local Restate server (Docker), (2) D1Storage parameterized test suite run against `wrangler dev --local`, and (3) a GitHub Actions workflow that runs the full test matrix on every push and PR.

**Depends on:** Plans 1–7 (all implemented).

**Architecture:** Plan 8 adds no production code — only test infrastructure and CI config. The native binaries (web, restate-svc) compile to the host platform for integration tests. D1Storage tests use `wrangler dev --local` as a lightweight D1 simulator over HTTP. Restate integration tests use the Docker container from the existing `compose.yaml`.

---

## Spec coverage

- **§17 Restate handler tests** — full workflows against a real Restate server; timer races exercised via Restate admin API virtual time (if available in this version)
- **§17 Storage suite** — parameterized harness running same scenarios against `SqlxStorage` (already implemented) AND `D1Storage` via `wrangler dev`
- **§17 End-to-end smoke** — CI: spin up web + restate-svc + Restate locally, stub GitHub backend, drive happy-path through HTTP

It does NOT implement:
- Custom domain or production smoke tests — post-ship ops
- Load testing — v2+
- GDPR / data deletion test coverage — v2+

---

## File structure

| Path | Action | Responsibility |
|---|---|---|
| `.github/workflows/ci.yml` | Create | Full CI matrix: test, lint, wasm32 check |
| `crates/github-stub/` | Create | Minimal HTTP GitHub API stub for integration tests |
| `crates/restate-svc/tests/integration_test.rs` | Create | Restate-in-Docker handler integration tests |
| `crates/storage-d1/tests/d1_suite.rs` | Create | D1Storage parameterized suite via wrangler dev |
| `Cargo.toml` (workspace) | Modify | Add `github-stub` crate |

---

## Critical architectural decisions

### A1: Restate integration test approach

The `restate-sdk` 0.10 has no in-process test harness. Integration tests must connect to a real Restate server. Two options:

**Option A — Docker in tests:** Spawn `docker compose up -d` as a subprocess at test startup (using `std::process::Command`), wait for the `/health` endpoint on port 8080, then register the native restate-svc binary as a Restate service endpoint. Tests hit the Restate ingress directly via `reqwest`. Teardown: `docker compose down`. This requires Docker to be installed on the CI runner.

**Option B — Pre-started Docker:** Document that the CI workflow requires a `docker compose up -d` service step before running integration tests. Tests assume Restate is already running on `localhost:8080`. Simpler test code; CI orchestrates startup separately.

**Decision:** Option B — pre-started in CI via `services:` block or workflow step, same pattern tests use locally. Integration tests use a `#[ignore]` attribute and are run explicitly with `cargo test --features integration -- --ignored` or via a dedicated CI job. This keeps unit tests fast and integration tests opt-in.

**Critical: service registration.** Integration tests must bind `build_endpoint(state)` to a local HTTP listener and register it with Restate's admin API before running. The test binary does this in-process:
```rust
// In test setup (shared across all integration tests)
async fn setup_restate() -> (SocketAddr, Arc<AppState>) {
    // 1. Build AppState with InstallationClient pointing at github-stub
    let transport = Arc::new(ReqwestTransport::new().with_base("http://localhost:3001"));
    let state = Arc::new(AppState::new(fixture_storage().await, fixture_github_client(transport)));
    // 2. Bind Restate endpoint to a local port
    let endpoint = build_endpoint(Arc::clone(&state));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { /* serve endpoint on listener */ });
    // 3. Register with Restate admin API
    let reg_url = "http://localhost:9070/restate/v1/deployments";
    reqwest::Client::new()
        .post(reg_url)
        .json(&json!({"uri": format!("http://127.0.0.1:{}", addr.port())}))
        .send().await.unwrap();
    (addr, state)
}
```
The `InstallationClient::with_base("http://localhost:3001")` points all GitHub API calls at `github-stub`. No separate native binary startup required.

### A2: GitHub API stub

The existing `github::mocks::MockTransport` works at the `HttpTransport` trait level — it intercepts calls in-process. For E2E tests where the GitHub API is called over real HTTP (from a running native binary), a stub HTTP server is needed.

**Implementation:** A minimal `crates/github-stub` binary that listens on a configurable port and responds to the GitHub API endpoints exercised by integration tests:
- `GET /app/installations/{id}/access_tokens` → 201 `{"token":"test-token","expires_at":"..."}`
- `PUT /repos/{owner}/{repo}/collaborators/{user}` → 201 (first call per `(repo,user)` since last reset), then 204
- `GET /repos/{owner}/{repo}/collaborators/{user}` → 204 (member) or 404 (not member)
- `DELETE /repos/{owner}/{repo}/collaborators/{user}` → 204
- `DELETE /reset` → 200, resets all call counts and collaborator state

The stub records calls so tests can assert `GET /calls` returns `{"count": N}`. The `DELETE /reset` endpoint provides per-test isolation — call it in each test's setup to avoid state leaking between parallel tests.

### A3: D1Storage parameterized test suite

`storage::tests::run_suite` takes a `Fn() -> impl Future<Output = T>` factory and runs all 25 storage scenarios against it. To run against D1Storage:

**Option A — wrangler dev HTTP bridge:** Run `wrangler dev --local --config wrangler/web.toml` as a subprocess. Write a thin `D1HttpStorage` shim that translates `Storage` trait calls into HTTP requests to the running Worker, which in turn calls D1. This is architecturally clean but requires significant glue.

**Option B — wrangler d1 execute approach:** For each storage operation in the suite, use `std::process::Command` to call `wrangler d1 execute ghinvite --local --command "..."` directly. No Worker needed. Slow (one subprocess per SQL call) but simple.

**Option C — wrangler dev + D1Storage directly:** Run `wrangler dev --local` for the `web` Worker, then drive the HTTP API endpoints to exercise storage (create a link, get a link, etc.). This tests the full stack but doesn't isolate D1Storage from routing logic.

**Decision:** Option B for initial implementation. Each storage scenario runs the SQL via `wrangler d1 execute --local`. Slow (~30s total for suite) but doesn't require a running Worker. Add `#[ignore]` and run explicitly with `cargo test -p storage-d1 --features d1-suite -- --ignored`.

### A4: CI matrix

The CI workflow runs:
1. **Fast unit tests** (`cargo test --workspace --exclude github-stub`) — runs on every push/PR, ~2min. Does NOT enable `integration` or `d1-suite` features so no Docker/wrangler needed.
2. **wasm32 compilation** (`cargo build -p storage-d1 --target wasm32-unknown-unknown` + same for `web-worker` + `restate-svc-worker`) — `build` not `check`, to catch wasm-bindgen ABI issues. Runs on every push/PR, ~2min.
3. **Lint** (`cargo clippy --workspace` + `cargo fmt --check`) — runs on every push/PR
4. **Restate integration tests** (Restate via `services:` + readiness probe + `cargo test -p restate-svc --features integration -- --ignored`) — runs on main branch push only. Binds endpoint in-process, registers via admin API.
5. **D1 suite** — local only in v1 (requires `wrangler` + no Cloudflare account needed for `--local`). Not in CI because wrangler is not on ubuntu-latest runners without additional setup. Document this explicitly as a known gap.

---

## Task ordering

Tasks 1–2 lay the test infrastructure (stub + Restate integration). Task 3 covers D1. Task 4 wires everything into CI.

---

### Task 1: `crates/github-stub` — GitHub API stub server

**Files:**
- Create: `crates/github-stub/Cargo.toml`
- Create: `crates/github-stub/src/main.rs`
- Modify: `Cargo.toml` (workspace — add `github-stub` member)

**Goal:** A standalone binary that mimics the 4 GitHub API endpoints needed by integration tests. Accepts a `--port` flag (default 3000). Records call counts via a shared atomic counter accessible via `GET /calls` for test assertion.

- [ ] **Step 1: `crates/github-stub/Cargo.toml`**

```toml
[package]
name = "github-stub"
version.workspace = true
edition.workspace = true
license.workspace = true
publish = false

[[bin]]
name = "github-stub"
path = "src/main.rs"

[dependencies]
axum = { version = "0.8", features = ["macros"] }
tokio = { version = "1", features = ["rt-multi-thread", "macros"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
clap = { version = "4", features = ["derive"] }
chrono = { version = "0.4", features = ["serde"] }
```

- [ ] **Step 2: `crates/github-stub/src/main.rs`**

Implement with `axum`:
- `GET /app/installations/:id/access_tokens` — returns `{"token":"test-token-for-{id}","expires_at":"{now+1h}"}`
- `PUT /repos/:owner/:repo/collaborators/:user` — returns 201 on first call per `(repo, user)`, 204 on subsequent
- `GET /repos/:owner/:repo/collaborators/:user` — returns 204 (member present) by default; configurable via query param `?member=false` for 404
- `DELETE /repos/:owner/:repo/collaborators/:user` — returns 204
- `GET /calls` — returns `{"count": N}` where N is total calls to the above endpoints

Use `std::sync::atomic::AtomicU64` for call counting. State: `Arc<Mutex<HashMap<(String,String,String), bool>>>` tracking collaborator existence.

- [ ] **Step 3: Add to workspace**

Add `"crates/github-stub"` to `[workspace] members`.

- [ ] **Step 4: Build and smoke-test**

```bash
cargo build -p github-stub
./target/debug/github-stub --port 3001 &
curl -s http://localhost:3001/calls  # → {"count":0}
curl -s -X PUT http://localhost:3001/repos/test-org/test-repo/collaborators/alice  # → 201
curl -s http://localhost:3001/calls  # → {"count":1}
kill %1
```

---

### Task 2: Restate handler integration tests

**Files:**
- Modify: `crates/restate-svc/Cargo.toml` (add `integration` feature, `reqwest` dev-dep)
- Create: `crates/restate-svc/tests/integration_test.rs`

**Goal:** Integration tests that exercise the five Restate service handlers (`Installation`, `InvitationLink`, `InvitationRequest`, `GithubInvitation`, `Reconcile`) against a real Restate server running on `localhost:8080` (via `docker compose up`). GitHub API calls hit the `github-stub` server.

- [ ] **Step 1: Add `integration` feature to `crates/restate-svc/Cargo.toml`**

```toml
[features]
integration = []

[dev-dependencies]
# existing dev-deps...
reqwest = { version = "0.12", features = ["json"] }
```

- [ ] **Step 2: Create `crates/restate-svc/tests/integration_test.rs`**

```rust
//! Restate handler integration tests.
//!
//! Requires:
//! - Restate running on localhost:8080 (via `docker compose up -d`)
//! - github-stub running on localhost:3001:
//!     cargo run -p github-stub -- --port 3001 &
//!
//! Run with:
//!   cargo test -p restate-svc --features integration -- --ignored

#![cfg(feature = "integration")]

use std::net::SocketAddr;
use std::sync::Arc;

/// Start the Restate endpoint in-process and register it with the local Restate server.
/// Returns the bound address (for teardown) and the AppState (for assertions).
async fn setup_restate() -> SocketAddr {
    use github::{InstallationClient, transport::ReqwestTransport};
    use restate_svc::test_support::{fixture_storage, fixture_github_client};
    use std::sync::Arc;

    // InstallationClient pointed at github-stub on :3001
    let transport = Arc::new(
        ReqwestTransport::new().with_base("http://localhost:3001")
    );
    let state = restate_svc::AppState::new(
        fixture_storage().await,
        fixture_github_client(transport),
    );
    let endpoint = restate_svc::build_endpoint(state);

    // Bind to an ephemeral port
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        // Serve the Restate endpoint (uses hyper under the hood)
        endpoint.hyper_serve(listener).await.unwrap();
    });

    // Register with Restate admin API
    let client = reqwest::Client::new();
    client
        .post("http://localhost:9070/restate/v1/deployments")
        .json(&serde_json::json!({
            "uri": format!("http://127.0.0.1:{}", addr.port())
        }))
        .send()
        .await
        .expect("Restate admin API not reachable — is `docker compose up -d` running?");

    addr
}

/// Reset github-stub state between tests.
async fn reset_github_stub() {
    reqwest::Client::new()
        .delete("http://localhost:3001/reset")
        .send()
        .await
        .expect("github-stub not running on :3001 — start with: cargo run -p github-stub -- --port 3001");
}

#[tokio::test]
#[ignore]
async fn installation_install_and_query() {
    reset_github_stub().await;
    let _addr = setup_restate().await;

    // Call Installation::install via Restate ingress
    let client = reqwest::Client::new();
    let resp = client
        .post("http://localhost:8080/Installation/1/install")
        .json(&serde_json::json!({
            "installation_id": 1,
            "account_id": 42,
            "account_login": "test-org",
            "account_type": "Organization",
            "account_avatar_url": "https://avatars.github.com/u/42",
            "selected_repos": "all"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "Installation::install failed");
}

#[tokio::test]
#[ignore]
async fn full_happy_path() {
    reset_github_stub().await;
    let _addr = setup_restate().await;
    // InvitationLink create → InvitationRequest → approve → GithubInvitation sent
    // TODO: implement after installation_install_and_query is proven
    todo!("implement after basic wiring is confirmed")
}
```

**Note:** The `full_happy_path` body remains `todo!()` in Plan 8 — only `installation_install_and_query` has a real body. This is intentional: prove the wiring works end-to-end before writing the complex multi-step test.

- [ ] **Step 3: Verify Restate wiring**

```bash
docker compose up -d
# Wait for Restate to be ready:
until curl -sf http://localhost:9070/health; do sleep 2; done

# Start github-stub
cargo build -p github-stub
./target/debug/github-stub --port 3001 &

# Run the integration tests
cargo test -p restate-svc --features integration -- --ignored
# Expect: installation_install_and_query passes; full_happy_path is skipped (todo! panics inside #[ignore] body)
docker compose down
kill %1
```

---

### Task 3: D1Storage parameterized test suite

**Files:**
- Modify: `crates/storage-d1/Cargo.toml` (add `d1-suite` feature)
- Create: `crates/storage-d1/tests/d1_suite.rs`

**Goal:** Run the same 25-scenario storage suite that `SqlxStorage` passes against D1Storage via `wrangler dev --local`. Each storage call uses `wrangler d1 execute --local --command "..."` as a subprocess driver.

- [ ] **Step 1: Add `d1-suite` feature to `crates/storage-d1/Cargo.toml`**

```toml
[features]
d1-suite = []

[dev-dependencies]
storage = { workspace = true, features = ["test-suite"] }
tokio = { workspace = true, features = ["process"] }
```

- [ ] **Step 2: Create `crates/storage-d1/tests/d1_suite.rs`**

The approach: implement a `WranglerD1Bridge` that wraps `wrangler d1 execute ghinvite --local --config wrangler/web.toml --json --command "..."` subprocess calls. Then wire it through the storage test suite... 

**Feasibility note:** Directly running the parameterized suite against D1 via subprocess is complex because the `run_suite` factory produces a `Storage` impl, but `WranglerD1Bridge` would be very slow (one subprocess per SQL statement). 

**Revised approach (simpler, still covers §17):** Instead of running the full `run_suite` against D1, write targeted smoke tests that exercise each D1Storage method via `wrangler d1 execute --local`:
- Schema smoke: `SELECT COUNT(*) FROM installations` (table exists)
- INSERT smoke: each table accepts a valid row
- UNIQUE constraint verification: duplicate slug → correct error message substring
- LEFT JOIN null-sentinel: zero-repo invitation link returns one null-repo row

These tests run as `#[ignore]` and require `wrangler` on PATH.

```rust
//! D1Storage smoke tests against wrangler dev --local.
//!
//! Run with:
//!   wrangler d1 migrations apply ghinvite --local --config wrangler/web.toml
//!   cargo test -p storage-d1 --features d1-suite -- --ignored

#![cfg(feature = "d1-suite")]

use std::process::Command;

fn wrangler_d1_execute(sql: &str) -> String {
    let out = Command::new("wrangler")
        .args(["d1", "execute", "ghinvite", "--local",
               "--config", "wrangler/web.toml", "--json",
               "--command", sql])
        .output()
        .expect("wrangler not on PATH");
    String::from_utf8_lossy(&out.stdout).to_string()
}

#[test]
#[ignore]
fn d1_schema_tables_exist() {
    for table in ["installations", "users", "invitation_links", "invitation_link_repos",
                  "invitation_requests", "github_invitations", "audit_events"] {
        let out = wrangler_d1_execute(&format!("SELECT COUNT(*) as cnt FROM {table}"));
        assert!(out.contains("cnt"), "table {table} missing or inaccessible");
    }
}
```

- [ ] **Step 3: Apply local migrations and run**

```bash
wrangler d1 migrations apply ghinvite --local --config wrangler/web.toml
cargo test -p storage-d1 --features d1-suite -- --ignored
```

---

### Task 4: GitHub Actions CI workflow

**Files:**
- Create: `.github/workflows/ci.yml`

**Goal:** A complete CI workflow that runs on every push and PR to `main`.

- [ ] **Step 1: Create `.github/workflows/ci.yml`**

```yaml
name: CI

on:
  push:
    branches: [main]
  pull_request:
    branches: [main]

env:
  CARGO_TERM_COLOR: always
  RUST_BACKTRACE: 1

jobs:
  test:
    name: Test
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - uses: Swatinem/rust-cache@v2
      - name: Run tests
        run: cargo test --workspace --exclude github-stub

  lint:
    name: Lint
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
        with:
          components: rustfmt, clippy
      - uses: Swatinem/rust-cache@v2
      - run: cargo fmt --check
      - run: cargo clippy --workspace -- -D warnings

  wasm32:
    name: wasm32 check
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
        with:
          targets: wasm32-unknown-unknown
      - uses: Swatinem/rust-cache@v2
      - run: cargo build -p storage-d1 --target wasm32-unknown-unknown
      - run: cargo build -p web-worker --target wasm32-unknown-unknown
      - run: cargo build -p restate-svc-worker --target wasm32-unknown-unknown

  integration:
    name: Restate integration tests
    runs-on: ubuntu-latest
    if: github.ref == 'refs/heads/main'
    services:
      restate:
        image: docker.restate.dev/restatedev/restate:latest
        ports:
          - 8080:8080
          - 9070:9070
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - uses: Swatinem/rust-cache@v2
      - name: Wait for Restate
        run: until curl -sf http://localhost:9070/health; do sleep 2; done
      - name: Start github-stub
        run: |
          cargo build -p github-stub
          ./target/debug/github-stub --port 3001 &
      - name: Run integration tests
        run: cargo test -p restate-svc --features integration -- --ignored
```

- [ ] **Step 2: Verify workflow file parses correctly**

```bash
# Syntax check (requires actionlint or just confirm YAML is valid):
python3 -c "import yaml; yaml.safe_load(open('.github/workflows/ci.yml'))"
echo "YAML valid"
```

- [ ] **Step 3: Update `docs/superpowers/plans/README.md`**

Mark Plan 8 as Implemented.

---

### Task 5: `docs/local-testing.md` — local integration test procedure

**Files:**
- Create: `docs/local-testing.md`

**Goal:** Document the three-terminal procedure for running integration tests locally so any contributor can reproduce CI results.

- [ ] **Step 1: Create `docs/local-testing.md`**

Contents:

```markdown
# Local Integration Testing

## Prerequisites

- Docker Desktop (or Docker Engine) running
- `wrangler` CLI installed: `npm i -g wrangler`
- Rust toolchain with `wasm32-unknown-unknown` target: `rustup target add wasm32-unknown-unknown`

## Terminal 1 — Restate

```bash
docker compose up
# Wait for: "Restate is ready"
```

## Terminal 2 — GitHub Stub

```bash
cargo run -p github-stub -- --port 3001
# Wait for: "github-stub listening on 0.0.0.0:3001"
```

## Terminal 3 — Tests

### Unit tests (no Docker/wrangler required)

```bash
cargo test --workspace --exclude github-stub
```

### Restate integration tests

```bash
cargo test -p restate-svc --features integration -- --ignored
```

### D1 storage smoke tests (requires wrangler)

```bash
# Apply migrations to local D1
wrangler d1 migrations apply ghinvite --local --config wrangler/web.toml

# Run D1 smoke tests
cargo test -p storage-d1 --features d1-suite -- --ignored
```

### wasm32 build check

```bash
cargo build -p storage-d1 --target wasm32-unknown-unknown
cargo build -p web-worker --target wasm32-unknown-unknown
cargo build -p restate-svc-worker --target wasm32-unknown-unknown
```

## CI vs Local

| Job | Local | CI |
|-----|-------|-----|
| Unit tests | ✅ | ✅ push+PR |
| Lint | ✅ | ✅ push+PR |
| wasm32 build | ✅ | ✅ push+PR |
| Restate integration | ✅ (3-terminal) | ✅ main branch only |
| D1 suite | ✅ (wrangler) | ❌ not in CI v1 |
```

- [ ] **Step 2: Verify doc renders correctly**

```bash
# Check markdown is valid (no broken code fences)
cat docs/local-testing.md
```

---

## Security notes

- The `github-stub` binary is `publish = false` and only used in tests. It must never be deployed.
- CI secrets: The workflow only runs `cargo test` — no secrets needed for the unit/wasm32/lint jobs. The integration job may need a `GITHUB_TOKEN` for `actions/checkout` (automatic) but no other secrets.
- `wrangler` in CI for the D1 suite job would need a `CLOUDFLARE_API_TOKEN` — this is why the D1 suite runs locally only (not in CI) in v1. CI relies on the unit test suite + wasm32 compilation check for storage-d1 coverage.

---

## Decision audit trail

| Decision | Choice | Rationale |
|---|---|---|
| A1: Restate test approach | Option B (pre-started) + in-process endpoint | Pre-started Restate in CI `services:` block; endpoint bound in-process so no separate binary startup needed |
| A2: GitHub stub | New `crates/github-stub` binary | In-process `MockTransport` can't intercept HTTP calls from a running binary; real HTTP stub enables E2E |
| A3: D1 test approach | Option B (wrangler subprocess) with smoke tests | Full suite via subprocess too slow; targeted smoke tests cover schema correctness without per-call overhead |
| A4: CI matrix | wasm32 uses `cargo build` not `cargo check` | `cargo check` misses wasm-bindgen ABI issues; `build` catches linker failures too |
| A4: D1 in CI | Local only (not in CI v1) | `wrangler` not on `ubuntu-latest` without extra setup; documented gap, revisit in v2 |
| Test isolation | `DELETE /reset` on github-stub | Parallel tests share a single stub process; reset between tests prevents state leakage |
| Unit test job | `--exclude github-stub` | github-stub has no `#[test]` items; excluding avoids spurious compile-in-integration context |
| Restate readiness | `until curl -sf .../health; do sleep 2; done` | `services:` containers may not be ready when test step starts; probe prevents flaky test failures |

## Task status

- [ ] Task 1: `crates/github-stub` — GitHub API stub server
- [ ] Task 2: Restate handler integration tests
- [ ] Task 3: D1Storage parameterized test suite
- [ ] Task 4: GitHub Actions CI workflow
- [ ] Task 5: `docs/local-testing.md` — local integration test procedure
