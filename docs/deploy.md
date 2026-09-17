# ghinvite Deployment Guide

## Prerequisites

- Cloudflare account with Workers paid plan (or free tier for testing)
- [Wrangler CLI](https://developers.cloudflare.com/workers/wrangler/install-and-update/) installed: `npm install -g wrangler`
- [`worker-build`](https://crates.io/crates/worker-build) installed: `cargo install worker-build`
- [Dioxus CLI](https://dioxuslabs.com/learn/0.7/getting_started/) 0.7.x installed (`cargo binstall dioxus-cli@0.7.9`) and the `wasm32-unknown-unknown` target (`rustup target add wasm32-unknown-unknown`), for the island bundle
- GitHub App created and configured (see GitHub App Setup below)
- Restate Cloud account (or self-hosted Restate server)

## Architecture

Two Cloudflare Workers share one D1 database:

- **ghinvite-web** (`crates/ghinvite-web-worker`) — handles HTTP requests, SSR, OAuth, webhooks
- **ghinvite-restate-svc** (`crates/ghinvite-workflows`) — Restate durable workflow handlers

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
```

### 2. Create D1 database

```bash
wrangler d1 create ghinvite
```

Copy the `database_id` from the output into `wrangler/web.toml` and `wrangler/restate-svc.toml`.

### 3. Apply database migrations

```bash
wrangler d1 migrations apply ghinvite --config wrangler/web.toml
```

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

# Restate identity key (from Restate Cloud console → Deployments → Identity key):
wrangler secret put RESTATE_IDENTITY_KEY --config wrangler/restate-svc.toml
```

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

### 8. Deploy

```bash
scripts/build-island.sh   # always first: Static Assets pick up dist/public
wrangler deploy --config wrangler/web.toml
wrangler deploy --config wrangler/restate-svc.toml
```

### 9. Register with Restate Cloud

```bash
restate deployments register https://ghinvite-restate-svc.YOUR_SUBDOMAIN.workers.dev
```

Re-register after any service interface changes.

Provision the ingress key in the same Restate Cloud environment as
`GHINVITE_RESTATE_INGRESS`, with permission to invoke the web application's services.
For self-hosted production, provide an HTTPS ingress gateway that enforces the same
Bearer contract and prevents direct access to the unprotected runtime ports.
Use the exact ingress URL; do not rely on redirecting gateways for authentication.
Authenticated remote URLs must use HTTPS; HTTP is accepted only for loopback
development (`localhost`, loopback IPv4/IPv6).

### 10. Smoke test

```bash
curl -s https://ghinvite.workers.dev/health           # → ok
curl -s https://ghinvite.workers.dev/ | grep ghinvite # → HTML
curl -s -o /dev/null -w "%{http_code}" \
  -X POST https://ghinvite.workers.dev/webhooks/github \
  -d '{}'                                             # → 401
curl -s -o /dev/null -w "%{http_code}" \
  https://ghinvite.workers.dev/assets/ghinvite-island.js  # → 200 (island loader)
```

Health and HTML alone do not exercise ingress credentials. Complete the
[authenticated production-binding verification](restate-ingress-gate.md) under
the operator-owned remote gate (#61) before authorizing rollout.

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
wrangler d1 migrations apply ghinvite --local --config wrangler/web.toml
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
wrangler dev --local --config wrangler/restate-svc.toml
```

Available at `http://localhost:8788`. Register with a local Restate server:

```bash
restate deployments register http://localhost:8788
```

Requires a local Restate server running at `http://localhost:8080` (see `compose.yaml`).

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

```bash
# Linux:
base64 -w0 new-private-key.pem | wrangler secret put GHINVITE_GITHUB_APP_PRIVATE_KEY --config wrangler/restate-svc.toml
# macOS:
base64 new-private-key.pem | tr -d '\n' | wrangler secret put GHINVITE_GITHUB_APP_PRIVATE_KEY --config wrangler/restate-svc.toml
```

## Rollback

After session-key rotation, redeploy the earlier compatible code with the **current
key** rather than blindly restoring an old Worker version and its secret bindings.
The commands below are only appropriate when their target version preserves the
current session key and record-protection contract.

```bash
wrangler versions list --config wrangler/web.toml
wrangler rollback --config wrangler/web.toml

wrangler versions list --config wrangler/restate-svc.toml
wrangler rollback --config wrangler/restate-svc.toml
```

> **Warning:** Rolling back Worker code does not revert D1 schema migrations. Take a D1 backup snapshot before applying migrations to production.

## GitHub App Setup

1. Create a GitHub App at https://github.com/settings/apps/new
2. Set **Homepage URL** to your `GHINVITE_BASE_URL`
3. Set **Callback URL** to `{GHINVITE_BASE_URL}/oauth/callback`
4. Set **Setup URL** to `{GHINVITE_BASE_URL}/setup/github`
5. Enable **Redirect on update**
6. Set **Webhook URL** to `{GHINVITE_BASE_URL}/webhooks/github`
7. Set repository permissions: **Administration** → **Read & write**, **Metadata** → **Read-only**
8. Subscribe to `Member` and `Repository invitation` if GitHub shows them for the selected permissions. `installation` and `installation_repositories` are app-level GitHub App events that GitHub sends by default.
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
