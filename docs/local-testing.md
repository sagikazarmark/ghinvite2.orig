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

```bash
cargo build -p ghinvite-storage-d1 --target wasm32-unknown-unknown
cargo build -p ghinvite-web-worker --target wasm32-unknown-unknown
cargo build -p ghinvite-workflows-worker --target wasm32-unknown-unknown
```

## CI vs Local

| Job | Local | CI |
|-----|-------|-----|
| Unit tests | ✅ always | ✅ push + PR |
| Lint | ✅ always | ✅ push + PR |
| wasm32 build | ✅ always | ✅ push + PR |
| Restate integration | ✅ 3-terminal setup | ✅ main branch only |
| D1 suite | ✅ requires wrangler | ❌ not in CI v1 |
