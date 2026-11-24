# Delivery control model

The isolated `ghinvite_core::delivery_control` reducer prepares the M1 S0 policy
in P1 §5.1, §5.3, §5.4 and §7.3. It performs no storage, clock, GitHub, budget or
Restate calls. Repository S0 and the request/delivery control inbox adapters are
implemented, together with link pause controls, fixed cascade progress and
separately fenced cancellation and own expiry under explicit local composition.
Production activation and live acceptance gates remain open.

## Owner contract

`State::new` binds the link ID, request ID, target kind, numeric target ID,
delivery generation, admission time and immutable invitation lifetime.
`reduce(state, facts, event, now)` returns a replacement state plus explicit
intents. `now` must come from a journaled handler clock. Numeric identifiers and
revisions use the shared positive portable integer range.

The adapter must persist state and durably arrange all returned effects before
acknowledging an operation. The reducer does not acknowledge operations or execute
external effects. A `RequestCancellation` effect is an instruction to a future
separately fenced, budgeted cancellation executor; issuing DELETE directly from
this effect would violate the contract.

Control envelopes carry the complete identity, operation ID, positive link
revision, greatest observed auto-hold epoch, authoritative control time and full
control state. Equal current revisions require exact payload equality. Older
revisions are ignored; newer revisions cannot undo revocation or reuse a released
hold epoch. Historical operation receipts remain a separate workflow obligation:
the model retains only the latest control envelope and does not detect reuse of
an older operation ID after a later revision.

An auto-pause carries its captured inclusive admission window
`[window_start, paused_at]`. A revoke also carries that window and epoch when the
hold was active, including when all cascade options are false. Consequently a
delayed revoke can abort a qualifying pre-write delivery without first receiving
the earlier hold. A later unrelated request is not captured by an old window.

`Facts` are current owner assertions, not SQL observations. In particular,
`NoWriteProven` requires positive knowledge that no effect occurred. An absent
receipt, a Planned label, an absent invitation listing, or an unresolved claimed
fence cannot establish that fact. A possibly effective attempt is
`MayHaveWritten`. Create receipts and request-decision receipts are not mutable
inputs or outputs of the reducer.

## State policy

| Current facts | Manual pause/resume | Qualifying auto-hold | Revoke without invitation cancellation | Revoke with invitation cancellation |
| --- | --- | --- | --- | --- |
| Proven no write | Continue | Hold | Abort only a captured active hold | Abort |
| May have written | Recover read-only | Recover read-only | Recover read-only | Retain cancellation intent and recover read-only |
| Owned invitation | Continue tracking | Continue tracking | Continue tracking | Arrange owned cancellation intent |
| Foreign invitation | Continue tracking | Continue tracking | Continue tracking | Stop local tracking; never DELETE |
| Final | Preserve | Preserve | Preserve | Preserve |

Aborting releases only a positively unused budget slot. Charged or uncertain slots
are not refunded. Holding alone does not refund a slot. Releasing a hold creates
a new wake generation; stale delayed wakes cannot authorize work. A confirmed
invitation is never converted into a no-write Aborted outcome.

The `decline_pending_requests` bit is bound by the envelope, but acts in the
request owner, not the delivery reducer. A false bit does not prevent abortion
of a captured hold. Foreign tracking stop is a local final disposition; it does
not rewrite AlreadyPending or claim GitHub cancelled the invitation.

## Own expiry

Only an owned invitation with lifetime shorter than seven days gets an own-expiry
timer. The deadline is immutable verified creation time plus invitation lifetime.
Changing that creation time or upstream identity after scheduling is refused.
Tracked foreign invitations and the default seven-day GitHub expiry do not get
this cancellation timer.

A due, matching timer requests fresh evidence. Evidence must match the current
timer generation, upstream identity and delivery revision, and must not predate
the current live-read request time, original deadline, confirmed creation or
latest observation watermark. Future
evidence is rejected. Stale current-timer responses schedule another read rather
than stranding the timer. Old timer generations have no effect.

A verified still-pending invitation produces a cancellation intent. Verified
acceptance produces an acceptance-recording intent and no DELETE. Absence or
unavailable evidence schedules another observation after sixty seconds; absence
also requests ordinary read-only recovery. None of these retries moves the
original expiry deadline. The first arranged cancellation reason remains stable
until terminal owner evidence completes it.

## Interpretation decisions

ID-41 records these choices. The specific §5.1 per-state cascade table and
§5.3 uncertainty rules take precedence over the abbreviated generic S0 wording:
OutcomeUnknown never becomes Aborted merely because a list is empty. The
§5.3/§5.4 foreign-invitation rule and ADR0009 preserve upstream history while a
cascade stops local tracking. Auto-holds cover the captured admission set, bounded
by the pause time. Controls take effect in the recipient object's exclusive
mailbox order, as §7.3 specifies; they cannot interrupt an already running write.

## Runtime adapters

`RepositoryDelivery.control(DeliveryControlInput)` accepts a complete envelope
plus immutable `admitted_at` and `invitation_lifetime_seconds`, before or after
create. It checks the object address and eventual command identity, binds those
metadata, and retains the exact original result per operation ID. Changed inputs
conflict even after a later revision. Newer snapshots and wake generations cannot
rewrite confirmed create receipts or original request decisions.

The request adapter accepts the same full snapshot before initialization, checks
it against immutable initialization metadata, and forwards it one-way before
create and after later controls. It forwards known delivery generations without
inventing reissues. Request-owner control does not wait for delivery execution or
call back into the link.

Repository S0 runs after immutable command binding and before prerequisites.
It reads the unreleased attempt fence when proving no write; fence errors are
conservative. A durable may-have-written marker is set before the guarded
`ctx.run`. Submitted and unknown receipts, a retained marker, or an unreleased
fence cannot become Aborted. A positively rejected generation is explicit
no-effect evidence even when its release acknowledgement fails. M0 has no budget
slot, so the adapter never invents or refunds one.

Held/Aborted/Unknown dispositions update only unconfirmed receipts and enqueue
independent projection. Hold and abort invalidate existing dependency rechecks.
Resume issues a generation-bound wake, consumed once, that loads the original
command. Reconciliation advances retained control after validated evidence;
positive presence can therefore recover Created and retain its pending
cancellation intent without a second create. Foreign cascade stops local tracking
while preserving the AlreadyPending receipt and upstream settlement history.

Owned cancellation and shorter-lifetime timer effects are explicitly retained as
pending work. The control adapter never issues DELETE directly. The separate
executor below consumes those intents only under explicit composition, with its
own fence and budget. A recovering uncertain create mirrors cancellation intent
without claiming ownership or authorizing an external write.

## Integration and activation status

1. Link auto-pause fanout is implemented: the link owns the admission window and
   immutable addressed request set; mutable controls remain separate from
   approved command inputs. The bounded signed Worker race and the actual native
   SD-04 load gate below provide local ordering evidence; deployed runtime
   capacity and live GitHub behavior remain separate gates.
2. Activate the separately fenced cancellation executor only through an explicit
   M1 composition after its gates pass. The preactivation implementation below
   uses the real budget owner. Normal composition retains intents without DELETE.
   Live budget assumptions (including SP-08) remain unverified.
3. Cascade progress retains authoritative addressed request/target slots at
   revoke time, including slots not initialized yet. Completion, no-op and
   waiting-for-evidence reports use idempotent target-bound receipts. SQL views
   display this fixed denominator rather than defining the addressed workset.
4. Complete production own-expiry activation and operational surfacing. The
   preactivation executor reads and retains the exact upstream creation timestamp
   and uses `tick_own_expiry`, separately from GitHub expiry observation. Missing
   metadata keeps the effect pending; confirmation/recovery time is not used as
   the upstream creation time.
5. Final-result aggregation, Lapsed and seven-day re-request arbitration are
   implemented. Held, Blocked and OutcomeUnknown remain non-final. Retained
   per-delivery Held/Released audit and requester/Console surfaces have local
   proof; final combined signed Worker evidence and remaining M1 event/reporting
   coverage are tracked in the implementation ledger. Org delivery remains
   behind its live SP gates.

These changes add journal entries and retained state. ID-21's immutable endpoint
pinning or draining requirements apply before integration deployment. This model
alone does not complete B1-08, B1-09, B1-10, B1-13 or B1-14.

## Local verification

Fifteen pure reducer tests pass, covering all 24 permutations of hold, release,
second hold and revoke; released-hold resurrection; exact identity/revision
conflicts; immutable receipts; retained JSON roundtrips; unknown effects; unused
slot refunds; foreign/owned distinctions; wake invalidation; and current-read
own-expiry evidence with an immutable deadline.

The actual native Restate protocol suite passes all eleven tests, including four
S0 scenarios. They cover pre-create inboxes, held release, delayed unseen-hold
revocation, historical receipt replay, consumed wakes, blocked retry invalidation,
positive rejection with failed fence release, a retained uncertainty SetState
strictly before the guarded run, timeout/unknown presence recovery,
and foreign tracking stop. Existing rank, two-generation rejection and three
lost-run-acknowledgement proofs remain green. Installation-token markers are
absent from the inspected control and create journals.

The signed Worker/D1 gate uses actual request approval/dispatch, held and aborted
projection, exact historical control/decision replay, three real failed
`ctx.run` acknowledgements around one PUT, and read-only unknown recovery with no
DELETE. CI runs it as `DELIVERY_CONTROL_ONLY=1 node tests/worker/admission.mjs`.
Source-derived journal paths include control and control_wake; the real table
walks 53 paths across 19 handlers. Endpoint-version compatibility remains a
separate deployment obligation.

Evidence: `/tmp/p1-delivery-control-reviewed-tests.log`,
`/tmp/p1-delivery-control-runtime-final.log`,
`/tmp/p1-delivery-control-marker-order2.log`,
`/tmp/p1-delivery-control-workerd-final.log`,
`/tmp/p1-delivery-control-journal-final2.log`,
`/tmp/p1-delivery-control-native-clippy-final.log`, and
`/tmp/p1-delivery-control-wasm-clippy-final3.log`. The final signed Worker gate
uses the bundle including shared lifetime serialization and deterministic
control-ID amendments. No deployment or live-GitHub behavior is claimed.


## Separately fenced cancellation: explicit local composition

`AppState::with_pre_activation_cancellation()` enables the executor only where
composition also binds the real `InviteBudget`. The isolated Worker feature
`cancellation-fixture` supplies this composition; ordinary and production builds
remain unchanged. There is no request parameter or runtime environment override.

The owner retains a full cancellation command (original create identity, owned
upstream invitation ID, first reason), obtains `OtherWrite` between journaled
reads and writes, and claims `step=cancel` independently from `step=repo`. The
write stage ensures credentials, repeats identity/pending/acceptance checks, and
issues one DELETE per claimed generation. A positive 401 releases only that
generation before one bounded remint/retry. Definite rejection refunds an unused
slot; uncertain HTTP/fence/run acknowledgements preserve both fence and charge.
An interrupted pre-write budget RPC is recovered with the same reservation key
and released only when the durable marker and readable absent fence prove it
unused. Delayed duplicate drives respect the retained retry time.

The owner retains each completed write result before budget or settlement calls.
An interrupted completion resumes that exact result, slot, generation and time;
it cannot turn a confirmed DELETE into another attempt or lose a rejection refund.
Positive read, mint and write rate limits close content capacity with a retained
absolute deadline. Read-side closure evidence is also retained before its budget
call. An uncertain external effect never releases its content charge.

A confirmed DELETE adds settlement without rewriting the original create receipt.
A received 404 needs a fresh verified reach/acceptance read; acceptance wins, and
unavailable evidence stays uncertain. A lost DELETE response cannot use later
absence to claim success or to authorize another DELETE. Ordinary absence sweeps
also cannot relabel an uncertain cancellation as a decline. Foreign invitations
only stop local tracking. The legacy cancel input no longer authorizes an owned
unfenced DELETE; it can only wake an already retained control intent.

Own expiry first reads the exact invitation's GitHub `created_at`, bound to its
numeric repository and invitee. It retains that immutable timestamp and schedules
an actual delayed tick at creation plus the fixed lifetime. GitHub timestamps may
have whole-second precision; the lower-bound comparison uses the approval second.
Default seven-day lifetime and foreign invitations schedule no own-expiry DELETE.
Each tick uses a current generation, retained not-before time, current delivery
revision and a fresh read. Pending evidence creates a cancellation intent; accepted
evidence settles acceptance; unavailable/absent evidence schedules another read
without moving the original expiry deadline. New timer fields require ID-21's
endpoint pin/drain discipline.

Native actual-Restate evidence: `/tmp/p1-cancellation-accounting-final.log`
passes nine scenarios, including immutable successful cancellation, three actual
run-result losses around one DELETE, HTTP uncertainty, double401 refunds,
verified 404/acceptance handling, and actual delayed own expiry with pending
versus accepted outcomes. It also interrupts the owner after a reservation and
after a retained DELETE result, checks completion/refund without a second DELETE,
and proves positive read/write rate-limit closures. A later absent-list observation
cannot settle an uncertain cancellation as declined; positive acceptance can.
The shared SQLite suite passes `/tmp/p1-cancellation-fence-sqlx-final.log`, including
input conflicts, step isolation, replay, stale generation releases and portable
IDs. Focused strict native Clippy passes `/tmp/p1-cancellation-accounting-clippy.log`.

The ordinary-composition regression passes
`/tmp/p1-cancellation-approved-runtime2.log`. An unavailable pre-write fence read
keeps control conservative and returns a retryable preflight failure without
inventing an external-write receipt. Restoring that storage path permits exactly
one PUT (`/tmp/p1-control-fence-outage.log`). A positive fence or durable
may-have-written marker still requires uncertainty handling.

The earlier ordinary-composition journal matrix passes
`/tmp/p1-cancellation-journal-final.log`: 86 actual invocations walk all 57
source-declared paths across 24 handlers, including default-disabled cancellation
and own-expiry handlers and cascade progress. Active preactivation cancellation
is covered by the effect/accounting tests above, not by this ordinary journal
matrix. ID-21 endpoint versioning remains required.

The final signed Worker/D1 proof `/tmp/platform-cancellation-cascade-workerd.log`
passes seven scenarios on the isolated artifact built with retained completion
accounting and rate-limit deadlines (`/tmp/platform-cancellation-cascade-worker-build.log`):

1. Owned cancellation spends a real budget slot, records a separate `cancel`
   fence beside `repo`, and issues one DELETE while preserving immutable receipts.
2. A foreign invitation stops local tracking without a DELETE, cancellation
   fence, settlement claim, or budget charge.
3. A forwarded successful DELETE with a lost response stays uncertain and charged.
   Three later drives after the retry deadlines preserve the same fence and do
   not repeat the DELETE or refund/acquire another slot.
4. A retained cancellation control queued while the actual PUT is blocked cancels
   once after creation; a late declined webhook cannot rewrite the winner.
5. Fresh verified acceptance wins before cancellation without a DELETE or charge.
6. Early and stale own-expiry ticks make no DELETE. A due tick verifies pending
   evidence and records `Cancelled` with `OwnExpiry` once. This case advances the
   fixture clock and invokes the real timer handler; it does not claim elapsed
   wall-time timer delivery (covered separately by the native runtime test).
7. An operation-bound link revoke captures one owned delivery, fans out to one
   DELETE, and reports completed cascade progress. Exact receipt replay retains
   the original addressed counts; changed input under the same operation conflicts.

The cancellation fixture also passes strict wasm Clippy
(`/tmp/platform-cancellation-cascade-wasm-clippy.log`). Its compile-time composition and
generated artifact are separate from the ordinary Worker bundle. Both CI
workflows run this gate; `npm run test:cancellation --prefix tests/worker` builds
the isolated artifact and current GitHub stub before running it.

Worker commands from the repository root:

```sh
WASM_BINDGEN=/tmp/p1-wasm-bindgen/wasm-bindgen ../../scripts/dev.sh node tests/worker/build.mjs cancellation-fixture
CANCELLATION_ONLY=1 ../../scripts/dev.sh node tests/worker/admission.mjs
```

This is local acceptance evidence, not live GitHub cancellation-policy validation
or production M1 activation. Request final-result aggregation/Lapsed and complete
cascade progress remain separately coordinated work.

### Final-result notification verification

Delivery owners now send identity-only final notices after saving their validated
snapshot and before sending its SQL projection. The request owner re-reads the
retained delivery and binds its complete command to the approved manifest;
notifications carry no caller-supplied final result. Repeated final notices do
not change create or decision receipts.

The actual repository delayed-expiry test passes in
`/tmp/p1-repo-final-notice-runtime.log`: a pending invitation produces
`LapsedInvite`, while fresh acceptance produces `Satisfied` without a DELETE.
Its recording consumer immediately reads the durable owner snapshot. The
organization suite passes all 26 tests in `/tmp/p1-org-final-notice-suite3.log`,
including an actual delayed timer's organization-and-team final notices and a
standard endpoint journey from foreign `AlreadyPending` to fresh disappearance
to request `Lapsed`, preserving original admission, approval and create receipts.
Invalid retained repository identity, receipt, time and revision checks pass
`/tmp/p1-final-notice-validation.log`; strict owner/protocol checks pass
`/tmp/p1-final-notice-clippy2.log`. These are local runtime proofs; production
activation and live GitHub policy acceptance retain their separate gates.


The email-domain/anonymous-offer update extends the ordinary-composition journal
matrix to 59 declared paths across 25 handlers. Actual journal inspection passes
`/tmp/p1-domain-offer-runtime2.log`, including capability-only offer success and
neutral invalid-capability refusal. That same run passes all five domain/request
completion scenarios, including prepared-operation identity preservation across
URL rotation. This evidence precedes the separate retained transient-audit work.


The retained transient-audit implementation advances the current repository/link
journal gate to 60 source-declared paths across 25 handlers. Actual inspection
passes `/tmp/repo-transition-final-journal.log`. New named audit-clock paths occur
only when a semantic episode begins or a hold is released; repeated blocked or
uncertain observations do not create another episode. The earlier 59-path result
above describes the preceding domain/offer implementation.

## Accepted access removal (M2, ID-62)

`GH_ACCESS_REMOVAL_ENABLED` is an explicit, matched web/workflow capability. It
binds `AccessGrant` and the real invitation budget. The default is false. The
immutable optional access lifetime applies only to positively P1-created access
that later settles Accepted. Existing, foreign, upgraded and email access never
acquires this removal authority. None lifetime retains explicit-revoke
eligibility but schedules no timer.

Each request/kind/target owner retains generation-bound creation provenance,
acceptance time, original grant and exact link or batch source. A removal reads
current numeric identities and roles, reserves OtherWrite, and claims its own
`access:<request>:<kind>:<target>:<generation>` / `remove:<kind>:<target>` fence.
Promoted owners, maintainers and higher/custom repository roles are skipped.
Unknown DELETE results retain their fence and charge; later absence does not
prove that the write was unused. The original invitation receipt and settlement
remain unchanged. Reissue stops acknowledged unused work and retains tombstones
for absent old generations so a late Accepted notification cannot revive an old
timer.

The Console obtains `InvitationLink.prepare_access_removal` using the same
operation ID as confirmation. The owner walks retained request manifests and
current delivery owners, registers positively accepted grants, and freezes the
exact list. It never enumerates SQL to obtain removal authority. Confirmation
requires this receipt before closing the link; late acceptance is excluded.
Unavailable capture disables removal in the form. Progress includes every frozen
grant, including live role-change skips and reissue stops; uncertain removals
remain visibly waiting on GitHub. The UI states the capture time and warns that
inherited access can remain, collaborator removal can delete private forks, and
organization removal affects team/repository membership.

Local evidence through the current captured artifact:

- `/tmp/access-grant-runtime3.log`: five real Restate repository-removal tests,
  including a real delayed timer, promotion skip, unknown ACK and tombstones.
- `/tmp/org-reissue-runtime5.log`: two organization old-generation barriers.
- `/tmp/access-preview-runtime-final.log`: six real organization/access tests,
  including independent org/team removal, exact capture/replay/conflict,
  missing-preview refusal before revocation, late-acceptance exclusion and
  reissue stop after capture.
- `/tmp/m2-shared47-d1.log`: actual D1 shared conformance, including AccessGrant
  source binding, separate fences, monotonic immutable projection and audits.
- `/tmp/m2-access-workflow-capture.log` and
  `/tmp/access-removal-workerd2.log`: actual signed Worker/D1 preview+revoke,
  one DELETE with real budget/fence, lost ACK retained through three replays,
  and an actual delayed three-second team-removal timer with promoted org owner
  skipped. No manual timer invocation substitutes for this delayed-send proof.
- `/tmp/access-lifetime-form-tests8.log`, `/tmp/access-lifetime-http3.log` and
  `/tmp/access-preview-ui.log`: three parsing/takeover/SSR tests, one HTTP
  capability/refusal/exact-duplicate test, and three revoke-confirmation tests.

Run the signed proof with `npm run test:access-removal --prefix tests/worker`.
Both CI workflows invoke `ACCESS_REMOVAL_ONLY=1` on their captured artifact.
The actual lifetime browser matrix passes all 12 native/enhanced/delayed-mount
cases (`/tmp/access-lifetime-browser2.log`) after correcting the wasm entrypoint
to forward the server capability. The fresh island is 475 KB gzip. The org
fixed inventory, including `prepare_reissue` and `cancel_unsent`, passes all 21
handlers (`/tmp/access-org-fixed-journal2.log`). AccessGrant fixed journal
coverage now passes 13 declared paths across all eight handlers. The active
preview uses exactly `access_preview_time` (`/tmp/access-preview-active-journal.log`).

The current actual native suite passes all 11 tests
(`/tmp/access-grant-native-final5.log`). It kills real invocations after the
generation registration, reserved budget, retained HTTP result and terminal
owner commit. Exact registration replay repairs the generation pointer and
arms an actual delayed timer; terminal retries resend the original projection
and audit. Three real guarded-run retries keep one DELETE and its charge.
A negative test caught automatic broker retries after 401 reusing one SQL generation
(`/tmp/access-removal-remint-negative.log`). The corrected single-attempt
broker call lets the owner release rejected generation 1, reread, remint and
claim generation 2. Tests cover 401→204, 401→502 and double 401, including exact
charge/refund and immutable acceptance. Latest workflow wasm strict passes
(`/tmp/access-removal-wasm-strict2.log`) and final native strict passes
(`/tmp/access-removal-native-strict4.log`). The final capture
(`/tmp/access-removal-workflow-final-capture.log`) passes all five signed Worker
cases (`/tmp/access-removal-workerd-final.log`), including first401→204 and
first401→lostACK with two token mints, generation2 fences, retained charge and
no repeated DELETE through three later drives. These local proofs do not satisfy live
AC-LIVE-3 or deploy an AccessGrant endpoint/migration. Retained timers require
endpoint pinning/draining; turning off new registration cannot abandon existing
removal work.


The bulk entry and review now expose the same access-removal consequences before
confirmation. Review shows the exact immutable lifetime (including non-whole-hour
values), direct-only eligibility, upgrade/pre-existing exclusions, promotion and
custom-role skips, the organization-email exception, and inherited-access/fork/
organization-removal consequences. No automatic expiry is stated explicitly when
the lifetime is absent. Native UI4 pass (`/tmp/bulk-access-policy-ui.log`); actual
Chromium2 with JavaScript enabled/disabled pass (`/tmp/bulk-access-policy-browser.log`),
including exact 3,600-second review and identical submission recovery after a lost
acknowledgement. Focused UI/web strict passes (`/tmp/bulk-access-policy-strict.log`).
This focused current-source proof follows the parent's completed broad native web
baseline; it does not claim that the earlier Worker artifact contains the new panel.


## SD-04 actual concurrent admission load

`authoritative_admission::load_ordering` passes three actual Restate 1.8.0 cases
(`/tmp/platform-sd04-runtime2.log`, 211.91 seconds with two test threads). Each
holds the first exclusive admission before its decision, sends 249 more distinct
admissions concurrently, waits for all durable ingress acknowledgements, enqueues
one lifecycle command, then concurrently enqueues 250 later admissions. Actual
Restate invocation metadata must show all 500 admissions still pending/running
and the single control pending before the barrier is released. Client spawn
order alone is not used as evidence of enqueue order.

- Pause with max uses 400: exactly 250 earlier admissions succeed; all 250 later
  receipts are `Paused` although capacity remains.
- Pause with max uses 100: exactly 100 admissions succeed; the exhausted link
  neutrally refuses every later admission. This is a separate capacity case so
  exhaustion cannot mask a broken pause gate.
- Revoke with max uses 1,000: exactly 250 earlier admissions succeed; all 250
  later receipts are `NotAvailable`.

Each case checks exact accepted/refused attempt replay, exact lifecycle replay,
and unchanged use accounting. All 1,500 callers have distinct numeric users and
operation IDs; no fixture bypasses the link owner or replaces admission with a
model. Pending requests use actual request owners and SQLite projections.

Complete bounded scans cover 18,195 / 35,445 / 44,958 raw journal entries and
1,609 / 3,259 / 3,760 state rows respectively. Only the hash of the private test
Link Secret enters commands; its raw marker is absent from decoded journal bytes
and retained state. Count/byte overflow fails the test rather than accepting a
partial scan. This complements, rather than replaces, the full browser/Worker
credential-disclosure gates. Focused strict Clippy passes
(`/tmp/platform-sd04-strict.log`). Both CI workflows run the full authoritative
suite using the pinned disposable server with two test threads.

Reproduce with `RESTATE_SERVER_BIN=/path/to/restate-server cargo test -p
ghinvite-workflows --features integration --test authoritative_admission
load_ordering -- --nocapture --test-threads=2`. This is a local concurrency and
ordering proof, not a Cloudflare CPU, deployed throughput or capacity benchmark.

## Requester invitation deadlines

`DeliverySnapshot` and `OrganizationDeliverySnapshot` carry optional
`invitation_timing` outside their immutable create and settlement receipts.
The first retained value binds the numeric upstream invitation, its GitHub
`created_at`, a completed observation time, and, when available, the exact
control inbox's immutable invitation lifetime. Missing fields remain absent on
historical snapshot wires. There is no backfill from approval or confirmation.

For an owned pending invitation the requester view shows its creation time plus
the retained lifetime (at most seven days). A tracked foreign invitation shows
GitHub's seven-day deadline regardless of the request's shorter lifetime. Teams
attached to that exact organization invitation share the deadline; independent
pending team membership has no inferred invitation clock. Missing or invalid
clock or owned-lifetime evidence displays “Invitation expiry is unavailable.”
An expected deadline never settles a delivery or authorizes cancellation.

The repository's typed 201 response treats a missing or malformed optional
creation timestamp as unavailable metadata while retaining the successful
invitation outcome. Its `permissions` is not optional: a 201 can carry a
pending invitation at its original role, so one without it is not decoded. A later complete identity-bound listing may add the first
valid timing at a newer snapshot revision without rewriting its create receipt.
Organization creation already returns a validated upstream timestamp; the owner
pairs it with its retained control policy. Existing strict invitation list/PATCH
and cancellation observation contracts are unchanged.

Migration `0025_invitation_timing.sql` validates numeric identity, UTC source
clock ordering at nanosecond precision, the second-precision approval floor for
owned invitations, and the finite lifetime range. Once recorded, timing cannot
be changed or cleared. The shared `scenario_invitation_timing`, raw SQLite/D1
controls, owner replay/recovery tests and requester presenter tests cover this
boundary; their current execution results are recorded in the implementation
ledger only after the corresponding gates complete.

Current timing verification passes: three actual Restate owner tests, one core
clock/policy test, three requester presenter tests, one optional-201 timestamp
test, and one raw SQLite guard test. The full shared SQLite/D1 inventory passes
all 51 scenarios, including exact first timing, stale replay and raw nanosecond
mutation/INSERT rollback controls. The signed organization Worker gate passes
timing, immutable recovery and full D1 snapshot equality on the captured build.
Evidence: `/tmp/invitation-timing-owner-proof.log`,
`/tmp/invitation-timing-unit-proof.log`,
`/tmp/requester-message-timing-d151.log`, and
`/tmp/invitation-timing-org-workerd2.log`. The first signed attempt found only an
obsolete fixture expectation for `canonical_v1`; the current canonical owner
correctly returns `canonical_v2`, and the corrected test passes without a
production change. These local proofs do not claim live GitHub validation or
production deployment.

The assembled island was refreshed successfully (475 KB gzip, 600 KB budget).
The full requester browser suite passes 12 cases across desktop JavaScript and
mobile without JavaScript, including one-hour owned timing, a foreign invitation
retaining its seven-day deadline despite the one-hour link policy, unavailable
historical timing, exact retained display after refresh, and no date-based
settlement claim (`/tmp/invitation-timing-island-build2.log`,
`/tmp/invitation-timing-browser2.log`).

Final combined native Clippy (`gh-toolkit`, core, SQLx, UI, workflow integration
and web lib/tests) and actual web/workflow Worker wasm Clippy pass with
`-D warnings`: `/tmp/invitation-timing-final-native-strict.log` and
`/tmp/invitation-timing-final-wasm-strict.log`. The strict run includes the
subsequent default-compatible toolkit feature split; the signed Worker timing
proof uses the earlier immutable timing/message/typed-transport capture.
