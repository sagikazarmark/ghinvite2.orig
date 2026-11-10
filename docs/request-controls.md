# Retained request controls and cancellation

The request owner receives complete link-control snapshots, binds them to its
link, request, admission time and immutable invitation lifetime, and records an
exact operation receipt. A control received before initialization stays in the
inbox. Conflicting operation reuse or immutable facts fail closed. Revision and
hold-epoch checks reject rollback and prevent a released hold from returning.

Initialization forwards the current control before creating any delivery. Later
controls are sent one-way to each known target/generation from the retained plan
and submitted commands. The owner does not wait for a delivery result or call
back into the link. Current M0 plans use generation1; retained later generations
are honored without synthesizing new generations.

## Pending request cancellation

`Revoked { decline_pending_requests: true }` cancels a Pending request with the
typed reason `LinkRevoked`. Its absolute decision deadline takes precedence at
the evaluation time. Cancellation records one terminal decision and one
`request.cancelled` audit fact, carrying only the bounded reason. It never
rewrites an approval, decline, expiry, admission receipt or old decision receipt.
A later approve/decline command receives `NotPending`; its exact replay retains
that result. An already scheduled expiry notification cannot change cancellation.

Core SQL checks state/reason consistency, immutable request identity and the
complete terminal decision across projections. A delayed Pending projection
cannot reopen the request, and a repeated audit event has the same identity and
content. SQLx and D1 execute these same statements. Migration0006 adds the
nullable reason and insert/update consistency guards. It does not infer a reason
for historical data.

The Console history and requester page explain that the request was cancelled
because the link was revoked. This operation has no GitHub effect; delivery
cancellation uses its own fence and executor.

## Verification and remaining integration

The shared conformance scenario verifies cancellation, stale projection repair,
reason/history decoding, once-only audit and refusal of terminal rewrites. The
real Restate request-control suite verifies pre-initialization cancellation,
control replay, unchanged approval receipts, delayed expiry and deadline
precedence, in addition to control-before-create dispatch ordering. Exact logs
and actual D1 evidence are recorded in the implementation ledger.

The revoke caller/confirmation flags, delivery cancellation, fixed cascade
progress denominator, org delivery parity and live release gates remain open.
The existing DecisionReceipt gains `NotPending`; this is not a claim that the
full Appendix A.2 error API has migrated. New journal paths and payloads require
immutable endpoint pinning or draining before release (ID-21/43).

## Operation-bound revoke and progress

`RevokeCommand` requires the link, numeric administrator assertion, operation ID,
and all three cascade booleans. Exact input replays return the retained success or
refusal. Reusing an operation with changed options conflicts; an unrelated operation
has no receipt merely because the link is already revoked. The Console confirmation
uses native CSRF POST, requires `REVOKE` for selected cascades, and retains the same
operation through validation and acknowledgement recovery.

The link closes admissions and retains bound revocation work before reading request
manifests. Once captured, the request IDs and delivery identities (including every
known dispatched generation) are immutable. Capture is a distributed cut after
closure, not a simultaneous snapshot across objects. Already-terminal requests
remain unchanged and count done. Later dispatch honors the retained full control
without expanding accepted counts. Progress reads require the captured identity and
control revision; unavailable observations and uncertain writes are not completion.

Confirmation counts are explicitly delayed SQL previews. The accepted receipt and
once-only cascade audit use a separate core-owned atomic projection, with exact
content and revision guards. This projection never performs request cancellation.
Normal composition currently refuses invitation cancellation with
`UnsupportedCascade`; explicit preactivation composition can enable the independently
fenced executor. M2 access removal remains refused. Neither an accepted intent nor a
progress query claims that a GitHub DELETE succeeded.

Local evidence: actual Restate operation/cascade test passed
`/tmp/revoke-cascade-runtime-final.log` (2 tests), covering immutable 2-request denominator,
approved versus pending, exact receipts, conflicts, unrelated-operation status,
refusal before mutation, and unavailable-owner progress remaining fixed at addressed2/done0/unavailable2 until recovery. The second test enables the standard endpoint's explicit
budget+cancellation composition and proves full link-to-delivery cascade, exactly one
PUT and DELETE, unchanged creation receipt and fixed 1/1 progress. Native confirmation document/authority/CSRF coverage
passed `/tmp/platform-revoke-web-fixtures2.log` (11 tests). Current shared32 SQLite and actual D1 pass `/tmp/revoke-shared-native2.log` and
`/tmp/revoke-shared-d1.log`. Chromium confirmation5 and mutation recovery1 pass
`/tmp/revoke-browser2.log` and `/tmp/revoke-mutation-browser.log`. Signed Worker
full link cascade passes within `/tmp/platform-cancellation-cascade-workerd.log`.
Native and workflow strict checks pass `/tmp/revoke-native-clippy.log` and
`/tmp/revoke-workflow-clippy-final.log`.
These retained command/receipt/journal changes require ID-21 endpoint pinning or drain;
no live deployment or organization parity is established by these tests.
