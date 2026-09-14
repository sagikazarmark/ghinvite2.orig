# Approved-request delivery recovery

The versioned endpoint opts in by binding `admission_v1`, `projection_v1`,
`request_lifecycle_v1`, and `delivery_v1::bind(builder, state)`. Keep ingress private.
Deployment of these services together is required: durable sends cannot complete
if the receiving service has not been registered. Legacy writers remain subject
to the controlled cutover in ADR 0003 and #58.

## Retained authority

- Link `v1/dispatch/<request>` contains the immutable approval-bound plan, including
  one invitation ID per repository. Scope is limited to 100 at link creation.
- Link `v1/submitted/<invitation>` stores the first acknowledged durable invocation
  identity. This means submitted, not GitHub success.
- Link `v1/consumed/<request>` records lifecycle consumption after fan-out (or
  after observing a non-approved terminal decision).
- `GithubCreateV1/<invitation>` retains `v1/input` and `v1/receipt` without TTL.
  The receipt is independent of the database invitation lifecycle.
- `delivery_attempts` is an input-bound database **write fence**, not the receipt
  authority. Claiming an attempt generation permits one PUT. If its own
  acknowledgement or the subsequent HTTP result is lost, recovery reads GitHub
  evidence and never repeats that PUT. This deliberately permits conservative
  outcome-unknown results even when a crash occurred before the HTTP send.
  An explicit 401/403/404/429 rejection releases only that exact generation for
  a later availability/rate-limit retry. A stale rejection cannot release a newer
  attempt. Transport errors and uncertain responses never release the fence.
- `delivery_outcomes` is a revisioned SQL read projection. A confirmed receipt
  can repair it by replay without external writes; lower revisions are ignored.

The fence must be backed up alongside Restate and must never be reset or expired.
Independent database restore requires maintenance and reconciliation of all
unfinished attempts before writes reopen. A Restate backup alone cannot recover
unjournaled HTTP effects. Missing state is never permission to generate new IDs.

## Ordinary and explicit recovery

Ordinary Restate replay finishes interrupted sends and checkpoints. For deliberate
repair after workflow retention, call the private ordinary service
`DeliveryRecoveryV1/recover` with `{link_id, request_id, requester_id}` from a trusted
operator/caller. It obtains the retained plan, checks receiving receipts for input
conflicts, durably resubmits original commands, and records submission checkpoints.
It never starts `InvitationRequestV1/run`. Its response describes submission only;
read `GithubCreateV1/<id>/status` or the current request page for delivery outcomes.

Blocked creates schedule a one-hour dependency recheck; explicit recovery may
check sooner. Unknown outcomes only perform read-only reconciliation on recovery.
Pending invitations are matched on numeric requester identity and permission;
membership evidence must likewise include numeric identity and permission.
Absence or failed reads retain unknown. Current collaborator evidence confirms
current access, not the historical provenance of an invitation. No distributed
exactly-once HTTP transaction is claimed.

During controlled import, call `InvitationLinkV1/<link>/retain_dispatch` with the
complete historical plan before workflow fan-out. It validates scope/approval and
rejects changed input or IDs once bound. Existing SQL Sent rows with upstream IDs
are affirmative create evidence; ambiguous historical rows are fenced before
reconciliation. Never infer a successful original create from decline/expiry alone.

## Notification retention and orphan promises

`notification_needed` validates the terminal fact and checks the retained consumed
checkpoint; the workflow's notify handler consults it before resolving an absent
promise. Deliberate late redelivery after consumption is suppressed. Read private
`InvitationRequestV1/<request>/notification_status` to observe retained promise
state without starting the run handler.

A notify invocation already in flight at cleanup may have journaled an earlier
`needed=true`, so it can still leave an orphan promise. For cleanup, first fence
notification ingress for that request, drain outstanding notify invocations, verify
the authoritative consumed checkpoint, and confirm no workflow run is active using
Restate's invocation inspector. Export state before removing only that workflow's
orphan state through the Restate administrative state API. Never purge link or
invitation-object state, never restart the workflow to clean a promise, and never
interpret an absent promise as loss of the authoritative decision. Exact destructive
management commands remain part of the #58 cutover rehearsal.

## Verification

`bash scripts/test-restate.sh retained_delivery` exercises real Restate 1.7.9,
native SQLx, and the GitHub HTTP stub: stable plans, 201/204/422, ambiguous 502,
HTTP-result and SQL-acknowledgement loss, Sent/declined replay, changed payload,
partial fan-out and send-checkpoint interruption, blocked scope restoration,
numeric identity/rename validation, and recovery after actual workflow cleanup.
`authoritative_admission` additionally checks missing delivery projection
prerequisites do not block link commands or workflow submission. Actual Worker/D1
verification is tracked in #59; compilation is not runtime evidence.

#49's approved policy is recorded in ADR 0004. #60 implements admission availability
enforcement and installation-event convergence; #57 handles full browser cutover.
