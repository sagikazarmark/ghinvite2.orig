# Local Integration Testing

## Prerequisites

- Docker Desktop (or Docker Engine) running
- `wrangler` CLI installed: `npm i -g wrangler`
- Rust toolchain with `wasm32-unknown-unknown` target: `rustup target add wasm32-unknown-unknown`

## Terminal 1 — Restate

```bash
docker run --rm -p 8080:8080 -p 9070:9070 docker.restate.dev/restatedev/restate:latest
```

Wait for: `Restate is ready` in the output.

## Terminal 2 — GitHub Stub

```bash
cargo run -p ghinvite-github --example stub -- --port 3001
```

Wait for: `github-stub listening on 0.0.0.0:3001`

## Terminal 3 — Tests

### Unit tests (no Docker/wrangler required)

```bash
cargo test --workspace
```

### GitHub webhooks

`POST /webhooks/github` uses `octoevents` for signature verification, header
validation, and event dispatch, without `octocrab`. Set a nonempty
`GHINVITE_WEBHOOK_SECRET` for local webhook delivery; an unset or empty secret
disables the endpoint with 503.

Deliveries require `Content-Type: application/json`, `X-GitHub-Event`,
`X-GitHub-Delivery`, and `X-Hub-Signature-256`. Sign the exact request bytes with
HMAC-SHA256. Successful deliveries (including ping and unhandled events/actions)
return 204. Missing or mismatched signatures return 401; malformed signatures,
missing event/delivery headers, and unsupported content types return 400.
Bodies above 2 MiB return 413. Matched payload decoding or downstream command
failures return an empty 500 response, with details in tracing logs.

Routes handle `repository_invitation.accepted/declined`, `installation.deleted`,
and `installation_repositories.added/removed`. GitHub does not automatically
redeliver failures; redelivery must be requested separately.

The signed HTTP and event-to-command tests run without GitHub or Restate:

```bash
cargo test -p ghinvite-web --test route_smoke webhook
cargo test -p ghinvite-web --lib commands::tests
```

### Restate integration tests

```bash
cargo test -p ghinvite-workflows --features integration -- --ignored
```

### D1 storage smoke tests (requires wrangler)

First, apply migrations to local D1:

```bash
wrangler d1 migrations apply ghinvite --local --config wrangler/web.toml
```

Then run the smoke tests:

```bash
cargo test -p ghinvite-storage-d1 --features d1-suite -- --ignored
```

### wasm32 build check

One crate per command, always with `-p`. `cfg(target_arch = "wasm32")` means
"Cloudflare Workers" in the Worker crates and "browser" in `ghinvite-ui` and
`ghinvite-island`, so a `--workspace --target wasm32-unknown-unknown` build
would unify features across both and is never what you want (ADR 0001).

```bash
cargo check -p ghinvite-ui --target wasm32-unknown-unknown
cargo check -p ghinvite-island --target wasm32-unknown-unknown
cargo build -p ghinvite-storage-d1 --target wasm32-unknown-unknown
cargo build -p ghinvite-web-worker --target wasm32-unknown-unknown
cargo build -p ghinvite-workflows-worker --target wasm32-unknown-unknown
```

### Island bundle (requires dx 0.7.x)

```bash
scripts/build-island.sh          # dx bundle → dist/public, prints sizes, enforces the 600 KB gzipped budget
cargo test -p ghinvite-island    # markup parity: island first frame == server HTML
```

Then browser-check it: `cargo run -p ghinvite-web` from the repo root serves
`dist/public/assets` under `/assets/`, or render a static fixture with
`cargo run -p ghinvite-island --example ssr_fixture > dist/public/index.html`
(local only — see `crates/ghinvite-island/README.md`).

## CI vs Local

| Job | Local | CI |
|-----|-------|-----|
| Unit tests | ✅ always | ✅ push + PR |
| Lint | ✅ always | ✅ push + PR |
| wasm32 build | ✅ always | ✅ push + PR |
| Island bundle | ✅ requires dx | ✅ push + PR |
| Restate integration | ✅ 3-terminal setup | ✅ main branch only |
| D1 suite | ✅ requires wrangler | ❌ not in CI v1 |
