> P1 implementation update (2026-10-17): installed-app setup and webhook routes
> are app-scoped: `/setup/repo`, `/setup/org`, `/webhooks/repo`, and
> `/webhooks/org`. Setup verifies the installation with the selected app's JWT;
> webhooks verify that app's HMAC. Unscoped legacy `/setup` and `/webhooks` routes
> are absent. Sign-in is token-free (typed, PKCE) and is the only sign-in path
> since GM-09: legacy OAuth and `GH_SIGNIN_ENABLED` are gone. Local
> implementation does not satisfy the live SP-19/SP-19a gates.
> Old owner keys and retained invocations require compatible pinned endpoints or
> draining. This guide does not authorize a deployment.

# ghinvite Deployment Guide

## Rollout status and order

The simplified owners require the coordinated
[clean prelaunch replacement](invitation-ownership.md#clean-prelaunch-replacement):
fresh Restate and SQL/D1 state, matching web/workflow artifacts, new sessions, and
retirement of old registrations and writers. Applying migrations to discarded
prelaunch state is not a supported import path.

**Production rollout remains blocked under [#61](https://github.com/sagikazarmark/ghinvite2.orig/issues/61).**
This procedure describes the target deployment and disposable remote rehearsal;
documentation, passing local tests, and provisioning alone do not authorize rollout.
#59 completed local Worker/D1 verification. The
installation-availability integration (#60) and authenticated web ingress (#77)
are implemented; their remote deployment evidence is still required.

Run commands from the repository root against an explicitly selected Cloudflare
account and Restate environment, in this order:

1. Keep web ingress (including alternate Worker URLs), webhooks and scheduled
   producers closed until readiness is verified.
2. Apply and verify remote migrations, then register a new immutable workflow
   endpoint. Include [installation availability](installation-availability.md):
   `Installation`, `AccountInstallation` and `InstallationProjection` must ship
   together, and so must the admission, lifecycle, delivery, settlement and
   projection services bound by `build_endpoint`.
3. Deploy web with its matching bindings, then verify authoritative web operations
   on restricted rehearsal ingress before opening producers. Complete the remote
   gates before live traffic.

Recovery is compatible forward repair or reviewed reverse reconciliation, never a
switch to stale SQL. Preserve admission outcomes, uses, deadlines,
dispatch/create/settlement receipts and historical audits.
See [Worker recovery limits](worker-admission-gate.md#remaining-rollout-gates--fault-model-limits)
and [rollback](#rollback) before any upgrade.

## Prerequisites

- Cloudflare account with Workers paid plan (or free tier for testing)
- Node.js 20.20.2 or a compatible supported Node release.
- [Wrangler CLI](https://developers.cloudflare.com/workers/wrangler/install-and-update/) **4.71.0**: `npm install -g wrangler@4.71.0` (command-validation baseline for this guide).
- Restate CLI **1.8.x**: `npm install -g @restatedev/restate@1.8`; configure the target environment's Admin URL and admin credential using the CLI/secret manager. Admin credentials are separate from the web ingress key.
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
| Restate Cloud environment ingress API key | `RESTATE_INGRESS_TOKEN` on **web** | Web → Restate; authorizes invoking services |
| Restate endpoint identity key | `RESTATE_IDENTITY_KEYS` on **restate-svc** | Restate → workflow Worker; verifies signed runtime requests |

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

For an existing database, close writers and retain a coordinated recovery point
before applying changes. Rehearse on disposable D1 first. All
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
`migrations/` for the exact release; this checkout currently contains `0001` through
`0027_requester_profile.sql`. Compare the returned schema definitions with those
migrations, including projection columns, delivery fences, settlement and
organization-webhook receipts, and the `attempt_continuations_expiry` index. The
obsolete repository-member `webhook_receipts` table must be absent after `0022`.
Before `0022`/`0023`, inventory and drain or route away incompatible serving and
pinned writers; endpoint pinning does not protect a writer from a shared-schema
drop. Follow the [migration-specific compatibility requirements](../migrations/README.md#retired-repository-member-event-matching),
including the canonical-delivery identity preflight, without changing previously
applied migration files. Fresh prelaunch replacement is not an arbitrary deployed
state import. Expect
no foreign-key violations. Stop on missing/mismatched schema, migration errors or
an unexpected UUID in Wrangler output. Apply migrations only via the web config,
which declares `migrations_dir`; the workflow config is used here for readback.

> **Important:** Do not run `sqlx migrate run` against D1. sqlx manages the local SQLite dev database; Wrangler manages D1. The same SQL files in `migrations/` are shared, but tracking is separate.

### 4. Create KV namespaces for sessions and authority evidence

```bash
wrangler kv namespace create "SESSIONS" --config wrangler/web.toml
wrangler kv namespace create "AUTHZ_CACHE" --config wrangler/web.toml
```

Copy each `id` into its matching `[[kv_namespaces]]` entry in
`wrangler/web.toml`. `AUTHZ_CACHE` contains read-only authority observations,
partitioned by app, installation, user and repository. It never contains GitHub
credentials or Link Secrets. Entries have a 300-second cleanup TTL, and each read
independently rejects evidence aged 300 seconds or with a future timestamp.

The web Worker also requires `AUTHZ_RL` with 10 reads per 60 seconds, keyed by
app and installation, alongside the public `RL_IP` and `RL_USER` bindings.
Missing cache or rate-limit bindings fail startup closed. Native hosting uses a
shared bounded in-memory cache and process-local rate limits; it does not provide
a cross-process global limit. Worker limits are approximate and per location.

### 5. Update wrangler config vars

Complete [GitHub App Setup](#github-app-setup) first, then return here with the install URL, App ID, OAuth credentials, and webhook secret.

In `wrangler/web.toml`, update `[vars]`:
- `P1_HOST` — your bare canonical DNS host (e.g. `ghinvite.example.com`); no scheme, port or path
- `RESTATE_INGRESS_URL` — your HTTPS Restate Cloud ingress URL; no credentials, query or fragment
- `GHINVITE_GITHUB_INSTALL_URL` — repository GitHub App installation URL
- `GH_REPO_APP_ID` and `GH_REPO_CLIENT_ID` — numeric App ID and client ID of that repository app

### 6. Set secrets

```bash
# Local keyring descriptor (install the companion anykms `akr` CLI):
akr new p1 | wrangler secret put P1_KEYRING --config wrangler/web.toml

# Sign-in App (from its GitHub App settings; the client ID may be a var):
wrangler secret put GH_SIGNIN_CLIENT_ID --config wrangler/web.toml
wrangler secret put GH_SIGNIN_CLIENT_SECRET --config wrangler/web.toml

# Repository app private-key list and rotated webhook-secret list:
wrangler secret put GH_REPO_PRIVATE_KEYS --config wrangler/web.toml
wrangler secret put GH_REPO_WEBHOOK_SECRETS --config wrangler/web.toml

# Web -> Restate: provision an ingress API key for the target environment in
# Restate Cloud, then paste it at Wrangler's secret prompt (no Bearer prefix):
wrangler secret put RESTATE_INGRESS_TOKEN --config wrangler/web.toml

# Restate service — paste the repo App's rotation list:
# kid:base64(PKCS#8 PEM);previous-kid:base64(PKCS#8 PEM)
# Convert downloaded PKCS#1 with openssl pkcs8 -topk8 -nocrypt before encoding.
wrangler secret put GH_REPO_PRIVATE_KEYS --config wrangler/restate-svc.toml

# Set P1_HOST, GH_REPO_APP_ID and GH_REPO_CLIENT_ID in restate-svc.toml [vars].
# Use the actual repo App numeric ID and client ID; neither is inferred/defaulted.

# Set RESTATE_IDENTITY_KEYS in restate-svc.toml [vars] to the comma-separated
# verification public keys from Cloud → Developers → Security → HTTP endpoints.
```

Configure both secret sets before creating the final release versions. The
workflow Worker refuses startup when RESTATE_IDENTITY_KEYS is absent, empty or
invalid. Discovery and invocation requests must be signed by one of the listed
keys. Verify the exact version URL below. These are verification **public** keys;
Restate retains the signing private keys. For rotation, deploy old and new keys
together before changing the signer, then remove the retired key after the
pinned versions and runtime have finished their transition.

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

### 8. Create the workflow version, then deploy web

Keep `preview_urls = true` in `wrangler/restate-svc.toml`. Provision secrets
first. On first creation only, bootstrap the workflow
Worker with `wrangler deploy --config wrangler/restate-svc.toml`; do not register
its mutable hostname. For the release (and every subsequent upgrade):

```bash
export WASM_BINDGEN_BIN="$(command -v wasm-bindgen)"
wrangler versions upload --config wrangler/restate-svc.toml
```

Record the full Worker version ID and its **version-prefixed Preview URL** from
Wrangler output/Cloudflare Deployments, plus commit, artifact digest, SDK version,
compatibility date and binding identities. Use that exact URL for
registration. An alias such as `staging-...` can move and is not version-specific.
`versions upload` makes the preview endpoint available without moving production
hostname traffic; it does not select Restate's deployment.

Deploy web, keeping its public ingress closed until readiness is verified:

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
this release. Registration changes routing for new invocations;
keep producers closed until readiness is complete. Do not use
`--force` to overwrite a deployment or bypass a compatibility refusal.

Register a **new version URL on every workflow code/config release**, even if
handler names and schemas are unchanged. Retained journals depend on code and SDK
entry ordering, not just wire compatibility. Never register the bare Worker
hostname, a mutable preview alias, or a redirect to "latest". In particular, do not
replay in-flight SDK 0.10 journals against SDK 0.12 code.

Retain each old version, exact endpoint, required compatible bindings/credentials
and artifact while any invocation is pinned to it, including suspended workflows,
retries and delayed timers. Inventory `pinned_deployment_id` in Restate's
`sys_invocation` table. Keep the original code reachable during an approved compatible drain, and verify
provider version/URL retention covers the drain and recovery window. Shared D1
schema changes must remain compatible with every still-running pinned version.
Deleting a version, disabling preview URLs, changing routes or revoking a needed
credential can strand pinned work; a new registration does not migrate it.

For an incompatible handler change, drain first or capture obligations/effects,
then permanently isolate the old URLs, controllers, DB access and outbound
credentials before handoff. Preserve artifacts and journals for controlled
recovery even after isolation. A runtime pause/kill or SQL fence cannot stop an
already-issued GitHub request. Retire a deployment only after every pinned
invocation and uncertain effect is accounted for; never repoint its URL at new code.

Provision the ingress key in the same Restate Cloud environment as
`RESTATE_INGRESS_URL`, with permission to invoke the web application's services.
For self-hosted production, provide an HTTPS ingress gateway that enforces the same
Bearer contract and prevents direct access to the unprotected runtime ports.
Use the exact ingress URL; do not rely on redirecting gateways for authentication.
Authenticated remote URLs must use HTTPS; HTTP is accepted only for loopback
development (`localhost`, loopback IPv4/IPv6).

### 10. Liveness and authenticated end-to-end readiness

`/health` returns constant `ok`: it does not query D1 or Restate. On restricted
rehearsal ingress, these checks establish HTTP/asset liveness and webhook signature rejection:

```bash
export WEB_BASE_URL=https://YOUR_WEB_HOST
curl --fail-with-body --max-time 20 "$WEB_BASE_URL/health" # → ok
curl --fail-with-body --max-time 20 "$WEB_BASE_URL/"       # → HTML
curl -sS --max-time 20 -o /dev/null -w "%{http_code}" \
  -X POST "$WEB_BASE_URL/webhooks/repo" \
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

Use `.dev.vars` for local Worker secrets. Generate a descriptor with `akr new p1`
and set `P1_KEYRING` to its complete `akr1;name=...` output; native development
reads the same descriptor from the environment. Keep this file out of version
control. Missing or malformed descriptors fail startup, with no default key or
legacy hex-key fallback. The keyring is immutable for the Worker isolate's life.
Set `P1_OPERATOR_IDS` to comma-separated positive numeric GitHub user IDs to
allow operator fingerprint inspection; omitted/empty denies everyone.

Production web configuration always requires `P1_HOST`, `RESTATE_INGRESS_URL`
and the `RESTATE_INGRESS_TOKEN` secret. This applies to the Worker under local
Wrangler as well: use an HTTPS ingress with bearer authentication. Missing
configuration, HTTP URLs (including loopback), credentials in URLs, query strings,
fragments and malformed secrets fail closed before serving. Legacy host/ingress
aliases and `GHINVITE_RESTATE_AUTH=local-unauthenticated` cannot disable this.

For a credential-free **native development runtime**, opt in explicitly:

```bash
GHINVITE_RESTATE_INGRESS=http://127.0.0.1:8080 \
GHINVITE_RESTATE_AUTH=local-unauthenticated \
cargo run -p ghinvite-web-server -- --local-dev
```

This mode still requires `P1_KEYRING` and the explicit repository-app registry.
Leave `GHINVITE_RESTATE_API_KEY` absent in this development mode; a supplied key
conflicts with unsigned mode. Native startup without `--local-dev` uses the same
canonical production parser as the Worker. Native hosting still uses its documented
SQLite and process-local cache/limit adapters; this does not establish remote
Worker deployment parity.

Production web requires the sign-in App's `GH_SIGNIN_CLIENT_ID` and
`GH_SIGNIN_CLIENT_SECRET`. `--local-dev` reads the same two names and calls back to
`{GHINVITE_BASE_URL}/oauth/callback`; without them `/login` answers that sign-in
is unavailable. Canonical host/ingress validation does not claim the final
production binding inventory is complete.

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
2. Update the web secret using `wrangler secret put RESTATE_INGRESS_TOKEN
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
Rotate `RESTATE_IDENTITY_KEYS` separately using Restate's endpoint-signing procedure.

### Session keyring

Sessions and attempt continuations use the anykms v1 envelope under `P1_KEYRING`.
The session cookie remains a bearer ID. Session AAD is `purpose=session` plus the
requested KV session ID; continuation AAD is `purpose=attempt-continuation` plus
the stored plaintext scope and binding columns. No legacy XChaCha decoder exists.
Existing PoC sessions and continuations require fresh sign-in/recovery.

Routine rotation must use the companion `akr` CLI and private descriptor files:
add a generation, deploy it as readable with the old current version, verify every
serving deployment, then promote it in a second rollout. Keep old generations
readable until every consumer completion gate is satisfied after the **last
possible old-generation write**, including pinned older deployments. For session
KV, wait at least 30 days plus one hour after the verified Promote boundary;
every session write sets a TTL no longer than 30 days. For D1 continuations,
perform the [verified CAS sweep and separate full scan](continuation-keyring-rotation.md)
with zero old and zero bad rows, including retained expired rows. Session lifetime
or opportunistic cleanup does not substitute for that full scan. Run `akr diff
OLD NEW` before deployment. Never reuse a version or change its root bytes.
Never lower the current version or roll back across Promote.

`GET /console/keyring` returns only the keyring fingerprint for signed-in numeric
IDs in `P1_OPERATOR_IDS`; all others receive 404. Compare it with the fingerprint
logged once per isolate and the expected `akr show` result, including every
serving version, before promoting or retiring generations. A newer unknown
version returns HTTP 503 and preserves the cookie and record. A removed generation
or corrupted ciphertext confers no session authority. A KCV/configuration mismatch
returns HTTP 500 and logs an operator alert, never an anonymous-session fallback.

For suspected key compromise, close ingress, drain old deployments/in-flight
requests, replace all exposed generations with a fresh higher generation, and
invalidate sessions and continuations. Confirm old cookies fail and fresh sign-in
works before reopening. Retain the new descriptor during code rollback. Merely
rewrapping is not recovery from a leaked KEK; any retained encrypted data needs
full decryption and encryption with fresh DEKs, and underlying credential rotation
where applicable. Already-authorized operations are not retroactively cancelled.

Unreadable records are never deleted or revoked just because decryption failed;
they remain for expiration cleanup. Only explicit sign-out writes revocations.
The envelope review and complete P1 sign-in prerequisite remain production gates.

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

Provision the replacement before uploading/registering the next workflow version.
Prepend a new `kid:base64(PKCS#8 PEM)` entry to the existing rotation list (the
first entry signs), preserving the old valid entry during the overlap:

```bash
wrangler secret put GH_REPO_PRIVATE_KEYS --config wrangler/restate-svc.toml
```

Existing pinned version bindings must also retain working credentials until their
approved drain completes. Older artifacts still require their legacy binding
names; provisioning `GH_REPO_PRIVATE_KEYS` does not rewrite those artifacts. A secret update on the current deployment does not prove
older version URLs use that replacement. Inventory and rehearse credential rotation
for retained versions under #61; if a key must be revoked immediately, isolate and
account for affected work using the
[recovery procedure](admission-v1.md#recovery-and-audit-retention).

## Native fallback

The [native fallback package and switch-readiness runbook](native-fallback.md)
describes CI artifacts and the current shared-storage blocker. Native binaries
are not a hot replacement for the D1-backed workflow Worker. The web Worker
stays on Workers under ADR0010; no production host or migration is selected.

## Rollback

Workflow recovery is governed by Restate deployment pinning, not by the Worker
production hostname's traffic selection. Do not run a workflow `wrangler rollback`
and assume it moves pinned invocations. Preserve their exact endpoints; select a
compatible deployment for new work only after reviewing shared state/schema.
Follow [forward repair/reconciliation](admission-v1.md#recovery-and-audit-retention).
Never hot-swap code behind a registered URL or purge journals to bypass a failure.

After session-key rotation, redeploy the earlier compatible code with the **current
key** rather than blindly restoring an old Worker version and its secret bindings.
The commands below are only appropriate when their target version preserves the
current session key and record-protection contract.

```bash
wrangler versions list --config wrangler/web.toml
wrangler rollback --config wrangler/web.toml
```

Web rollback must also preserve the current ingress key, command contracts and
session protection. Worker code rollback does not revert D1 schema
or Restate authority. A D1 snapshot alone is not a coordinated recovery point;
independent restore can lose receipts and repeat effects. Retain paired recovery
evidence and historical audit archives as specified in
[admission recovery](admission-v1.md#recovery-and-audit-retention).

## Command validation and operator-owned execution (#61)

Command syntax was checked on 2026-09-18–19 with Wrangler **4.71.0**, Restate CLI
**1.7.9**, and Node **20.20.2**, using CLI help for D1 `migrations list/apply`,
`execute`, Worker `deploy`, `versions upload/list`, `rollback`, secrets, and
Restate `deployments register/list`. The repository pins Restate runtime 1.8.1,
SDK 0.13 with its Cargo patch (fork `wasm` branch), worker-build 0.8.1 and wasm-bindgen 0.2.120; use
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
- Coordinated D1/Restate restore rehearsal and issued-effect isolation.
- Administrative kill/purge, coordinated and independent restore, forward recovery,
  and preservation of operation/dispatch/create receipts and historical audits.
- Installation availability and real GitHub effects/webhook evidence, remaining
  fault-model limitations, and an explicit rollout verdict.

These remote actions have **not** been executed by this documentation change.

## GitHub App Setup

Use the three checked-in [registration manifests](../deploy/github-apps/README.md)
as the permission and callback source of truth. Replace the example host and
review the exact resulting files before any operator-owned registration.

- **Sign-in:** `emails: read`, no repository/organization permissions, no webhook,
  and `/oauth/callback`. Set token expiry manually and record SP-19a evidence.
- **Repo:** `administration: write` and `metadata: read`, `/setup/repo` and
  `/webhooks/repo`, no organization permissions or member-event subscription.
  Repository acceptance is observed by reconciliation; authenticated legacy
  member/repository-invitation events do not settle deliveries.
- **Org:** `members: write` and `metadata: read`, `/setup/org` and `/webhooks/org`,
  organization/membership events as declared by its manifest.

All manifests disable automatic OAuth on installation. Store each installed app's
numeric App ID, actual Client ID, signing keys and webhook secrets under its
matching `GH_REPO_*` or `GH_ORG_*` configuration. The sign-in client secret belongs
only to the web host. Do not add repo-app Members permission to restore removed
member-webhook behavior. Registration and provider behavior remain live gates.

### Troubleshooting GitHub App setup

- **No redirect after install**: confirm the GitHub App **Setup URL** is `https://{P1_HOST}/setup/repo` and **Redirect on update** is enabled.
- **`missing installation_id`**: GitHub did not return through the Setup URL. Recheck the Setup URL and install the app from the app installation URL again.
- **`installation is not visible to signed-in user`**: sign out of ghinvite, sign in with the GitHub user that installed or can administer the app installation, then retry the GitHub App install/update.
- **`Restate error` or setup returns 502**: check registration/routing and the target environment's `RESTATE_INGRESS_TOKEN`. HTTP 401/403 indicates rejected ingress access, not a browser login problem. Confirm the required `RESTATE_INGRESS_TOKEN` is current; `RESTATE_IDENTITY_KEYS` cannot authorize web-to-ingress traffic. Errors deliberately omit upstream bodies and transport details; use redacted runtime invocation metadata for diagnosis.
- **Repository picker is empty after a selected-repository install**: reopen the GitHub App installation settings, verify repository access, and use **Update** so GitHub redirects back to ghinvite with `setup_action=update`.
- **Permission failures when inviting collaborators**: confirm repository permissions are **Administration: Read & write** and **Metadata: Read-only**.

## P1 app split configuration

The three [app manifests](../deploy/github-apps/README.md) and the pure
`ghinvite_config::apps` parser implement a reviewable configuration seam
(`ghinvite_web::config::apps` remains a compatibility re-export).
Both workflow composition roots parse the explicitly selected registry stage and construct
the typed repository broker plus the transitional installation read client from
that same registry. They require `P1_HOST`, `GH_REPO_APP_ID`,
`GH_REPO_CLIENT_ID` and `GH_REPO_PRIVATE_KEYS`, with no legacy-variable or dummy-key
fallback. Web startup uses the same stage for installed-app setup, webhook and
authority registries. Web always requires complete `GH_SIGNIN_CLIENT_ID/SECRET`
and runs PKCE login/callback with durable encrypted pending state and token-free
sessions; there is no switch. Live SP-19/SP-19a remain separate unpassed gates.

**Legacy OAuth removal (GM-09).** Before deploying a build without legacy OAuth:
set `GH_SIGNIN_CLIENT_ID`/`GH_SIGNIN_CLIENT_SECRET` (callback URL
`https://{P1_HOST}/oauth/callback` on the sign-in App), and delete
`GHINVITE_GITHUB_CLIENT_ID`, `GHINVITE_GITHUB_CLIENT_SECRET` and
`GH_SIGNIN_ENABLED`, which nothing reads any more. Sessions signed in through
legacy OAuth stay signed in until their fixed deadline: they carry the same
identity, the stored user token is never read, and the next write of each
record drops it. Sign-ins in flight across the deploy fail once ("no sign-in in
progress") and restart. Tokens legacy OAuth issued were never revoked by
ghinvite; revoke them in the legacy OAuth App's settings if wanted. See
[ADR0002](adr/0002-session-protection-and-invalidation.md).
An unknown pending-state key generation returns503 without consuming the exact
pending row or exchanging its code, allowing the same callback to retry after
keyring rollout. KCV/configuration failures return500 with the fixed operator alert.


Deployment decision, 2026-10-16: the installed-app registry needs both an actual
numeric App ID and an actual client ID. P1 §20.1 now explicitly includes
`GH_REPO_CLIENT_ID` and `GH_ORG_CLIENT_ID` on both Workers. Never derive a client
ID from an App ID or supply a placeholder default. The sign-in registry entry
has `app_id: None`: it needs only `GH_SIGNIN_CLIENT_ID` and
`GH_SIGNIN_CLIENT_SECRET`, and neither a signing key nor a webhook secret.

Both native and Worker composition roots read `P1_APP_STAGE`: absent or exact
`repo` selects the repository registry; exact `repo-and-org` additionally requires
`GH_ORG_APP_ID`, `GH_ORG_CLIENT_ID` and `GH_ORG_PRIVATE_KEYS`, plus
`GH_ORG_WEBHOOK_SECRETS` on web. Empty, unknown or noncanonical stage values fail
with the fixed `P1_APP_STAGE` field; incomplete org configuration fails closed.
Default Wrangler templates explicitly retain `repo`. The stage enables the org
registry for read-only installation/owner authority. Organization targets and
deliveries additionally require `GH_ORG_DELIVERY_ENABLED=true` on both web and
workflow runtimes; absent or exact `false` keeps them disabled. Other values fail
startup, and `true` requires `P1_APP_STAGE=repo-and-org` and its complete registry.
The workflow setting binds the organization owner, the real invitation budget,
and cancellation capability; web uses the same setting for fresh target selection
and submission. Existing record authorization still checks current authority.
This explicit composition setting does not satisfy the remaining M1/live gates
or authorize a deployment. See ID-40, ID-50 and
[org observation verification](org-observation-verification.md). `parse_web_apps` requires the
sign-in credentials plus installed-app signing keys and rotated webhook lists.
`parse_mint_apps` requests only `P1_HOST` and the installed apps' IDs, client IDs
and signing keys. The small `ghinvite-config` crate is shared by native and
Worker composition roots and has no web, storage or Restate dependency.

Bulk issuance is a separate opt-in: `GH_BULK_ENABLED=true` enables the Console
capability and standard BulkInviteBatch owner; absent or exact `false` disables
it, and every other value fails startup. Configure both web and workflow
runtimes consistently. Repository-only bulk needs no mailbox key. Bulk with
`GH_ORG_DELIVERY_ENABLED=true` also requires `P1_EMAIL_KEYRING`, an `akr1`
descriptor whose ring name is exactly `p1-email`. Use the same current and
retained generations in web and workflow deployments, with the normal staged
key rollout; do not reuse the `p1` web-session descriptor. Its key check value and
shape are verified before startup, and diagnostics name only the field. The
web stages encrypted, fully bound mailbox records; only the email delivery's
fenced write run can load an activated record. Both owners use the real shared
invitation budget. These local capability flags do not establish SP-07/SP-18
live behavior or authorize deployment. See ID-61 and
[email delivery verification](email-delivery-verification.md).

`P1_HOST` is a canonical lower-case ASCII DNS host (IDNs in A-label form), with
no scheme, port, credentials, path or trailing dot. URLs use HTTPS. Numeric IDs
are positive canonical decimals; signing lists are
`kid:base64(PKCS#8 PEM);kid:base64(PKCS#8 PEM)` (first signs), and webhook lists
are nonempty distinct semicolon-separated secrets. Errors contain fixed binding
names only. Input copies returned by the source are zeroized for secrets; a
native/Worker source adapter must also protect and validate its original input,
including non-Unicode environment failures. Registry parsing does not validate
KV/D1/rate-limit bindings, ingress credentials or the session keyring; their
existing/future startup validators remain required by B0-22.


Workflow startup validates app configuration before opening native storage or
building the Worker endpoint. Each incoming Worker request creates the broker,
JWT source and transports together, so in-flight mint I/O cannot cross request
contexts. `InstallationClient::from_registry` is a transitional bridge for
unmigrated availability/read paths: its signer internally clones the registry's
primary key and uses the actual client-ID issuer. It exposes no raw key accessor.
This does not claim that all legacy client paths have gained typed-broker
permission restrictions or secret-handling guarantees. Existing request-identity
validation requirements and remote rollout gates remain unchanged.

Local verification of this startup transition: `registry_legacy_bridge` verifies
the RS256 signature and actual client-ID issuer; the native `startup` tests
exercise rejection before SQL plus real HTTP/2 discovery.
`tests/worker/workflow-startup.mjs` exercises actual Worker `/discover` for valid,
legacy-only, missing-client-ID and malformed configurations, with no GitHub calls
and no secret-marker disclosure. It runs with the shared D1 suite in
`npm run test:storage`. These local checks do not replace deployed registration,
permission or request-identity verification.


Access removal is a separate opt-in capability: set `GH_ACCESS_REMOVAL_ENABLED=true`
in both web and workflow deployments only after validating the AccessGrant gates.
The default is false. Installed app configuration and the real InviteBudget binding
remain required; organization/team targets also need the organization stage.
A disabled web rejects a submitted finite access lifetime and preserves its raw value.
A disabled workflow refuses a new lifetime-bearing creation, while exact creation
receipts remain recoverable. Pin old invocation versions or drain their scheduled
AccessGrant work before disabling/removing its service; changing the flag does not
cancel retained removal obligations. Live removal safety evidence remains a release
gate, independent of local fixture tests.


### Administrator reissue

`GH_REISSUE_ENABLED` defaults to `false` and accepts only the literal values
`true` and `false`. Set it consistently on web and workflow deployments. The web
setting exposes protected confirmation controls; the workflow setting binds
reissue with the real invitation budget, old-invitation cancellation and
AccessGrant removal barriers. Repository-only reissue does not require an
organization installation. Organization targets require the separately enabled
organization delivery composition.

A reissue creates a new generation from the retained approved target, preserves
the original approval and source, and charges the administrator account budget
without the source link's share. Unknown old deletion or removal work blocks
reissue. Enabling the setting does not satisfy live GitHub activation probes or
retained endpoint migration gates.

### Continuation keyring rotation

Use the [verified CAS sweep and separate full-pass completion gate](continuation-keyring-rotation.md). Retained expired D1 rows are included; the session KV TTL rule does not substitute for this gate.
