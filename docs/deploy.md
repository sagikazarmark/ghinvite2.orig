# ghinvite Deployment Guide

## Rollout status and order

**Production rollout remains blocked under [#61](https://github.com/sagikazarmark/ghinvite2.orig/issues/61).**
This procedure describes the target deployment and disposable remote rehearsal;
documentation, passing local tests, and provisioning alone do not authorize rollout.
#58/#59 completed native cutover tooling and local Worker/D1 verification. The
installation-availability integration (#60) and authenticated web ingress (#77)
are implemented; their remote deployment evidence is still required.

Run commands from the repository root against an explicitly selected Cloudflare
account and Restate environment. For an existing installation, use this order:

1. Close web ingress (including alternate Worker URLs), webhooks and scheduled
   producers; set web `GHINVITE_ADMISSION_MODE=maintenance`. Inventory pinned
   invocations, artifacts, bindings and uncertain external effects.
2. Prepare the coordinated checkpoints and old-writer fencing/isolation from
   [admission cutover](admission-cutover.md). Drain old installation invocations
   on their original code before isolating their endpoints. The cutover SQLite
   CLI is **not** a live D1 export/adoption tool; the remote procedure must first
   be implemented and rehearsed under #61.
3. Apply and verify remote migrations, then register a new immutable workflow
   endpoint. Include [installation availability](installation-availability.md):
   `Installation`, `AccountInstallationV1` and `InstallationProjectionV1` must ship
   together. Unchanged wire interfaces do not imply unchanged journals.
4. Complete the [invitation settlement cutover](invitation-settlement.md#deployment-and-pinned-invocations),
   including versioned webhook/scheduler callers and retained receipts. Select
   `authoritative` for the new workflow endpoint only through the cutover process.
5. Deploy web in maintenance with its matching bindings. Follow the cutover
   runbook for projection adoption, import, verification and activation/handoff,
   then verify authoritative web operations on restricted rehearsal ingress before
   reopening producers. Both configs default to `legacy`; neither a migration nor
   a deployment switches authority. A fresh empty environment has no legacy rows
   to import, but still requires the remote gates before authoritative live traffic.

After activation/new-authority writes, recovery is compatible forward repair or
reviewed reverse reconciliation, never a switch to stale SQL. Preserve admission
outcomes, uses, deadlines, dispatch/create/settlement receipts and historical audits.
See [Worker recovery limits](worker-admission-gate.md#remaining-rollout-gates--fault-model-limits)
and [rollback](#rollback) before any upgrade.

## Prerequisites

- Cloudflare account with Workers paid plan (or free tier for testing)
- Node.js 20.20.2 or a compatible supported Node release.
- [Wrangler CLI](https://developers.cloudflare.com/workers/wrangler/install-and-update/) **4.71.0**: `npm install -g wrangler@4.71.0` (command-validation baseline for this guide).
- Restate CLI **1.7.9**: `npm install -g @restatedev/restate@1.7.9`; configure the target environment's Admin URL and admin credential using the CLI/secret manager. Admin credentials are separate from the web ingress key.
- [`worker-build`](https://crates.io/crates/worker-build) **0.8.1** installed: `cargo install worker-build --version 0.8.1 --locked`
- wasm-bindgen CLI **0.2.120**, matching `Cargo.lock`: `cargo install wasm-bindgen-cli --version 0.2.120 --locked`.
  Export `WASM_BINDGEN_BIN=$(command -v wasm-bindgen)` when invoking Wrangler so
  worker-build uses the matching CLI. The CI packaging gate sets this explicitly.
- [Dioxus CLI](https://dioxuslabs.com/learn/0.7/getting_started/) 0.7.x installed (`cargo binstall dioxus-cli@0.7.9`) and the `wasm32-unknown-unknown` target (`rustup target add wasm32-unknown-unknown`), for the island bundle
- GitHub App created and configured (see GitHub App Setup below)
- Restate Cloud account (or self-hosted Restate server)

## Architecture

Two Cloudflare Workers share one D1 database:

- **ghinvite-web** (`crates/ghinvite-web-worker`) — handles HTTP requests, SSR, OAuth, webhooks
- **ghinvite-restate-svc** (`crates/ghinvite-workflows-worker`, handlers in `crates/ghinvite-workflows`) — Restate durable workflow handlers

Production requires protected HTTPS Restate ingress. The web server authenticates
every ingress call/send with `Authorization: Bearer <API key>`, including setup,
authoritative reads and mutations. This is separate from endpoint identity signing:

| Credential | Secret binding | Direction / purpose |
|---|---|---|
| Restate Cloud environment ingress API key | `GHINVITE_RESTATE_API_KEY` on **web** | Web → Restate; authorizes invoking services |
| Restate endpoint identity key | `RESTATE_IDENTITY_KEY` on **restate-svc** | Restate → workflow Worker; verifies signed runtime requests |

Neither replaces the other. Do not expose ingress API keys to browsers or put them
in URLs, UI props, assets, `[vars]`, tracked configuration, or diagnostic logs.

## First-time Setup

### 1. Authenticate with Cloudflare

```bash
wrangler login
wrangler whoami
```

### 2. Create D1 database

```bash
wrangler d1 create ghinvite
```

Copy the `database_id` from the output into `wrangler/web.toml` and
`wrangler/restate-svc.toml`. Both `DB` bindings must name the same intended remote
database UUID in the selected account. Record that UUID in deployment evidence;
the display name `ghinvite` alone is not sufficient identification. These examples
use the top-level configs; if using named Wrangler environments, supply the same
`--env` on every migration, verification, secret and deployment command and verify
their environment-specific bindings.

### 3. Apply and verify remote database migrations

For an existing database, close writers and retain the coordinated recovery point
described above before applying changes. Rehearse on disposable D1 first. All
commands in this section deliberately use **`--remote`** and the application's
**`DB` binding**; local migration success is unrelated to remote schema state.

```bash
wrangler d1 migrations list DB --remote --config wrangler/web.toml
wrangler d1 migrations apply DB --remote --config wrangler/web.toml
wrangler d1 migrations list DB --remote --config wrangler/web.toml

# Verify the applied ledger AND schema through each intended binding:
for config in wrangler/web.toml wrangler/restate-svc.toml; do
  wrangler d1 execute DB --remote --config "$config" \
    --command 'SELECT id, name, applied_at FROM d1_migrations ORDER BY id;'
  wrangler d1 execute DB --remote --config "$config" \
    --command "SELECT type, name, tbl_name, sql FROM sqlite_schema WHERE name NOT LIKE 'sqlite_%' ORDER BY type, name; PRAGMA foreign_key_check;"
done
```

Expect no unapplied migrations and ledger entries matching every SQL file in
`migrations/` for the release (currently 0001–0009). Compare the returned schema
definitions with those migrations, including projection columns, delivery fences,
settlement/member-webhook receipts, and the `admin_attempts_expiry` index. Expect
no foreign-key violations. Stop on missing/mismatched schema, migration errors or
an unexpected UUID in Wrangler output. Apply migrations only via the web config,
which declares `migrations_dir`; the workflow config is used here for readback.

> **Important:** Do not run `sqlx migrate run` against D1. sqlx manages the local SQLite dev database; Wrangler manages D1. The same SQL files in `migrations/` are shared, but tracking is separate.

### 4. Create KV namespace for sessions

```bash
wrangler kv namespace create "SESSIONS" --config wrangler/web.toml
```

Copy the `id` into `wrangler/web.toml` under `[[kv_namespaces]]`.

### 5. Update wrangler config vars

Complete [GitHub App Setup](#github-app-setup) first, then return here with the install URL, App ID, OAuth credentials, and webhook secret.

In `wrangler/web.toml`, update `[vars]`:
- `GHINVITE_BASE_URL` — your public domain (e.g. `https://ghinvite.example.com`)
- `GHINVITE_RESTATE_INGRESS` — your Restate Cloud ingress URL
- `GHINVITE_RESTATE_AUTH` — keep `bearer` in production (also the default if omitted)
- `GHINVITE_GITHUB_INSTALL_URL` — GitHub App installation URL

### 6. Set secrets

```bash
# Session secret (32 random bytes as 64-char hex):
openssl rand -hex 32 | wrangler secret put GHINVITE_SESSION_SECRET --config wrangler/web.toml

# GitHub OAuth (from GitHub App settings):
wrangler secret put GHINVITE_GITHUB_CLIENT_ID --config wrangler/web.toml
wrangler secret put GHINVITE_GITHUB_CLIENT_SECRET --config wrangler/web.toml

# GitHub webhook secret:
wrangler secret put GHINVITE_WEBHOOK_SECRET --config wrangler/web.toml

# Web -> Restate: provision an ingress API key for the target environment in
# Restate Cloud, then paste it at Wrangler's secret prompt (no Bearer prefix):
wrangler secret put GHINVITE_RESTATE_API_KEY --config wrangler/web.toml

# Restate service — GitHub App private key PEM as downloaded (base64-encoded for safe storage):
# Linux:
base64 -w0 private-key.pem | wrangler secret put GHINVITE_GITHUB_APP_PRIVATE_KEY --config wrangler/restate-svc.toml
# macOS:
base64 private-key.pem | tr -d '\n' | wrangler secret put GHINVITE_GITHUB_APP_PRIVATE_KEY --config wrangler/restate-svc.toml

# GitHub App ID is a var, not a secret — update GHINVITE_GITHUB_APP_ID in wrangler/restate-svc.toml [vars] directly.

# Restate verification public key (Cloud → Developers → Security → HTTP endpoints):
wrangler secret put RESTATE_IDENTITY_KEY --config wrangler/restate-svc.toml
```

Configure both secret sets before creating the final release versions. The
workflow Worker currently accepts unsigned requests when its identity binding is
absent: a successful upload is not proof that request identity is enforced. Verify
the exact version URL below. `RESTATE_IDENTITY_KEY` contains the verification
**public** key; Restate retains the signing private key.

### 7. Build the island bundle

`wrangler/web.toml` declares `[assets] directory = "../dist/public"`, the
Dioxus island bundle served by Cloudflare Static Assets. Build it before every
`wrangler deploy` (the directory must exist, and its hashed file names change
with the code):

```bash
scripts/build-island.sh   # dx bundle → dist/public/assets/ + dist/public/_headers
```

The script runs `dx bundle -p ghinvite-island --platform web --profile
island`, recreates `dist/public/` and copies into it only the hashed
`.js`/`.wasm` files, the stable `assets/ghinvite-island.js` loader the SSR
page references, and a `_headers` file marking the hashed files immutable. It
prints the raw and gzipped sizes and fails if the gzipped bundle exceeds
600 KB. Requires the Dioxus CLI 0.7.x (`cargo binstall dioxus-cli@0.7.9`, or
`cargo install dioxus-cli --version 0.7.9`) and the `wasm32-unknown-unknown`
target; `dx` runs the `cargo` on your `PATH`, so keep the rustup-managed one
first so `rust-toolchain.toml` applies. Do not put anything else in
`dist/public/` — in particular no `index.html`, which Static Assets would
serve for `/`. (The `ssr_fixture` example in `crates/ghinvite-island` writes
one for local smoke tests; re-run the script before deploying.)

### 8. Create the workflow version, then deploy web in maintenance

Keep `preview_urls = true` in `wrangler/restate-svc.toml`. Provision secrets and
the correct admission mode first. On first creation only, bootstrap the workflow
Worker with `wrangler deploy --config wrangler/restate-svc.toml`; do not register
its mutable hostname. For the release (and every subsequent upgrade):

```bash
export WASM_BINDGEN_BIN="$(command -v wasm-bindgen)"
wrangler versions upload --config wrangler/restate-svc.toml
```

Record the full Worker version ID and its **version-prefixed Preview URL** from
Wrangler output/Cloudflare Deployments, plus commit, artifact digest, SDK version,
compatibility date, admission mode and binding identities. Use that exact URL for
registration. An alias such as `staging-...` can move and is not version-specific.
`versions upload` makes the preview endpoint available without moving production
hostname traffic; it does not select Restate's deployment.

Deploy web with `GHINVITE_ADMISSION_MODE=maintenance` in its config while the
cutover/rehearsal is in progress (restore the approved target mode only at the
activation/readiness step):

```bash
scripts/build-island.sh   # always first: Static Assets pick up dist/public
wrangler deploy --config wrangler/web.toml
```

### 9. Register with Restate Cloud

```bash
export RESTATE_ENVIRONMENT=YOUR_CONFIGURED_ENVIRONMENT
export WORKFLOW_VERSION_URL=https://VERSION_PREFIX-ghinvite-restate-svc.YOUR_SUBDOMAIN.workers.dev
restate deployments register "$WORKFLOW_VERSION_URL"
restate deployments list
```

Verify the returned Restate deployment ID points to that exact version URL in the
intended environment, and discovery exposes the expected services/handlers for
the chosen admission mode. Registration changes routing for new invocations;
keep producers closed until cutover and readiness are complete. Do not use
`--force` to overwrite a deployment or bypass a compatibility refusal.

Register a **new version URL on every workflow code/config release**, even if
handler names and schemas are unchanged. Retained journals depend on code and SDK
entry ordering, not just wire compatibility. Never register the bare Worker
hostname, a mutable preview alias, or a redirect to "latest". In particular, do not
replay in-flight SDK 0.10 journals against SDK 0.12 code.

Retain each old version, exact endpoint, required compatible bindings/credentials
and artifact while any invocation is pinned to it, including suspended workflows,
retries and delayed timers. Inventory `pinned_deployment_id` using the
[cutover inventory](admission-cutover.md#inventory-and-coordinated-checkpoint).
Keep the original code reachable during an approved compatible drain, and verify
provider version/URL retention covers the drain and recovery window. Shared D1
schema changes must remain compatible with every still-running pinned version.
Deleting a version, disabling preview URLs, changing routes or revoking a needed
credential can strand pinned work; a new registration does not migrate it.

For an incompatible writer cutover, drain first or capture obligations/effects,
then permanently isolate the old URLs, controllers, DB access and outbound
credentials before import/handoff. Preserve artifacts and journals for controlled
recovery even after isolation. A runtime pause/kill or SQL fence cannot stop an
already-issued GitHub request. Retire a deployment only after every pinned
invocation and uncertain effect is accounted for; never repoint its URL at new code.

Provision the ingress key in the same Restate Cloud environment as
`GHINVITE_RESTATE_INGRESS`, with permission to invoke the web application's services.
For self-hosted production, provide an HTTPS ingress gateway that enforces the same
Bearer contract and prevents direct access to the unprotected runtime ports.
Use the exact ingress URL; do not rely on redirecting gateways for authentication.
Authenticated remote URLs must use HTTPS; HTTP is accepted only for loopback
development (`localhost`, loopback IPv4/IPv6).

### 10. Liveness and authenticated end-to-end readiness

`/health` returns constant `ok`, including in maintenance: it does not query D1 or
Restate. Maintenance returns 503 for other dynamic routes; static assets may
still serve. After enabling the intended mode on restricted rehearsal
ingress, these checks establish HTTP/asset liveness and webhook signature rejection:

```bash
export WEB_BASE_URL=https://YOUR_WEB_HOST
curl --fail-with-body --max-time 20 "$WEB_BASE_URL/health" # → ok
curl --fail-with-body --max-time 20 "$WEB_BASE_URL/"       # → HTML
curl -sS --max-time 20 -o /dev/null -w "%{http_code}" \
  -X POST "$WEB_BASE_URL/webhooks/github" \
  -d '{}'                                             # → 401
curl -sS --max-time 20 -o /dev/null -w "%{http_code}" \
  "$WEB_BASE_URL/assets/ghinvite-island.js"             # → 200 (island loader)
```

Readiness additionally requires the remote D1 readback in step 3, successful signed
Restate discovery/invocation at the exact version URL, rejection of unsigned direct
workflow requests, and the [authenticated production-binding verification](restate-ingress-gate.md).
Use actual web secret bindings for setup, authoritative reads/mutations and durable
webhook sends; verify invocation completion and projected results, not just queued
acknowledgements. Exercise missing/revoked ingress keys and rotation as that gate
requires. A direct operator curl with a valid key cannot substitute for web binding
evidence. Publish redacted evidence and the remaining #61 gates with an explicit
affirmative or blocked verdict before live producers are reopened.

## Local Development

### 1. Set up `.dev.vars`

Use `.dev.vars` for local Worker secrets. Generate your own session key with
`openssl rand -hex 32` and set `GHINVITE_SESSION_SECRET` there, along with local
GitHub OAuth app credentials when testing OAuth. Keep this file out of version control.

`GHINVITE_SESSION_SECRET` must be exactly 64 hexadecimal characters (32 bytes when
decoded). Native development reads the same format from the environment. Missing,
invalid, short, and oversized values are rejected; there is no default key.

For a credential-free **local runtime**, explicitly set these non-secret values
in your ignored `.dev.vars` beside the Wrangler config (native: environment):

```dotenv
GHINVITE_RESTATE_INGRESS=http://127.0.0.1:8080
GHINVITE_RESTATE_AUTH=local-unauthenticated
```

Leave `GHINVITE_RESTATE_API_KEY` absent in this mode; supplying both local mode
and a key is an error. Never use local mode with production ingress. For local
testing against Restate Cloud, use `bearer` and supply the API key as a secret.
Native boot and Worker configuration use the same parser: omitted auth mode
requires a key; missing/empty/malformed keys and unknown modes fail configuration.
Native reads `GHINVITE_RESTATE_API_KEY` from its server process environment;
Workers read it with `env.secret`, never from serialized application data.

### 2. Apply migrations to local D1 simulation

```bash
wrangler d1 migrations apply DB --local --config wrangler/web.toml
```

### 3. Run web Worker locally

```bash
scripts/build-island.sh   # once; wrangler refuses to start if dist/public is missing
wrangler dev --local --config wrangler/web.toml
```

Available at `http://localhost:8787`. (Without the island bundle you can
`mkdir -p dist/public` to satisfy wrangler; the new invitation link form then
works as the plain server-rendered form.)

### 4. Run restate-svc Worker locally

In a separate terminal:

```bash
wrangler dev --local --port 8788 --config wrangler/restate-svc.toml
```

Available at `http://localhost:8788`. Register with a local Restate server:

```bash
restate deployments register --environment local --use-http1.1 http://host.docker.internal:8788
```

Requires a local Restate server running at `http://localhost:8080` (see `compose.yaml`).
The URL must be reachable from Restate: with Compose, bind Wrangler to an
interface reachable from the container (for example `--ip 0.0.0.0` on a trusted
development host) and use `host.docker.internal` as above. For a host-native Restate
server, use `http://localhost:8788`. This local mutable endpoint is for disposable
development state, not replay compatibility across production releases.

## Secrets Rotation

### Restate ingress API key

1. Create a replacement environment ingress API key in Restate Cloud (or the
   self-hosted ingress gateway). Keep the current key active during normal rotation.
2. Update the web secret using `wrangler secret put GHINVITE_RESTATE_API_KEY
   --config wrangler/web.toml`; for native, update the secret-manager-injected
   environment and restart every serving process. Do not pass keys in CLI arguments.
3. Verify all serving versions/bindings use the replacement, including alternate
   Worker routes. Run the call, authoritative read/mutation and send checks in the
   [remote gate](restate-ingress-gate.md) with the replacement.
4. Revoke the old key at Restate/the gateway, confirm it now receives 401/403,
   and repeat a web operation successfully. Retain only redacted evidence.
5. A rollback must use the **current** key; do not restore a revoked secret binding.

If compromised, revoke first and accept temporary ingress unavailability while
replacing the key. A denied/failed ingress operation must not be reported as
successful; recover uncertain mutations using the original operation identity.
Rotate `RESTATE_IDENTITY_KEY` separately using Restate's endpoint-signing procedure.

### Session secret

Session records are encrypted with XChaCha20-Poly1305 under the single configured
application key. The ID cookie is a bearer reference, not an encrypted token
container. Cloudflare-managed at-rest encryption is additional provider protection.

Replacing `GHINVITE_SESSION_SECRET` rejects old-key sessions **on deployments
using the new key**. An old-key deployment can still accept old sessions, so
updating a secret alone is not proof that global invalidation has completed.

Emergency cutover:

1. Put web traffic into maintenance at the ingress, including alternate Worker
   URLs, so no new session-dependent requests reach old-key deployments.
2. Stop routing to old versions and allow their in-flight requests to drain.
   Include requests awaiting GitHub or Restate; previously authorized operations
   are not retroactively cancelled by session invalidation.
3. Generate and install a fresh server-only key, then deploy the protected web
   code with that key. Do not use a gradual old/new-key traffic split.

```bash
openssl rand -hex 32 | wrangler secret put GHINVITE_SESSION_SECRET --config wrangler/web.toml
```

4. Verify all serving versions use the new key, an old browser cookie cannot
   reach an authenticated page, and a fresh GitHub login succeeds. Resume traffic.
   Only now declare global session invalidation complete.
5. Retain the new key during any code rollback. Never restore a retired key or
   roll back to code that accepts unprotected session records.

Existing plaintext sessions are rejected on the first protected deployment;
users must sign in again and pending OAuth flows must restart. Wrong-key,
unknown-format, malformed, and tampered records grant no authority and are left
for expiration cleanup. They are not deleted or revoked merely because a reader
cannot decrypt them: they may belong to an overlapping new-key deployment.

### Session lifetime and individual sign-out

Authenticated sessions have a fixed 30-day maximum from successful sign-in;
earlier inactivity expiry still applies. Anonymous sessions (including pending
OAuth) have a fixed 30-minute lifetime. Ordinary requests do not extend either
maximum. A fresh successful OAuth login starts a new authenticated lifetime.

Sign-out clears the current browser cookie and persists a separate deny-only KV
marker before reporting success. The marker remains for 31 days (30 days plus a
one-day clock margin); normally synchronized server/provider clocks within that
margin are assumed. Ordinary session writes cannot erase markers. A late write
may recreate ciphertext but, once the marker is visible, cannot restore authority.
After marker expiration, the authenticated record deadline independently rejects
old snapshots. Do not delete revocation markers as part of session cleanup.

KV is eventually consistent, including cached negative lookups. Cloudflare
documents propagation taking **60 seconds or more**, not a guaranteed upper
bound. A copied cookie can remain usable during propagation. Sign-out does not
cancel already-authorized operations, a concurrent completing GitHub login,
other browsers' sessions, GitHub authorization, or repository access.

If revocation persistence fails, the response clears the browser cookie but
returns 503 with an explicit message that server-side sign-out could not be
confirmed. Do not infer revocation from cookie removal. If a copied cookie must
be disabled dependably in an incident, use the global key-cutover procedure.
Once revocation persists, ciphertext-cleanup failure is logged and sign-out is
successful under these eventual semantics. KV rate limits or storage outages
can also fail login/session persistence; these must not report successful login.

Application protection does not revoke a GitHub token already copied elsewhere,
detect replay of valid same-session ciphertext before expiry/revocation, or defend
against privileged deletion/rollback of revocation state. Keys stay server-only
and must never be included in browser assets or logs.

See [ADR 0002](adr/0002-session-protection-and-invalidation.md) and
[Cloudflare KV consistency](https://developers.cloudflare.com/kv/concepts/how-kv-works/).

### GitHub private key

Provision the replacement before uploading/registering the next workflow version:

```bash
# Linux:
base64 -w0 new-private-key.pem | wrangler secret put GHINVITE_GITHUB_APP_PRIVATE_KEY --config wrangler/restate-svc.toml
# macOS:
base64 new-private-key.pem | tr -d '\n' | wrangler secret put GHINVITE_GITHUB_APP_PRIVATE_KEY --config wrangler/restate-svc.toml
```

Existing pinned version bindings must also retain working credentials until their
approved drain completes. A secret update on the current deployment does not prove
older version URLs use that replacement. Inventory and rehearse credential rotation
for retained versions under #61; if a key must be revoked immediately, isolate and
account for affected work using the cutover recovery procedure.

## Rollback

Workflow recovery is governed by Restate deployment pinning, not by the Worker
production hostname's traffic selection. Do not run a workflow `wrangler rollback`
and assume it moves pinned invocations. Preserve their exact endpoints; select a
compatible deployment for new work only after reviewing shared state/schema and
the cutover phase. After activation or any new authoritative write, follow
[forward repair/reconciliation](admission-cutover.md#recovery-restoration-and-audit-retention).
Never hot-swap code behind a registered URL or purge journals to bypass a failure.

After session-key rotation, redeploy the earlier compatible code with the **current
key** rather than blindly restoring an old Worker version and its secret bindings.
The commands below are only appropriate when their target version preserves the
current session key and record-protection contract.

```bash
wrangler versions list --config wrangler/web.toml
wrangler rollback --config wrangler/web.toml
```

Web rollback must also preserve the current ingress key, admission mode, command
contracts and session protection. Worker code rollback does not revert D1 schema
or Restate authority. A D1 snapshot alone is not a coordinated recovery point;
independent restore can lose receipts and repeat effects. Retain paired recovery
evidence and historical audit archives as specified in the cutover runbook.

## Command validation and operator-owned execution (#61)

Command syntax was checked on 2026-09-18–19 with Wrangler **4.71.0**, Restate CLI
**1.7.9**, and Node **20.20.2**, using CLI help for D1 `migrations list/apply`,
`execute`, Worker `deploy`, `versions upload/list`, `rollback`, secrets, and
Restate `deployments register/list`. The repository pins Restate runtime 1.7.9,
SDK 0.12 with its Cargo patch, worker-build 0.8.1 and wasm-bindgen 0.2.120; use
[the Worker gate's version inventory](worker-admission-gate.md#reproduce).

References: [Cloudflare version-specific preview URLs](https://developers.cloudflare.com/workers/configuration/previews/),
[Restate Worker registration](https://docs.restate.dev/services/deploy/cloudflare-workers),
and [Restate versioning](https://docs.restate.dev/services/versioning).
CLI help validates syntax, not remote execution or provider retention guarantees.
On 2026-09-19, the `--local` equivalents were executed against disposable D1:
all nine migrations applied, `migrations list` reported none pending, ledger and
schema readback succeeded, and `foreign_key_check` returned no violations. This
validates the SQL/readback syntax, not the intended remote bindings.

The operator must record reproducible evidence under #61 for:

- Intended remote account/DB UUID, complete migrations/schema and deployed bindings.
- Immutable endpoint/Restate deployment identities, signatures, authenticated
  ingress, discovery/routing, runtime compatibility and retained-version reachability.
- Regional behavior and CPU/memory/subrequest/input/repository-scope limits.
- Live D1 coordinated export/fence/adoption and restore rehearsal; old-writer and
  issued-effect isolation; SDK 0.10 pinned-work inventory and compatible handoff.
- Administrative kill/purge, coordinated and independent restore, forward recovery,
  and preservation of operation/dispatch/create receipts and historical audits.
- Installation availability and real GitHub effects/webhook evidence, remaining
  fault-model limitations, and an explicit rollout verdict.

These remote actions have **not** been executed by this documentation change.

## GitHub App Setup

1. Create a GitHub App at https://github.com/settings/apps/new
2. Set **Homepage URL** to your `GHINVITE_BASE_URL`
3. Set **Callback URL** to `{GHINVITE_BASE_URL}/oauth/callback`
4. Set **Setup URL** to `{GHINVITE_BASE_URL}/setup/github`
5. Enable **Redirect on update**
6. Set **Webhook URL** to `{GHINVITE_BASE_URL}/webhooks/github`
7. Set repository permissions: **Administration** → **Read & write**, **Metadata** → **Read-only**
8. Enable organization permission **Members → Read-only** and subscribe to **Member** for repository invitation acceptance (`member.added`). `installation` and `installation_repositories` are sent to GitHub Apps by default. See [webhook evidence and subscription requirements](invitation-settlement.md#member-added-webhooks) before enabling acceptance in a deployment.
9. Generate and download a private key (used for `GHINVITE_GITHUB_APP_PRIVATE_KEY`)
10. Note the **App ID** (used for `GHINVITE_GITHUB_APP_ID`)
11. Note the **Client ID** and generate a **Client Secret** (used for OAuth vars)
12. Generate a **Webhook Secret** (used for `GHINVITE_WEBHOOK_SECRET`)

### Troubleshooting GitHub App setup

- **No redirect after install**: confirm the GitHub App **Setup URL** is `{GHINVITE_BASE_URL}/setup/github` and **Redirect on update** is enabled.
- **`missing installation_id`**: GitHub did not return through the Setup URL. Recheck the Setup URL and install the app from the app installation URL again.
- **`installation is not visible to signed-in user`**: sign out of ghinvite, sign in with the GitHub user that installed or can administer the app installation, then retry the GitHub App install/update.
- **`Restate error` or setup returns 502**: check registration/routing and the target environment's `GHINVITE_RESTATE_API_KEY`. HTTP 401/403 indicates rejected ingress access, not a browser login problem. Confirm `GHINVITE_RESTATE_AUTH=bearer`; `RESTATE_IDENTITY_KEY` cannot authorize web-to-ingress traffic. Errors deliberately omit upstream bodies and transport details; use redacted runtime invocation metadata for diagnosis.
- **Repository picker is empty after a selected-repository install**: reopen the GitHub App installation settings, verify repository access, and use **Update** so GitHub redirects back to ghinvite with `setup_action=update`.
- **Permission failures when inviting collaborators**: confirm repository permissions are **Administration: Read & write** and **Metadata: Read-only**.
