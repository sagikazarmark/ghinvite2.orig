# Local Integration Testing

## Workspace tests (no Docker/wrangler required)

```bash
cargo test --workspace
```

This excludes the feature-gated real Restate acceptance gate, the Worker/D1 gates,
and browser tests. A passing workspace suite does **not** verify Restate runtime
compatibility or container networking.

### Restate ingress authentication

```bash
cargo test -p ghinvite-web --test restate_ingress --locked
cargo test -p ghinvite-web --test invitation_resolution --locked
cargo check -p ghinvite-web-worker --target wasm32-unknown-unknown --locked
```

The HTTP client tests exercise Bearer calls, authoritative calls and sends,
explicit credential-free local mode, missing/invalid credentials, redacted Debug,
and upstream/decoding/transport error sanitization. The signed-in browser-route
test verifies that ingress credentials stay out of HTML and error responses.
`npm run test:admission --prefix tests/worker` additionally exercises the actual
Worker secret-binding → Wasm client → Fetch Authorization header path using a
synthetic Bearer key and real local Restate; the session-only smoke retains
explicit credential-free local mode.
Remote deployment/binding verification belongs to the
[operator-owned ingress gate](restate-ingress-gate.md), not these mock tests.

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

Repository-selection deliveries require `repository_selection` (`all` or
`selected`) and both repository arrays, as specified by
[GitHub's payload contract](https://docs.github.com/en/webhooks/webhook-events-and-payloads#installation_repositories).
Each repository must have an unsigned integer `id`; unrelated extra fields are
ignored. Missing or malformed consumed fields fail decoding instead of being
silently defaulted or skipped.

The signed HTTP and event-to-command tests run without GitHub or Restate:

```bash
cargo test -p ghinvite-web --test route_smoke webhook
cargo test -p ghinvite-web --lib commands::tests
```

### OAuth sessions

HTTP regression tests exercise OAuth state validation, ID rotation, old-cookie
invalidation, return destinations, and session-store failures. Console tests
verify that both same-user sign-in and switching GitHub users discard cached
organization authority and use the new access token:

```bash
cargo test -p ghinvite-web --test oauth_flow
cargo test -p ghinvite-web --test session_protection
cargo test -p ghinvite-web --test console_flow oauth_signin_discards
cargo build -p ghinvite-web-worker --target wasm32-unknown-unknown
```

Successful OAuth uses tower-sessions' `cycle_id()` to revoke the prior session
ID, replaces session data with fresh credentials and empty authorization caches,
and lets the session middleware persist the new record and send its cookie.
Revocation or persistence failure returns HTTP 500 instead of a login redirect.
Validated OAuth state is persisted as consumed before contacting GitHub, so a
failed exchange or user lookup cannot replay it on a consistent store.

Native development uses SQLite; production Workers use **Workers KV** for
sessions (D1 stores domain data). KV is eventually consistent: deletions and
state updates may remain stale in other locations, and a new ID may not yet be
visible everywhere. Correct rotation calls therefore do **not** guarantee
instantaneous worldwide logout or globally atomic OAuth-state consumption.
The main OAuth paths use encrypted SQLite; targeted tests inject MemoryStore
failures. Protection tests inspect raw ciphertext, tampering, wrong keys,
cross-session substitution, expiry, and a late write after revocation. Rotation
tests share a backend between old/new-key apps and prove old-cookie rejection
and fresh login success. The Worker build checks target compatibility; these
tests do not measure distributed KV propagation. See the
[session runbook](deploy.md#session-secret) for cutover and sign-out guarantees.

The [Worker runtime smoke](../tests/worker/README.md) runs the actual Wasm in
workerd with local KV and dummy configuration. It verifies encrypted persistence,
later-request state loading, and logout cookie clearing on KV-read failure,
including the JavaScript clock and console logging paths that compilation alone
cannot exercise. It runs in CI and blocks all outbound network requests.

### Browser mutation CSRF protection

Authenticated native forms use a server-generated, 256-bit synchronizer token
stored in the session. `CsrfForm` verifies the URL-encoded `csrf_token` field
before dispatch; missing, invalid, duplicate, or another-session tokens return
the same 403 response. Console authentication/account concealment runs first.
Tokens are reusable across open forms, carried in the island props when it
replaces the SSR form, and replaced with identity at every successful OAuth
sign-in. Dynamic responses use `Cache-Control: private, no-store`. Tokens stay
out of URLs and command payloads; rejected parser bodies are not logged/echoed.

Every authenticated session stores its token. A stored authenticated session
without one is malformed and confers no identity, like any unreadable record:
protected storage treats it as absent, and the session helper reads it as
signed out.

Protected POST inventory:

- `/console/accounts/{login}/links` — create
- `/console/accounts/{login}/links/{link_id}/edit` — edit metadata
- `/console/accounts/{login}/links/{link_id}/revoke` — revoke
- `/console/accounts/{login}/requests/{request_id}/approve` and `/decline`
- `/i/{slug}` — submit request (the request ID is only an idempotency key)
- `/logout` — sign out; GET returns 405

GitHub redirect and webhook contracts are independent of form tokens:

- `GET /oauth/callback` requires the session's one-use OAuth state before code
  exchange and session rotation. An installation ID on this callback does not
  onboard an installation.
- `GET /setup/github` requires authentication and resolves the supplied
  installation ID against GitHub's user-visible installations, then fetches
  repository selection through that user's API before dispatch. It is the
  verified GitHub setup/update return, not a general browser mutation endpoint.
  `/install` only redirects to GitHub; neither route gains a POST alternative.
- `POST /webhooks/github` uses its existing HMAC/header/payload verification.

Run `cargo test -p ghinvite-web --test csrf_flow` for rendered form inventory,
same-site sibling-origin forgery, cross-session rejection, command isolation,
accepted native actions, and validation/service retry preservation. Session
rotation/logout cases are in `oauth_flow`; setup and webhook tests retain their
independent authentication contracts. The browser suite verifies real fields
and submission with the island mounted, JavaScript disabled, and the bundle
blocked (see `tests/browser/README.md`).

### Restate approval and delivery acceptance gate

From the repository root, run the same targets as CI:

```bash
bash scripts/test-restate.sh authoritative_admission   # also the default target
bash scripts/test-restate.sh retained_delivery
bash scripts/test-restate.sh installation_availability
bash scripts/test-restate.sh installation_audit_replay
bash scripts/test-restate.sh durable_projection
```

Prerequisites:

- Rust **1.92.0**, selected by `rust-toolchain.toml`, with Cargo dependencies
  available (`--locked` uses the checked-in lockfile). The runner prints the
  actual compiler version: non-rustup environments such as Nix may override the
  toolchain file; use 1.92.0 to reproduce the CI compiler exactly.
- Bash, GNU coreutils `timeout` (on macOS, `brew install coreutils` provides
  `gtimeout`), and Docker Compose v2+.
- A running **local** Docker Engine 20.10+ on Linux, or Docker Desktop on
  macOS/Windows (run Bash on the host that Docker Desktop forwards to). Registry
  access is needed on the first run. Remote Docker daemons and running the test
  inside another container are not supported by this host/bridge topology.
- The container must be able to reach an ephemeral TCP port on the host. Host
  firewalls must allow traffic from the Docker bridge to that test listener.
  No GitHub App credentials, separately started GitHub stub, Wrangler, or dev stack is needed.

`compose.yaml` pins Restate **1.7.9** by image digest; the dev stack and smoke
service share that pin. CI uses Ubuntu 24.04 and the same Rust version and runner
script. To upgrade Restate, update the shared image pin and rerun these commands.

The script runs the GitHub stub HTTP contract tests (for `retained_delivery`),
compiles the selected integration-test file, creates a uniquely named
Compose project, and publishes admin/ingress on randomly allocated **loopback**
ports. The Rust test serves the current workflow endpoint on `0.0.0.0:0`.
Restate reaches it using `host.docker.internal`, explicitly mapped by Compose
to Docker's `host-gateway` on Linux (also supported by Docker Desktop). No bridge
subnet or fixed IP is assumed. The readiness probe uses cleartext HTTP/2 and the
SDK discovery media type. Successful deployment registration verifies the
container-to-host discovery path before any invocation is sent.

`retained_delivery` starts its own GitHub HTTP stub on `127.0.0.1:0`, using the same router
as the standalone example. The production `InstallationClient` and
`ReqwestTransport` exchange a synthetic App JWT for a fake installation token
and send real HTTP requests to that stub. Both servers are Tokio tasks owned by
the test runtime, so success, assertion failure, or process termination closes
their sockets. No credentials are injected into this gate.

Each target documents its scenarios: [authoritative admission](admission-v1.md#native-acceptance)
and [request lifecycle](request-lifecycle-v1.md), [retained delivery](delivery-recovery.md#verification),
[installation availability](installation-availability.md), and
[durable projection](admission-v1.md#projection-tests). Focused HTTP
authorization/session tests remain in `console_flow`, `route_smoke`, and `oauth_flow`.

**Boundaries:** local stub responses are not evidence of live GitHub webhook
availability or delivery. Native SQLx runtime tests are **not D1 adapter
conformance**. Full browser OAuth is outside this gate. No test claims
exactly-once delivery across a GitHub-success/database-failure gap.

#### Standalone GitHub stub

```bash
cargo run --locked -p ghinvite-github --features test-stub --example stub -- --port 3001
```

This loopback-only fixture provides token minting, collaborator PUT/GET/DELETE,
and pending-invitation GET/DELETE. Successful PUTs return JSON with an invitation
ID, invitee, repository, permissions, and creation timestamp; pending invitations
do not count as accepted collaborators. It models the consumed API subset, not
all GitHub validation, authorization, pagination, or pending-invitation updates.

`POST /outcomes` accepts `{"owner":"test-org","repo":"api","user":"requester",
"outcome":"transient_once"}`. Outcomes are `already_collaborator` (204),
`terminal_failure` (422), or `transient_once` (one 502, then normal 201).
`GET /calls` returns `{"count":N,"requests":[...]}` with method, path, permission,
and status only, never headers/JWTs/tokens. `DELETE /reset` clears all fixture
state. Configuration and ledger requests are excluded from the API call count.

The shell bounds each compilation/contract-test step (600s), container startup/pull (180s), test execution
(150s), and cleanup (30s), escalating to a kill after a further 5s if a child
ignores termination; CI also has a 30-minute job deadline. Registration
and invocation failures stop immediately, reporting the phase and HTTP status
without printing response bodies, headers, or configured URLs. On failure the
script prints container status and at most 60 log lines from its own disposable
runtime, which has only synthetic fixtures and no injected secrets. Actions
retains these diagnostics in the job logs. The failure output includes the
failed assertion/phase and the runtime retry status/target when available.

Every run has fresh SQLite and Restate state and unique object/workflow keys.
An exit trap removes the container, volumes, and network on success, failure,
or interruption; cleanup failure is itself a failed gate and prints a retry
command. Reruns can coexist with `docker compose up` for development.
Missing Docker, readiness timeout, or any failed assertion exits nonzero: report
that as a **failed gate / verification gap**, never as a successful smoke test.
Enabling `--features integration` runs the test rather than leaving it ignored;
direct Cargo execution requires `RESTATE_ADMIN_URL`, `RESTATE_INGRESS_URL`, and
`RESTATE_ENDPOINT_HOST` from an isolated runtime. Prefer the script, which owns
those values and runtime cleanup.

### Admission protocol proof (#48, retired)

The throwaway #48 protocol proof
([`crates/ghinvite-workflows/tests/admission_protocol_proof.rs` at `f78d8d2`](https://github.com/sagikazarmark/ghinvite2.orig/blob/f78d8d2/crates/ghinvite-workflows/tests/admission_protocol_proof.rs),
with its `split_state.rs`, `deadlines.rs`, and `notifications.rs` extensions)
registered test-only handlers and a miniature SQL schema to check the admission
protocol before production handlers existed. It has since been retired in
favour of `authoritative_admission`, and the runner no longer accepts an
`admission_protocol_proof` target. Read the files at that commit for the
original evidence; [ADR 0003](adr/0003-restate-authoritative-admission.md)
keeps its recorded results.

The production suites now cover each proof scenario:

- `authoritative_admission`: journaled decision followed by coherent state
  writes and projection/workflow sends, SDK task aborts at every recovery
  checkpoint with exclusive status reads waiting for recovery, final-use
  concurrency, replay/payload conflicts, expiry before versus after a durable
  decision, projection outage during admission/revocation, terminal release and
  stale-pointer protection, deadline arbitration and timer races, lazy state
  without eager history transfer, and workflow notification replay, interrupted
  resolution, and the 409 for a conflicting terminal signal.
- `durable_projection`: commit acknowledgement loss and ordered projection
  convergence.
- `retained_delivery`: late notification after workflow completion does not
  recreate promise state.
- The `ghinvite-core` storage test suite: stale snapshots cannot regress a
  revoked link.

The runner sets the disposable runtime's cleanup scan to one second
(`RESTATE_PROOF_CLEANUP_INTERVAL`) so retention cleanup is observable; ordinary
smoke/dev runs retain the default hourly scan.

### D1 storage conformance (requires wasm-bindgen CLI)

```bash
npm run test:storage --prefix tests/worker
```

This builds the actual workflows Worker with `runtime-tests` and runs every
shared storage scenario (`ghinvite_core::storage::test_suite`, the same suite
`sqlx_suite` runs natively) against the real `D1Storage` inside workerd. Each
scenario gets its own Miniflare instance with a fresh in-memory D1 database
migrated from `migrations/`. It also executes the shared audit seek query over
historical UTC encodings, exact page boundaries and nanosecond/ID seeks through
the adapter, and asserts D1's expression-index query plans. No Docker, Restate or
Wrangler is needed; setup is as in the [Worker smoke](../tests/worker/README.md).

The [Worker admission gate](worker-admission-gate.md) separately exercises the
D1 binding path end to end via workerd and real Restate
(`npm run test:admission --prefix tests/worker`).

### wasm32 build check

One crate per command, always with `-p`. `cfg(target_arch = "wasm32")` means
"Cloudflare Workers" in the Worker crates and "browser" in `ghinvite-ui` and
`ghinvite-island`, so a `--workspace --target wasm32-unknown-unknown` build
would unify features across both and is never what you want (ADR 0001).
Install the target first: `rustup target add wasm32-unknown-unknown`.

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

### Document shell (#68)

Every full HTML response is one standards-mode document with an explicit
language and a mobile viewport. The markup is asserted against the real router
in the workspace suite:

```bash
cargo test -p ghinvite-web --test document_shell
```

Standards mode and the layout viewport are the browser's verdict, not the
markup's, so a phone-sized engine has to render the pages:

```bash
npm exec --prefix tests/browser -- playwright test --config document.config.mjs
```

That config starts the `#[ignore]`d `document_browser_server` fixture from the
same test file (the production router over in-memory storage, no island bundle
or Restate) and visits the public and Console documents at 390 x 844 and
1440 x 1000. See `tests/browser/README.md` for the covered pages.

## CI vs Local

| Job | Local | CI |
|-----|-------|-----|
| Unit tests | ✅ always | ✅ push + PR |
| Lint | ✅ always | ✅ push + PR |
| wasm32 build | ✅ always | ✅ push + PR |
| Island bundle | ✅ requires dx | ✅ push + PR |
| Document shell browser suite | ✅ requires Playwright | ✅ push + PR |
| Restate integration | ✅ `bash scripts/test-restate.sh` | ✅ push + PR |
| D1 storage conformance | ✅ requires wasm-bindgen CLI | ✅ push + PR |
