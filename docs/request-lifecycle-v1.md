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
