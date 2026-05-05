# ghinvite

GitHub invitation management via share links. Organisation owners create invite links; recipients request access; owners approve; the app sends the GitHub collaborator invitation automatically.

## Architecture

Two Cloudflare Workers sharing one D1 database:

- **ghinvite-web** (`crates/web-worker`) — HTTP, SSR, OAuth, webhooks
- **ghinvite-restate-svc** (`crates/restate-svc-worker`) — durable workflow handlers via [Restate](https://restate.dev)

The core business logic lives in native Rust crates (`crates/restate-svc`, `crates/web`, `crates/storage`, etc.) compiled for both the host (tests) and wasm32 (Workers).

## Prerequisites

- Rust 1.85 (managed by `rust-toolchain.toml` — `rustup` picks it up automatically)
- Node.js 20+ (for building CSS)
- Docker (for Restate)

With [devenv](https://devenv.sh): `devenv shell` gives you Rust + Node + lld in one step.

## GitHub App setup

You need a GitHub App before the service can authenticate users or send invitations.

1. Go to **GitHub → Settings → Developer settings → GitHub Apps → New GitHub App**
2. Fill in:
   - **Homepage URL**: `http://127.0.0.1:8787` (or your public URL)
   - **Callback URL**: `http://127.0.0.1:8787/oauth/callback`
   - **Webhook URL**: `http://<public-url>/webhooks/github` (use [ngrok](https://ngrok.com) for local dev)
   - **Webhook secret**: any random string — set it as `GHINVITE_WEBHOOK_SECRET`
3. **Repository permissions**: Members → Read & Write, Metadata → Read-only
4. **Subscribe to events**: `Installation`, `Member`
5. After creating: note the **App ID** (`GHINVITE_GITHUB_APP_ID`) and generate a **private key** (`.pem` file, `GHINVITE_GITHUB_APP_PRIVATE_KEY_FILE`)
6. Under **OAuth** in the app settings: note the **Client ID** and generate a **Client secret** (`GHINVITE_GITHUB_CLIENT_ID` / `GHINVITE_GITHUB_CLIENT_SECRET`)
7. Install the app on your org or personal account and note the install URL: `https://github.com/apps/<your-app-name>/installations/new`

## Running unit tests

No services required:

```bash
cargo test --workspace --exclude github-stub
```

## Running locally (full stack)

Two options: native Rust binaries (faster iteration) or Wrangler dev (closer to production).

### Option A — Native binaries (recommended for development)

First-time only — build the CSS:

```bash
cd crates/web && npm install && npm run build:css && cd ../..
```

Three terminals:

**Terminal 1 — Restate**

```bash
docker compose up
```

Wait for: `Restate is ready`

**Terminal 2 — restate-svc**

```bash
GHINVITE_GITHUB_APP_ID=<your-app-id> \
GHINVITE_GITHUB_APP_PRIVATE_KEY_FILE=path/to/private-key.pem \
GHINVITE_DATABASE_PATH=./dev.sqlite \
cargo run -p restate-svc
```

On first run (or after `rm dev.sqlite`), migrations are applied automatically. You can also pass the PEM inline via `GHINVITE_GITHUB_APP_PRIVATE_KEY` instead of a file path. Without either, the binary starts but GitHub API calls will fail at runtime.

After it starts, register it with Restate once:

```bash
# macOS / Docker Desktop — restate-svc is on the host, Restate is in Docker:
curl -X POST http://localhost:9070/restate/v1/deployments \
  -H 'Content-Type: application/json' \
  -d '{"uri": "http://host.docker.internal:9080"}'

# Linux — use the Docker bridge IP instead (host.docker.internal is not automatic):
# curl -X POST http://localhost:9070/restate/v1/deployments \
#   -H 'Content-Type: application/json' \
#   -d '{"uri": "http://172.17.0.1:9080"}'
```

> **Note:** `restate-svc` binds to `127.0.0.1:9080` by default, so the URI you give to Restate must resolve to the host from *inside* the Restate container — not just from your shell. Use `GHINVITE_LISTEN_ADDR=0.0.0.0:9080 cargo run -p restate-svc` if `host.docker.internal` is unavailable on your platform.

**Terminal 3 — web**

```bash
GHINVITE_GITHUB_CLIENT_ID=<your-oauth-client-id> \
GHINVITE_GITHUB_CLIENT_SECRET=<your-oauth-client-secret> \
GHINVITE_GITHUB_INSTALL_URL=https://github.com/apps/<your-app-name>/installations/new \
GHINVITE_DATABASE_PATH=./dev.sqlite \
cargo run -p web
```

App available at `http://127.0.0.1:8787`. Both services point at the same `dev.sqlite` file. OAuth login requires a real GitHub App with `http://127.0.0.1:8787/oauth/callback` as the callback URL. All other env vars have safe defaults.

### Option B — Wrangler dev (production-equivalent)

First-time only:

```bash
wrangler d1 migrations apply ghinvite --local --config wrangler/web.toml
```

Then:

```bash
# Terminal 1
docker compose up

# Terminal 2
wrangler dev --config wrangler/web.toml

# Terminal 3
wrangler dev --config wrangler/restate-svc.toml
```

Register restate-svc with Restate after it starts (port may differ — check Wrangler output):

```bash
curl -X POST http://localhost:9070/restate/v1/deployments \
  -H 'Content-Type: application/json' \
  -d '{"uri": "http://localhost:8787"}'
```

See `.dev.vars` (created during deploy setup — `docs/deploy.md`) for the full env var list.

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
