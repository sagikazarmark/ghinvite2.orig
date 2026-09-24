# Continuous recovery checks (#82)

CI exercises the current recovery contract through three separate gates:

| Gate | Command | Evidence |
| --- | --- | --- |
| Native durable projection | `bash scripts/test-restate.sh durable_projection` | Real Restate with SQLx: committed-but-lost acknowledgement, projection invariant repair, and complete historical audit recovery. |
| Installation on actual bindings | `npm run test:installation --prefix tests/worker` | Real Restate ingress, production Wasm handlers and local D1: unavailable installation; incomplete second-page GitHub observation returns retryable 503 without consuming a use; same-attempt recovery; replacement installation; delayed duplicate old uninstall/onboard/repository events; committed installation audit acknowledgement loss and durable replay without duplicate audit. |
| Production-feature packaging | `npm run test:production --prefix tests/worker` | The exact Wrangler build commands using worker-build release packaging, independently of runtime-test fixtures. Both generated entrypoints execute in workerd; web health, authoritative service discovery, and rejection of every current fixture-only route are checked. |

Run `npm ci --prefix tests/worker` first. Missing tools, missing artifacts, failed
builds, unreachable runtimes, failed assertions, and cleanup errors fail the gate;
none are reported as skipped success. CI job configuration can run independently
of the hosted-job execution work in #52; local passes do not prove hosted jobs ran.

## Versions and isolation

- CI Rust **1.92.0**, Node **20.20.2**, wasm-bindgen **0.2.120** (checked against
  `Cargo.lock`), worker-build **0.8.1** (checked at startup).
- worker-build pins Binaryen/wasm-opt **129** and esbuild **0.28.0**. Its default
  bindgen version is overridden with the lockfile-matching CLI. Packaging-changing
  environment overrides are rejected. The fixture builder separately uses
  esbuild **0.27.3**.
- Miniflare **4.20260302.0** / workerd **2026-03-02** via the npm lockfile;
  compatibility date **2024-09-23** and `nodejs_compat` match Wrangler.
- Restate **1.7.9**, pinned by digest in `compose.yaml`; Rust SDK and dependencies
  are pinned in `Cargo.lock`. Packaging uses Python **3.12.9** in CI (local Python
  **3.11+** is needed for `tomllib`); the installation runner also needs Docker
  Compose, Python 3 and host-gateway networking.

The installation target reuses the admission runner's lifecycle: random loopback
Restate ports, a unique disposable Compose project, ephemeral D1/KV, bounded HTTP
calls/convergence (25 seconds), five-minute runtime deadline, signal handling,
and cleanup on success or failure. It controls only GitHub HTTP and the response
after a real D1 audit write. Attaching to the exact interrupted invocation proves
successful recovery, rather than merely observing that the projector released its key.
Public Storage reads verify D1 state; authoritative reads verify replay, uses,
and immutable repository scope. All credentials are synthetic and external Worker
traffic is denied. No persistent developer or production database is opened.

The entire installation command is bounded at 20 minutes, including compilation;
termination signals the process group and allows 35 seconds for cleanup.
Native tests compile before infrastructure starts, with bounded execution and
Compose cleanup. Production builds have a 15-minute deadline per artifact and
the disposable workerd check a 90-second deadline; CI additionally bounds each
job. Ordinary exits dispose workerd; process termination owns ephemeral resources.
SIGKILL/host failure may require manual Compose cleanup using the printed project
name. Generated production artifacts live under each crate's ignored `build/`
directory, separately from the fixture bundles.
Custom build commands run from the repository root, matching the documented
`wrangler deploy --config wrangler/web.toml` and `restate-svc.toml` invocations.
The packaging gate uses that same working directory; entrypoint paths remain
relative to the Wrangler configuration files.
The runtime check anchors Miniflare's module root at each artifact's `build/`
directory and retains `build/worker/shim.mjs` as the entrypoint, including when
invoked from npm's `tests/worker` working directory.

## Evidence limits and ownership

These gates do not authorize production rollout. **#61** still owns live signed
Restate identity/discovery/routing and private ingress validation; remote regional
behavior and Cloud CPU/memory/subrequest/input/scope limits; live D1 restore;
old-deployment/credential isolation; administrative kill/purge and
coordinated/independent restore with forward recovery. Local unsigned discovery and controlled GitHub HTTP are not live-signature
or live-GitHub proof. Static island assets retain their separate CI gate.

Targeted cross-feature fixes and their regressions remain with their owning bug-fix
tickets (the admission gate now covers #119's retained onboarding during projection
outages, replacing #66's obsolete SQL-adoption protocol). This ticket adds continuous recovery coverage and packaging verification;
it does not redefine installation/admission behavior.

Local verification uses the available Nix Rust **1.94.0**, rather than claiming
verification of the CI compiler. See [the Worker admission gate](worker-admission-gate.md)
for the larger runtime matrix and [admission recovery](admission-v1.md#recovery-and-audit-retention)
for operator recovery procedures.

## Local verification on 2026-09-18

- Passed: native `durable_projection` against disposable Restate; new installation
  gate including successful attachment to the interrupted projection; both
  worker-build release artifacts and production endpoint-exclusion checks; Worker
  smoke, decision queue, and header/body deadline checks; workspace typechecking
  and the full default Rust workspace test suite.
- The full Rust suite used two build jobs, debug info disabled, and LLVM's linker
  to fit the local host. Long-running browser fixture tests remain intentionally
  ignored by that default suite; this is not evidence of running Playwright.
- The broader existing Worker admission gate reached its five-minute watchdog
  after settlement checks, before deadline recovery completed. A diagnostic run
  of the baseline admission runner was also inconclusive (timed out). Neither run
  is recorded as a pass; no increase to its watchdog or success-skipping change
  was made. Its disposable infrastructure was removed.
- `cargo fmt --check` reports existing formatting differences in base-revision
  files, including storage, web command, and workflow tests. `git diff --check`
  passes. Hosted CI and the pinned CI Rust compiler were not executed locally.
