# Credential and journal verification

The assembled Worker journey scans every discovered public Restate `sys_*`
table after three real failures of `guarded_github_create`'s `ctx.run` closure.
It also checks all Restate state, every readable application D1 table, encrypted
session KV, populated authority-cache KV, and complete bounded Worker/Restate
logs. The fixture uses actual web and workflow Wasm, signed endpoint requests,
pinned Docker Restate 1.8.1, and D1. GitHub/OAuth responses are controlled HTTP
fixtures.

Seven unique per-run markers cover the creator and requester OAuth codes,
their exchanged `ghu_` tokens, the web authority installation token, and the
workflow's read and write installation tokens. Each token must actually
authorize the expected production HTTP call. Codes differ from legitimate
user logins, avoiding false positives in ordinary identity records. Neither
credentials nor marker values are printed by the scanner.

The deterministic Link Secret mode also verifies the exact creation and
rotation entropy and stored hashes. It scans both encoded secrets, both raw
32-byte secrets, and their common marker prefix, including the creation secret
whose show-once acknowledgement was lost.

## Forced retry and bounds

Only the `runtime-tests` workflow feature contains the run-result fault hook.
Its exact deployment binding, `P1_DELIVERY_RUN_RETRY_FIXTURE=three-losses-v1`,
arms three failures after a confirmed GitHub result or positive recovery. The
isolate counter is never reset by repeated request bootstrap. Request input
cannot arm it. With no binding, ordinary runtime tests have no fault; invalid
bindings fail startup. A build without `runtime-tests` rejects any such binding.

The journey checks the counter is exactly three, replays the immutable
confirmed receipt, and verifies exactly one GitHub PUT. It then performs the
full scans. This is a real run-closure retry proof, in addition to the lost
create/admit/decide ingress acknowledgements already exercised by the journey.

`SHOW TABLES` discovers the public system inventory. Each identifier is
validated, each table is queried with `LIMIT 100001`, and more than 100000 rows
or a mismatch with `COUNT(*)` fails the gate. No discovered system table is
silently skipped. Hex, base64 and byte-array representations are decoded before
matching. A separate complete raw-journal scan remains in place. D1 tables use
the same explicit row bounds and count checks; only inaccessible platform
metadata `_cf_METADATA` is excluded. Log capture fails on its 16 MiB bound.

Two runtime negative controls insert a known marker into an isolated D1 table,
first as a BLOB and then as base64 text. The same discovered-table scanner must
fail in both cases. The fixture removes the table before the positive absence
scan. Three Node tests additionally prove detection in a newly discovered
system table, all supported binary encodings, and fail-closed inventory,
identifier and truncation checks.

Run the complete deterministic gate using the dependencies documented for the
Worker suite:

```sh
npm run test:link-secret-fixture --prefix tests/worker
```

## Local evidence, 2026-10-16

Both current-bundle journeys passed with three real guarded-run failures and
exactly one PUT. The normal mode scanned 879 rows across all 17 discovered
public system tables, including 643 raw journal entries. Deterministic mode
scanned 890 system rows, including 650 raw journal entries. Both also scanned
24 state rows, 27 rows across all 15 application D1 tables, 4 session KV records,
2 authority-cache records, and complete bounded logs. All seven credential
markers were absent. Both persisted negative controls were detected and removed.

The runtime startup matrix passed, as did the default-build rejection unit test,
the bounded/reconfiguration fixture unit test, all three scanner tests, native
strict Clippy for the fixture-feature workflow library, and strict wasm Clippy
for the workflow Worker. Actual production-packaging startup refusal is owned
by the separate production Worker gate.

Evidence logs:

- `/tmp/p1-b020-run-retry-journey.log`
- `/tmp/p1-b020-run-retry-entropy-journey.log`
- `/tmp/p1-b020-run-startup.log`
- `/tmp/p1-b020-run-guard-native.log`
- `/tmp/p1-b020-run-guard-fixture.log`
- `/tmp/p1-b020-scanner-tests.log`
- `/tmp/p1-b020-run-guard-clippy.log`
- `/tmp/p1-b020-run-guard-wasm-clippy.log`

This does not establish B0-12's token-free production sign-in migration, live
SP-19a acceptance, or marker redaction for every possible GitHub API error
response. The session scan checks raw encrypted persistence; it does not claim
that the transitional sign-in implementation never stores a user token inside
an encrypted session.
