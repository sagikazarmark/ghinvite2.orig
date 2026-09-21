# Authoritative request lifecycle (#55)

`build_endpoint` binds `admission::bind`, `projection::bind`, and
`request_lifecycle::bind` on the private Restate endpoint. The web
`AppState::new` always uses the Restate request lifecycle
(`AppState::with_request_lifecycle` only substitutes a test double). Its admin
routes retain current GitHub account-admin authorization and CSRF checks.

## Commands and replay

`InvitationLink/<link_id>/decide` takes version 1, request ID, a strict ULID
operation ID, verified account/admin IDs, and `approve` or `decline { reason }`.
The identity is `(link_id, lifecycle operation ID)` in its own command namespace.
Canonical input binds request, actor, action, version and trimmed optional reason.
Missing/empty reasons normalize to absent; timestamps are not accepted. Reusing
an identity with different input returns generic conflict. Authorization precedes
receipt lookup. Receipts are retained without automatic expiry.

The result distinguishes `applied`, `already_completed` (same action and, for a
decline, same normalized reason), and `incompatible`. Replaying the same operation
returns its original result. Another authorized admin can observe a matching
already-completed action without rewriting its original actor. An incompatible
decision returns current state, not a claim of approval. Imported cancelled states
remain terminal and retry-eligible, with no new cancellation command.

Pending deadlines are snapshotted at admission, seven days independently of link
expiration. The complete journaled decision samples current time: strictly before
deadline allows an admin decision; equality/later expires. Expiry's effective time
is the deadline and evaluation time is recorded separately. Exclusive status and
fresh admission materialize expiry; admission receipt replay does not. Combined
expiry/readmission touches at most two requests. Uses are never refunded.

## Projection, workflow, and handoff

One immutable logical terminal event is projected with the terminal request.
Decision actor/time/reason update the existing SQLx/D1 request columns, with the
same revision and identity checks as other projected fields. Audit payloads exclude
decline reasons. Requester status removes the reason; the web page renders only
lifecycle state. `/i/<code>?request_id=<id>` can read an acknowledged request before
its projected row arrives. The complete [v1 browser path](browser-admission-v1.md)
also resolves fresh link codes and attempts directly through Restate.

The workflow reads exclusive authority on startup and after any wake. A direct
`terminal` promise carries identity/revision, never approval authority. The link
only awaits durable one-way sends, so workflow-to-link calls cannot form a
synchronous cycle. Early timers recheck and wait the remaining absolute duration;
delayed execution immediately rechecks eligibility. There is no scheduler latency
guarantee. Auto-approved requests never enter the pending wait.

The wake target is absolute. Immediately when constructing the SDK sleep syscall,
the adapter recomputes its relative argument from the current clock without
branching on that clock. SDK 0.10 journals the sleep's absolute timestamp and its
replay comparison ignores the timestamp (shared-core `SleepCommandMessage`
`header_eq` compares the name). Thus an already journaled sleep retains its target,
while interrupted setup creates a due sleep rather than replaying a stale duration.
Native tests interrupt precisely between the target journal and sleep construction.

`prepare_dispatch` returns a stable approved handoff and retains it under
`v1/dispatch/<request_id>` in the link object. The workflow result exposes that
handoff. This is an authorization/input checkpoint, not evidence of submission or
GitHub delivery. #56 extends it with per-repository dispatch and receiving receipts.
Workflow journal/promise cleanup cannot delete link-owned receipts, terminal state,
or handoff. Admission replay never restarts a workflow. Late notifications may
recreate orphan promise state after cleanup; they do not start the workflow.

## Verification

`bash scripts/test-restate.sh authoritative_admission` exercises the real native
runtime in request-response mode, controlled exact deadline boundaries, combined
expiry/rejection, interrupted journal/write/send recovery, direct notifications,
absolute timers, and workflow retention versus link authority.
`bash scripts/test-restate.sh durable_projection` verifies lifecycle projection
through actual SQLx writes, outage, invariant repair, and lost acknowledgements.
Web tests cover link-keyed calls, current authorization, incompatible responses,
and authoritative requester status with no projected request. Worker/D1 runtime
execution remains a separate rollout verification obligation.
