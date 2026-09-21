# Approved-request delivery recovery

`build_endpoint` binds `admission`, `projection`, `request_lifecycle`,
and `delivery::bind(builder, state)`. Keep ingress private. Deployment of these
services together is required: durable sends cannot complete if the receiving
service has not been registered.

## Retained authority

- Link `v1/dispatch/<request>` contains the immutable approval-bound plan, including
  one invitation ID per repository. Scope is limited to 100 at link creation.
- Link `v1/submitted/<invitation>` stores the first acknowledged durable invocation
  identity. This means submitted, not GitHub success.
- Link `v1/consumed/<request>` records lifecycle consumption after fan-out (or
  after observing a non-approved terminal decision).
- `GithubCreate/<invitation>` retains `v1/input` and `v1/receipt` without TTL.
  The receipt is independent of the database invitation lifecycle.
- `delivery_attempts` is an input-bound database **write fence**, not the receipt
  authority. Claiming an attempt generation permits one PUT. If its own
  acknowledgement or the subsequent HTTP result is lost, recovery reads GitHub
  evidence and never repeats that PUT. This deliberately permits conservative
  outcome-unknown results even when a crash occurred before the HTTP send.
  An explicit rejection releases only that exact generation for a later
  availability/rate-limit retry: a 401/403/404 refusing access, or any throttled
  refusal (see **GitHub throttling**), which a 429 always is. A stale rejection
  cannot release a newer attempt. Transport errors and uncertain responses never
  release the fence.
- `delivery_outcomes` is a revisioned SQL read projection. A confirmed receipt
  can repair it by replay without external writes; lower revisions are ignored.

## Create-outcome audit history

Confirmed receipts retain `confirmed_at` in the invitation object before database
projection. Created, already-collaborator, and definitive-failure outcomes publish
`invitation.sent` (System), `invitation.accepted` (GitHub), and
`invitation.send_failed` (System), respectively. The target is the internal GitHub
invitation ID and account scope comes from the immutable create command. Blocked
and outcome-unknown receipts publish none of these events.

The audit ID is the first 128 bits of SHA-256 of
`ghinvite/delivery/audit/v1/<invitation_id>`, encoded as a ULID for existing audit
cursors. This mapping and the event metadata contract are versioned, retained
protocol: changing them requires explicit migration. Metadata contains only
repository name, numeric requester identity, and upstream invitation ID or a safe
outcome reason. Audit browsing renders allowlisted summaries, never raw metadata,
request justification, internal notes, or GitHub diagnostics.

SQLx transactions and D1 batches apply receipt/lifecycle and audit insertion
atomically. Audit insertion runs even for stale receipts or a lifecycle already
beyond Sending; identical event content replays, conflicting content fails the
whole projection. Ordinary retry and `DeliveryRecovery/recover` repair missing
history without another GitHub write, including after workflow retention cleanup.

Pre-#64 receipts lack a reliable original confirmation time. On first create
replay, the receiver journals a recovery observation time, retains it with
`recovered: true`, and advances the receipt revision once. Historical SQL `Sent`
rows with upstream IDs are likewise marked recovered. Console history labels
these entries **Confirmed outcome observed during recovery**; their time is not
the original send time. Existing audit history is preserved.

The fence must be backed up alongside Restate and must never be reset or expired.
Independent database restore requires closing writes and reconciling all
unfinished attempts before writes reopen. A Restate backup alone cannot recover
unjournaled HTTP effects. Missing state is never permission to generate new IDs.

## Ordinary and explicit recovery

Ordinary Restate replay finishes interrupted sends and checkpoints. For deliberate
repair after workflow retention, call the private ordinary service
`DeliveryRecovery/recover` with `{link_id, request_id, requester_id}` from a trusted
operator/caller. It obtains the retained plan, checks receiving receipts for input
conflicts, durably resubmits original commands, and records submission checkpoints.
It never starts `InvitationRequest/run`. Its response describes submission only;
read `GithubCreate/<id>/status` or the current request page for delivery outcomes.

Blocked creates schedule a one-hour dependency recheck, or the throttling wait
below when that is what blocked them; explicit recovery may
check sooner. Unknown outcomes only perform read-only reconciliation on recovery.
Pending invitations are matched on numeric requester identity and permission;
membership evidence must likewise include numeric identity and permission.
The pending-invitation read follows every GitHub page, so absence means absence
from the whole listing; a failed, malformed, or unfollowable later page is a
failed read, not absence. Absence or failed reads retain unknown. Current collaborator evidence confirms
current access, not the historical provenance of an invitation. No distributed
exactly-once HTTP transaction is claimed.

Existing SQL Sent rows with upstream IDs are affirmative create evidence;
ambiguous rows are fenced before reconciliation. Never infer a successful
original create from decline/expiry alone.

## GitHub throttling

A 403 or 429 carrying documented rate-limit evidence is classified at the
transport boundary as throttling rather than as an answer about the request.
`retry-after` and the 429 status address the request itself; the rate-limit
wording in the body names the limit. `x-ratelimit-remaining: 0` is weaker: the
quota headers ride along on every response, so a permission refusal served as the
hour's last request reports an exhausted quota too. An exhausted quota therefore
only counts where GitHub named no other reason, and a 403 with no evidence at all
remains a permission refusal.

The wait follows whichever evidence describes it. A `retry-after` — seconds or an
HTTP-date — names this request's wait and wins outright. Failing that, only an
exhausted quota makes `x-ratelimit-reset` this request's wait; the reset rides
along on a healthy quota too, and idling out an untouched hour is worse than the
unguided backoff. A numeric `retry-after` is already a duration; the HTTP-date
form and the reset are read against the response's own `date`, so a skewed local
clock cannot shorten them. The resulting wait is clamped to between one second
and one hour. Each wait is bounded; the retries are not. Giving up would settle
an invitation on a limit never shown to be permanent, which is the failure this
policy exists to prevent.

Sending `retry-after` at all cites a limit, even where the value yields no usable
wait: discarding that evidence would leave a refusal looking permanent. The scope
records which limit GitHub cited; the wait may still come from the quota reset,
because which limit was cited and when the request can next succeed are different
questions.

Delivery records a `blocked` receipt reading `GitHub throttled delivery` and rechecks after
the same wait instead of the hourly dependency cadence; a throttled *read* during
reconciliation earns that recheck too, because rereading is safe and is the only
way a delivery GitHub would not let us observe resolves on its own. Exactly one
recheck stands per invitation: a timer already sent cannot be withdrawn, so a
sooner one would mean two, and telling which is stale needs a due time and a
token on `recheck` — retained protocol, for a case only explicit recovery during
a blocked window reaches. A throttle observed then waits the pending hour out;
nothing is settled wrongly by it.

The settlement sweep waits a limit out once per account, not once per row or
once per sweep: a quota belongs to the installation, so one wait covers that
account's remaining rows and says nothing about any other. An account throttled
again after its wait has its remaining rows left to the next sweep rather than
spending more of a quota it has already run out of. Either way the wait is a
durable continuation or a service-side timer, never a held retry: no handler
keeps an invitation object's lock across a rate-limit window.

Only a response classifies. Transport errors, timeouts, and every other uncertain
result stay outcome-unknown and keep the write fence, so throttling handling can
never conclude that an ambiguous PUT was not applied.

Settlement's `cancel` and `tick_expire` are not covered by the continuation
policy above. A throttled DELETE or listing is transient there, so Restate
retries it inside the invitation object's lock — the same way a 5xx or a 429
already did before throttling was classified. That is deliberate: the alternative
is settling a cancellation GitHub never performed, which is the failure this work
exists to prevent, and the lock is one invitation's rather than the account's.
Giving those handlers their own bounded continuations is follow-up work, not part
of #80's delivery and observation scope.

## Notification retention and orphan promises

`notification_needed` validates the terminal fact and checks the retained consumed
checkpoint; the workflow's notify handler consults it before resolving an absent
promise. Deliberate late redelivery after consumption is suppressed. Read private
`InvitationRequest/<request>/notification_status` to observe retained promise
state without starting the run handler.

A notify invocation already in flight at cleanup may have journaled an earlier
`needed=true`, so it can still leave an orphan promise. For cleanup, first fence
notification ingress for that request, drain outstanding notify invocations, verify
the authoritative consumed checkpoint, and confirm no workflow run is active using
Restate's invocation inspector. Export state before removing only that workflow's
orphan state through the Restate administrative state API. Never purge link or
invitation-object state, never restart the workflow to clean a promise, and never
interpret an absent promise as loss of the authoritative decision.

## Verification

`bash scripts/test-restate.sh retained_delivery` exercises real Restate 1.7.9,
native SQLx, and the GitHub HTTP stub: stable plans, 201/204/422, ambiguous 502,
HTTP-result and SQL-acknowledgement loss, Sent/declined replay, changed payload,
partial fan-out and send-checkpoint interruption, blocked scope restoration,
numeric identity/rename validation, throttled delivery resuming on its own
scheduled recheck, and recovery after actual workflow cleanup.
`authoritative_admission` additionally checks missing delivery projection
prerequisites do not block link commands or workflow submission.

For #64, `cargo test -p ghinvite-storage-sqlx --test sqlx_suite` and `--test projection`
verify all confirmed audit outcomes, uncertain/blocked exclusion, stale event
delivery, immutable-content conflicts, old serialized receipt compatibility,
audit-write rollback and retry. `retained_delivery` checks the same history through
real Restate, including stable recovery observations and workflow cleanup.
`npm run test:admission --prefix tests/worker` runs the shared conformance scenario
on actual D1, injects audit insertion failures into D1 batches, and checks events
from actual Worker GitHub 201/204/422 delivery. Console HTTP
summary/privacy coverage is in `console_flow` (`audit_` filter).

#49's approved policy is recorded in ADR 0004. #60 implements admission availability
enforcement and installation-event convergence; #57 moved the browser onto
authoritative admission.
