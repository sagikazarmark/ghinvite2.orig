# Email invitation delivery — local M2 evidence

This lane sends GitHub organization invitations by email. It does not send application email. Live SP-07/SP-18 behavior and production activation remain separate gates.

## Privacy and recovery boundary

The toolkit `email_invitation::Address` owns a zeroized mailbox and has no Clone or Serialize implementation. Its Debug output and parse errors are redacted. The accepted syntax is an ASCII dot-atom local part (including plus tags), followed by a normalized IDNA DNS domain. Quoted local parts, comments, SMTPUTF8 local parts, domain literals and trailing-dot domains are refused with a row-only field error. Local-part case is preserved.

The web layer allocates random 256-bit `EmailRef` values. Normalized duplicate addresses point to the first row's reference; only that row is staged. Handles remain available for GitHub resolution. A versioned encrypted web continuation preserves the exact allocation plan across uncertain staging. It uses the registered AttemptContinuation purpose, a distinct `bulk-email-plan/v1` scope, and the trusted session ID plus batch/account/org-installation context. It expires after at most one hour and never substitutes for current actor, CSRF or account authority checks.

Migration 0015 stores immutable encrypted recovery records. The dedicated `bulk-email` purpose binds the random reference, batch, row, account and org installation. An address digest lives inside the ciphertext, never in a queryable hash column. Staging expires after 24 hours; active uncertain deliveries retain recovery data. A confirmed create or terminal no-write result retires the recovery record, preserving a 24-hour retry horizon. Bounded cleanup clears at most 100 ciphertexts per call and permanently retains the reference tombstone. No raw mailbox belongs in a manifest, Restate command/run result, delivery receipt, audit event, log or public URL. The existing hourly minute-five maintenance opportunity runs cleanup independently of the daily report. Data becomes eligible after 24 hours and is purged on a later successful bounded tick; exact 24-hour deletion is not claimed.

## Cross-admin reviewed-address recovery

ADR0010 adds a separate `email_review` escrow (migration0019), using registered
`bulk-email-review` context and the same dedicated `p1-email` keyring. It binds
each ciphertext to the original reference, batch, row, account and organization
installation. The original one-hour expiry cannot slide, and cleared references
and row identities remain permanent tombstones. Only the authorized web layer
can stage a reviewed selection; the API returns no plaintext. It validates and
decrypts every selected row before the first delivery-recovery write. Removed
rows never enter delivery recovery and can have their review ciphertext cleared
after authoritative removal. An uncertain staging/submit acknowledgement keeps
the selected escrow until its original expiry. Personal-link capabilities and
creator labels retain their separate browser/session binding.

Native SQLite6 passes (`/tmp/platform-email-review-native2.log`): cross-instance
selected staging, all-before-write rejection, immutable retries/deadlines,
concurrent writes and lost committed ACK, key-generation/configuration retry,
exact purpose/context separation, removed-row idempotence and bounded100+1
expiry cleanup. The actual cross-admin HTTP10 gate also passes
(`/tmp/bulk-cross-admin-http.log`), including a distinct submitting admin/browser,
selected-only delivery vault, exact lost-ACK retry and no cross-browser labels or
personal capabilities. Actual D1 and scheduled Worker proof passes
(`/tmp/platform-email-review-workerd.log`): multiple isolates, selected-only
staging, fixed expiry, all-before-write failures, generation/configuration retry,
committed ACK loss/concurrent writers, cross-purpose/context rejection, SQL
immutability and permanent tombstones. The actual scheduled export independently
recovers a review-cleanup failure and clears 100+1 expired records using processing
time. The native hourly-maintenance SQLite test also passes
(`/tmp/platform-email-review-scheduler-native2.log`), preserving the original
expiry across the 100+1 cleanup boundary. All 31 application tables and complete bounded logs contain no mailbox or
SQL markers, with zero outbound requests. Both CI workflows and npm
`test:email-review` run this proof against the ordinary web fixture artifact.

## Local results

- Toolkit adapter:3 native tests passed (`/tmp/platform-email-adapter-native2.log`): exact org POST, members-write mint body, no member/user lookup or automatic POST retry, and bounded response/redaction behavior.
- Core reference/settlement:4 tests passed (`/tmp/platform-email-model-native2.log`): strict reference wire shape, exact upstream-ID observation, conservative unknown/stale/contradictory evidence and revision overflow.
- Personal-link derivation:3 matching core tests passed (`/tmp/platform-personal-link-native.log`), including the bulk mixed-target contract. Commands retain batch source, one use, no approval, fixed submitted-at+7days expiry, exact repository grants and optional access lifetime. Organization and team targets are not duplicated into the personal link.
- SQLite:7 tests passed (`/tmp/platform-email-delivery-storage-native2.log`). They cover encrypted context isolation, rollout retry, concurrent staging, lost committed acknowledgement, bounded cleanup, full-command write fences, concurrent claim, stale release, and immutable/scoped receipts.
- Actual D1/Worker: `tests/worker/email-vault.mjs` passed (`/tmp/platform-email-fence-workerd.log`). Multiple real isolates share D1 and exercise staging races, lost ACK, generation lag/wrong keys, copied ciphertext, all immutable binding columns, fixed terminal horizon, tombstones, concurrent fence claims, generation release, receipt replay and account scope. All three port tables and complete bounded runtime logs are scanned for mailbox markers. The fixture rejects every outbound network request. The latest run applies the complete current migration chain in an isolated database; it is not the shared storage scenario-suite claim.

## Owner protocol and remaining gates

`AppState::with_email_deliveries` is explicit and binds the real invitation budget through the standard endpoint composition. The owner has no requester/request lifecycle and never creates AccessGrants or Lapsed state. Active recovery binding is independently checked in the HTTP run. The full-command `org-email` fence is independent of repository and organization-user fences. Unknown responses cannot release the slot or repeat POST. Positive rejection is retained before fence release, budget release and bounded limit reporting. Exact replays preserve the original command and receipt.

Shared organization list observations route explicit `email:batch:row:org` registrations. Only a known upstream invitation ID can settle; no email/login matching recovers a lost 201. Pending/failed overlap stays pending. Missing from both complete lists means NoLongerPending, never Accepted. Cancel stops only the unsent remainder, including a pre-initialization cancellation; known or uncertain effects remain unchanged.

Actual Restate owner tests passed 5/5 (`/tmp/platform-email-owner-runtime5.log`) using the standard endpoint composition, real budget, encrypted vault and SQLite fences. They prove one POST under three exact replays, immutable conflict refusal, budget charging, mailbox/token absence from journals, shared organization reconciliation of known IDs, unknown responses remaining unknown after complete absence, staged-address refusal with no charge, pre-initialization cancel, and positive 422 release without repeated writes.

Web preparation/recovery tests passed (`/tmp/platform-email-rows-native3.log`, three new tests within six matching tests). Normalized duplicate rows preserve the first reference, malformed addresses produce only a row number, and encrypted recovery checks all context fields and expiry. Personal capability tests passed 2/2 (`/tmp/platform-personal-link-web.log`): only the hash-only plan is durable; interrupted submissions restore the original URL under exact session/account/installation/batch/row/link/hash binding. Recovery uses a distinct `bulk-personal-link/v1` scope and expires within one hour. Current authorization is still required before every route action.

Atomic email receipt/audit projection passed SQLite2 (`/tmp/platform-email-audit-storage-native.log`) and actual D1 (`/tmp/platform-email-audit-workerd.log`). Injected audit insertion failure rolls back the new receipt, replay retains one create event and one neutral settlement event, and metadata contains no mailbox reference/hash/address. The neutral event is `email_invitation.no_longer_pending`, never an acceptance event.

Scheduled maintenance passed actual SQLite with an advancing clock (`/tmp/platform-email-maintenance-native.log`) and the exported Worker scheduled handler (`/tmp/platform-email-maintenance-workerd.log`). The Worker proof covers 100+1 bounded cleanup, active and open retry horizons, actual processing time, independent digest/cleanup failures and recovery, permanent tombstones, no outbound calls and complete bounded safe logs. Both Worker gates are wired after the ordinary web capture in both CI workflows.

Personal-link source/immutable projection and atomic audit proof passed SQLite1 (`/tmp/platform-personal-link-storage-native.log`). Native email-owner lib/test strict passed (`/tmp/platform-email-owner-strict-final2.log`). Both ordinary Worker crates pass wasm strict Clippy (`/tmp/platform-email-workers-wasm-strict-final2.log`). Full bulk Console flow and the combined final CI run remain in progress. No complete B2-03 or live-role-preservation claim follows from the adapter/vault tests alone.

## Explicit deployment and bulk review seams

`GH_BULK_ENABLED` accepts only `true` or `false` and defaults to false in both
native and Worker composition. Repository-only bulk can run without email
recovery. Enabling bulk together with organization delivery requires the
separate `P1_EMAIL_KEYRING` descriptor whose ring name is `p1-email`; its key
check value and full descriptor are validated before startup. The web and
workflow deployments must receive compatible generations of this dedicated
ring. The web session keyring remains a separate purpose/name boundary.
Standard composition registers BulkInviteBatch, EmailInviteDelivery and their
real shared InviteBudget only when the corresponding capabilities are enabled.
Config unit2 and actual native startup8 passed in
`/tmp/platform-bulk-config-native.log` and
`/tmp/platform-bulk-startup-native.log`. Actual signed workflow startup29 and
web startup34 passed (`/tmp/platform-bulk-workflow-startup.log`,
`/tmp/platform-bulk-web-startup-final.log`), including dedicated-ring absence,
malformed configuration and fixed field-only failures with no outbound calls. This is local capability wiring, not a live probe claim.

The read-only bulk review service validates the 200-row bound and current
actor/account/installation/target authority before lookups. It distinguishes
404 from unavailable responses, resolves aliases to numeric users before
repeating role reads, and returns explicit per-target observed access and
planned actions. It obtains advisory timing from the real InviteBudget snapshot
RPC and never reserves or writes. Existing five-minute cache and bounded
uncached-read limits remain enforced; an incomplete large review stays
Unavailable until retried with evidence or unresolved rows are removed. Three
native service tests passed (`/tmp/platform-bulk-review-native2.log`).

Email review labels can be read transiently from the restored session-bound
PreparedEmailRows through `review_address(row)`. The UI must HTML-escape the
normalized mailbox and must not put it in a manifest, receipt, audit, log or
plaintext continuation. Duplicate rows share the original transient label. Submit uses `stage_selected`: it validates the complete selected command set against the protected original row/reference/account/installation bindings before any write, then stages only unique selected references. Removed rows remain unstaged. Five preparation tests passed (`/tmp/platform-email-selected-rows-native.log`), including actual SQLite selected-only persistence, identical ciphertext on retry, and later-row mismatch refusing the whole selection before any write.

## Actual 60-row organization cap

The standard opt-in BulkInviteBatch → EmailInviteDelivery → InviteBudget graph
passed the real 60-row acceptance case in 70.45 seconds
(`/tmp/platform-bulk60-runtime2.log`). Sixty distinct encrypted bindings were
staged, the reviewed batch performed no POST, and Submit activated the exact
bindings. Normal pacing produced exactly 50 organization POSTs; the other ten
owners retained Bucket throttles until the first slot's 24-hour expiry. Original
create/submit operation replay produced no 51st POST. No InvitationLink,
InvitationRequest or RepositoryDelivery was invoked, and all retained email
budget inputs had null link attribution. The bounded complete scan covered
33,918 raw journal rows plus retained state, with no mailbox or token markers.
This is a native real-Restate proof of email batch orchestration, not a live
GitHub test or a signed Worker batch claim.

The test first exposed an owner bug: RetryAt is an immutable budget receipt, so
reusing its request identity after the deadline repeatedly returns the old
refusal. Email delivery now clears only that positively refused, uncharged
reservation and advances its ordinal in the same retained update as Throttled.
An uncertain budget response still preserves the original identity. A focused
actual three-second closure/automatic-wakeup regression passed in five seconds
(`/tmp/platform-email-budget-wakeup.log`), yielding exactly one charged POST.


## Signed Worker owner and D1 graph

`EMAIL_DELIVERY_ONLY=1 node tests/worker/admission.mjs` passed on the ordinary
explicitly enabled workflow artifact. The final current-artifact rerun passed
(`/tmp/platform-email-owner-workerd-final.log`) after the fresh web capture
(`/tmp/platform-email-web-capture-final2.log`).
The production bootstrap binds Bulk, org/email owners and real InviteBudget;
the separate test web isolate stages encrypted records without network access.
The proof exercises Batch Submit activation, one 201 POST under exact replay,
changed-command conflict, cancellation preserving confirmed work, known-ID
shared neutral reconciliation and its audit; ambiguous503 retains one charge
and generation1 fence under replay/absence/cancel; positive422 releases the
exact generation and unused charge; inactive staging is denied/refunded; and
preinitialization cancel prevents a later POST. No membership/user lookup,
requester lifecycle or DELETE is used.

It scanned 1,025 raw journal rows, all 32 retained state rows, every application
D1 table (30), and complete bounded runtime/Restate logs for mailbox and token
markers. Both installed read/write profiles were actually used. The final run supplied
only dedicated P1_EMAIL_KEYRING to both staging and production workflow
composition. It supersedes the initial staging-fixture alias boundary; the
independent final startup matrix verifies the fixed configuration-error response.
The npm `test:email-delivery` command builds both ordinary artifacts, and both
CI workflows run the signed gate after their current ordinary captures.

The native legacy owner5 regression also passed after the RetryAt fix
(`/tmp/platform-email-owner-legacy5-final.log`, 8.24 seconds). An attempted
combined seven-test replay was terminated externally with SIGTERM mid-cap;
that run is not recorded as passing. The earlier completed cap proof and
focused deadline proof remain the evidence for those two cases.


## Fixed named-journal inventory

The actual Restate gate `journal::fixed_email_journal_paths_cover_every_deployed_handler`
passes in the current eight-test run (`/tmp/bulk-email-drain-runtime4.log`).
It walks all 16 fixed, source-reviewed paths across all six deployed
EmailInviteDelivery/EmailDeliveryProjection handlers. Cases include confirmed
create/replay, positive rejection and release, ambiguous response, staged recovery
failure then activated retry, budget refusal, stale wake, known and unknown
reconciliation, preinitialization cancel, an existing write fence and projection
replay. The table is independent of observed journals, discovers new deployed
handlers, and detects a removed retained journal. It is a named-path inventory,
not an exhaustive claim about all crash points or combinations of external failures.
The existing native CI email target includes the inventory.

## Terminal replay drains

Email owner state now retains the last drained revision alongside the authoritative
snapshot. A terminal replay resends the exact Bulk progress notice, SQL projection
and sweep registration, then retries encrypted-record retirement before advancing
that marker. Retirement always uses the original confirmation or completion time;
an interruption cannot extend the recovery horizon by substituting a retry clock.
The real native test interrupts a confirmed create while SQLite rejects retirement,
then repairs it through a terminal replay without a second POST or a changed receipt.
The current eight-test native owner run passed in 22.77 seconds
(`/tmp/bulk-email-drain-runtime4.log`), including that interruption proof and
the fixed 16-path/all-six-handler email journal inventory. The separate 60-row
cap proof above remains unchanged. The current signed Worker gate passed (`/tmp/bulk-email-drain-workerd.log`)
on `/tmp/platform-email-review-workflow-capture.log`. It repeats the actual D1
retirement failure and process kill, verifies the exact callback and original
retirement horizon on replay, then preserves all prior owner/fence/refund cases.
Its complete bounded privacy scan covered 1,028 raw journal rows, 33 retained
state rows, all 31 application D1 tables and full runtime/Restate logs.

Bulk progress notices carry source/owner identities only. The batch owner reads the
current exact submitted command and current generation, then derives final results
from the delivery owner. Email absence stays `NoLongerPending`; it never becomes
an accepted membership or a requester final result. Transient owner-read failures
queue a fresh delayed notice, because replaying the failed RPC alone would retain
its original error. Bounded progress pages only expose cached request results when
the whole retained target-generation set matches the current approved dispatch.

The current request progress suite passed four real Restate tests
(`/tmp/bulk-items-runtime5.log`): reissue resets the generation-bound cache,
old notices cannot restore stale final results, prewrite cancellation retains
Unfulfillable without rewriting original approval, and a killed terminal
request repairs its notification on replay. A transient 503 owner read journals
a fresh delayed notice; the test verifies scheduling, not a wall-clock 60-second
wait. Together the request and email fixtures walk 14 unique source-reviewed
Bulk paths across every one of its nine deployed handlers. Production workflow
wasm strict passed (`/tmp/bulk-callback-wasm-strict2.log`).

## Email operational clocks

`EmailDeliverySnapshot` now carries default-absent `unknown_since` and
`last_observed_at`. The first uncertain create retains its journaled completion
clock; discovering an existing write fence during cancellation records a separate
fixed `email_unknown_time` run. Legacy unknown receipts with no onset remain
explicitly incomplete. Approval and projection times never substitute for it.
A completed current-installation observation of a known pending invitation
advances the watermark without changing its create outcome, confirmation time
or settlement. An observation started before the retained watermark is ignored.
The current authoritative installation login binds renamed organizations while
the immutable original command remains unchanged.

Core five tests pass (`/tmp/email-telemetry-core.log`), and migration 0021 has
an independent raw SQLite proof (`/tmp/email-telemetry-raw-sqlite.log`). The
triggers compare UTC values with padded nanosecond keys: even a one-nanosecond
regression is rejected. Missing legacy clocks remain valid; an established
unknown onset cannot change or clear, and an observation cannot regress, clear
or precede approval/confirmation.

The actual signed Worker gate passed (`/tmp/email-telemetry-workerd.log`) on
`/tmp/m2-bulk-operations-workflow-capture.log`. It proves complete pending-list
observation and D1 projection, immutable unknown onset, all raw D1 temporal
negative controls, and the earlier callback/retirement/fence/refund cases. The
bounded privacy scan covered 1,098 raw journal rows, 33 retained state rows,
all 31 application D1 tables and full runtime/Restate logs. Operational report
and digest inclusion of email/bulk deliveries is documented separately by the
shared report implementation; no email acceptance or requester identity is inferred.

The real Restate watermark/rename regression passed in 6.53 seconds
(`/tmp/email-telemetry-runtime4.log`). It verifies still-pending observation,
exact replay, rejection of a stale absence, current numeric-installation rename
verification, neutral terminal settlement and SQL convergence without another
POST. Its first run exposed a fixture mistake: onboarding correctly reread the
live installation, whose stub still returned the old name. Updating the live
fixture metadata was required; merely supplying a changed name to onboarding
did not authorize it. The other eight email-owner tests passed in
`/tmp/email-telemetry-runtime2.log`, including the updated fixed16-path inventory.
Current workflow/email native strict and production Worker wasm strict passed
(`/tmp/email-telemetry-native-strict3.log`, `/tmp/email-telemetry-wasm-strict.log`).

## Retained email transition audit

P1 §14.1's `DeliveryBlocked{reason}` and `DeliveryOutcomeUnknown` apply to email
Deliveries too. Email delivery has no Held state, and the spec defines no
Throttled event. Repeated polling of the same blocking reason does not append an
event; a changed reason or a later new blocking episode does. Entering uncertainty
records one event at the immutable first-unknown time.

The email owner commits its snapshot, semantic cursor and pending facts in one
retained record. Existing journaled transition clocks supply the facts' original
times. Projection sends carry a default-empty flat-compatible envelope; pending
facts clear only after durable send acknowledgement. Legacy records establish a
cursor baseline without inventing missing historical events or approval-derived
timestamps. Existing terminal event IDs and metadata remain unchanged.

A separate transition namespace binds the physical email delivery key, transition
revision and phase. Metadata contains batch/row, numeric organization/team IDs,
revision and a bounded reason; it never contains an address, address digest or
`EmailRef`. The projector appends historical facts even if a newer snapshot is
already stored. An equal/stale snapshot does not UPDATE the immutable receipt.
Predecessor CAS, receipt update and audit append share a transaction; changed
payloads under an existing event ID roll back the entire projection.

Current native evidence:

- Cursor two tests pass in `/tmp/email-transition-unit.log`: repeated polling,
  changed reasons, new episodes and missing legacy history.
- Full shared SQLite suite passes in `/tmp/email-transition-shared2.log`,
  including delayed/equal audit append, exact replay and conflicting-payload
  rollback without receipt regression.
- Deterministic interleaved-projector CAS passes in
  `/tmp/email-transition-cas.log`: stale predecessor returns a retryable storage
  error, inserts no fact, and a fresh retry appends exactly once.
- Actual Restate two tests pass in `/tmp/email-transition-runtime3.log`: killing
  after the atomic blocked snapshot/fact but before projection, then recovering
  to Created, preserves the original event time; a committed SQL acknowledgement
  loss retries without duplicate facts. Unknown replay preserves one POST,
  one charged fence and the exact onset timestamp.
- Workflow library/email integration strict Clippy passes in
  `/tmp/email-transition-strict.log`.

The deterministic interruption controls exist only in the `integration` build
and require explicit fixture construction. No production environment or request
can enable them. The earlier telemetry capture does not establish these new audit transitions.

The fresh signed Worker audit/privacy gate passes in
`/tmp/email-transition-workerd.log`, using the immutable combined capture
`/tmp/member-facade-combined-workflow-capture.log`. It asserts exact unknown event
time, original blocked fact/reason and replay dedupe, while preserving the prior
write-fence, refund, retirement and neutral-settlement cases. The complete bounded
scan covers 1,173 raw journal rows, 33 retained state rows, all 30 application D1
tables and full logs. Audit rows additionally exclude the exact opaque EmailRefs.
The fixed ordinary email journal inventory still passes all six handlers and
16 paths (`/tmp/email-transition-journal.log`); production acquires no new run
names. Current workflow wasm strict passes
(`/tmp/email-transition-wasm-strict.log`).

Actual shared D1 conformance49 also passes on that capture
(`/tmp/member-facade-d1.log`), including this audit scenario's delayed/equal
append, immutable receipt and conflicting-fact rollback. The SQLx library/tests
strict rerun including the CAS regression passes
(`/tmp/email-transition-storage-strict.log`). These are local acceptance proofs;
they do not verify the outstanding live GitHub email-role or webhook gates.

## Interrupted Bulk email handoff recovery

The batch outbox now retains separate request, email and personal-link handoff
cursors after each durable enqueue acknowledgement. A completed early email may
retire its recovery record while a later row is temporarily unavailable; replay
continues at the first unacknowledged item instead of activating earlier rows
again. Older pending records default to zero cursors. To cover that boundary and
a kill between enqueue acknowledgement and cursor persistence, the batch reads
the email owner and validates its entire immutable command before resending its
idempotent `create`. A matching owner does not require ciphertext reactivation,
even after purge; a different command is refused with 409. Resending also drains
an interrupted child's projection and retirement work.

A database error during activation remains retryable. A definite false result
(missing, expired or retired recovery) still hands the exact accepted command to
the email owner, which independently refuses an unactivated recovery record as
`Blocked{EmailRecoveryUnavailable}` without claiming a write fence or sending a
GitHub invitation. The batch receipt and cancellation remain recoverable. No
ciphertext is restored, retention extended or unavailable delivery called Sent.

The deterministic actual-Restate regression first failed on the original code:
one email confirmed and retired, a database trigger blocked activation of the
second row, and the batch invocation was killed. After lifting the database
fault, the exact Submit timed out reactivating the retired first record
(`/tmp/bulk-handoff-red-runtime.log`, 6.64 seconds). With the fix, all three
focused tests pass (`/tmp/bulk-handoff-green-runtime.log`, 13.02 seconds), covering
that same interruption, an exact completed/purged owner, conflicting owner
commands, and missing/expired recovery with zero POSTs/fences and successful
cancellation. The original first receipt and retirement timestamps are retained;
exact Submit replay does not add writes. Raw journals exclude the address and
token markers.

The existing fixed journal checks also pass unchanged
(`/tmp/bulk-handoff-journal-runtime.log`, two tests): Bulk's deployed handler
inventory and email's 16 paths across all six handlers remain covered. The fault
trigger in that native proof is SQLx `test-util` only. A fresh signed Worker
proof now also passes (`/tmp/p1-final-gap-email-workerd.log`): an actual D1
activation fault holds row two while row one confirms and retires; killing the
real Bulk Submit and replaying it preserves its acknowledged cursor, both child
receipts and the first retirement/purge clocks, with exactly two POSTs. Missing
and expired mailbox cases retain precise Blocked outcomes, accepted Submit
replay and cancellability, without any fence, charge or POST. The trigger is
restored in a finally block. All inherited email cases remain in the same run;
its bounded privacy scan covers 2679 raw journals, 75 state rows, all 30 D1
application tables and complete runtime logs. No deployment or old endpoint
drain is claimed.

Focused workflow library plus email integration strict Clippy passes on the
changed source (`/tmp/bulk-handoff-strict.log`, `-D warnings`).
