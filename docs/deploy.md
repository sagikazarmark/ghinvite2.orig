# ghinvite Deployment Guide

## Prerequisites

- Cloudflare account with Workers paid plan (or free tier for testing)
- [Wrangler CLI](https://developers.cloudflare.com/workers/wrangler/install-and-update/) installed: `npm install -g wrangler`
- [`worker-build`](https://crates.io/crates/worker-build) installed: `cargo install worker-build`
- GitHub App created and configured (see GitHub App Setup below)
- Restate Cloud account (or self-hosted Restate server)

## Architecture

Two Cloudflare Workers share one D1 database:

- **ghinvite-web** (`crates/web-worker`) — handles HTTP requests, SSR, OAuth, webhooks
- **ghinvite-restate-svc** (`crates/restate-svc`) — Restate durable workflow handlers

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
wrangler kv:namespace create "SESSIONS" --config wrangler/web.toml
```

Copy the `id` into `wrangler/web.toml` under `[[kv_namespaces]]`.

### 5. Update wrangler config vars

In `wrangler/web.toml`, update `[vars]`:
- `GHINVITE_BASE_URL` — your public domain (e.g. `https://ghinvite.example.com`)
- `GHINVITE_RESTATE_INGRESS` — your Restate Cloud ingress URL
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

# Restate service — GitHub App private key (base64-encoded for safe storage):
base64 -w0 private-key.pem | wrangler secret put GHINVITE_GITHUB_APP_PRIVATE_KEY --config wrangler/restate-svc.toml
wrangler secret put GHINVITE_GITHUB_APP_ID --config wrangler/restate-svc.toml

# Restate identity key (from Restate Cloud console → Deployments → Identity key):
wrangler secret put RESTATE_IDENTITY_KEY --config wrangler/restate-svc.toml
```

### 7. Deploy

```bash
wrangler deploy --config wrangler/web.toml
wrangler deploy --config wrangler/restate-svc.toml
```

### 8. Register with Restate Cloud

```bash
restate deployments register https://ghinvite-restate-svc.YOUR_SUBDOMAIN.workers.dev
```

Re-register after any service interface changes.

### 9. Smoke test

```bash
curl -s https://ghinvite.workers.dev/health           # → ok
curl -s https://ghinvite.workers.dev/ | grep ghinvite # → HTML
curl -s -o /dev/null -w "%{http_code}" \
  -X POST https://ghinvite.workers.dev/webhooks/github \
  -d '{}'                                             # → 401
```

## Local Development

### 1. Set up `.dev.vars`

Copy `.dev.vars` from the repo root (already provided with safe local defaults). Update with your local GitHub OAuth app credentials if testing OAuth locally.

GHINVITE_SESSION_SECRET must be a 64-character hex string (32 bytes when decoded).

### 2. Apply migrations to local D1 simulation

```bash
wrangler d1 migrations apply ghinvite --local --config wrangler/web.toml
```

### 3. Run web Worker locally

```bash
wrangler dev --local --config wrangler/web.toml
```

Available at `http://localhost:8787`.

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

### Session secret

Rotating `GHINVITE_SESSION_SECRET` invalidates all existing sessions — all users are logged out. No rolling rotation in v1.

```bash
openssl rand -hex 32 | wrangler secret put GHINVITE_SESSION_SECRET --config wrangler/web.toml
```

### GitHub private key

```bash
base64 -w0 new-private-key.pem | wrangler secret put GHINVITE_GITHUB_APP_PRIVATE_KEY --config wrangler/restate-svc.toml
```

## Rollback

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
4. Set **Webhook URL** to `{GHINVITE_BASE_URL}/webhooks/github`
5. Generate and download a private key (used for `GHINVITE_GITHUB_APP_PRIVATE_KEY`)
6. Note the **App ID** (used for `GHINVITE_GITHUB_APP_ID`)
7. Note the **Client ID** and generate a **Client Secret** (used for OAuth vars)
8. Generate a **Webhook Secret** (used for `GHINVITE_WEBHOOK_SECRET`)
