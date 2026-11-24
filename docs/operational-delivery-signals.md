# Delivery operational signals

### Bulk and email origins

Migration `0020_bulk_operational_signals.sql` extends the complete operational
view and immutable daily digest to linkless Bulk requests and email deliveries.
Repository, organization and team rows derive their account from their retained
link or batch; missing parent evidence still fails the report. Email rows retain
their own numeric account, batch and row, with no manufactured request or link.
The report includes independent email counts and fixed blocked reasons without
mailbox addresses or vault references. Each logical email row has one delivery
identity; a retried write attempt does not create a new delivery generation.

Repository and organization telemetry accepts a batch-derived account under the
same exact request/target/generation rules as linked work. It cannot alter the
create receipt, revision, settlement or audit. An account administrator's daily
digest includes its Bulk and email steps. A repository delegate sees a Bulk
step only when the complete immutable manifest contains repositories alone and
the delegate administers every one; organization-bearing batches and email
steps remain outside that scope. A later projection cannot change a frozen
daily snapshot. Existing snapshots are preserved by the migration.

The operator HTML and JSON show batch/row provenance and omit nonexistent
request/link fields. Unknown email time remains explicitly incomplete when no
owner-retained timestamp exists; approval time never supplies uncertainty onset.
Verification (2026-10-17): all47 shared SQLite scenarios pass
(`/tmp/m2-bulk-operations-native-http4.log`), protected operator HTML/JSON5 pass
(`/tmp/m2-bulk-operations-http5.log`), and all47 scenarios plus direct SQL origin
guards pass on actual D1 (`/tmp/m2-bulk-operations-d1.log`). These checks cover
missing/wrong origins, account isolation, complete-manifest delegation, immutable
daily snapshots, address-reference exclusion and report/snapshot overflow.
Local evidence does not establish deployed monitoring or alerts.

P1 §14.2 delivery signals use two independent nullable projection columns:
`deliveries.unknown_since` and `deliveries.last_observed_at`. Fresh migration
`0001_initial.sql` creates them. This repository starts from fresh migrations;
this is not an upgrade migration for an existing deployment.

`DeliveryStorage::project_repository_telemetry` accepts a `RepositoryTelemetry`
bound to the canonical request/target/generation key and numeric account. The
projection must already exist. Missing rows return `ProjectionDependency` so the
same-key projector can retry after its predecessor; account/kind/identity or
chronology conflicts return `ProjectionInvariant`. IDs must fit the common
SQLite/D1 exact-positive-integer range. Timestamps cannot precede the delivery's
creation time, including nanosecond differences.

An atomic SQL transaction/batch validates the input before updating only the two
telemetry columns. Unknown time is immutable once established; identical replay
or a null input preserves it. Observation time is the maximum seen so stale
projections cannot regress it. Null inputs never clear either field. Telemetry
cannot change the retained receipt, projection revision, create outcome,
settlement, tracking stop or audit facts. UTC timestamps are stored with fixed
nanosecond precision so SQLite text comparisons do not lose submillisecond
boundaries.

`OperationalReadStorage::delivery_signals(now)` returns every projected
repository, organization and team step's aggregate contribution, globally and
grouped by numeric account. The core owns the query, decode and aggregation for
both adapters. A complete report accepts at most 10,000 steps; the query fetches
one extra row to detect overflow and then fails the read. It never publishes a
truncated count. Missing parent identities or undecodable rows likewise fail the
read rather than producing an apparently complete report.

- `unknown` counts current `OutcomeUnknown` rows using only `unknown_since`.
  Strictly more than one hour sets `over_threshold`; approval time is never a
  substitute for missing unknown time.
- `pending` counts `Created` and `AlreadyPending` rows with no terminal settlement
  or tracking stop. Its observation baseline is `last_observed_at`, falling back
  to the original `sent_at`. Strictly more than two hours is stalled at the
  specified hourly sweep cadence.
- Missing and future timestamps increment explicit counters, contribute no
  fabricated age, and make `DeliverySignals::incomplete()` true. The report must
  not render incomplete evidence as healthy.
- `blocked` groups current blocked steps by explicit kind and typed operational `BlockedReason`.

The report is operator information, not an account-authorization decision. The
web layer must enforce its operator guard; possession of this storage read port
does not grant authority to mutate links or deliveries.

The shared conformance scenario covers replay, stale/null updates, wrong account,
invalid IDs, missing parent/key, immutable unknown time, exact threshold edges,
future/missing evidence, terminal and stopped foreign invitations, account
partitioning and unchanged receipts. Native SQLite and actual D1 additionally
mutate a fixture row's kind to prove repository telemetry refuses another kind,
and compare every column before/after an accepted observation update.

Local verification (2026-10-16):

- `cargo test -p ghinvite-storage-sqlx --test sqlx_suite`: passed all 27 registered
  shared scenarios, including repository telemetry.
- `cargo test -p ghinvite-storage-sqlx --lib operational_tests`: passed the raw-row
  kind guard and unchanged-column assertion.
- `cargo clippy -p ghinvite-core -p ghinvite-storage-sqlx --lib --tests -- -D warnings`:
  passed.
- A fresh `node tests/worker/build.mjs workflows` followed by
  `node tests/worker/storage-suite.mjs`: all 27 scenarios passed on actual D1,
  along with raw-row kind refusal and unchanged-column proof. The suite's
  inventory may grow independently as later budget scenarios land.
- `cargo clippy -p ghinvite-workflows-worker --lib --target wasm32-unknown-unknown --features runtime-tests -- -D warnings`:
  passed, including the D1 adapter and workflow telemetry handler.

### Operator report and workflow evidence

Signed-in users whose numeric GitHub ID is in `P1_OPERATOR_IDS` can read
`/console/operations`. `/console/operations.json` returns the same typed aggregate
and per-account report, adding an explicit `incomplete` flag. Anonymous users and
non-operators receive a concealed404 before storage is read. Every response,
including failure, uses `private, no-store`. This access grants no account Console
mutation authority.

The HTML report shows the timestamp counters, threshold alerts and blocked
reasons with checks for installation permissions, repository selection, identity,
organization policy or credential problems. An empty projection explicitly
provides no health evidence. A storage failure returns503, never a zero-count
report. Projection lag can hide newer retained facts even when all available
timestamps are valid. Inspect owner state and reconciler progress before acting;
an unknown result does not authorize a replacement GitHub write.

- Protected-session and actual SQLite HTTP tests:3/3 pass
  (`/tmp/platform-operations-final-tests.log`). They cover authorized HTML/JSON,
  per-account counts, incomplete/future timestamps and blocked hints; anonymous
  and non-operator concealment; no-store; storage outage and empty projections.
- Strict web all-target Clippy and production web Worker wasm Clippy pass
  (`/tmp/platform-operations-clippy2.log`, `/tmp/platform-operations-wasm-clippy.log`).
- Actual Restate1.8 workflow sweep tests:3 pass in6.06s
  (`/tmp/p1-operational-runtime.log`), including owner independence during a
  projection outage and eventual convergence, immutable uncertainty onset across
  retries, monotonic observation-only updates without receipt/revision changes,
  stale/wrong-upstream/incomplete evidence ignored, and exactly one GitHub PUT.
  Focused workflow unit tests:2 pass (`/tmp/p1-operational-unit2.log`).

The operator report is on demand. The daily account-scoped snapshot storage is
described below; it does not send external notifications. Remaining §14.2 mint,
webhook, rate-limit, budget and latency monitoring is outside this slice. No live
GitHub or deployed Cloudflare evidence is claimed.


Observation ordering also guards settlement. A no-change listing keeps the same
receipt revision, but its newer observation time rejects an older same-revision
absence before either telemetry or settlement is applied. Equal observation
timestamps remain admissible without advancing the watermark. Confirmation time
is the lower bound when it exists. The real Restate regression failed with an
incorrect Declined settlement before this guard and passes after it
(`/tmp/p1-operational-ordering-negative.log`,
`/tmp/p1-operational-ordering-positive.log`). The positive run also walks all40
fixed named-run paths across61 real journals and15 discovered handlers. These
paths include first-unknown create/recheck clocks and rejected-generation release.

The final actual Restate1.7.9/workerd/D1 sweep also passes after that ordering
fix (`/tmp/p1-operational-workerd-final.log`). It checks a same-revision newer
listing followed by stale absence, queues observation telemetry during a D1
projection outage, verifies both accepted receipts after recovery, and confirms
an unknown receipt retains its first timestamp across read-only retry with one
PUT. The rebuilt fixture also passes all28 shared D1 scenarios including the
new budget projection (`/tmp/invite-budget-d1-shared.log`). Native approved
parent-outage and retained replay suites pass after the guard (16.10s and21.55s;
`/tmp/p1-operational-approved-regression.log`,
`/tmp/p1-operational-retained-regression.log`). Final focused workflow unit2 and
strict native checks pass (`/tmp/p1-operational-unit-final.log`,
`/tmp/p1-operational-workflows-clippy-final.log`).


### Daily blocked-delivery snapshot storage

Migration `0004_blocked_digest.sql` adds a UTC-day header and immutable delivery
items. `DailyDigestStorage::record_blocked_digest(now)` atomically freezes the
first successful observation of each day from projected repository, organization and team steps
that are Blocked, without terminal settlement or stopped tracking. It records
the actual observation time and changes no delivery, receipt, revision or audit
row. Same-day retries do not add items from later projections. A failed item
insert rolls back the header as well as retention cleanup. Older/backward calls
cannot backfill a day or regress the latest snapshot. The latest recorded UTC
day and its six predecessors are retained; deleting a header removes its items.

`ConsoleStorage::scoped_blocked_digest(scope, requested_day)` reads the newest
available snapshot at or before that day in one statement. Its `None` result
means missing evidence; `Some` with empty counts means no blocked deliveries
visible in that scope at the recorded observation. `BlockedDigest::stale_for`
marks an older observation explicitly. Counts are grouped only after account
and all-target repository authorization predicates run. A delegate sees no part
of a multi-repository link unless all its targets are authorized; organization
or team targets exclude that link from delegated scope. Revocation does not hide
historical blocked work. No global counts are returned to account readers.

The Console view applies current authorization to immutable historical items;
it does not use the historical observation to grant access. As with other SQL
views, projection lag can omit newer retained facts. A daily zero is not a claim
that no blocked delivery has appeared since its observation.

Verification (2026-10-16):

- Full shared native suite:29 scenarios pass, including daily snapshot scope,
  frozen retry, stale/missing/healthy-zero distinctions, day rollover, seven-day
  cleanup, clock rollback and retained receipt preservation
  (`/tmp/p1-blocked-digest-native3.log`).
- SQLite trigger-induced failure proves header/item rollback, retention safety,
  same-day no reinsertion and unchanged delivery columns; inconsistent imported
  lifecycle rows cannot inflate the counts
  (`/tmp/p1-blocked-digest-transaction2.log`).
- Fresh workflow Worker build and all29 actual D1 shared scenarios pass. D1
  trigger-induced rollback, frozen retry, retention and every-delivery-column
  equality pass too (`/tmp/p1-blocked-digest-worker-build.log`,
  `/tmp/p1-blocked-digest-d1.log`).
- Strict native core, SQLx, toolkit and workflows lib/test checks and actual
  workflow Worker wasm runtime-tests check pass
  (`/tmp/p1-blocked-digest-native-clippy.log`,
  `/tmp/p1-blocked-digest-wasm-clippy.log`).

These storage proofs do not claim a deployed scheduled trigger or an external
alert delivery. Scheduler and Console route acceptance are separate boundaries.


### Scheduled recording and Console presentation

The web Worker's exported `scheduled` handler uses actual processing UTC time.
The digest reads only the DB binding; the separate sweep watchdog (#86, [ADR 0012](adr/0012-retained-sweep-chains-with-an-hourly-watchdog.md))
is its only ingress traffic. Wrangler configures `5 * * * *`: the first successful
run records the day, and later runs preserve that snapshot. A SQL failure
rejects the scheduled event and emits the fixed `daily_delivery_digest_failed`
diagnostic. It never reports a successful empty observation after an error.
A delayed event's scheduled timestamp cannot backdate present database contents.

The native server records immediately after binding its listener, then at each
hour's minute five. Failures retry after 60 seconds. The task ends when the
server does. Deployments running both paths safely share the same atomic daily
claim. Apply all migrations, including 0013, before starting either path.

Account overview pages use current Console authorization before reading counts.
They show observation time and separate missing, stale and unavailable states.
A failed read returns HTTP500 while preserving other overview sections. A
recorded scoped zero says only that the observed daily snapshot contains no
visible blocked work. This is the in-Console digest; no external messages are
sent, and automated alert delivery remains open.

Local acceptance includes the actual exported Worker scheduled event with a
historical scheduled timestamp, unchanged delivery columns, frozen retries,
trigger-induced transaction failure/recovery, no outbound calls and a complete
bounded diagnostic capture (`/tmp/p1-digest-scheduled.log`). The test uses
[Miniflare's scheduled-event API](https://developers.cloudflare.com/workers/testing/miniflare/core/scheduled/).
Native hour/midnight/year boundaries pass (`/tmp/p1-digest-native-job.log`).
UI missing/stale/failed-versus-zero coverage passes (`/tmp/p1-digest-ui.log`).
An authenticated Console route test proves account-filtered counts and all four
presentation states, including storage failure without SQL disclosure
(`/tmp/p1-digest-console.log`). Root and workspace CI run the scheduled test
after the current web and workflow fixtures are built.

The final digest implementation follows ADR 0005's positional-parameter rule:
core owns four fixed statements and validated UTC day/observation strings;
SQLx binds them in a transaction and D1 binds them in a batch. It does not extend
the admission projection's single-JSON-parameter exception. After this
correction, all28 SQLx library tests and the shared29 scenarios pass
(`/tmp/p1-blocked-digest-positional-native.log`), and a fresh workflow fixture
passes all29 D1 scenarios plus the raw transaction/retention/unchanged-column
checks (`/tmp/p1-blocked-digest-positional-worker-build.log`,
`/tmp/p1-blocked-digest-positional-d1.log`). Native core/SQLx strict and workflow
Worker wasm strict checks pass (`/tmp/p1-blocked-digest-positional-native-clippy.log`,
`/tmp/p1-blocked-digest-positional-wasm-clippy.log`).


Final integration verification after positional SQL binding correction and CSS
regeneration: `/tmp/p1-digest-scheduled-css-final.log` passes the real scheduled
Worker/D1 gate; `/tmp/p1-digest-console-full.log` passes104 Console tests with
3 pre-existing ignored fixtures. Native web/server/UI all-target strict checks
and Worker wasm strict checks pass (`/tmp/p1-digest-web-native-clippy.log`,
`/tmp/p1-digest-web-wasm-clippy.log`). Exact worker-build0.8.1 release web packaging
passes (`/tmp/p1-digest-production-build-final.log`); the generated production
scheduled entrypoint passes missing-schema failure, migration/recovery and frozen
snapshot checks in workerd (`/tmp/p1-digest-production-runtime-final.log`). This
artifact has production features and excludes the runtime fixture routes. The
workflow release artifact used by the same test is older and does not establish
acceptance of subsequent M1 work. None of these local checks deploys a trigger.


### Organization and team step observations (ID-57)

Migration 0013 adds independent organization_step_telemetry storage keyed by the
physical organization delivery ID and org or team:<numeric ID>.
OrganizationStepTelemetry binds the request, organization, account, delivery
generation and step. The projector requires the parent receipt and verifies team
membership in its immutable approved command. It enforces portable positive IDs,
immutable uncertainty onset and monotonic observation timestamps at or after
approval. Neither a null nor stale observation clears newer evidence. Existing
repository telemetry payloads and IDs are unchanged.

Each step needs its own retained evidence. An organization membership read does
not freshen an independently pending team membership. Missing uncertainty onset
stays missing: the projection never substitutes its current clock or receipt
update time. Pending steps can use their original positive confirmation time
until a later observation exists. Invalid future evidence is shown as incomplete.

The operator HTML and JSON include explicit account, request, link, delivery
generation, kind, target ID and step identities. Aggregates separately count
repository deliveries, organization deliveries and team steps; blocked reasons
include missing seats, identity-provider-managed teams and enterprise-managed
membership. These reports remain operator-only, concealed from other users and
private/no-store. They neither authorize account mutations nor repeat uncertain
GitHub writes.

The migration preserves historical daily items as repository steps without
changing their observation time or reason. New daily items use
(day, delivery_key, step) identity and retain their kind. Organization and team
items remain invisible to delegated repository-only scope; current all-target
scope is applied before aggregation. The daily transaction permits at most
10,000 blocked steps. It inserts an extra row to detect overflow and aborts the
entire new snapshot, preserving prior days. Completed same-day retries remain
immutable even when the current projection exceeds that limit. At capacity the
operator must partition or extend the read model; this implementation reports
unavailability rather than claiming completeness.

This extension covers projected Unknown, pending/stalled and Blocked steps. It
does not implement external alert delivery, mint metrics, webhook-failure
metrics, latency monitoring or end-to-end projection completeness.

Local unified-signal verification (2026-10-17):

- Shared SQLite43 pass, including independent organization/team telemetry and
  immutable scoped daily counts: /tmp/platform-operational-shared3.log.
- Native SQLx overflow rollback passes among32 library tests; the separate
  corrected upgrade fixture applies every prior migration and preserves the
  exact old daily item/timestamp: /tmp/platform-operational-sqlx-tests.log and
  /tmp/platform-operational-migration.log. The initial upgrade fixture omitted
  prerequisite tables and failed before correction; it was not a migration
  failure against the real prior schema.
- Protected operator HTTP4, scoped Console digest HTTP1 and digest UI1 pass:
  /tmp/platform-operational-http.log,
  /tmp/platform-operational-digest-http.log,
  /tmp/platform-operational-digest-ui.log.
- Current actual D143 plus raw kind/version guards and >10,000-step
  report/digest refusal pass: /tmp/platform-operational-d1.log. The invalid-kind
  control preserves exact child receipts and now requires the unified report to
  reject invalid organization/account identity rather than silently omit it.
- A fresh ordinary web build and the actual exported scheduled Worker handler
  pass both repository and organization/team scenarios, including atomic
  failure/recovery, frozen retry, zero outbound calls and bounded logs:
  /tmp/platform-operational-web-build.log,
  /tmp/platform-operational-digest-worker.log.
- Core/SQLx/web/UI lib+tests strict Clippy passes:
  /tmp/platform-operational-strict.log.
- Organization owner41 actual Restate tests pass, including retained first
  uncertainty onset through three replays, projection outage/lost acknowledgement
  and later confirmation, plus independent team observation failures preserving
  watermark and receipt. Native and Worker wasm strict checks pass:
  /tmp/org-canonical-telemetry-runtime.log,
  /tmp/org-canonical-telemetry-clippy.log,
  /tmp/org-canonical-telemetry-wasm-clippy.log.

These are local SQLite, Restate and workerd/D1 proofs. They do not establish
remote cron registration, deployed migration safety or live GitHub activation.


## Recorded budget refusals

The Settings budget panel separates GitHub spam refusals from ordinary rate-limit
responses. The count window is the previous 24 hours at the snapshot's recorded
whole-second time. Its coverage start is retained; a newly instrumented owner
cannot establish a full historical window. A spam refusal prompts an activity
review even when coverage is partial. Missing old counts display as unavailable.

An owner binds an opaque observation identity before retaining positive HTTP
refusal evidence. Recovering or resending that evidence preserves its reason,
time and identity, so the budget applies one closure and counts it once. Changing
a retained observation's payload conflicts. An absent retry deadline is anchored
to the original observation time; delayed replay never starts another 24-hour
closure. Older unclassified commands still apply their original closure policy
without inventing a spam count.

The existing budget state write commits the observation and closure together.
Observation receipts expire with the rolling window, but replaying expired source
evidence remains outside the count window and cannot extend its original
absolute closure. At most 10,000 observations are retained. Overflow preserves
closure enforcement and marks counts incomplete; it never silently publishes a
complete lower total. Future observations are counted separately as incomplete,
not assigned to the current window. Direct GitHub activity and responses lost
before owner evidence retention are outside these recorded counts.

SQLx and D1 retain the summary in `invite_budget_projection.snapshot_json` under
`limit_counts`: `spammed`, `rate_limited`, `coverage_started_at`,
`future_observations` and `incomplete`. Read `observed_at` from the same snapshot;
an old projected window is not a current zero. Default-absent old fields are
unknown. Account Settings preserves the existing app/account authorization,
including the current-owner requirement for organization budget information.
These snapshots remain advisory and cannot authorize a write. ID-58 records the
retention contract, and ID-21 applies to the changed producer/budget payloads.

## Broker and web log-derived signals (ID-60)

The toolkit `EventSink` and its production `TracingSink` emit bounded records at
actual dispatch/verification boundaries. Web edge checks emit the same structured
tracing format. Every new record contains `occurred_at` as UTC Unix seconds;
clock quality and complete log ingestion remain operator responsibilities.

| Event | Dimensions | Meaning |
| --- | --- | --- |
| `installation_mint_attempt` | `app`, `app_id`, `installation_id`, `cause` | One token POST handed to the transport, including failed attempts. Cause is `acquire`, `installation_unauthorized`, or `app_jwt_retry`. |
| `installation_api_call` | `app`, `installation_id` | One broker-handle authenticated API dispatch, including failed responses, pagination and retries. |
| `webhook_signature_failure` | configured `app`, `reason` | Missing or invalid signature (`missing` / `invalid`); unrelated malformed metadata and unknown app routes do not count as signature failures. |
| `edge_binding_rejection` | `bucket`, `reason` | One checked IP/user/authority binding returned limited or unavailable (`limited` / `unavailable`). Successful checks do not emit this event. |

Count mint-attempt records by UTC hour, app and installation. For the same
interval and dimensions, the 401-remint percentage is
`100 * count(installation_mint_attempt where cause=installation_unauthorized) /
count(installation_api_call)`. A zero denominator is undefined, not a healthy
zero. The numerator counts an actual replacement mint after a generation-matched
401 invalidation, including a failed replacement attempt. JWT refresh/fallback
POSTs use `app_jwt_retry` instead. Coalesced waiters, cache hits, mere handle
acquisition, stale generation invalidation and manual invalidation cannot inflate
the 401 numerator. Every fallback POST still contributes to total mint attempts.
A transport failure before sending bytes remains a dispatch attempt; these
records do not assert that GitHub received the request or issued a token.

Signature counts group by configured app; binding counts group by bucket and
reason. Binding initialization/startup failures precede a checked binding and
remain startup diagnostics, not synthetic rejected requests. No event includes
credential material, URLs, request/response bodies, email addresses, signatures,
webhook delivery IDs, bucket keys, user IDs or IP addresses. Installation and app
IDs are the explicit bounded operational dimensions. The diagnostic cause flag
never changes credential validity, retry policy or authorization.

These are **log-derived rates**, separate from durable projected delivery and
budget counts. There is no isolate-local counter presented as a global total and
no D1 write per request. An external collector must ingest all relevant isolates
without sampling and account for delivery loss/duplicates to publish deployment
rates. Local bounded collection, dashboard and alert-rule templates are now provided in
[`deploy/observability`](../deploy/observability/README.md). Hosted ingestion,
alert dispatch and live production validation are not installed by this change. The original mint completion/coalesced events
remain available; do not count them as token POST attempts.

`tests/worker/operational-signals.mjs` runs the real Worker broker transport,
webhook verifier and rate-limit bindings. It compares emitted events to outbound
requests, checks twenty coalesced callers produce one mint, exercises a 401 and
retry, and verifies successful and failed calls contribute to the denominator.
It captures complete runtime logs with a 256 KiB fail-on-overflow bound and scans
private token/body/signature/identity markers. The script is wired into both CI
workflows and `npm run test:operational-signals --prefix tests/worker`.

Local actual Worker proof passes `/tmp/platform-operational-signals-worker.log`
on `/tmp/platform-diagnostic-web-build.log`: three mint dispatches, exactly one
401-remint, three authenticated API calls, two signature failures and three real
binding limit rejections. Native webhook contract7 passes
`/tmp/platform-signature-signals-native2.log`. Broker23 passes
`/tmp/platform-broker-signals-native2.log`, including failed mint transport,
JWT fallback, stale invalidation and coalescing checks. Native edge8 passes
`/tmp/platform-binding-signals-native.log`, including unavailable checks and
private-key log exclusion. Native toolkit/web lib+tests and actual web-worker
runtime-tests wasm strict Clippy pass `/tmp/platform-diagnostic-native-clippy.log`
and `/tmp/platform-diagnostic-wasm-clippy.log`. These proofs do not establish
external ingestion or alert delivery.

The final captured M1 artifact (`/tmp/m1-final-workflow-capture.log`) passes real
signed Worker/D1 refusal observation cases. Organization invitation spam yields
one actual POST, one typed spam count, an anchored bucket closure and a denied
independent reservation; three owner replays preserve the count and exact D1
snapshot (`/tmp/m1-final-org-delivery.log`). Organization cancellation spam yields
one actual DELETE, releases its unused content charge, retains no cancellation
settlement, and projects the typed count and separate content deadline to D1
(`/tmp/m1-budget-spam-cancellation-worker-final.log`). The test waits for both
closure recording and release: these are retained, separate cross-owner calls,
not an atomic visibility claim across RPCs. Real repository owner evidence covers
rate/spam creation and interrupted success/rate/spam/generic-validation cancellation
(`/tmp/budget-observations-runtime.log`,
`/tmp/budget-observations-cancellation-final.log`). Complete repository32 and actual
journal60paths/25handlers pass `/tmp/budget-observations-full-runtime.log`;
workflow/UI strict passes `/tmp/budget-observations-strict.log`. Shared actual D144
passes `/tmp/installation-audit-d1.log`, including typed counts in snapshot JSON.
These logs concern the captured M1 source; subsequent M2 additions require new
verification and do not inherit this artifact's claims.


## Admission timing and retained journal sample

`admission_rpc_completed` records one actual web-to-LinkAuthority RPC attempt,
including an exact operation replay. Its only dimensions are bounded outcome,
clock kind and certainty; elapsed milliseconds and UTC observation time carry
no identity or input. It adds no Restate named run, state mutation or SQL write.
Cancellation is `interrupted`, not a claim that no admission occurred. The
Worker clock is checked as a finite raw host value before integer conversion;
invalid time produces missing timing evidence rather than epoch-zero health.

The bounded [collector and configuration](../deploy/observability/README.md)
cover the §14.2 signal families without presenting isolate counters as global
rates. The fixed count-only Restate query intentionally samples only retained
completed journals. Production's two-second retention can leave no sample;
this is unavailable and does not establish SP16. A baseline must carry actual
evidence identity and matching clock kind. External hosted ingestion, routing,
production observations and the baseline acceptance gate remain open.

Current post-reset local evidence is retained under
`.verification/20261017-recovery/p1-observability/`; earlier `/tmp` references
above are historical evidence descriptions, not files recovered by this slice.
