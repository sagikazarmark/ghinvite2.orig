# Local Integration Testing

## Workspace tests (no Docker/wrangler required)

```bash
cargo test --workspace
```

This excludes the feature-gated real Restate acceptance gate, the opt-in ignored D1 suite,
and browser tests. A passing workspace suite does **not** verify Restate runtime
compatibility or container networking.

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

The session helper retains deterministic CSRF fallback coverage for injected
test stores, but production protected storage rejects legacy unencrypted
sessions outright. Users must restart sign-in after the protection upgrade.

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

From the repository root, run the same command used by CI:

```bash
bash scripts/test-restate.sh
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
script. To upgrade Restate, update the shared image pin and rerun this command.

The script runs the GitHub stub HTTP contract tests, compiles the single
integration-test file, creates a uniquely named
Compose project, and publishes admin/ingress on randomly allocated **loopback**
ports. The Rust test serves the current workflow endpoint on `0.0.0.0:0`.
Restate reaches it using `host.docker.internal`, explicitly mapped by Compose
to Docker's `host-gateway` on Linux (also supported by Docker Desktop). No bridge
subnet or fixed IP is assumed. The readiness probe uses cleartext HTTP/2 and the
SDK discovery media type. Successful deployment registration verifies the
container-to-host discovery path before any invocation is sent.

The test starts its own GitHub HTTP stub on `127.0.0.1:0`, using the same router
as the standalone example. The production `InstallationClient` and
`ReqwestTransport` exchange a synthetic App JWT for a fake installation token
and send real HTTP requests to that stub. Both servers are Tokio tasks owned by
the test runtime, so success, assertion failure, or process termination closes
their sockets. No credentials are injected into this gate.

The bounded scenarios verify:

- Request expiration after a two-second decision window, including the stored
  link description/scope/use count and four audit records, with **zero** GitHub calls.
- Auto-approval and manual approval through the web application's public
  `RestateCommands` → `RestateClient` → real ingress/handlers. Manual approval
  resolves the actual durable decision promise; pending requests dispatch no
  GitHub work. Each link has **four repositories**, with `201` (sent with an ID),
  `204` (already collaborator), `422` (failed), and one `502` followed by `201`.
  The final retry is performed by Restate, not a retry loop in the test/client.
- Persisted request approval separately from per-repository delivery success,
  stored upstream invitation IDs matched against GitHub list responses, requester,
  repository and permission on outbound calls, exact attempt counts, and safe
  account-scoped audit metadata. Reads use the public `Storage` API.
- Re-submitting the **same request ID and payload after completed success**
  leaves one request, one link use, the same invitation IDs/audit count, and no
  additional outbound GitHub calls. A transparent loopback HTTP observer forwards
  real command bytes/responses and captures Restate's send receipts: both sends
  must return the same invocation ID, whose workflow output is completed. It
  never synthesizes an ingress response. This covers stable-operation replay, not
  arbitrary browser resubmissions that generate a new request ID.

The gate starts below the web router; focused HTTP authorization/session tests
remain in `console_flow`, `route_smoke`, and `oauth_flow`. The browser POST echo
fixture is not used as evidence of the web-to-Restate wire contract.

**Boundaries:** local stub responses are not evidence of live GitHub webhook
availability or delivery. Native SQLx runtime tests are **not D1 adapter
conformance**. Full browser OAuth, automatic lifecycle scheduling, and deep
partial-commit/crash-recovery fault injection are outside this gate (Batch 3
covers the latter). The transient scenario fails before upstream success; it
does not claim exactly-once delivery across a GitHub-success/database-failure gap.

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

Readiness has a 30-second deadline per service; harness HTTP calls have a 20-second
deadline (3-second connect), the real web client has its production 15-second
timeout, request polling is bounded at 15s and fanout/retry polling at 30s, and
all scenarios share a 120-second overall deadline.
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

### Admission protocol proof (#48)

```bash
bash scripts/test-restate.sh admission_protocol_proof
```

This optional throwaway experiment reuses the disposable runtime runner and
registers test-only handlers from
`crates/ghinvite-workflows/tests/admission_protocol_proof.rs`. It verifies a
complete journaled decision followed by a coherent state write and durable
projection/workflow sends. The endpoint uses request-response mode on native.
It aborts SDK execution tasks at four recovery checkpoints, rejects real SQLite
projection writes with a trigger while admission/revocation proceed, then tests
commit acknowledgement loss and duplicate/out-of-order projection convergence.
It also checks final-use concurrency, replay/payload conflicts, and expiration
before versus after a durable decision.

The split-state extension in `tests/admission_protocol_proof/split_state.rs`
uses lazy-loaded link, operation, request, and requester-blocker keys. It aborts
between state writes, verifies exclusive status reads wait for recovery, checks
terminal release/approved suppression/stale-pointer protection, and observes
request bodies to ensure unrelated history is not eagerly transferred. It
does not test production lifecycle timers, cancellation authorization, or
lifecycle projection/audit.

The miniature SQL schema and handlers are protocol evidence, not production
admission implementation or D1/Worker conformance. See
[ADR 0003](adr/0003-restate-authoritative-admission.md) for results and remaining
verification. The default runner still executes the existing production workflow
acceptance gate; the proof is explicitly selected by the argument above.

### D1 storage smoke tests (requires wrangler)

Install `wrangler` separately (`npm i -g wrangler`).

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

## CI vs Local

| Job | Local | CI |
|-----|-------|-----|
| Unit tests | ✅ always | ✅ push + PR |
| Lint | ✅ always | ✅ push + PR |
| wasm32 build | ✅ always | ✅ push + PR |
| Island bundle | ✅ requires dx | ✅ push + PR |
| Restate integration | ✅ `bash scripts/test-restate.sh` | ✅ push + PR |
| D1 suite | ✅ requires wrangler | ❌ not in CI v1 |
