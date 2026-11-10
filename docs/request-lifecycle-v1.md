# Request authority

`request_owner::bind` registers `InvitationRequest`, a Virtual Object keyed by
request ULID. Private ingress supplies authenticated requester identities and
currently verified account-admin assertions. The concrete web `RequestAuthority`
calls the owner for decisions and delivery progress; link requester-page reads
inspect the request owner before returning lifecycle status.

## Retained records

- Immutable initialization context: admission time, requester, link/account,
  original installation provenance, repositories, permission and approval policy.
- Current request snapshot and terminal decision, independent of SQL projection.
- Request-scoped, canonical-input-bound decision receipts (actor/action/reason).
- Input-bound admission eligibility verdicts with their original evaluation time.
- Immutable approved manifest and per-repository durable-send progress.

Manual requests have an absolute seven-day deadline established at admission.
Decisions strictly before it may apply; equality or later expires pending state.
A complete journaled decision remains binding after recovery past deadline.
Inspection materializes overdue expiry. Early delayed messages schedule another
wake; terminal states are immutable. Auto-approval is an admission-time fact with
no pending deadline. No handler sleeps awaiting an administrator.

The link's latest-request pointer is an index. A fresh admission gets a retained
operation-bound verdict from the request. A blocking verdict determines the
attempt's existing-request rejection and evaluation time, even if publication is
interrupted across the deadline. A terminal verdict permits admission subject to
current link guardrails. Missing expected request authority means retryable
uncertainty. Requests never call back synchronously to the link or release its
pointer asynchronously.

Approval retains the entire manifest before arranging durable dispatch. Dispatch
does not await GitHub completion. `InvitationRequest/<id>/recover`, or the private
`DeliveryRecovery/recover` ingress, resubmits the retained commands explicitly.
Admission replay never redrives delivery. GitHub effects remain protected by the
repository-delivery owner and SQL/D1 attempt fence.

`RequestProjection` applies owner snapshots and immutable audit facts atomically.
Terminal snapshots can create missing rows once link/user parents exist. Stale
snapshots cannot regress state and still publish missing audit facts;
equal-revision content differences are invariant failures.

## Verification and deployment

Run `scripts/test-restate.sh request_owner`, `authoritative_admission`,
`canonical_link`, `retained_delivery`, and `durable_projection`. The shared storage
suite includes request projection ordering/conflicts on SQLx and actual D1.

This is a clean prelaunch cutover. Native and Worker deployments require matching
web/workflow artifacts, fresh migrations and a fresh Restate environment. The old
Workflow registration, terminal promises and link decision/dispatch handlers are
removed; existing retained state is not imported. Environment reset remains an
explicit operator action.


## Direct decision account boundary (ID-31)

Trusted web ingress remains responsible for current per-target administrator
checks. Fresh `InvitationRequest/decide` also calls the shared retained-state-only
`AccountInstallation/repo:<account>/decision_authority` handler. Missing,
uninstalled or suspended context refuses; a personal account requires its numeric
owner; an organization requires its signed Installing Owner or enabled Repository
Admin Delegation. This call performs no GitHub or SQL read and cannot call back
into the request owner. It grants no implicit org-app membership bypass.

The existing wire adapter returns `DecisionReceipt.outcome = not_authorized` with
the unchanged request snapshot. It retains that refusal by operation ID. Exact
input replay returns the original successful or refused receipt before consulting
current authority; changed input conflicts, and a fresh operation observes the
current owner/delegation setting. A refused decision does not expire, project,
change or dispatch the request. The normal timer still owns deadline progress.

The actual Restate `installation_authority` scenario covers delegation off/on,
refusal and success replay across changes, missing and uninstalled contexts, owner
replacement, and reinstall without inherited ownership. Its explicit named-run
table covers `decide` only: fresh authorized decline executes `decision_time`,
`record_decision`; refusal and exact replay execute no named run. The new durable
call is a journal change despite unchanged named-run names: deploy with endpoint
pinning/draining under ID-21, never infer compatibility from this current table.

## Explicit requester-visible decline messages

New `Decline` actions may carry `requester_message`, independent of the retained
administrator-only `reason`/`decline_reason`. Missing or blank public text remains
absent. Normalization trims surrounding whitespace and normalizes CRLF to LF;
the bound is 500 Unicode characters, with control characters other than newline
and tab rejected. Normalization occurs before retained operation comparison.
The completed decision fixes the public message; changed same-operation input
conflicts, and a new operation cannot replace a terminal decision's message.

Old actions and decisions decode with `requester_message = None` and reserialize
without a new property. Migration0026 adds a separate nullable projection column,
without copying historical notes. Projection and raw terminal guards preserve
that separation, including old decisions with a private reason. This is additive
schema work; apply it before serving the updated projection SQL. It does not
remove the endpoint pin/drain requirements for existing workflow invocations.

The Console explicitly labels the optional textarea as shown to the requester.
CSRF and current target/account authority still guard dispatch, and unknown-result
retries retain the exact normalized message. The authenticated `/join` and
`/requests/{id}` views show escaped public text only for their own declined request.
Both request-owned status paths strip historical private decline notes. The
request-ID status handler also omits domain evidence. These HTML views render the
explicit message, not administrative context. Authorized Console history can
distinguish the public message from the private note.

The focused real-owner test also fixes the new `requester_status` journal contract:
owned Pending, Declined and overdue Expired reads execute `evaluation_time`;
foreign requester and missing request reads execute no named run. This targeted
assertion does not stand in for the separate full service handler inventory.

Verified local evidence (ID-66):

- The actual Restate owner case passes (`/tmp/requester-message-owner-runtime.log`), including exact original success/refusal replay, changed-input conflicts, current administrator checks and the targeted journal sequences above.
- Both core compatibility/normalization tests pass (`/tmp/invitation-timing-ghinvite-core-requester_message_tests.log`); legacy decision/action bytes remain identical.
- The new Console HTTP case passes in the full 122-test Console run (`/tmp/pending-navigation-console_flow-runtime.log`), covering CSRF, bounds, authenticated actor binding and exact unknown-result retry.
- All38 requester HTTP cases pass, with two fixture servers ignored (`/tmp/requester-message-http-runtime.log`). Both requester routes display escaped explicit text, hide private notes and deny another requester. The initial test expected named HTML entities while Dioxus emits numeric entities; only that exact escaping expectation was corrected, with raw-script and private-note checks retained.
- Shared SQLite51 and actual D151 pass (`/tmp/requester-message-storage51-runtime.log`, `/tmp/requester-message-timing-d151.log`). The raw D1 guard refuses replacing/clearing a terminal message and backfilling an old private note, preserving every original row. The same run retains the existing raw migration/audit/telemetry guards and includes the separate timing guards.

These are local tests and captured Worker/database proofs, not live acceptance or deployment. No historical note is relabeled as public.
