# Local Integration Testing

## Workspace tests (no Docker/wrangler required)

```bash
cargo test --workspace
```

This excludes the feature-gated real Restate smoke, the opt-in ignored D1 suite,
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
cargo test -p ghinvite-web --test console_flow oauth_signin_discards
cargo build -p ghinvite-web-worker --target wasm32-unknown-unknown
```

Successful OAuth uses tower-sessions' `cycle_id()` to delete the prior session
ID, replaces session data with fresh credentials and empty authorization caches,
and lets the session middleware persist the new record and send its cookie.
Deletion or persistence failure returns HTTP 500 instead of a login redirect.
Validated OAuth state is persisted as consumed before contacting GitHub, so a
failed exchange or user lookup cannot replay it on a consistent store.

Native development uses SQLite; production Workers use **Workers KV** for
sessions (D1 stores domain data). KV is eventually consistent: deletions and
state updates may remain stale in other locations, and a new ID may not yet be
visible everywhere. Correct rotation calls therefore do **not** guarantee
instantaneous worldwide logout or globally atomic OAuth-state consumption.
These HTTP tests use MemoryStore and SQLite, and the Worker build checks target
compatibility; they do not simulate KV propagation.

### Restate integration tests

From the repository root, run the same command used by CI:

```bash
bash scripts/test-restate.sh
```

Prerequisites:

- Rust **1.92.0**, selected by `rust-toolchain.toml`, with Cargo dependencies
  available (`--locked` uses the checked-in lockfile).
- Bash, GNU coreutils `timeout` (on macOS, `brew install coreutils` provides
  `gtimeout`), and Docker Compose v2+.
- A running **local** Docker Engine 20.10+ on Linux, or Docker Desktop on
  macOS/Windows (run Bash on the host that Docker Desktop forwards to). Registry
  access is needed on the first run. Remote Docker daemons and running the test
  inside another container are not supported by this host/bridge topology.
- The container must be able to reach an ephemeral TCP port on the host. Host
  firewalls must allow traffic from the Docker bridge to that test listener.
  No GitHub App credentials, GitHub stub process, Wrangler, or dev stack is needed.

`compose.yaml` pins Restate **1.7.9** by image digest; the dev stack and smoke
service share that pin. CI uses Ubuntu 24.04 and the same Rust version and runner
script. To upgrade Restate, update the shared image pin and rerun this command.

The script compiles the single integration-test file, creates a uniquely named
Compose project, and publishes admin/ingress on randomly allocated **loopback**
ports. The Rust test serves the current workflow endpoint on `0.0.0.0:0`.
Restate reaches it using `host.docker.internal`, explicitly mapped by Compose
to Docker's `host-gateway` on Linux (also supported by Docker Desktop). No bridge
subnet or fixed IP is assumed. The readiness probe uses cleartext HTTP/2 and the
SDK discovery media type. Successful deployment registration verifies the
container-to-host discovery path before any invocation is sent.

The bounded scenario onboards an installation, creates a described invitation
link with one repository, then submits an approval-required invitation request
with a two-second decision window. It waits for the workflow's **expired** result
and reads through the public `Storage` trait to assert the installation, link
description/scope/use count, terminal request/decision timestamp, and four audit
events. GitHub transport rejects calls; invitation request expiration does not
send GitHub invitations.
This exercises native SDK + real Restate + SQLite, not Workers/D1 or GitHub delivery.

Readiness has a 30-second deadline per service; HTTP calls have a 20-second
deadline (3-second connect), and the scenario has a 120-second overall deadline.
The shell bounds compilation (600s), container startup/pull (180s), test execution
(150s), and cleanup (30s), escalating to a kill after a further 5s if a child
ignores termination; CI also has a 20-minute job deadline. Registration
and invocation failures stop immediately, reporting the phase and HTTP status
without printing response bodies, headers, or configured URLs. On failure the
script prints container status and at most 60 log lines from its own disposable
runtime, which has only synthetic fixtures and no injected secrets.

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
