# Worker/D1 admission gate (#59)

## Verdict

**Local actual-binding gate implemented; production rollout remains blocked.**
The maintainer resumed #59 and approved local workerd, real Restate ingress,
the Rust D1 adapter, browser HTTP, and disposable cutover fixtures as test seams.
Local execution is not evidence of Cloudflare regional behavior, deployment
credentials/routing, remote D1 backup/restore, or live GitHub guarantees.
The [continuous recovery gates](recovery-ci.md) add native durable-projection CI,
the actual-binding installation failure matrix, and separate production-feature
worker-build packaging checks. The remote rollout obligations remain in #61.

## Reproduce

From the repository root:

```sh
npm ci --prefix tests/worker
npm run test:admission --prefix tests/worker
```

Requires local Docker/Compose with host-gateway networking, Python 3, Node
20.20.2, Rust with `wasm32-unknown-unknown`, and wasm-bindgen CLI **0.2.120**.
`WASM_BINDGEN=/absolute/path/to/wasm-bindgen` and `CARGO_TARGET_DIR` are supported.
CI selects Rust **1.92.0**; the recorded local run used Nix Rust/Cargo **1.94.0**.
Do not describe that run as verification of the CI compiler.

Pins:

- Restate **1.7.9**, digest
  `sha256:329e32e12059610b681e165161bcd0722d193325b6c893bc46bfec72cd54b595`
  from `compose.yaml`.
- Rust SDK **0.12.0**, Cargo patch pinned to
  `e882e9e04f9ad9942b4fe7f3788999a521dfa336` from the maintainer's
  [upstream PR #128](https://github.com/restatedev/sdk-rust/pull/128).
  Shared core **7.0.3**; `Cargo.lock` pins transitive dependencies.
- worker-rs **0.8.1**, Miniflare **4.20260302.0**, workerd **2026-03-02**,
  esbuild **0.27.3** from the checked-in npm lockfile.
- Compatibility date **2024-09-23**, `nodejs_compat`, matching both Wrangler
  configurations. Wrangler/worker-build is not used by this local runner: it
  builds Rust Wasm, runs matching wasm-bindgen, and bundles its imports directly.

The original SDK 0.10.0 compiled but panicked in `ctx.run`'s native `Instant`.
The patch supplies actual JS clocks and avoids Tokio input-drain timers on Wasm.
Awaited sends now use SDK 0.12's `SendHandle.await`, retaining the durable send
identity barrier. Worker tracing uses the same timer-free console writer as web:
workerd does not implement tracing-wasm's `performance.mark/measure` hooks.

## Fixtures and execution

The runner owns a uniquely named disposable Compose project, ephemeral workerd
D1/KV, an HTTP/2 bridge, and the existing Rust GitHub HTTP stub. Runtime ports are
random; ingress/admin are loopback-only. The bridge listens on the host so Docker
can reach it. Use a trusted local host/network. No `.dev.vars`, App secrets,
production D1 IDs, or production Restate URLs are loaded. Worker outbound requests
are restricted to the stub; browser OAuth uses explicit synthetic responses.

`runtime-tests` exposes only the public Rust Storage operations to the local
harness. Do **not** enable this feature in deployed builds. The production endpoint
and all business handlers are otherwise identical. The fixture shim can lose a
D1 batch acknowledgement after actually committing it; controlled-clock cases
use request-local AsyncLocalStorage. Ordinary cases exercise unmodified JS clocks.
The HTTP observer parses real protocol frame boundaries and can truncate a
response after its first actual state write. It never synthesizes an SDK result.

HTTP calls and convergence are bounded at 25 seconds, runtime execution at five
minutes, CLI import at one minute. Cleanup removes the runner's runtime and local
fixtures. Failure/missing tools is a failed/unrun gate, never a pass. SIGKILL or
host failure may need manual cleanup of the printed unique Compose project.

## Evidence covered

- Actual Wasm discovery and Restate request-response calls; lazy reads and
  suspension; real clock sampling and seven-day deadline.
- Final-use competitors, revoke, original-receipt replay, changed-input conflict,
  and authoritative completion while D1 user parents are missing. Installation
  facts are adopted separately for #60's account availability integration.
- D1 identity-parent restoration followed by durable projection convergence,
  with verification through the Rust Storage read interfaces.
- Fixed-batch reordered/duplicate snapshots, stale snapshot with missing historical
  audit, equal-version conflict rollback including events, event content conflict,
  and successful-commit/lost-ack replay.
- First-state-write response loss: exclusive status waits; replay preserves the
  complete accepted decision even when the clock advances beyond link expiration.
- Lazy-read interruption before decision: resumption samples the later clock and
  rejects expiration, rather than reusing an early receipt-time sample.
- Exact deadline equality, overdue expiry/readmission, original replay and no use
  refund, direct notification before startup, late startup, and real durable timer.
  Timer evidence requires an observed sleep/suspension for that workflow and a
  still-pending authoritative read before expiry.
- Auto/manual approval, retained dispatch and receiving create outcomes using real
  Worker outbound HTTP to the controlled GitHub stub.
- Real web Worker OAuth/session and authorized invitation status while D1 has no
  link/request projections; admin-only note remains private.
- #66 acknowledged repository webhook across a real D1 installation-read outage:
  duplicate and never-adopted events are retained, synchronous observation still
  fails promptly, and restoration alone converges the projected scope. The
  pre-existing command state is seeded through Restate's admin state API.
- #58 checkpoint preparation, projection adoption archive loaded into D1,
  actual Worker import/verification/activation and identical resume, historical
  pending/approved/cancelled/expired records, confirmed Sent identity, late old SQL
  revoke/decision fences, and rejection of stale rollback after new admission.

On 2026-09-14 the local lazy-history experiment retained fourteen 16-KiB
rejected operations. A fresh rejection before and after unrelated history used
**7 Worker round trips**, about **34,956 bytes** maximum request and **17,294 bytes**
maximum response. Largest observed state-write frame payload: **16,669 bytes**;
largest observed message: **17,241 bytes**. These are measured fixtures, not maximum
production capacity or billing estimates. The runner prints measurements each run.

## Remaining rollout gates / fault-model limits

1. Deploy the production-feature artifacts to disposable remote Workers/D1 with
   private Restate ingress and identity signing. Verify actual signatures,
   deployment routing, service discovery, runtime compatibility, regional lag,
   limits, CPU/memory/subrequest budgets and maximum repository/input envelopes.
   This session does not provision or verify remote Cloudflare infrastructure.
2. #58 CLI still uses a **local SQLite checkpoint/progress ledger**. This rehearsal
   loads its adopted archive into D1 and runs import against D1-backed Worker
   handlers. It does not implement a live D1 coordinated export/fence/adoption
   transaction or validate Cloud restore. That operator path must be implemented
   and rehearsed before rollout; do not point `--database` at a workerd internal DB.
3. Truncation verifies ordinary journal replay across a transport boundary, not
   arbitrary isolate termination at every individual instruction. The local D1
   ack-loss wrapper does not inject a remote storage-engine failure during commit.
   Administrative kill/purge and independent checkpoint restores still require the
   reconciliation procedure in `admission-cutover.md`.
4. SQL fences and tombstone routing do not revoke old endpoints' network access or
   fence an already-issued GitHub request. Remote old deployment URLs, controllers,
   database access and outbound credentials must be permanently isolated; verify
   this operationally. Do not reopen old pinned URLs with incompatible SDK code.
5. This SDK upgrade is verified on fresh disposable runtime state. It is not a
   claim that in-flight 0.10 journals can be hot-swapped to 0.12 code. Inventory,
   preserve pinned artifacts, drain/fence and transfer under the cutover procedure.
6. GitHub stub results only establish the consumed fixture contract. They make no
   live GitHub exactly-once, authorization or webhook availability guarantee.

#59 is closed for its completed local gate; the maintainer moved these deferred
production-target checks to #61. Keep the blocked rollout verdict until #61 has
reproducible results and an explicit affirmative verdict. Use the
[deployment guide](deploy.md) for remote migrations, immutable endpoint registration
and readiness ordering.

## Verification commands

The local gate passed on 2026-09-14 using the versions above. SDK-upgrade native
regressions also passed through `scripts/test-restate.sh` with targets
`integration_test`, `authoritative_admission`, `retained_delivery`, and
`writer_cutover`. These native results complement rather than replace the Worker
gate. Standard checks passed: `cargo check --workspace`,
`cargo clippy --workspace -- -D warnings`, `cargo fmt --check`, and
`cargo test --workspace --locked`. The separate Worker session smoke and five
Python migration tests also passed. A SIGTERM rehearsal while the GitHub stub
was running exited 143 through the shared cleanup path.
