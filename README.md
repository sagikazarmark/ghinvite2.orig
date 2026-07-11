# ghinvite

GitHub invitation management via invitation links. Organisation owners create invite links; recipients request access; owners approve; the app sends the GitHub collaborator invitation automatically.

## Architecture

Two Cloudflare Workers sharing one D1 database:

- **ghinvite-web** (`crates/ghinvite-web-worker`) — HTTP, SSR, OAuth, webhooks
- **ghinvite-restate-svc** (`crates/ghinvite-workflows-worker`) — durable workflow handlers via [Restate](https://restate.dev)

The core business logic lives in native Rust crates (`crates/ghinvite-workflows`, `crates/ghinvite-web`, `crates/ghinvite-storage-sqlx`, etc.) compiled for both the host (tests) and wasm32 (Workers).

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
   - **Setup URL**: `http://127.0.0.1:8787/setup/github` (or `{GHINVITE_BASE_URL}/setup/github` in production)
   - Enable **Redirect on update** so repository-selection changes return to ghinvite
   - **Webhook URL**: `http://<public-url>/webhooks/github` (use [ngrok](https://ngrok.com) for local dev)
   - **Webhook secret**: any random string — set it as `GHINVITE_WEBHOOK_SECRET`
3. **Repository permissions**: Administration → Read & write, Metadata → Read-only
4. **Subscribe to events**: `Member` and `Repository invitation` if available for your selected permissions. GitHub sends `installation` and `installation_repositories` to GitHub Apps by default; they are app-level events and may not appear as repository-level checkboxes.
5. After creating: note the **App ID** (`GHINVITE_GITHUB_APP_ID`) and generate a **private key** (`.pem` file, `GHINVITE_GITHUB_APP_PRIVATE_KEY_FILE`)
6. Under **OAuth** in the app settings: note the **Client ID** and generate a **Client secret** (`GHINVITE_GITHUB_CLIENT_ID` / `GHINVITE_GITHUB_CLIENT_SECRET`)
7. Install the app on your org or personal account and note the install URL: `https://github.com/apps/<your-app-name>/installations/new`

### Troubleshooting GitHub App setup

- **No redirect after install**: confirm the GitHub App **Setup URL** is `{GHINVITE_BASE_URL}/setup/github` and **Redirect on update** is enabled.
- **`missing installation_id`**: GitHub did not return through the Setup URL. Recheck the Setup URL and install the app from the app installation URL again.
- **`installation is not visible to signed-in user`**: sign out of ghinvite, sign in with the GitHub user that installed or can administer the app installation, then retry the GitHub App install/update.
- **`Restate error` or setup returns 502**: make sure Restate is running, the `Installation` service is registered, and `GHINVITE_RESTATE_INGRESS` points at the active Restate ingress.
- **Repository picker is empty after a selected-repository install**: reopen the GitHub App installation settings, verify repository access, and use **Update** so GitHub redirects back to ghinvite with `setup_action=update`.
- **Permission failures when inviting collaborators**: confirm repository permissions are **Administration: Read & write** and **Metadata: Read-only**.

## Running unit tests

No services required:

```bash
cargo test --workspace
```

## Running locally (full stack)

Two options: native Rust binaries (faster iteration) or Wrangler dev (closer to production).

### Option A — Native binaries (recommended for development)

First-time only — build the CSS:

```bash
cd crates/ghinvite-web && npm install && npm run build:css && cd ../..
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
cargo run -p ghinvite-workflows
```

On first run (or after `rm dev.sqlite`), migrations are applied automatically. You can also pass the PEM inline via `GHINVITE_GITHUB_APP_PRIVATE_KEY` instead of a file path. GitHub App keys work as downloaded, whether they start with `BEGIN RSA PRIVATE KEY` or `BEGIN PRIVATE KEY`. Without either, the binary starts but GitHub API calls will fail at runtime.

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

> **Note:** `ghinvite-workflows` binds to `127.0.0.1:9080` by default, so the URI you give to Restate must resolve to the host from *inside* the Restate container — not just from your shell. Use `GHINVITE_LISTEN_ADDR=0.0.0.0:9080 cargo run -p ghinvite-workflows` if `host.docker.internal` is unavailable on your platform.

**Terminal 3 — web**

```bash
GHINVITE_GITHUB_CLIENT_ID=<your-oauth-client-id> \
GHINVITE_GITHUB_CLIENT_SECRET=<your-oauth-client-secret> \
GHINVITE_GITHUB_INSTALL_URL=https://github.com/apps/<your-app-name>/installations/new \
GHINVITE_DATABASE_PATH=./dev.sqlite \
cargo run -p ghinvite-web
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
  ghinvite-core/         — domain types, audit events, Storage trait + conformance suite
  ghinvite-storage-sqlx/ — SqlxStorage: Storage impl for native dev/tests
  ghinvite-storage-d1/   — D1Storage: Storage impl for wasm32/production
  ghinvite-github/       — GitHub API clients (OAuth + installation tokens);
                           examples/stub.rs is the API stub for integration tests
  ghinvite-workflows/    — Restate handler logic (native, tested without Workers)
  ghinvite-workflows-worker/ — wasm32 entry point wiring workflows + D1
  ghinvite-web/          — axum app + Dioxus SSR layouts (native, tested without Workers)
  ghinvite-web-worker/   — wasm32 entry point wiring web + D1
migrations/        — Shared SQL migration files (sqlx + wrangler D1)
wrangler/          — wrangler.toml configs for both workers
docs/
  deploy.md        — production deployment guide
  local-testing.md — integration test procedure
```

## CI

GitHub Actions runs on every push and PR to `main`:

- **Test** — `cargo test --workspace`
- **Lint** — `cargo fmt --check` + `cargo clippy`
- **wasm32 build** — `cargo build` for all three wasm32 targets
- **Integration** (main branch only) — Restate-in-Docker + the github stub example + `cargo test --features integration`
