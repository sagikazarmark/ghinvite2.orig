# ghinvite

GitHub invitation management via invitation links. Organisation owners create invite links; recipients request access; owners approve; the app sends the GitHub collaborator invitation automatically.

## Architecture

See [invitation ownership and recovery](docs/invitation-ownership.md) for the
current owners, retained safety/history records, acceptance gates and coordinated
clean prelaunch replacement, following ADR 0009.

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
dagger check ghinvite:toolkit
```

`dagger check` runs all nine checks (including `ghinvite:toolkit` for `libs/octoauth`). The CSS check fails when the tracked
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
   - **Setup URL**: `http://127.0.0.1:8787/setup/repo` (or `https://{P1_HOST}/setup/repo` in production)
   - Enable **Redirect on update** so repository-selection changes return to ghinvite
   - **Webhook URL**: `http://<public-url>/webhooks/repo` (use [ngrok](https://ngrok.com) for local dev)
   - **Webhook secret**: any random string — set it as `GHINVITE_WEBHOOK_SECRET`
3. **Repository permissions**: Administration → Read & write, Metadata → Read-only
4. For webhook acceptance, enable **Organization permissions: Members → Read-only** and subscribe to **Member** (`member`, action `added`). GitHub sends `installation` and `installation_repositories` to GitHub Apps by default. See [supported webhook evidence and subscription requirements](docs/invitation-settlement.md#member-added-webhooks), including personal-account availability and legacy compatibility.
5. For the workflow repo App, record its actual **App ID** (`GH_REPO_APP_ID`) and **Client ID** (`GH_REPO_CLIENT_ID`), then generate a private key. The workflow uses the `GH_REPO_PRIVATE_KEYS` PKCS#8 rotation list described below; web signs in through the separate sign-in App (`GH_SIGNIN_CLIENT_ID` / `GH_SIGNIN_CLIENT_SECRET`).
6. For sign-in, use the sign-in App ([manifests](deploy/github-apps/README.md)): note its **Client ID** and generate a **Client secret** (`GH_SIGNIN_CLIENT_ID` / `GH_SIGNIN_CLIENT_SECRET`); its callback URL is `http://127.0.0.1:8787/oauth/callback` locally
7. Install the app on your org or personal account and note the install URL: `https://github.com/apps/<your-app-name>/installations/new`

### Troubleshooting GitHub App setup

- **No redirect after install**: confirm the GitHub App **Setup URL** is `https://{P1_HOST}/setup/repo` and **Redirect on update** is enabled.
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

Invitation links use `/i/<26-character-ULID>` (for example,
`/i/01ARZ3NDEKTSV4RRFFQ69G5FAV`). This is the same ID shown as the Invitation Code
in the Console. Confirmed links open immediately even while SQL projection lags.

The single-ID model is a clean prelaunch replacement. Start native SQLite or D1
from the current migrations and pair it with a fresh Restate environment running
the matching handlers and web build. Existing 16-character codes have no aliases.
Resetting an existing local environment is a separate, explicitly chosen operator
action; updating or building this checkout does not reset it.

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
P1_HOST=localhost \
GH_REPO_APP_ID="<your-repo-app-id>" \
GH_REPO_CLIENT_ID="<your-repo-app-client-id>" \
GH_REPO_PRIVATE_KEYS="<kid:base64-PKCS8-PEM>" \
GHINVITE_DATABASE_PATH=./dev.sqlite \
GHINVITE_LISTEN_ADDR=0.0.0.0:9080 \
cargo run -p ghinvite-workflows-server -- --local-unsigned
```

The explicit `--local-unsigned` flag is for this local Restate container. Native
production startup requires `RESTATE_IDENTITY_KEYS`, a comma/whitespace-separated
list of `publickeyv1_…` verification keys, and omits that flag. Invalid or missing
keys stop startup before opening SQL. The Worker always requires the key list.

On first run (or after `rm dev.sqlite`), migrations are applied automatically. All four app variables above are required; missing or malformed values fail before storage startup. `GH_REPO_PRIVATE_KEYS` is a semicolon-separated `kid:base64(PKCS#8 PEM)` list, first entry signs. Convert downloaded PKCS#1 keys with `openssl pkcs8 -topk8 -nocrypt` before encoding. Supply the list through your secret manager or shell environment without committing it. There is no dummy-key or old-variable fallback. Tests use their own explicit fixtures.

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

Generate a local keyring descriptor once in this shell (or keep it in your local
secret manager). Both native and Worker deployments require a valid kekring v1
descriptor (`libs/kekring/README.md`); there is no built-in development key.

```bash
export P1_KEYRING="$(cargo run -q -p kekring --example descriptor -- new p1)"
```

```bash
GH_SIGNIN_CLIENT_ID=<sign-in-app-client-id> \
GH_SIGNIN_CLIENT_SECRET=<sign-in-app-client-secret> \
GHINVITE_GITHUB_INSTALL_URL=https://github.com/apps/<your-app-name>/installations/new \
GHINVITE_DATABASE_PATH=./dev.sqlite \
GHINVITE_RESTATE_AUTH=local-unauthenticated \
cargo run -p ghinvite-web-server -- --local-dev
```

App available at `http://127.0.0.1:8787`. Both services point at the same `dev.sqlite` file for domain data. Native sessions use a separate in-memory SQLite database, encrypted with the configured session key, and disappear on restart. Sign-in requires a real sign-in GitHub App with `http://127.0.0.1:8787/oauth/callback` as the callback URL. Other env vars have local defaults.

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
  ghinvite-workflows/    — Restate handler logic (runtime-agnostic, tested without Workers)
  ghinvite-workflows-server/ — native entry point wiring workflows + SqlxStorage
  ghinvite-workflows-worker/ — wasm32 entry point wiring workflows + D1
  ghinvite-ui/           — Dioxus view components + shared form model (Dioxus + core only; browser-buildable)
  ghinvite-island/       — browser island (dioxus-web + dioform) for the new-link form; built by scripts/build-island.sh
  ghinvite-web/          — axum app + Dioxus SSR of ghinvite-ui (runtime-agnostic, tested without Workers)
  ghinvite-web-server/   — native entry point wiring web + SqlxStorage + SQLite sessions
  ghinvite-web-worker/   — wasm32 entry point wiring web + D1
  ghinvite-config/       — environment and GitHub App configuration parsing
  ghinvite-email-vault/  — encrypted email escrow for bulk invites
  ghinvite-e2e/          — Restate end-to-end harness glue (tests only)
libs/
  octoauth/          — the GitHub App toolkit, its own Cargo workspace: `octoauth`/`octotoken`
                       (App JWT, installation tokens), `octologin` (sign-in), `octoscrub` (redaction)
  kekring/           — `kekring`: provisioned versioned KEK (AES-256-KW) + canonical AAD for anykms envelopes;
                       formats in docs/kekring-format.md, fuzz/ and conformance/ are workspaces of their own
migrations/        — Shared SQL migration files (sqlx + wrangler D1)
scripts/           — build-island.sh: dx bundle → dist/public (Static Assets)
wrangler/          — wrangler.toml configs for both workers
docs/
  deploy.md        — production deployment guide
  local-testing.md — integration test procedure
  adr/             — architecture decisions of this codebase
  decisions/       — cross-cutting decisions inherited from the planning repository
  specs/           — product and component specs, build plans and research the code was built from
```

GitHub access is split between the toolkit crates in `libs/octoauth`
(`octoauth`/`octotoken` for App authentication and installation tokens,
`octologin` for sign-in, `octoscrub` for redaction; `octoevents` from crates.io
for webhooks) and `crates/ghinvite-github`, ghinvite's own REST client: transport
(native and Worker), typed endpoints and fixtures. Its `examples/stub.rs`
supplies the GitHub API fixture for integration tests. Third-party sources,
pinned forks and their reasons are listed in [`PROVENANCE.md`](PROVENANCE.md).

## CI

GitHub Actions runs on every push and PR to `main`:

- **Test** — `cargo test --workspace`
- **Lint** — `cargo fmt --check` + `cargo clippy`
- **GitHub App toolkit** — fmt, clippy, tests and the wasm32 check of the `libs/octoauth` workspace
- **wasm32 build** — `cargo check -p ghinvite-ui` and `-p ghinvite-island` (browser) plus `cargo build` for the three Worker-side crates, each with `-p` and `--target wasm32-unknown-unknown`
- **Island bundle** — `scripts/build-island.sh` (dx bundle, size budget), `cargo test -p ghinvite-island` (markup parity), clippy for wasm32; uploads `dist/public` as an artifact
- **Restate approval and delivery gate** — `bash scripts/test-restate.sh <target>` for `authoritative_admission`, `retained_delivery`, `installation_availability`, `installation_audit_replay` and `durable_projection`: digest-pinned disposable Restate, real GitHub HTTP client against a local GitHub stub, authoritative admission/lifecycle, retained delivery and projection recovery
