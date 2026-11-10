# Native fallback artifact and switch readiness

Build-plan §7 and ADR0010 decisions3–4 require native binaries from the same
release source and a recovery procedure. The `native-fallback` CI job builds both
`ghinvite-workflows` and `ghinvite-web` in release mode, bundles current island
assets, migrations, configuration reference, documentation and available package
license files, and uploads a checkout-SHA-named artifact. `manifest.json` binds
that SHA, Cargo.lock, compiler identity, exact build command and every payload
file hash. `dependency-licenses.json` records Cargo's declared licenses; copied
license files retain their package origin. This inventory is not a legal review
or a claim that every dependency provides license text.

The job runs for PR/main source; it does not create a release or publish a package.
For a future release, require the artifact SHA to equal the selected release
commit and retain it outside CI's finite retention period. A PR merge-ref artifact
is not evidence for a different final merge commit. Verify every manifest file
hash before use. CI registration is not evidence that hosted CI has run.

## Current hard boundary

The native workflow server has an explicit `GHINVITE_STORAGE=d1-bridge` mode.
It accesses the retained web Worker's existing D1 authority through a private
binding bridge. The default `local-sqlite` mode remains local development only.
An exported SQLite copy is never a live fallback authority: fences, budgets,
mailboxes and receipts must remain shared with delayed/pinned Worker producers.

Required native settings are `GHINVITE_D1_BRIDGE_URL` (HTTPS `/v1/d1`),
`GHINVITE_D1_BRIDGE_TOKEN` (32 random bytes as lowercase hex), and
`GHINVITE_D1_AUTHORITY_MANIFEST` (path to reviewed `authority.json`). Bridge mode
requires signed Restate ingress and rejects `GHINVITE_DATABASE_PATH`. Its startup
readiness checks are authenticated and read-only. They compare exact database
identity, migration source hashes, existing Wrangler ledger names and canonical
schema digest; there is no DDL or ledger-adoption operation. Unknown transport
outcomes are not retried by the bridge; the original owner fences govern retries.

The bridge deployment binds the original database ID, has restricted private
HTTPS ingress, and provisions its own secret without logs. Its manifest generator
is `native-d1-bridge/manifest.py`. Do not treat the manifest as proof that an
operator correctly bound the database: independently verify the deployment's D1
binding and historical migration ledger. Freeze migrations while old and new
producers coexist. Manifest/schema readiness does not lock out an independently
authorized concurrent DDL operator.

This code is a local continuity implementation, not deployment approval. Require
the native bridge storage gate, signed real Restate endpoint/pinned-producer
rehearsal, operational TLS/access controls and selected release artifacts before
a switch. Local Miniflare validates the actual binding/transaction path; remote
primary semantics rely on the documented D1 binding `first-primary` contract and
must be checked on the selected deployed authority.

The packaged native web binary is a buildable companion, not an approved way
around that boundary. It uses in-memory sessions, authority cache and rate limits;
its deployment would change ADR0010's web-host decision and needs separate
session, multi-instance and edge-protection design. The fallback platform is
intentionally unselected. Do not point the native endpoint at an empty database,
use `--local-unsigned`, migrate live state automatically, or open native ingress
based on a successful `/health` response.

## Operator decision and quiescence checklist

No remote actions below have been performed by this documentation change.

1. Record the actual switch trigger: CPU/subrequest exhaustion, endpoint signature
   or routing failure, JWT CPU threshold, or replay-cost trend (ADR0010 decision4).
   Preserve deployment IDs, source/artifact hashes and redacted measurements.
2. Validate the selected private D1 bridge against the retained database and
   unchanged migration ledger. Run actual native/Worker continuity gates covering
   atomicity, budgets, fences, credentials, latency and committed response loss.
   An environment variable alone is not sufficient readiness evidence.
3. Close public writes, webhooks, scheduler and delayed producers; account for
   alternate Worker URLs and all pinned invocations. Pausing ingress alone does
   not stop already-issued GitHub calls or durable timers. Inventory unresolved
   effects and retained receipts; preserve old compatible endpoints for recovery.
4. Establish an acknowledged no-writer boundary, including native/Worker readers
   that can mutate continuation/session state. Take a coordinated Restate-state
   and database recovery point. An independent D1 snapshot is insufficient.
5. Preserve the same database identity and Wrangler ledger. Record every table
   and receipt/fence digest at the recovery point, encrypted mailbox contexts and
   outstanding effects. No export/import, SQLx migration-ledger synthesis or
   automatic DDL is part of this fallback. A future whole-system authority
   migration is a separate design and cannot be inferred from this bridge.
6. Rehearse lost ACK, replay, pending timers, blocked/unknown effects and pinned old
   endpoints against the same retained authority with stubbed outbound effects. Verify
   original IDs/deadlines, exactly retained receipts and no duplicate write. Then
   perform separately authorized restricted remote readiness checks with signature
   verification, host TLS/network controls, exact capability flags and secrets.
7. Register a new immutable endpoint only after these prerequisites pass. Keep old
   pinned endpoints reachable. Open new traffic only after all participants use
   the same validated authority and readiness is explicitly approved.

## Rollback boundary

Before opening traffic, the disposable destination can be abandoned while the
unchanged original recovery point remains authoritative. After any new write,
**do not switch back to the old D1 snapshot or old SQLite copy**. Close producers
again, classify issued effects and uncertain fences, and perform reviewed forward
repair or reverse reconciliation of the complete coordinated state. Preserve
historical audit/decision bytes and endpoint pinning; never reset a write fence to
make a retry succeed. Keyring rollback must retain the promoted current key.

See [deployment ordering](deploy.md), [migration compatibility](../migrations/README.md),
[delivery recovery](delivery-recovery.md), and [installation availability](installation-availability.md).
The actual native production switch remains gated by the selected deployment,
signed endpoint/pinned-invocation rehearsal and independently verified identity.
A successful artifact build or `/health` response does not close those gates.

The native package includes only `native-d1-bridge/{bridge.mjs,worker.mjs,manifest.py}`.
It does not include a generated authority manifest or bearer credential. Run the
packaged `manifest.py` with the independently reviewed database UUID, schema hash,
and packaged migration directory into a new private output directory. The generator
copies `worker.mjs` and `bridge.mjs` next to `authority.json` and `wrangler.json`, so
`main: worker.mjs` resolves relative to that emitted config. Supply the bridge token
through the host secret manager; configuration generation performs no deployment
or database write. Native POST transport disables automatic retries, redirects,
and proxy discovery; uncertain writes return to the existing retained owner protocol.

The bridge bounds each request and response at 32 MiB. This accommodates the
continuation rotation page's five ciphertexts of at most 4 MiB each, and CAS
carrying both old and replacement ciphertext, for the sealed ASCII/base64 wire
format. Arbitrary malformed TEXT with heavy JSON escaping can exceed this bound;
transport failure remains an error and cannot establish successful rotation or
zero-old completion. The bridge is not an unrestricted bulk SQL export API.

D1's documented maximum string/BLOB/row size is 2,000,000 bytes
(<https://developers.cloudflare.com/d1/platform/limits/>), below the generic
rotation parser's 4 MiB ceiling. The actual binding test preserves an attempted
near-4 MiB rejection and proves unchanged epoch, then exercises five 1,999,000-byte
rows and a same-size replacement with unchanged expiry and exact CAS receipt.
The 32 MiB transport limit does not enlarge the database's own limits.

## Reproduce the local same-authority cutover gate

Use Node 22.23.2, Rust 1.98.0 with `wasm32-unknown-unknown`, and
`wasm-bindgen-cli` 0.2.120. Install the pinned Node dependencies with
`npm ci --prefix tests/worker`, from the repository root.
Extract the official Restate 1.8.1 executable from the same pinned image used in CI:

```sh
fixture_container=$(docker create docker.restate.dev/restatedev/restate:1.8.1@sha256:fea6b4a893616fd8ec4594bd72fbf9967f9ab9242c8c75008de75edfbb58b2c7)
trap 'docker rm -f "$fixture_container" >/dev/null 2>&1 || true' EXIT
docker cp "$fixture_container:/usr/local/bin/restate-server" /tmp/restate-server-1.8.1
chmod +x /tmp/restate-server-1.8.1
printf '%s  %s\n' cb4c0890a347d3193d9ab5e04e37c83d555ebdb2d42ff24e5f447e4dcaeaeb0c /tmp/restate-server-1.8.1 | sha256sum --check
CARGO_BUILD_JOBS=1 CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 \
  RESTATE_SERVER_BIN=/tmp/restate-server-1.8.1 \
  EVIDENCE_ROOT=dist/native-d1-cutover-evidence \
  bash scripts/verify-native-d1-cutover.sh
```

The evidence directory must be new. The helper builds the actual workflow Worker
and selects the exact native endpoint and GitHub stub examples from Cargo JSON,
then copies their runtime artifacts before starting owned loopback processes. Its
underlying build commands are:

```sh
cargo build --locked -p ghinvite-workflows-server --features bridge-fixture \
  --example native-bridge-endpoint --message-format=json
cargo build --locked -p ghinvite-github --features test-stub --example stub --message-format=json
node tests/worker/build.mjs workflows
```

The helper executes `node tests/native-d1-bridge/cutover.mjs` with
`RESTATE_SERVER_BIN`, `NATIVE_BRIDGE_ENDPOINT_BIN`, `GITHUB_STUB_BIN`,
`WORKFLOW_WORKER_MODULE` and `EVIDENCE_ROOT`, and verifies the resulting raw rows
with `python3 scripts/verify-native-d1-cutover.py "$EVIDENCE_ROOT" --self-test`.
The `native-d1-cutover` job in P1 CI runs this gate; hosted execution is not claimed
by local results. The separate `verify-native-d1-bridge.sh` gate exercises all
53 shared storage scenarios, five native adapter tests and 11 startup tests.

The current local cutover proof keeps an unfinished invocation pinned to the
original Worker deployment after native endpoint registration, drops its response
ACK, recovers the exact original receipt without a second provider write, and
runs a fresh native delivery through the real stub HTTP path. There is one PUT
for each of the two deliveries, retained generation-one fences, an unchanged
Wrangler ledger, and the same D1 binding and budget authority throughout.
Per-request test proxies add signatures checked by the production endpoint
verifiers; this does not demonstrate Restate Cloud generating identity signatures.
The native fixture differs from the production root only in its explanatory
header, explicit loopback GitHub URL and test-only local bridge constructor.

The final local run retained 848 journal rows, 56 state rows, 75 invocation rows,
32 D1 tables and eight complete EOF output streams. Each Restate inventory uses a
same-query window count to reject truncation without racing a separate count
query. Runtime checks scan actual provisioned capabilities, bridge credentials,
key forms and minted tokens, including decoded journal/state values, before
writing a passing report. The independent verifier correlates pinning, provider
calls, raw fences, budget, ledger and child close receipts and rejects nine altered
proofs. These owned-local results do not authorize a remote cutover, prove host
TLS configuration or replace the producer inventory and recovery procedure above.
