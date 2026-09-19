# Isolated authoritative admission (#53)

Native cutover tooling and endpoint/routing activation are now available under
[#58's maintenance procedure](admission-cutover.md). The original isolated-path
description below documents the earlier slices; defaults remain legacy until an
operator performs cutover. #59 completed local Worker/D1 verification; production
activation remains blocked by the operator-owned remote gates in #61.

`ghinvite_workflows::admission_v1::bind(builder)` registers `InvitationLinkV1`
with lazy state. The default native/Worker endpoint and browser writers use legacy
services; `authoritative` mode now selects the integrated cutover path. An isolated
endpoint must also bind implementations of
the `InvitationProjectionV1` and `InvitationRequestV1` consumer contracts.
`projection_v1::bind(builder, Arc<dyn ProjectionStorage>)` supplies the durable
SQLx/D1 projector (#54); #55 supplies the request lifecycle consumer.
Do not enable legacy and authoritative writers for the
same link before #58's cutover.

## Trusted command boundary

Restate ingress is private, accessible only to trusted application callers.
Callers authenticate the requester and verify current GitHub-derived account
admin authority, installation/account relationship, and available repository
selection before constructing commands. `AccountAdmin` and `requester_id` are
trusted identity assertions, not credentials or browser-supplied authority. The
object checks account scope, nonzero identities, canonical object key, and
guardrails without consulting SQL projections. Installation access loss follows
[the #60 availability integration](installation-availability.md): bind
`AccountInstallationV1` alongside the link service (the cutover endpoint does so).
Bind with endpoint identity verification when deployed.

Allocate `InvitationLinkId::new()` **once before the first create submission**
and retain the entire `CreateLink` command across uncertain transport failures.
The canonical link ID is both object key and creation identity. Reuse with
different normalized creation input conflicts. Successful creation replay returns
the original creation snapshot, including its invitation code, even after revoke.
Invitation-code routing is implemented in the isolated [browser path](browser-admission-v1.md); projection uniqueness conflicts remain
inspectable pending repair rather than changing the authoritative creation;
new commands currently address immutable link IDs directly.

| Command | Input | Result |
|---|---|---|
| `create` | Version 1, preallocated link ID, admin/account/installation, fixed guardrails and metadata | Original link creation snapshot |
| `admit` | Version, link ID, strict ULID operation ID, authenticated requester ID, optional justification | Original accepted/rejected admission receipt |
| `revoke` | Link ID and current account-admin assertion | Revoked link snapshot |
| `link_status` | Link ID and current account-admin assertion | Current admin link snapshot |
| `request_status` | Link/request IDs and authenticated requester ID | Requester's authoritative request snapshot |

All handlers are exclusive. Requester status excludes admin-only link metadata;
another requester's request is not found. Admission identity is link-scoped;
another requester reusing it receives only a generic conflict. No transport
idempotency key is required: retries must reach application input comparison.

Operation IDs accept canonical Crockford ULIDs with ASCII case normalization;
overflow, ambiguous character aliases, missing IDs and arbitrary strings fail
validation. Justification trims outer whitespace and normalizes absent/empty to
`None`, preserving internal content. A changed version reaches retained input
comparison before unsupported-version validation for fresh attempts.

HTTP 400 means invalid structure/guardrails, 404 means missing or inaccessible,
and 409 means identity conflict. HTTP 200 carries either an accepted receipt or
a durable business rejection, with precedence revoked, expired, exhausted, then
existing pending/approved request, then installation/repository unavailability.
An unknown availability observation returns 503 without a business receipt.
An uncertain transport/infrastructure result
requires retrying the same identity/input; it is not business rejection.

## State and recovery

The object directly reads `v1/op/<canonical operation ID>` before eligibility.
The small `v1/link` record contains no histories. Requests and exact requester
blockers use `v1/request/<request ID>` and `v1/blocker/<GitHub user ID>`.
`v1/creation` retains the original creation result. No business receipt expires
automatically. Missing authority is not reconstructed from SQL.

Admission journals the entire bounded decision with a fresh decision-time clock
sample and server-generated request ID. It applies link, request, blocker and
operation after-images in order, then durably sends projection and workflow
startup, awaiting only send identities. Ordinary Restate replay reuses the
original reads/decision and completes unfinished writes/sends. Exclusive reads
queue behind recovery. A separately invoked replay returns its receipt without
dispatching again, independently of completed runtime journal/workflow retention.

Projection envelopes carry a monotonic link snapshot, touched requests, stable
transition ID, and stable immutable audit intents. Workflow envelopes carry
request identity/state, repository scope, permission, installation, approval
policy and deadline, without requiring a projected row. Final-use exhaustion
and request/link creation audit identities survive retries. Projection failure
cannot hold admission or revocation open. Consumer success and workflow lifetime
are not command completion conditions.

Manual admission is pending with an independent seven-day absolute deadline;
auto-approved admission is already approved with no deadline. Both suppress new
requests. Terminal declined/expired/cancelled records are retry-eligible and uses
are never refunded. #55 adds authoritative pending transitions, overdue status
materialization/readmission and direct workflow notifications. There is no new
cancellation action in this slice. Administrative kill/purge or state editing is
outside ordinary recovery and requires the #58 maintenance/repair procedure.

Current command bounds: 100 repositories, 256 bytes per repository full name,
120 characters for a nonempty single-line description, and 16 KiB for internal
note/normalized justification. These bound touched payloads, not total retained
state; capacity and retention operations remain rollout responsibilities.

## Native acceptance

```sh
bash scripts/test-restate.sh authoritative_admission
```

The runner provisions disposable pinned Restate 1.7.9 and fails explicitly if
Docker/runtime infrastructure is unavailable. It exercises the implementation in
request-response mode, with feature-gated journal fault injection and fixture
consumers. Tests verify creation conflicts/guardrails, final-use races, pending
and approved suppression, revoke ordering, queued expiry, interruption around
the full decision and every touched write/send, consumer outage, canonical input
and confidentiality, cross-link operation reuse, actual runtime retention cleanup,
and bounded transport after accumulating retained history. Local actual Worker/D1
verification is covered by the [completed #59 gate](worker-admission-gate.md);
remote rollout verification remains #61.

## Durable query projection (#54)

`InvitationProjectionV1/apply_transition` is an internal ordinary Restate service.
It accepts the existing v1 wire envelope, whose types now live in
`ghinvite_core::storage::projection` and are re-exported by `admission_v1`.
It applies one full link snapshot, at most two independently revisioned touched
requests, and at most eight immutable events. Repository scope is bounded at 100;
the expanded batch payload is capped at 2 MiB, allowing worst-case JSON escaping
of every bounded input. No accumulated history is sent.

SQLx uses a transaction; D1 uses the same fixed seven-statement batch. Named CHECK
assertions validate dependency and content conflicts inside the atomic application.
Link and repository records precede requests. Newer snapshots replace older ones;
equal revisions require equal content; lower revisions cannot regress records.
Request revisions are independent of link revisions. Audit insertions always run,
even alongside stale snapshots, and uses are assigned from authoritative snapshots.
The v1 `creation` field is immutable command input, including original metadata;
future metadata commands must add a separate current-metadata snapshot rather than
rewrite retained creation identity. `update_metadata` now writes a separate optional current metadata snapshot.

Migration 0004 adds nullable revision/content/identity columns, request deadlines,
and logical audit IDs/evaluation times. NULL revisions identify legacy-owned rows;
this projector rejects collisions with them pending #58's explicit import. The
pending-request unique index continues guarding legacy rows. Projected pending
rows can temporarily overlap under reordered lifecycle delivery; Restate alone
enforces eligibility. No rows are deleted or uses refunded to resolve that lag.

The existing Console storage reads remain eventually consistent. The additional
`ProjectionStorage::get_projected_request` read exposes the versioned snapshot and
admission deadline; absent/legacy rows return `None`, never proof of rejection.
Existing audit pagination retains ULID IDs: projectors derive them deterministically
from the domain-separated SHA-256 of the logical event ID (first 128 bits), retain
the logical ID, and compare immutable content. Collisions fail rather than silently
deduplicating. Audit occurrence time is the effective time; evaluation time is
retained separately. No request justification or internal note is copied to audit.

### Inspection, repair, and redrive

Every failed write keeps the Restate invocation retrying, including invariant and
encoding failures. Trace logs correlate transition ID, link ID and failure class;
Restate's `sys_invocation` exposes invocation ID, status, and last failure. Keep
ingress and retained invocation inputs private: they contain domain payloads.

1. Inspect pending `InvitationProjectionV1` invocations and correlate the link and
   transition. Do not interpret lag as an admission rejection or refund a use.
2. For `projection dependency missing`, restore the fully owned installation/user
   rows via their existing owners or a compatible backup. Installation account ID
   must match. Projectors never fabricate user/installation stubs or update profiles,
   installation selection/status, or account authority. An uninstalled but retained
   matching installation remains a valid historical parent.
3. For an invariant conflict, compare the retained envelope to the affected records
   and authoritative state under controlled repair. Restore the correct row facts
   or deploy a compatible encoding fix; do not increase a revision just to win.
4. Once repaired, ordinary retries redrive the same envelope automatically. A
   trusted operator may also resubmit that exact envelope through the internal
   service. Duplicate and ambiguous-commit replay is safe; never reconstruct an
   admission command with a new operation identity as a projection repair.

Administrative kill/purge and independent database loss need #58's coordinated
recovery procedure. Completed Restate journals are not a permanent event archive;
current snapshots alone cannot rebuild historical audit events. Preserve a
compatible backup/event source before discarding retained work.

```sh
cargo test -p ghinvite-storage-sqlx --test projection
bash scripts/test-restate.sh durable_projection
cargo check -p ghinvite-workflows-worker --target wasm32-unknown-unknown
```

Native tests exercise public reads after real writes, reordered/duplicate envelopes,
late historical audit, conflicting identities/content with atomic rollback,
parent recovery, SQL failure/repair, retained invariant failures, and lost commit
acknowledgement before Restate run completion. The real runtime binds production
admission and projector code; a fixture receives #55's workflow startup.
Actual local Worker/D1 runtime verification is covered by the
[Worker gate](worker-admission-gate.md); remote rollout verification remains #61.
