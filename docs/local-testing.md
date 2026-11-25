# Local Integration Testing

## Console authority and selection checks

```bash
cargo test -p ghinvite-web --test console_authority --test console_delegation
cargo test -p ghinvite-web --lib authority
cd tests/browser
npx playwright test --config repository-selection.config.mjs
npx playwright test --config repository-picker.config.mjs
```

The repository picker suite runs the production create route over a
120-repository installation (two GitHub listing pages), with and without the
shared script: search, paging, selections kept across both, and creation of a
link whose repositories came from different pages.

The route tests exercise real scoped SQL, typed installation-token HTTP and the
concrete Restate ingress seam. The browser tests exercise the shipped shared
script: pending/denied authority prevents submission, deselection clears its
blocker, and stale responses cannot authorize a replacement form. Creation
independently checks all selected repositories; browser feedback is not a grant.

`AUTHZ_CACHE` evidence is session-independent and expires logically at 300 seconds.
Outage fixtures age actual observed evidence before expecting upstream failure;
a fresh cached observation is intentionally reusable. The actual Worker cache
and rate-binding gates are `tests/worker/authority-cache.mjs` and
`tests/worker/rate-limit.mjs`. Run them after building the matching Worker bundle.

## Workspace tests (no Docker/wrangler required)

```bash
cargo test --workspace
```

This excludes the feature-gated real Restate acceptance gate, the Worker/D1 gates,
and browser tests. A passing workspace suite does **not** verify Restate runtime
compatibility or container networking.

### Restate ingress authentication

Public invitation links use `/i#<LinkSecret>`; the Console shows the fragment
URL once after creation or rotation. The internal ULID is not an Invitation
Code and legacy `/i/<ULID>` routes return a neutral 404. Test authenticated creation, opening and
submission before projection with `bash scripts/test-restate.sh canonical_link`.
The authoritative-admission gate also verifies creation interruption, concurrent
replay and retained receipts after actual invocation cleanup.

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

`POST /webhooks/{repo|org}` uses the selected app's current/previous HMAC
secrets from the installed-app registry. `GH_REPO_WEBHOOK_SECRETS` is required
for the repository app; legacy `GHINVITE_WEBHOOK_SECRET` is not a fallback.
Unknown app scopes return 404 and an absent registry fails closed with 503.

Deliveries require JSON content type, `X-GitHub-Event`, `X-GitHub-Delivery`, and
`X-Hub-Signature-256`. Sign the exact request bytes. The receipt identity is the
signed payload hash, never the unsigned delivery header. Missing/mismatched
signatures return 401; missing event/delivery headers and malformed consumed
payloads return 400; bodies over 2 MiB return 413.

Authenticated installation created/deleted/suspend/unsuspend/accepted-permission
and repository-selection events dispatch canonical app-scoped commands and return
202. Setup facts contain no installer; only an authenticated installation sender
can establish the Installing Owner. Unknown actions and obsolete member or
repository-invitation events return 202 without delivery dispatch. Settlement
uses verified reconciliation reads. No live webhook delivery claim follows from
these local boundary tests.

```bash
cargo test -p ghinvite-web --test app_webhooks
cargo test -p ghinvite-web --lib commands::tests
node tests/worker/app-routes.mjs
```

### OAuth sessions

HTTP regression tests exercise OAuth state validation, ID rotation, old-cookie
invalidation, return destinations, and session-store failures. Console tests
verify user-bound installation authority independently of the session. Same-user
sign-in may reuse fresh evidence; switching users cannot reuse another user's permission observations:

```bash
cargo test -p ghinvite-web --test oauth_flow
cargo test -p ghinvite-web --test session_protection
cargo test -p ghinvite-web --test console_flow oauth_signin_keeps_user_bound
cargo build -p ghinvite-web-worker --target wasm32-unknown-unknown
```

Successful OAuth uses tower-sessions' `cycle_id()` to revoke the prior session
ID and replaces session identity and CSRF state,
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
- `/i/{code}` — submit request (the code is the canonical link ULID; the operation ID identifies the attempt)
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
RESTATE_SERVER_BIN=/path/to/restate-server cargo test --locked -p ghinvite-workflows \
  --features integration --test authoritative_admission -- --test-threads=2
RESTATE_SERVER_BIN=/path/to/restate-server cargo test --locked -p ghinvite-workflows \
  --features integration --test repository_protocol -- --test-threads=2
RESTATE_SERVER_BIN=/path/to/restate-server cargo test --locked -p ghinvite-workflows \
  --features integration --test installation_authority --test invite_budget \
  --test journal_steps --test link_capability --test request_control \
  --test settlement_evidence --test sweep_schedule
bash scripts/test-restate.sh request_owner   # also the default target
bash scripts/test-restate.sh canonical_link
bash scripts/test-restate.sh retained_delivery
bash scripts/test-restate.sh approved_delivery
bash scripts/test-restate.sh installation_availability
bash scripts/test-restate.sh installation_audit_replay
bash scripts/test-restate.sh durable_projection
```

Prerequisites:

- Rust **1.100.0**, selected by `rust-toolchain.toml`, with Cargo dependencies
  available (`--locked` uses the checked-in lockfile). The runner prints the
  actual compiler version: non-rustup environments such as Nix may override the
  toolchain file; use 1.100.0 to reproduce the CI compiler exactly.
- Bash, GNU coreutils `timeout` (on macOS, `brew install coreutils` provides
  `gtimeout`), and Docker Compose v2+.
- A running **local** Docker Engine 20.10+ on Linux, or Docker Desktop on
  macOS/Windows (run Bash on the host that Docker Desktop forwards to). Registry
  access is needed on the first run. Remote Docker daemons and running the test
  inside another container are not supported by this host/bridge topology.
- The container must be able to reach an ephemeral TCP port on the host. Host
  firewalls must allow traffic from the Docker bridge to that test listener.
  No GitHub App credentials, separately started GitHub stand-in, Wrangler, or dev stack is needed.

`compose.yaml` pins Restate **1.8.1** by image digest; the dev stack and smoke
service share that pin. CI uses Ubuntu 24.04 and the same Rust version and runner
script. To upgrade Restate, update the shared image pin and rerun these commands.

**Fixture clocks must keep delayed sends in the future (Restate >= 1.8).** The
workerd suite pins the Worker's `Date.now()` through the `x-test-clock` shim, and
the SDK turns `send_after(delay)` into `Date.now() + delay`. A fictional past
timeline therefore makes every "seven days later" `tick_expire` self-send already
due for the real server, forming a self-send chain on one object key. The legacy
inbox (<= 1.7) interleaved inboxed ingress calls so the chain ended after a few
links; VQueues (unconditional since 1.8.0) schedule due timers ahead of inboxed
calls, so a queued `decide` starves until the client timeout. Bisected on 1.7.9
with and without `RESTATE_EXPERIMENTAL_ENABLE_VQUEUES=true`; the SDK version makes
no difference, and production clocks are real, so the loop cannot form there.
Steps in `tests/worker/admission.mjs` that schedule request expiry therefore
anchor their timeline at the real clock and let lazy expiry in
`decide`/`request_status` do the work.

**The pinned clock must also cover the response body (workerd >= 1.20260619).**
The shim keeps the `x-test-clock` time in an `AsyncLocalStorage` store. The
Restate handler does not finish inside the Worker's `fetch`: in
`RequestResponse` mode the Worker returns a streaming body, and each `pull()` of
that wasm-backed `ReadableStream` polls the handler, which is where most clock
reads (`ctx.run` steps such as `organization_expiry_tick_time`) happen. Up to
workerd 1.20260617.1, workerd pumped a returned JS stream with a JS promise loop
(`PumpToReader`), and `pull()` saw the request's store. The "draining read" path
(autogate `workerd-autogate-enable-draining-read-on-standard-streams`, on in
production for a while) calls `pull()` with no store, so it reads the real time.
workerd 1.20260619.1 removed the autogate and the old path (workerd commits
`82ff81fb02` and `c329332639`, in cloudflare/workerd#6822). From miniflare
4.20260623.0 on, the due `tick_own_expiry` read the real time, saw nothing due
and returned 200 without the re-read or the DELETE, so the `ORG_CANCELLATION_ONLY`,
`CANCELLATION_ONLY` and `test:admission` own-expiry steps timed out ("convergence
deadline exceeded"). Bisected on miniflare/workerd releases: 1.20260617.1 is good
and 1.20260619.1 is bad, and 1.20260617.1 with that one autogate enabled is bad.
Running a pull that no JS caller started in the empty context is defensible
(the change shipped without a compatibility flag, but production already behaved
so), so the fix is on our side and nothing is filed upstream. The shim now wraps a clocked response body
in a stream whose every `pull()` re-enters the store before it reads the Worker's
body. This works on both the old and the new path, and nothing changes for
uncontrolled requests or for production.

**One link admits about 3 per second in the workerd fixture: round trips, not a
delay.** The `PAUSE_ONLY` burst (21 concurrent `admit`s on one unlimited link)
is strictly serialized. It took 15-16 s on 1.7.9 and 1.8.1 alike while the Worker
rebuilt its endpoint on every request; since that endpoint is built once per isolate
(2026-11-19) it takes about 6 s. Timing every
Restate-to-Worker request in `admission.mjs`'s HTTP/2 proxy (2026-11-15, 1.8.1)
showed no sleep, timer or poll in the path: Restate re-invokes 1.4-1.7 ms (median)
after each suspension. The time is the number of rounds times the cost of a round:

- The workflows Worker must use `ProtocolMode::RequestResponse`, and the owners use
  lazy state. Every awaited `ctx.get` of an unloaded key, every `ctx.run` (its
  completion must be acknowledged) and every awaited call ends the request with a
  suspension, and Restate re-sends the whole journal in a new request. The burst made
  about 1,070 endpoint requests, roughly 51 per admission: 55% were suspensions on
  lazy state reads, 15% on run acknowledgements, the rest on sends and calls. About
  45 of them sit on the critical path under the link's exclusive lock: `admit` (17
  rounds), two `InvitationRequest/apply_link_control` (about 6 each) and
  `InvitationRequest/initialize` (15).
- One round cost 14-17 ms (median) in this fixture on 2026-11-15. Of a bare request,
  Miniflare dispatch was about 0.9 ms, GitHub app and identity configuration about
  1.1 ms, and `build_endpoint_with_identity` about 11.5 ms: the Worker rebuilt the
  endpoint on every request, and binding a service builds its discovery metadata,
  including the schemars JSON schemas of every handler. The fixture runs a debug Wasm
  build.

**The endpoint is now built once per isolate** (`ghinvite-workflows-worker`,
2026-11-19). The Worker keeps the composed endpoint in a thread-local together with
everything composition read: the `DB` binding object and every var and secret with
its value. A request whose env differs (a fixture's fault-injecting `env.DB` proxy, a
changed var) recomposes and replaces it; an invalid env fails every request and is
never cached. Identity verification is part of `Endpoint::handle`, so it still runs
per request. The Worker also answers 404 itself for paths the pinned SDK cannot route
(fewer than three `/`-separated parts, not `health` or `discover`): on such a path,
once the signature has verified, the SDK computes `parts.len() - 3`, which panics in
debug builds and kills the instance. The native server already built its endpoint
once at startup.

Measured on one 4-core machine with the same debug Wasm build, alternating the old and
new bundle three times (`PAUSE_ONLY` burst, 21 admissions, ~1,060 Worker requests;
two other agents were compiling, load 3-6, so absolute times are above the quiet
2026-11-15 figures):

| | burst | per admission | per request, median (p10) |
|---|---|---|---|
| endpoint per request | 18.7-33.2 s | 0.89-1.58 s | 18.5-33.3 ms (15.0-18.7 ms) |
| endpoint per isolate | 6.0-10.8 s (two of three 6.0-6.5 s) | 0.29-0.52 s | 5.1-11.1 ms (3.6-5.5 ms) |

The p10 gap (about 11.5-14 ms per request) matches the rebuild cost measured above.

The round count is production behaviour, so production has the same per-link
serialization: about 45 times (Restate-to-Worker RTT plus per-request Worker CPU) per
admission. A release build makes the CPU part smaller, but it does not go away. The
fixture is faithful here, so the burst keeps its 90 s client timeout (hosted runners
are slower). What remains is cutting rounds: issue independent state reads together
so that one suspension covers them, and merge the two `admission_*time` runs. Both
change journal shapes and need the usual deployment versioning; not started.

**`authoritative_admission` needs a Restate server per test.** Each of its tests
deploys its own endpoint, with its own fault handles and in-memory storage, under the
same service names. On one shared server that breaks two ways (reproduced on 1.8.1,
2026-11-15):

- Restate routes every new invocation of a service to the latest registered
  deployment. Two tests started together (`abuse_window_…` and `control_replay`,
  `--test-threads=2`) registered 32 ms apart, and `InvitationLink` then pointed at
  the second; the first test's armed fault never ran, so it got `200` where it
  expected `503`. The `load_ordering` timeouts are the same cross-routing.
- Object state outlives the test. Every test onboards `AccountInstallation/repo:100`;
  on a server where that key is already onboarded, `onboard` is a no-op and emits no
  `InstallationProjection/repo:100/apply`, so the new test's empty storage never gets
  the installation/account rows and its link/request projections retry forever.
  `requester_message` therefore passes on a fresh server and fails the second time
  on the same one, even alone (`--test-threads=1` over all ten: 9/10, 318 s).

So the suite uses `Harness::start_owned()` (`ReusePolicy::Never`: it spawns
`RESTATE_SERVER_BIN` per test and ignores `RESTATE_ADMIN_URL`/`RESTATE_INGRESS_URL`),
and `scripts/test-restate.sh authoritative_admission` refuses with the native
command. CI runs it natively: about 190 s on 4 cores at `--test-threads=2`, 10/10 on
1.8.1. To get the binary, copy `/usr/local/bin/restate-server` out of the pinned
image (see `.github/workflows/ci.yml`). The script's other targets are single-test
files, one endpoint per runtime, so sharing is safe for them.

The script compiles the selected integration-test file, creates a uniquely named
Compose project, and publishes admin/ingress on randomly allocated **loopback**
ports. The Rust test serves the current workflow endpoint on `0.0.0.0:0`.
Restate reaches it using `host.docker.internal`, explicitly mapped by Compose
to Docker's `host-gateway` on Linux (also supported by Docker Desktop). No bridge
subnet or fixed IP is assumed. The readiness probe uses cleartext HTTP/2 and the
SDK discovery media type. Successful deployment registration verifies the
container-to-host discovery path before any invocation is sent.

**GitHub is the emulator.** `ghinvite_e2e::Harness::start` (and
`start_owned`, `github_emulator*`) serves an embedded
[octoemu](../libs/octoemu/crates/octoemu/README.md) on `127.0.0.1:0` seeded
with `ghinvite_e2e::github::seed` (`acme` 100, `acme/api` 10, `acme/web` 11,
`creator` 7, `alice` 8, App 123 installed as 1, App 456 as 2). The production
`InstallationClient` and `ReqwestTransport` sign real App JWTs with
`test-fixtures/github-app-rsa.pem`, which the emulator verifies, and use the
stateless `ghs_…` installation tokens it mints. GitHub-side actions go
through the emulator's admin API (`harness.github()`, a
`ghinvite_e2e::github::World`): invitees accept or decline, users rename or
leave, an installation's repositories change, an invitation expires, an
organization renames itself. Answers GitHub would not give on its own (a
refused, throttled, unanswered or lost write, a recycled login, an
installation reported under another account, an outage) are
[fault rules](../libs/octoemu/crates/octoemu/README.md#fault-rules), and
counts come from the emulator's call log (how closely it matches real
GitHub is checked by its fidelity suite, whose live side is set up in
[live-github-tests.md](live-github-tests.md)). Every suite runs on it except
`repository_protocol` and `organization_delivery`, which keep their
scenario fakes (`start_with_github_router`). Their AC-DLV-3 sequence
properties (`tests/common/delivery_sequence.rs`, checked by
`verify-delivery-sequences.py`) script one generated answer per write,
including answers GitHub never gives to that route (a 204 to an
organization invitation POST). `repository_protocol`'s fake also reads
ghinvite's own write fence from SQLite as each request arrives, tells
token generations apart by their fixed names, and empties the
invitation list right after a listing it counts (`presence_listings`):
none of that is GitHub behaviour an emulator could offer. The emulator and the
servers are Tokio tasks owned by the test runtime, so success, assertion
failure, or process termination closes their sockets. No credentials are
injected into this gate.

Each target documents its scenarios: [authoritative admission](admission-v1.md#native-acceptance)
and [request lifecycle](request-lifecycle-v1.md), [retained delivery](delivery-recovery.md#verification),
[installation availability](installation-availability.md), and
[durable projection](admission-v1.md#projection-tests). Focused HTTP
authorization/session tests remain in `console_flow`, `route_smoke`, and `oauth_flow`.

**Boundaries:** local emulator responses are not evidence of live GitHub webhook
availability or delivery. Native SQLx runtime tests are **not D1 adapter
conformance**. Full browser OAuth is outside this gate. No test claims
exactly-once delivery across a GitHub-success/database-failure gap.

#### GitHub outside the Rust suites

The old standalone GitHub stub (`ghinvite-github`'s `test-stub` feature and
`examples/stub.rs`) is gone (GE-09c). Every other process reaches the
`octoemu` emulator through the GitHub base-URL variables (`GITHUB_API_BASE`,
`GITHUB_WEB_BASE`, `GITHUB_AVATAR_BASE`;
[deploy](deploy.md#p1-app-split-configuration)), which non-production roots
accept: `ghinvite-web-server --local-dev`, `ghinvite-workflows-server
--local-unsigned`, and the Workers' `runtime-tests` builds (as Miniflare
bindings). `crates/ghinvite-web/tests/emulator_config.rs` runs web's local
configuration end to end against an embedded emulator that way, and
`tests/browser/emulator-signin.config.mjs` signs a real browser in on the
emulator's authorize page (including the expired-session form bounce, which
the Content-Security-Policy's `form-action` allows because it names the
configured web origin).

Every Worker driver in `tests/worker` starts the emulator binary through
`tests/worker/octoemu.mjs` (`node tests/worker/build-octoemu.mjs` builds it;
`OCTOEMU_BIN` or `OCTOEMU_TARGET_DIR` select another build) and points the
Workers at it with the bindings. The sign-in drivers sign in on its authorize
page; the `admission.mjs` family starts from a native world
(`tests/worker/github-world.mjs`, or the sub-driver's own `seed`) and takes
GitHub-side actions through the admin API (an invitee accepts, a user
renames, an installation's repositories change). Failures are fault rules
(held, stalled or lost writes, injected refusals) and counts come from the
call log. No driver intercepts GitHub's hostnames with Miniflare's
`outboundService`; drivers whose `outboundService` handles Restate pass the
emulator's requests through. `transport.mjs` and `deadlines.mjs` keep their
own upstream (`transport.test`): they test the generic transport's redirect,
size and stall handling, not GitHub.

The native D1 cutover rehearsal (`scripts/verify-native-d1-cutover.sh`,
[native fallback](native-fallback.md)) starts the emulator binary too: the
pinned workflows Worker (a `runtime-tests` build) and the signed
`native-bridge-endpoint` example both reach it through `GITHUB_API_BASE`
(the example requires it and never reaches GitHub), and the provider writes
it checks come from the emulator's call log.

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

### Assembled invitation journey (#122)

`canonical_link` drives authenticated native web routes through real Restate
owners and production GitHub HTTP to signed settlement and recovery. Its owner
and requester sign in through the emulator's OAuth web flow (authorize page,
code exchange, `/user`, revocation), with no canned sign-in answers. The actual
Worker/D1 equivalent is `npm run test:journey --prefix tests/worker`.
See [ownership and evidence](invitation-ownership.md#reproducible-evidence) for
the exact proof surface and [fresh setup](invitation-ownership.md#clean-prelaunch-replacement).

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

- `authoritative_admission` and `request_owner`: initialization and journaled
  decision recovery, final-use concurrency, replay/input conflicts, deadline
  arbitration, retained eligibility verdicts, expiry/readmission, projection
  outage, completed-execution cleanup and lazy retained history.
- `durable_projection`: commit acknowledgement loss and ordered projection
  convergence.
- `retained_delivery`: original manifests and explicit recovery survive execution
  cleanup; confirmed/uncertain GitHub writes are not repeated.
- The `ghinvite-core` storage test suite: stale snapshots cannot regress a
  revoked link.

The runner sets the disposable runtime's cleanup scan to one second
(`RESTATE_PROOF_CLEANUP_INTERVAL`) so retention cleanup is observable; ordinary
smoke/dev runs retain the default hourly scan.

### D1 storage conformance (requires wasm-bindgen CLI)

```bash
npm run test:storage --prefix tests/worker
npm run test:delivery --prefix tests/worker
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

Then browser-check it: `cargo run -p ghinvite-web-server` from the repo root serves
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
