# Deterministic Link Secret boundary verification

The local AC-SEC-1 entropy-hook acceptance runs production create and rotate
handlers in actual workerd, with actual Restate owners and D1 persistence. The
dedicated `link-secret-fixture` feature replaces only the 32-byte entropy fill
used by `LinkSecret::generate`. Session encryption, CSRF, OAuth state and other
randomness continue using their normal entropy sources.

Ordinary builds call `getrandom::fill` and reject any
`P1_LINK_SECRET_FIXTURE` binding, including an empty value. Dedicated fixture
builds require the exact deployment binding `known-marker-v1`; missing or wrong
configuration fails closed. Request input cannot activate or reset the hook.
The fixture emits 32 ASCII bytes: `LINKSECRET_MARKER_` followed by a 14-digit
decimal sequence starting at one. Repeated Worker configuration does not reset
the sequence. Poisoned state or sequence exhaustion fails without a fallback.
Production packaging rejects either fixture feature in deployment commands.

Run from the workspace, with Docker, the matching wasm-bindgen CLI and the
documented Worker dependencies installed:

```sh
npm run test:link-secret-fixture --prefix tests/worker
```

This builds the normal Worker and the separate `link-secret-worker.mjs` fixture
bundle. It first checks six isolated startup cases: normal build with no binding,
normal build with the preset or empty binding, fixture build without a binding,
fixture build with a wrong binding, and fixture build with the preset. A request
header bearing the preset cannot enable the hook. It then runs the assembled
authenticated journey with forced lost creation, admission and decision
acknowledgements, delayed D1 projections, and delivery recovery.

The test checks that the retained creation hash is SHA-256 of exactly marker
sequence one, despite loss of its show-once response. Rotation must return the
canonical base64url encoding of exactly sequence two, and retain its hash at
generation two. Both full encoded secrets, both decoded 32-byte secrets and the
shared plaintext marker prefix are searched through every raw retained journal
entry, all Restate state, every readable application D1 table, encrypted session
KV, populated authority-cache KV, and complete captured Worker/Restate logs.
Counts are checked against journal/state totals; log overflow fails the test.
D1's inaccessible `_cf_METADATA` platform table is excluded explicitly.

Local evidence on 2026-10-16:

- Normal native entropy tests: 5 passed; fixture-feature entropy tests: 4 passed.
- Actual workerd startup cases: passed.
- Actual assembled deterministic journey: passed, scanning 460 raw journal
  entries, 23 state rows, 27 rows across all 15 application D1 tables, 4 session
  KV rows, 2 authority-cache KV rows, and bounded complete runtime logs.
- Native strict Clippy for the fixture-feature web library: passed.
- Wasm strict Clippy for the fixture-feature Worker: passed.

Logs: `/tmp/p1-secret-entropy-native.log`,
`/tmp/p1-secret-entropy-fixture-native.log`,
`/tmp/p1-secret-entropy-startup.log`,
`/tmp/p1-secret-entropy-journey.log`, and
`/tmp/p1-secret-entropy-clippy.log`, and
`/tmp/p1-secret-entropy-wasm-clippy.log`.

The later [combined credential/journal gate](credential-journal-verification.md)
also passed with current per-target forms and signed workflow requests: three
actual guarded-run failures, one PUT, all 17 discovered public system tables
(890 rows, including 650 raw journal entries), 24 state rows, and the complete
D1/KV/log scans. Seven distinct OAuth/user/installation credential markers were
included, and persisted negative controls verified scanner failure detection.

This closes the local deterministic Link Secret hook gap. Live sign-in/App
configuration acceptance, deployed telemetry verification, token-free sign-in
migration and exhaustive API-error disclosure acceptance remain separate gates.

### Current isolated feature verification (2026-10-17)

Fresh dedicated cancellation and link-secret Worker variants build with the
locked wasm-bindgen 0.2.120, separately from the immutable ordinary 32-mode
capture. The actual cancellation composition passes all seven scenarios and its
budget-cap mode passes. The entropy startup matrix passes both ordinary-build
refusal and exact dedicated-binding acceptance; request headers cannot enable
the hook. The default guarded-run startup matrix also passes.

The dedicated entropy journey passes through actual web/Restate/GitHub-client
paths, three guarded-run failures, one PUT, delayed projections and lost replies.
The discovered system inventory contains 1,563 rows, including 1,253 raw journal
entries; scans cover 33 state rows, 27 rows across all 30 D1 application tables,
four session KV rows, two authority-cache rows and complete runtime logs. The
capability and seven credential markers are absent; persisted BLOB/encoded-text
negative controls are rejected. No deployed or live-provider claim follows.

Exact per-mode exit codes/log hashes: `/tmp/p1-remaining-worker/results.json`.
Generated feature artifact/tool/source hashes:
`/tmp/p1-remaining-worker/feature-manifest.json`. Ordinary capture files remain
unchanged. Production packaging and the complete browser gate are recorded
separately; this is not a whole-CI command claim.

### Final dependency refresh (2026-10-17)

All five supplemental modes pass again against the final default Worker/stub
capture, including the formatted W03 SDK and explicit Wasm std dependency/lock
correction. Cancellation uses a freshly built isolated variant. The dedicated
secret web variant is deliberately reused: it does not depend on the SDK, and
the later sidebar CSS padding is separately verified by the rebuilt production
and browser gates. This exception is recorded rather than presenting that
variant as freshly rebuilt.

The final entropy journey discovers 1,575 system rows, including 1,261 retained
raw journal entries, and scans 33 state rows, 27 rows across all 30 D1 tables,
four session KV rows, two authority-cache KV rows and complete runtime logs.
Capability and seven credential markers remain absent; persisted BLOB and
encoded-text negative controls are rejected. Three actual guarded-run failures
still result in one GitHub PUT, with lost-reply recovery and signed settlement.

`/tmp/p1-worker-final-supplemental-20261017/{manifest,results}.json` retain exact
source/artifact/runner/log hashes, five terminal exit-zero results and successful
post-runtime artifact immutability checks. The manifest binds the parent final
capture SHA-256 `007c3def10ddc9bc9a732f345286bc8347e3f2852fee8e78ea1a2a66533f7d63`.
Earlier results and failures remain preserved. No release, remote deployment or
live-provider acceptance is asserted.
