# Approved-request delivery recovery

`build_endpoint` binds `admission`, `projection`, `request_owner`,
and `delivery::bind(builder, state)`. Keep ingress private. Deployment of these
services together is required: durable sends cannot complete if the receiving
service has not been registered.

## Retained authority

- Request `plan` contains the immutable approval-bound plan, including
  one invitation ID per repository. Scope is limited to 100 at link creation.
- Request `submitted/<repository>` stores the acknowledged durable invocation
  identity. This means submitted, not GitHub success.
- `RepositoryDelivery/<request>:<repository>` retains the bound command, original
  create receipt, current snapshot and terminal settlement audit without TTL.
  First use binds the complete command issued by the private request-owned
  dispatch path. Bound retries use that input without fetching a plan again.
  The command contains immutable numeric account/requester/repository identities,
  repository address, permission, approval ID/time and installation provenance.
  The create receipt remains unchanged by subsequent invitation settlement.
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
- `DeliveryProjection/<request>:<repository>/apply` is an independent durable queue, bound
  by `delivery::bind`. Create retains its input-bound receipt (including stable
  audit facts), durably sends projection and any continuation, then returns.
  SQL dependency, invariant, or availability failures retry in that queue and
  never prevent the create owner from handling recovery commands. Confirmed
  replay sends the same receipt for repair without contacting GitHub.

Retained `RepositoryDelivery/status` reports create history and current lifecycle during a
projection outage. SQL-backed views may show the last projected observation or
updates unavailable; projection failure is not a definitive GitHub failure.

Delivery reads `AccountInstallation/current_installation`, a shared retained-only
read, for the current installation of the command's numeric account. Fresh
onboarding establishes this context without SQL adoption. Missing retained
authority returns recovery uncertainty (503); known uninstall blocks execution.
The original installation ID is provenance, so same-account reinstall uses the
replacement credentials with the original command and delivery ID. Before an
effect or reconciliation, GitHub must confirm the current numeric installation
and account, repository ID, and requester ID at its freshly resolved login.

**Read-projection independence is not safety-fence database independence.**
Delivery does not read projected requests, links, users, installation rows, or a
SQL `Approved` flag. It still needs SQL/D1's input-bound `delivery_attempts` fence
before a new PUT. Fence unavailability produces zero PUTs. Fresh claim and the
conditional PUT remain in one re-executable effect step; a lost claim or HTTP
acknowledgement remains conservative unknown. A retained unknown receipt itself
also prohibits writes, even if read projections or a fence row disappear.

This is a clean prelaunch protocol replacement, not live-state migration. Deploy
matching native/Worker account, link, create and projector registrations together
in a fresh environment and onboard accounts through the verified setup path.
Pre-seeding SQL installations is not onboarding or authority reconstruction.

## Create-outcome audit history

Confirmed receipts retain `confirmed_at` in the invitation object before database
projection. Created, already-collaborator, and definitive-failure outcomes publish
`invitation.sent` (System), `invitation.accepted` (GitHub), and
`invitation.send_failed` (System), respectively. The target is the internal GitHub
invitation ID and account scope comes from the immutable create command. Blocked
and outcome-unknown receipts publish none of these events.

The audit ID is the first 128 bits of SHA-256 of
`ghinvite/delivery/audit/<invitation_id>`, encoded as a ULID for existing audit
cursors. This mapping and the event metadata contract are retained
protocol: changing them requires explicit migration. Metadata contains only
repository name, numeric requester identity, and upstream invitation ID or a safe
outcome reason. Audit browsing renders allowlisted summaries, never raw metadata,
request justification, internal notes, or GitHub diagnostics.

SQLx transactions and D1 batches apply receipt/lifecycle and audit insertion
atomically, including establishing the initial invitation row and validating its
request/repository identity. Callers have no read/check/insert ordering obligation.
Missing parents return `ProjectionDependency`. Immutable identity conflicts,
equal-revision differing receipts, and differing immutable audit content return
`ProjectionInvariant`; both retry outside the delivery owner. Duplicate receipts
converge, including after commit acknowledgement loss. Creation time is the
retained approval time; an initial confirmed lifecycle advance uses the retained
confirmation time for `updated_at`. Later settlement is never overwritten.
Audit insertion runs even for stale receipts or a lifecycle already
beyond Sending; identical event content replays, conflicting content fails the
whole projection. Ordinary retry and `DeliveryRecovery/recover` repair missing
history without another GitHub write, including after workflow retention cleanup.

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
read `RepositoryDelivery/<request>:<repository>/status` or the current request page for delivery outcomes.

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

A `github_invitations` row is never create evidence: only the receiving object's
own projection writes it, after the receipt it projects is retained, so it never
knows more than `receipt`. Without a confirmed receipt, a retained unknown outcome
or existing write fence requires read-only reconciliation. Never infer a successful
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
recheck stands per invitation. The retained `recheck_scheduled` generation is the
receipt revision that arranged the timer; `recheck` carries `{command, generation}`.
Only that generation may consume the timer and advance delivery. Duplicate or
stale wakes are no-ops; confirmed/terminal receipts only arrange projection repair.
Explicit recovery keeps an existing timer, even if new guidance would be sooner.
A throttle observed then can wait the pending hour out. Restate journals the state
and delayed send, so interruption resumes the same scheduling obligation without
holding the create lock during the wait. This is a coordinated fresh-environment
protocol change; register the matching create and projection handlers together.

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

## Request retention

The request VO retains decisions, manifests and submission progress independently
of invocation cleanup. There are no terminal promises or notification-consumption
records. Explicit delivery recovery resubmits the original request-owned manifest;
admission replay only returns the original receipt. See [request authority](request-lifecycle-v1.md).

## Verification

`bash scripts/test-restate.sh approved_delivery` checks delivery with all four
projected parent records withheld, independent projection convergence, partial
two-repository progress, exact first-use/replay identity checks, same-account
reinstall, current requester login, account/requester mismatches, stale SQL Pending
alongside retained approval, fence outage, applied PUT with lost result and no
remaining invitation, and read-only unknown replay with missing projections/fence.
`npm run test:delivery --prefix tests/worker` verifies withheld parents, real HTTP
delivery, applied PUT/result loss, read-only recovery and convergence on Worker/D1.

`bash scripts/test-restate.sh retained_delivery` exercises real Restate 1.7.9,
native SQLx, and the GitHub HTTP stub: stable plans, 201/204/422, ambiguous 502,
HTTP-result and SQL-acknowledgement loss, Sent/declined replay, changed payload,
partial fan-out and send-checkpoint interruption, blocked scope restoration,
numeric identity/rename validation, throttled delivery resuming on its own
scheduled recheck, and recovery after actual workflow cleanup.
It also holds receipt/audit projection unavailable through blocked delivery,
exclusive recovery, scheduled resumption, and confirmed replay; then verifies
convergence. SDK-task interruption after receipt retention, projection send, and
continuation send preserves outcomes/timestamps and one effective external write.
Duplicate/stale wakes preserve the retained result.
`authoritative_admission` additionally checks missing delivery projection
prerequisites do not block link commands or workflow submission.

For #64, `cargo test -p ghinvite-storage-sqlx --test sqlx_suite` and `--test projection`
verify all confirmed audit outcomes, uncertain/blocked exclusion, stale event
delivery, immutable-content conflicts, audit-write rollback and retry.
`retained_delivery` checks the same history through real Restate, including
workflow cleanup.
`npm run test:storage --prefix tests/worker` runs the shared conformance suite,
including this scenario, on actual D1. `npm run test:admission --prefix tests/worker`
injects audit insertion failures into D1 batches and checks events from actual
Worker GitHub 201/204/422 delivery. Console HTTP summary/privacy coverage is in `console_flow` (`audit_` filter).

#49's approved policy is recorded in ADR 0004. #60 implements admission availability
enforcement and installation-event convergence; #57 moved the browser onto
authoritative admission.
