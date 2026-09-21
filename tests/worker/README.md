# Worker Runtime Smoke Test

For the installation failure/replacement matrix on real Restate and Worker/D1,
run `npm run test:installation --prefix tests/worker`. For the independent release
packaging gate, install worker-build **0.8.1** and run
`npm run test:production --prefix tests/worker` (also requires Python 3.11+ and GNU
`timeout`). This uses the Wrangler build commands and generated production shims,
checks entrypoints and rejects fixture-only routes. See
[continuous recovery checks](../../docs/recovery-ci.md) for pins, evidence, and
the remaining #61 live rollout obligations.

For shared storage conformance on the actual D1 adapter, run
`npm run test:storage --prefix tests/worker`. It runs every
`ghinvite_core::storage::test_suite` scenario inside workerd, each against its
own freshly migrated in-memory D1 database, plus D1 audit seek plans. See
[local testing](../../docs/local-testing.md#d1-storage-conformance-requires-wasm-bindgen-cli).

For the authenticated decision queue, run
`npm run test:queue --prefix tests/worker`. This exercises the actual web Worker
and D1 adapter with non-expiring links, earlier/later link expiration, persisted
historical deadlines, missing deadlines, auto-approved requests, and overdue
pending projections. Controlled lifecycle responses verify truthful expired and
already-decided HTTP results for stale queue actions; lifecycle arbitration
itself is covered by the real Restate admission gate.

For stalled upstream headers/bodies, run
`npm run test:deadlines --prefix tests/worker`. The admission gate also verifies
durable timeout recovery and released account exclusivity. See the
[network deadline policy and evidence](../../docs/network-deadlines.md).
Test builds enable `runtime-tests` to expose production client/storage seams;
these fixture routes are excluded from ordinary deployment builds.

For authoritative admission, recovery and D1 projection, run
`npm run test:admission --prefix tests/worker` from the root. See the
[separate gate/results](../../docs/worker-admission-gate.md), including its explicit
blocked production rollout verdict. The session smoke below remains independent.

The admission gate's browser fixture uses a synthetic ingress API-key binding in
Bearer mode. Its `outboundService` checks the actual Wasm Fetch Authorization
header before forwarding each ingress request to local Restate. The authenticated
status page must succeed without leaking the key in its body or headers. This
exercises Worker secret loading and header emission; remote Cloud credential
validation remains in the operator-owned #61 gate.

Runs the actual `ghinvite-web-worker` Wasm in Miniflare/workerd with local KV and
D1 bindings. This catches runtime failures that a successful Wasm build misses,
including unsupported clocks and tracing APIs. There are no clock, crypto,
logging, or Wasm-import bypasses.

## Run

Requirements: Node.js 20+, GNU `timeout` (coreutils), the repo's Rust toolchain with
`wasm32-unknown-unknown`, and the **wasm-bindgen CLI matching `Cargo.lock`**
(currently 0.2.120). The build script checks the CLI version before building.
Use the matching platform binary from the
[wasm-bindgen releases](https://github.com/wasm-bindgen/wasm-bindgen/releases), or:

```bash
cargo install wasm-bindgen-cli --version 0.2.120 --locked
```

From the repository root:

```bash
npm ci --prefix tests/worker
npm test --prefix tests/worker
```

`WASM_BINDGEN=/absolute/path/to/wasm-bindgen` selects a downloaded CLI without
installing it globally. `CARGO_TARGET_DIR` is supported. The pinned npm lockfile
installs Miniflare 4.20260302.0 (workerd 2026-03-02) and esbuild 0.27.3.

`npm test` rebuilds the actual web Worker with:

```bash
cargo build --locked -p ghinvite-web-worker --target wasm32-unknown-unknown --features runtime-tests
```

It then runs `wasm-bindgen --target web --no-typescript`, bundles the generated
JavaScript with esbuild, and executes `smoke.mjs`. The shim passes workerd's
precompiled Wasm module directly to the generated `initSync`; generated snippet
imports are bundled automatically. Bindings and bundles live in ignored paths.
To rerun the smoke against the last build, use `node tests/worker/smoke.mjs`.

## Coverage and isolation

- `/health` returns 200 `ok` through the production router and middleware.
- `/login` returns a dummy GitHub authorization redirect and persists one
  versioned encrypted KV record. Raw bytes contain neither the OAuth state nor
  session field names/return destination.
- A callback with a deliberately wrong state returns 400 with the **state did
  not match** failure page rather than the **no sign-in pending** one, proving
  the persisted record was decrypted and loaded on a later request. The page
  carries ghinvite's own copy and a `/login` link, never an internal or
  upstream diagnostic. It does not rewrite ciphertext or issue another cookie.
- A logout request with an injected KV-read failure returns 503, clears the
  browser cookie, reports unconfirmed server sign-out, and attempts no storage
  mutation. This request alone uses the explicit test-only KV failure wrapper in
  `shim.mjs`; the Rust routes and session middleware remain unchanged.

All configuration is dummy data supplied directly by the test; it never reads
`.dev.vars` or production secrets. Redirects are not followed. The Worker outbound
service rejects all external requests, and the test asserts zero attempts. Local
KV/D1 are ephemeral and the runtime is disposed on success or failure.

Compatibility date/flags match `wrangler/web.toml`; keep them aligned when changing
deployment compatibility. The harness does not apply domain migrations or perform
a successful OAuth exchange. Native tests cover authenticated lifetime/rotation;
this smoke does not establish distributed KV propagation or production rate limits.
