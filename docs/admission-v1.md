# Isolated authoritative admission (#53)

`ghinvite_workflows::admission_v1::bind(builder)` registers `InvitationLinkV1`
with lazy state. The existing native/Worker endpoint and browser writers still
use the legacy services. An isolated endpoint must also bind implementations of
the `InvitationProjectionV1` and `InvitationRequestV1` consumer contracts. The
native acceptance fixture provides these consumers; production consumers are
tracked in #54 and #55. Do not enable legacy and authoritative writers for the
same link before #58's cutover.

## Trusted command boundary

Restate ingress is private, accessible only to trusted application callers.
Callers authenticate the requester and verify current GitHub-derived account
admin authority, installation/account relationship, and available repository
selection before constructing commands. `AccountAdmin` and `requester_id` are
trusted identity assertions, not credentials or browser-supplied authority. The
object checks account scope, nonzero identities, canonical object key, and
guardrails without consulting SQL projections. Installation access loss remains
#49's policy decision. Bind with endpoint identity verification when deployed.

Allocate `InvitationLinkId::new()` **once before the first create submission**
and retain the entire `CreateLink` command across uncertain transport failures.
The canonical link ID is both object key and creation identity. Reuse with
different normalized creation input conflicts. Successful creation replay returns
the original creation snapshot, including its invitation code, even after revoke.
Invitation-code routing and projection uniqueness conflicts are part of #54/#57;
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
existing pending/approved request. An uncertain transport/infrastructure result
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
and bounded transport after accumulating retained history. Worker/D1 verification
is deferred to #59.
