# P1 session envelope integration evidence

Local implementation checkpoint, 2026-10-16. This records B0-19's implemented
crypto/storage boundary; it does not declare the complete P1 sign-in milestone or
production gate complete.

Amended 2026-11-15: the keyring is now kekring v1 (anykms#35 redesign,
[`libs/kekring/docs/kekring-format.md`](../libs/kekring/docs/kekring-format.md)).
The contract below is updated; the 2026-10-16 measurements further down were taken
on the earlier format and are kept as history.

## Implemented contract

- `P1_KEYRING` is a required validated kekring v1 descriptor. Native boot parses once;
  Workers publish one immutable `Arc<Keyring>` per isolate, only after successful
  parsing. Failures publish nothing and return startup 500 with a bounded operator
  alert. Successful loads log only the fingerprint.
- Sessions use anykms envelope v1 and canonical AAD v1 of the `Session` purpose from the requested
  session ID. Lifetime metadata remains inside authenticated plaintext; fixed
  30-day/30-minute deadlines and independent 31-day revocation markers are retained.
  Session KV writes use relative TTL bounded by 30 days.
- Continuation payloads use the `AttemptContinuation` purpose from the stored
  scope/binding columns. Shared core SQL returns those columns for both SQLite and
  D1; the schema and first-writer-wins/conflict/Bound/release semantics are unchanged.
- Records wrapped under a generation above the deployment's high-water mark
  (`DecryptFailed` with code `VersionAboveHighWaterMark`) return 503 without
  clearing the browser cookie or changing stored records. Removed generations,
  key ids of other root material (`KeyIdMismatch`) and corrupt sessions confer no
  identity and log their code without an alert. A descriptor whose key ids do not
  match its roots fails startup with 500 and an operator alert. Invitation recovery
  propagates crypto errors instead of treating retained input as absent.
- `/console/keyring` exposes only the fingerprint, with private/no-store caching,
  to authenticated numeric GitHub IDs explicitly listed in `P1_OPERATOR_IDS`.
  Anonymous and nonoperator identities receive 404.
- The app XChaCha dependency and legacy decoder are removed. PoC ciphertext has
  no migration fallback. Explicit test-key constructors build production keyrings;
  deployment entrypoints accept only parsed descriptors.

## Verification completed locally

`CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=2 cargo test -p ghinvite-web --tests`:
**319 passed, zero failed, six existing ignored browser-server fixtures**, across
16 suites. Includes 100 unit tests, four new HTTP/TTL keyring tests, six session
protection tests, 15 OAuth tests, and existing console/invitation recovery suites.

`cargo clippy -p ghinvite-web --tests -- -D warnings`: passed.

`cargo check -p ghinvite-web-worker --target wasm32-unknown-unknown`: passed.
This check is compilation evidence, separate from actual Worker execution.

`scripts/check-keyring-features.sh` (removed with the kekring redesign, which has
no `raw-kek` feature left to guard): passed native and Wasm production dependency
graphs. `Cargo.lock` no longer includes `chacha20poly1305`. Locked package metadata declares
no dependency MSRV above the P1 CI Rust 1.92 version; the local compiler is 1.98.

Native cases cover overlap 1 → 2 + 1, writes under 2, retirement of 1, old-isolate 503,
unchanged ciphertext/cookie on retry, wrong-root rejection, copied session ID rejection,
continuation scope/binding substitution, retained logical binding under rotation,
operator denial, and existing sign-out/storage failure behavior.

## Worker runtime gate

`tests/worker/smoke.mjs` uses the actual Worker entrypoint with KV and D1. It checks
AKE/v1 ciphertext, no plaintext leaks, TTL bounds, cookie-preserving reads, sign-out
failure recovery, and production continuation roundtrip/Bound/substitution behavior
on D1 through a feature-gated helper. It also checks immutable successful keyring
initialization and missing/malformed descriptors failing startup without disclosure.
Executed locally against actual workerd through Miniflare with the generated
Worker Wasm and matching wasm-bindgen 0.2.120: **passed**. The final run used
`node smoke.mjs` after bundling the latest test shim. Observed one fingerprint log
for the successful isolate, a 491-byte opaque session record, unchanged ciphertext
on callback mismatch, sign-out read-failure 503 with cleared cookie and no mutation,
D1 continuation roundtrip/binding recovery/substitution rejection, and 500 for both
missing and malformed cold-start descriptors. The immutable-descriptor override
assertion passed. No outbound requests occurred. No remote deployment was used.

## Remaining milestone gates

B0-12's new token-free sign-in session payload has not been implemented by this
change; existing PoC sign-in payload/lifecycle remains. The anykms envelope review,
full P1 prerequisite acceptance, production-feature Worker packaging, and remote
rollout/staging checks remain separate gates. No remote deployment was performed.
