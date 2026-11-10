# Keyring verification record

Local verification on 2026-10-16, refreshed on 2026-10-17, Rust 1.98.0, Node 22.23.2, x86_64 Linux.
The implementation uses the component spec's default decisions (separate crates,
AES-256-GCM wrapping, separate product trust units). This is source/build evidence,
not an external crypto review or a production deployment approval.

## Persistent native recovery, 2026-10-17

After the execution-environment reset removed the earlier `/tmp` captures, the
seven native gates in `.github/workflows/keyring.yml` were rerun on exact Rust
1.98.0 and Node 22.23.2. Both production and all-target Clippy graphs pass with
warnings denied. The global/std-loaders suite passes, including the million-case
context property test, all seven forbidden-API compile checks, and the actual
Worker loader's native retry/immutable-success test. Raw-KEK conformance passes
with `ANYKMS_FAIL_ON_SKIP=1`: 25 passed, 451 explicitly declared not applicable,
and zero fixture-lacks skips. The separate raw-KEK four tests, CLI two tests, and
independent Node/OpenSSL vectors also pass.

Inspectable evidence is retained at
`.verification/20261017-recovery/keyring/anykms-native/independent-verification.json`
from the repository root. It binds all seven commands/logs, 319 copied source and
compile-fail snapshot inputs, and 15 copied native executables. The original 312
guarded inputs and both the AnyKMS and application lockfiles remained unchanged;
the existing application target was reused with jobs=1 and debug info disabled.
The loader's existing child-process test selects only its own case; the parent
suite is unfiltered. Raw-KEK tests are separately feature-enabled, rather than
claimed from the global/std-loaders configuration in which they are disabled.

The earlier `/tmp` entries below are historical execution records whose captures
were lost, not current inspectable artifacts. External cryptographic review, live
rollout and release gates remain open as described below.

## Persistent Wasm and local Worker recovery, 2026-10-17

The subsequent isolated Wasm build/dependency guard, warnings-denied Clippy check,
and actual Node Wasm acceptance test all pass. The matching official
wasm-bindgen/test-runner 0.2.126 tools are installed separately from the
application's 0.2.120 tools. Exact normal dependency and keyring-feature trees are
retained; the excluded runtime packages and raw-KEK feature are absent. The
254-input Wasm source guard and lockfiles remained unchanged.

The Worker example was freshly built against its own locked Worker 0.8.5 graph.
Actual `wrangler dev --local` passes all six existing requests across two isolates:
three generic invalid-descriptor failures with one error log, then successful
roundtrip/context refusal/fingerprint and two `/keyring` reads matching `akr show`.
Five fixture controls cover late and interleaved split markers, overflow, spawn
errors, failed-action cleanup and wrapper exit before its child-close callback.
The harness now captures raw workerd output directly to files, preserves stdin
and control fd3, and requires Wrangler close, parent-stream EOF and termination
of the actual owned process group before per-stream privacy scans. An internal
callback after Wrangler exit is not required; missing callbacks remain visible
in shutdown receipts. The initial overstrict callback-oracle failure is retained.
No cryptographic or production Rust source changed.

`.verification/20261017-recovery/keyring/anykms-wasm-worker/independent-verification.json`
binds these checks, 264 final runtime inputs, compiled Wasm/JS/CLI artifacts,
official tool hashes, eight raw workerd streams and four fully drained Wrangler
streams. The final runtime used unchanged compiled artifacts after the JS-only
shutdown correction. This proves local execution and captured-log privacy, not
hosted CI, deployed CPU, Secrets Store propagation, rollout, external crypto
review or a package release.

| Gate | Evidence |
|---|---|
| T1, T6 | `tests/keyring.rs`, `tests/raw_kek.rs`; context-bound roundtrip, add/promote/remove, async/sync equivalence, commitment-checked rewrap, first-poll Pending guard |
| T1b | `tests/ui.rs` with default features: raw AAD, inner accessor/Aead, KeyWrap on Keyring, Rewrap on KeyringKek, arbitrary backend, direct KEK construction/provider/raw method, and context Default all fail to compile |
| T2 | `tests/vectors.rs` parser boundaries and leak checks, `anykms-keyring-cli/tests/cli.rs`, native loader tests; dedicated seeded libFuzzer completed 10,000 runs with no crash |
| T3 | `scripts/keyring-vectors.mjs --check` independently reproduces HKDF, KCV, fingerprint, wrapped DEK, full envelopes and context ambiguity vectors using Node/OpenSSL; Rust opens positives and checks negative error classes |
| T4 | `tests/context_properties.rs` completed 1,000,000 cases varying two arbitrary Unicode strings, u64 values, optional presence and insertion order; explicit NUL/delimiter/Unicode ambiguity and 255/256-byte boundary checks |
| T5 | `ANYKMS_FAIL_ON_SKIP=1 cargo test -p anykms-keyring --features raw-kek --test conformance`: 25 passed, 451 declared n/a, no failures. `conformance::declarations` passed. No fixture-lacks skips |
| T7 | `tests/rotation_sqlite.rs`: SQLite conditional write loses to a concurrent application write, completion selection, removed/foreign errors; additional keyring tests cover swapped-slot refusal and lagging descriptor hints. KV TTL is an operator/consumer policy documented in the guide, not enforced by `akr remove` |
| T7b | `tests/raw_kek.rs::full_reencryption_after_descriptor_compromise`: two leaked generations, fresh DEKs checked by unwrap, changed payloads, clean descriptor with only generation 3, every retained old ciphertext rejected as KeyVersionUnusable |
| T8 | `scripts/check-keyring-wasm.sh`: wasm build with RUSTFLAGS unset; production graph excludes tokio/reqwest/rustls/ring/aws-lc; raw-kek absent |
| T9 | `examples/keyring-wasm`: wasm-bindgen-test under Node completed keyring acceptance/rotation/sync execution |
| T10 | `examples/worker`: actual `wrangler dev --local` returned `ok=true`, `wrong_ctx_fails=true`; `/keyring` and response fingerprint equal `akr show` |
| T13 native | `tests/global.rs`: get before installation is Unavailable, same descriptor installation idempotent, different descriptor Configuration. Application-level serde/Restate classification is a consumer gate |
| T14, T15 | Padding lengths 0..=600, padding mismatch, fingerprint changes and canonical descriptor ordering covered by native tests |
| Regression | Core unit tests: 98 passed with tracing/serde enabled; recipes, keyring and CLI native suites plus doctests pass. Strict production graph and strict keyring all-target graph now pass `-D warnings`; see the 2026-10-17 closure below |

The native criterion benchmark for a 520-byte payload measured intervals of
4.218–4.240 µs encrypt, 2.547–2.563 µs decrypt, and 2.417–2.444 µs commitment-checked
rewrap on this host (10 samples, 1-second warmup and measurement). These are native
measurements only. They are not Workers CPU measurements.

Reproduce fuzzing without building unrelated backend fuzz targets:

```
cargo install cargo-fuzz --locked
cargo +nightly fuzz run --fuzz-dir fuzz/keyring keyring_descriptor -- -runs=10000 -max_len=8192
```

The recorded local run used the installed stable compiler with `RUSTC_BOOTSTRAP=1`
for sanitizer instrumentation. CI can use nightly normally. The corpus seed uses
public fixed test material, never a deployed descriptor.

Outstanding outside this local lane: T11/SD-07 live Secrets Store/deploy/rollback
experiments; T12 actual Restate journal-marker scan if `Sealed<T>` is adopted;
consumer-level T13 serialization/error mapping for optional `Sealed<T>`; deployed SP-16
CPU measurements; external cryptographic review; release commit/tag/publication.
The source is integrated in this monorepo and does not have a separate anykms Git
repository in which to cut its own release tag.

## BK local closure, 2026-10-17

The imported pedantic warnings are no longer a blocker for the keyring production
or conformance build graph. Fixes add API error contracts and `must_use` annotations,
make redacted Debug implementations explicitly non-exhaustive, use checked length
conversions, and remove unnecessary allocations/imports. Retry-duration diagnostics
saturate rather than truncate. Public crypto wire formats and backend capabilities
are unchanged; the independent Node/OpenSSL vectors still match byte-for-byte.

There are no crate-wide lint suppressions. Narrow documented exceptions retain the
borrowed-future lifetime in generated trait methods, the independent capability
flags, intentionally ready-on-first-poll async implementations, JWT's three-state
header override, feature-dependent fallibility, and the two explicit conformance
inventories. The async exceptions preserve the existing lazy polling behavior.
Compile-fail snapshots changed only for the compiler's elided dynamic-trait names
and formatted source excerpts; all seven forbidden API programs still fail.

Current checks and session logs:

| Check | Evidence |
|---|---|
| Strict native production graph, including tracing/serde | `/tmp/keyring-production-strict-final2.log`; enabled-Transit facade feature compile also passes `/tmp/keyring-facade-transit-check.log` |
| Strict keyring/CLI all targets, including raw-KEK conformance and benchmarks | `/tmp/keyring-all-target-strict-final.log` |
| Core 98, recipes, keyring, CLI, doctests, million context cases, negative API fixtures | `/tmp/keyring-closure-native3.log`; all passed |
| Current fail-on-skip conformance | `/tmp/keyring-conformance-final2.log`: keyring25 pass/451 declared N/A, local225 pass/251 declared N/A; no fixture-lacks skips |
| Current compile-fail and exact-loader replay | `/tmp/keyring-global-final.log` verifies actual global installation/conflict; `/tmp/keyring-ui-loader-final.log`: all seven API escape fixtures still rejected; same-isolate native loader retry passes |
| Independent canonical/context/full-envelope vectors | `/tmp/keyring-closure-vectors.log` |
| Isolated Wasm build and dependency exclusion | `/tmp/keyring-closure-wasm-graph.log` |
| Isolated Wasm strict and Node execution | `/tmp/keyring-wasm-strict-final.log`, `/tmp/keyring-wasm-tests-final.log`; matching wasm-bindgen 0.2.126 |
| Actual workerd success/failure, fingerprint and diagnostic privacy | `/tmp/keyring-closure-workerd3.log`; three bad-binding requests return generic 500 with one error log; three valid-binding requests log one fingerprint and pass context refusal; 16 MiB capture fails on overflow |
| Real loader retry and immutable success | `tests/worker_loader.rs` uses the exact example loader; failed loads leave global state empty, a corrected descriptor initializes it, later calls do not reread secrets |

`.github/workflows/keyring.yml` now includes both strict native commands and the
isolated strict Wasm consumer. This file is CI wiring, not evidence of a remote CI
run. The tests use public synthetic key material only. No real credentials,
release tag, package publication, external review or deployment was performed.

Reproduce the strict checks from the anykms workspace:

```sh
cargo clippy -p anykms-core -p anykms-recipes -p anykms-keyring \
  -p anykms-keyring-cli -p anykms --no-default-features \
  --features anykms/keyring,anykms/context,anykms/global,anykms/tracing,anykms-core/serde,anykms-keyring/std-loaders,anykms-keyring/raw-kek \
  --lib --bins -- -D warnings
cargo clippy -p anykms-keyring -p anykms-keyring-cli \
  --features anykms-keyring/global,anykms-keyring/std-loaders,anykms-keyring/raw-kek \
  --all-targets -- -D warnings
env -u RUSTFLAGS cargo clippy --manifest-path examples/keyring-wasm/Cargo.toml \
  --target wasm32-unknown-unknown -- -D warnings
```

BK-00–10 and BK-12 have local source/test evidence. BK-11's local Wasm checks and
CI definition are complete but remote CI execution is unverified. BK-13's facade,
registered-purpose guide and strict checks are complete locally; the required
release/tag/publication is still open. T11/SD-07 live binding/rollout behavior,
conditional T12/T13 `Sealed<T>` integration, deployed CPU measurement and external
cryptographic review remain separate gates. Historical missing live evidence is
not replaced by the new loader fixture.
