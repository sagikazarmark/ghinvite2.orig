# ghinvite

GitHub invitation management via share links. Organisation owners create invite links; recipients request access; owners approve; the app sends the GitHub collaborator invitation automatically.

## Architecture

Two Cloudflare Workers sharing one D1 database:

- **ghinvite-web** (`crates/web-worker`) — HTTP, SSR, OAuth, webhooks
- **ghinvite-restate-svc** (`crates/restate-svc-worker`) — durable workflow handlers via [Restate](https://restate.dev)

The core business logic lives in native Rust crates (`crates/restate-svc`, `crates/web`, `crates/storage`, etc.) compiled for both the host (tests) and wasm32 (Workers).

## Prerequisites

- Rust 1.85 (managed by `rust-toolchain.toml` — `rustup` picks it up automatically)
- Node.js 20+ (for Tailwind/wrangler)
- Docker (for Restate in local integration tests)

With [devenv](https://devenv.sh): `devenv shell` gives you Rust + Node + lld in one step.

## Running unit tests

No services required:

```bash
cargo test --workspace --exclude github-stub
```

## Running locally (full stack)

Three terminals:

**Terminal 1 — Restate**

```bash
docker compose up
```

Wait for: `Restate is ready`

**Terminal 2 — web worker** (Wrangler dev mode, uses local D1 + KV)

First-time only — apply migrations to local D1:

```bash
wrangler d1 migrations apply ghinvite --local --config wrangler/web.toml
```

Then start the worker:

```bash
wrangler dev --config wrangler/web.toml
```

Copy the `.dev.vars` template (see `docs/deploy.md`) and fill in your GitHub App credentials before starting.

**Terminal 3 — restate-svc worker**

```bash
wrangler dev --config wrangler/restate-svc.toml
```

After both workers are running, register the restate-svc endpoint with Restate:

```bash
curl -X POST http://localhost:9070/restate/v1/deployments \
  -H 'Content-Type: application/json' \
  -d '{"uri": "http://localhost:8787"}'
```

The web worker is then available at `http://localhost:8788` (or whichever port Wrangler assigns).

## Integration tests

See [`docs/local-testing.md`](docs/local-testing.md) for the full integration test procedure (Restate handler tests, D1 smoke tests, wasm32 build checks).

## Deploying to production

See [`docs/deploy.md`](docs/deploy.md) for the full Cloudflare + Restate Cloud deployment guide.

## Project layout

```
crates/
  domain/          — types, IDs, state machines, slug
  audit/           — audit event definitions
  storage/         — Storage trait + SqlxStorage (native dev/tests)
  storage-d1/      — D1Storage (wasm32/production)
  github/          — GitHub API clients (OAuth + installation tokens)
  restate-svc/     — Restate handler logic (native, tested without Workers)
  restate-svc-worker/ — wasm32 entry point wrapping restate-svc
  web/             — axum app + Dioxus SSR layouts (native, tested without Workers)
  web-worker/      — wasm32 entry point wrapping web
  github-stub/     — GitHub API stub binary for integration tests
migrations/        — Shared SQL migration files (sqlx + wrangler D1)
wrangler/          — wrangler.toml configs for both workers
docs/
  deploy.md        — production deployment guide
  local-testing.md — integration test procedure
```

## CI

GitHub Actions runs on every push and PR to `main`:

- **Test** — `cargo test --workspace --exclude github-stub`
- **Lint** — `cargo fmt --check` + `cargo clippy`
- **wasm32 build** — `cargo build` for all three wasm32 targets
- **Integration** (main branch only) — Restate-in-Docker + github-stub + `cargo test --features integration`
