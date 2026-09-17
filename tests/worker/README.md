# Worker Runtime Smoke Test

For authoritative admission, recovery, D1 projection and cutover execution, run
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

Requirements: Node.js 20+, the repo's Rust toolchain with
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
cargo build --locked -p ghinvite-web-worker --target wasm32-unknown-unknown
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
- A callback with a deliberately wrong state returns 400 **CSRF state mismatch**,
  proving the persisted record was decrypted and loaded on a later request.
  It does not rewrite ciphertext or issue another cookie.
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
