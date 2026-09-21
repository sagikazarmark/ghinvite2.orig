# ghinvite

GitHub invitation management via invitation links. Organisation owners create invite links; recipients request access; owners approve; the app sends the GitHub collaborator invitation automatically.

## Architecture

Two Cloudflare Workers sharing one D1 database:

- **ghinvite-web** (`crates/ghinvite-web-worker`) — HTTP, SSR, OAuth, webhooks
- **ghinvite-restate-svc** (`crates/ghinvite-workflows-worker`) — durable workflow handlers via [Restate](https://restate.dev)

The core business logic lives in native Rust crates (`crates/ghinvite-workflows`, `crates/ghinvite-web`, `crates/ghinvite-storage-sqlx`, etc.) compiled for both the host (tests) and wasm32 (Workers).

The Dioxus view components live in their own crate, `crates/ghinvite-ui`, which depends only on Dioxus, dioform-core and `crates/ghinvite-core` — no axum, sessions, GitHub client, or storage. `ghinvite-web` renders them on the server with `dioxus_ssr`; per [ADR 0001](docs/adr/0001-ssr-first-with-dioxus-islands.md) it is also the only crate a browser-side Dioxus island may depend on. The first such island is `crates/ghinvite-island`: a `dioxus-web` bundle (built with `scripts/build-island.sh`, served from Cloudflare Static Assets under `/assets/`) that mounts on the new invitation link form and re-renders the same component with dioform bindings — inline validation with the server's own rules, and a plain browser POST when nothing blocks. Because `cfg(target_arch = "wasm32")` means "Cloudflare Workers" in the server crates and "browser" in `ghinvite-ui` / `ghinvite-island`, wasm32 builds are always per crate (`cargo build -p <crate> --target wasm32-unknown-unknown`), never `--workspace`.

## Prerequisites

- Rust 1.92 (managed by `rust-toolchain.toml` — `rustup` picks it up automatically)
- Node.js 20+ (for building CSS)
- Docker (for Restate)

With [devenv](https://devenv.sh) 2.3.1: `devenv shell` gives you Rust + Node + lld
and Dagger v1.0.0-beta.14 in one step. The repository pins devenv's modules in
`devenv.yaml`; upgrade an older host CLI using the
[devenv installation instructions](https://devenv.sh/getting-started/).

## Dagger checks and builds

With Docker running, enter `devenv shell`, then run from the repository root:

```bash
dagger version                  # v1.0.0-beta.14
dagger check -l                 # list project checks
dagger check ghinvite:fmt ghinvite:clippy ghinvite:test
dagger check ghinvite:wasm
dagger check ghinvite:css
dagger check ghinvite:island ghinvite:web-worker ghinvite:workflows-worker
```

`dagger check` runs all eight checks. The CSS check fails when the tracked
stylesheet needs regeneration, just like CI. Builds run in containers and can be
exported explicitly:

```bash
dagger api call ghinvite island --output dist/public
dagger api call ghinvite web-worker --output crates/ghinvite-web-worker/build
dagger api call ghinvite workflows-worker --output crates/ghinvite-workflows-worker/build
```

`dagger.toml` installs commit-pinned Rust, Dioxus, and worker-build modules from
[`daggerverse-beta`](https://github.com/sagikazarmark/daggerverse-beta), wiring
their `Container` outputs into `.dagger/main.dang`. That small project module
selects workspace flags, builds Wasm crates individually, and runs the existing
island staging/size-budget script. Tool installation and Cargo dependency caching
are owned by the upstream modules. `dagger.lock` records resolved container images.

The shell's packaged Dagger CLI uses `DAGGER_X_RELEASE=v1.0.0-beta.14` to download
and cache the selected beta on first use. A directly installed beta.14 CLI also
works. To update modules, change their commit references together in `dagger.toml`,
run `dagger workspace update`, run the checks, and include `dagger.lock` with the
configuration changes. Update the beta pin in both `devenv.nix` and
`.dagger/dagger-module.toml` when upgrading Dagger.

Refresh the pinned devenv and Dagger Nix inputs independently with
`devenv update devenv` and `devenv update dagger`. The existing nixpkgs revision is retained;
updating all inputs also upgrades the native Rust/Node toolchain and should be
validated separately.

The full Restate, Worker/D1 runtime, and Playwright acceptance suites are run by
the existing CI workflow and the commands in [local testing](docs/local-testing.md).
Dagger's Worker checks build production artifacts; the runtime gates additionally
verify their behavior. See [deployment](docs/deploy.md) for rollout requirements.

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
4. For webhook acceptance, enable **Organization permissions: Members → Read-only** and subscribe to **Member** (`member`, action `added`). GitHub sends `installation` and `installation_repositories` to GitHub Apps by default. See [supported webhook evidence and subscription requirements](docs/invitation-settlement.md#member-added-webhooks), including personal-account availability and legacy compatibility.
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

This excludes the real Restate acceptance gate and the opt-in D1/browser tests. Run the
Restate gate with `bash scripts/test-restate.sh`; see
[`docs/local-testing.md`](docs/local-testing.md) for prerequisites and coverage.

## Running locally (full stack)

Two options: native Rust binaries (faster iteration) or Wrangler dev (closer to production).

### Option A — Native binaries (recommended for development)

First-time only — build the CSS:

```bash
cd crates/ghinvite-web && npm install && npm run build:css && cd ../..
```

Optional — build the browser island for the new invitation link form (needs
`dx` 0.7.x and the `wasm32-unknown-unknown` target; without it the form is the
plain server-rendered one):

```bash
scripts/build-island.sh   # → dist/public, served by the native binary under /assets
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
GHINVITE_LISTEN_ADDR=0.0.0.0:9080 \
cargo run -p ghinvite-workflows
```

On first run (or after `rm dev.sqlite`), migrations are applied automatically. You can also pass the PEM inline via `GHINVITE_GITHUB_APP_PRIVATE_KEY` instead of a file path. GitHub App keys work as downloaded, whether they start with `BEGIN RSA PRIVATE KEY` or `BEGIN PRIVATE KEY`. Without either, the binary starts but GitHub API calls will fail at runtime.

After it starts, register it with Restate once:

```bash
# Restate is in Docker; compose.yaml maps this host alias on Linux/Desktop.
curl --fail-with-body --max-time 20 -X POST http://localhost:9070/deployments \
  -H 'Content-Type: application/json' \
  -d '{"uri": "http://host.docker.internal:9080"}'
```

> **Note:** The explicit listen address above lets the container reach the host
> endpoint. The binary's default `127.0.0.1:9080` is host-loopback only. The host
> firewall must allow the Docker bridge to reach port 9080.

**Terminal 3 — web**

Generate a local session secret once in this shell (or keep it in your local
secret manager). Both native and Worker deployments require exactly 64 hex
characters; there is no built-in development key.

```bash
export GHINVITE_SESSION_SECRET="$(openssl rand -hex 32)"
```

```bash
GHINVITE_GITHUB_CLIENT_ID=<your-oauth-client-id> \
GHINVITE_GITHUB_CLIENT_SECRET=<your-oauth-client-secret> \
GHINVITE_GITHUB_INSTALL_URL=https://github.com/apps/<your-app-name>/installations/new \
GHINVITE_DATABASE_PATH=./dev.sqlite \
GHINVITE_RESTATE_AUTH=local-unauthenticated \
cargo run -p ghinvite-web
```

App available at `http://127.0.0.1:8787`. Both services point at the same `dev.sqlite` file for domain data. Native sessions use a separate in-memory SQLite database, encrypted with the configured session key, and disappear on restart. OAuth login requires a real GitHub App with `http://127.0.0.1:8787/oauth/callback` as the callback URL. Other env vars have local defaults.

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
wrangler dev --ip 0.0.0.0 --port 8788 --config wrangler/restate-svc.toml
```

Register restate-svc with the local Restate server after it starts. Use a trusted
development host: the explicit listen address permits the Compose container to
reach Wrangler on port 8788.

```bash
restate deployments register --environment local --use-http1.1 http://host.docker.internal:8788
```

See `.dev.vars` (created during deploy setup — `docs/deploy.md`) for the full env var list.

## Integration tests

See [`docs/local-testing.md`](docs/local-testing.md) for the full integration test procedure (Restate handler tests, D1 smoke tests, wasm32 build checks).

## Deploying to production

See [`docs/deploy.md`](docs/deploy.md) for remote D1 verification, immutable
workflow endpoints and authenticated readiness. Production rollout remains blocked
on the operator-owned remote gates in #61; local verification does not
authorize deployment to live traffic.

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
  ghinvite-ui/           — Dioxus view components + shared form model (Dioxus + core only; browser-buildable)
  ghinvite-island/       — browser island (dioxus-web + dioform) for the new-link form; built by scripts/build-island.sh
  ghinvite-web/          — axum app + Dioxus SSR of ghinvite-ui (native, tested without Workers)
  ghinvite-web-worker/   — wasm32 entry point wiring web + D1
migrations/        — Shared SQL migration files (sqlx + wrangler D1)
scripts/           — build-island.sh: dx bundle → dist/public (Static Assets)
wrangler/          — wrangler.toml configs for both workers
docs/
  deploy.md        — production deployment guide
  local-testing.md — integration test procedure
```

## CI

GitHub Actions runs on every push and PR to `main`:

- **Test** — `cargo test --workspace`
- **Lint** — `cargo fmt --check` + `cargo clippy`
- **wasm32 build** — `cargo check -p ghinvite-ui` and `-p ghinvite-island` (browser) plus `cargo build` for the three Worker-side crates, each with `-p` and `--target wasm32-unknown-unknown`
- **Island bundle** — `scripts/build-island.sh` (dx bundle, size budget), `cargo test -p ghinvite-island` (markup parity), clippy for wasm32; uploads `dist/public` as an artifact
- **Restate approval and delivery gate** — `bash scripts/test-restate.sh <target>` for `authoritative_admission`, `retained_delivery`, `installation_availability`, `installation_audit_replay` and `durable_projection`: digest-pinned disposable Restate, real GitHub HTTP client against a local GitHub stub, authoritative admission/lifecycle, retained delivery and projection recovery
