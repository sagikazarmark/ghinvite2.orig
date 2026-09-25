# Invitation-link admission authority

[ADR 0009](adr/0009-unified-repository-delivery.md) is the current ownership
decision. ADR 0003 records the historical protocol proof. See the
[ownership overview](invitation-ownership.md) for the assembled system.

## Trusted command boundary

`InvitationLink/<link ULID>` owns fixed guardrails, current admin-only metadata,
revocation, uses, original creation/admission receipts, prepared attempts and the
latest-request index. The same application-generated ULID identifies creation,
the Console link and public `/i/<ULID>` URL. There is no code registry.

Private ingress accepts identity assertions from trusted web callers. Web
authenticates requesters and verifies current GitHub-derived account-admin
authority and repository selection. `AccountAdmin` and `requester_id` are not
credentials. The owner validates key, account scope, nonzero IDs and guardrails;
it never reconstructs authority from SQL. Deploy with endpoint identity checking.

| Link command | Purpose |
|---|---|
| `create` | Bind preallocated ID and normalized input; replay original snapshot |
| `update_metadata`, `revoke` | Change current metadata or stop new admission |
| `link_status` | Current account-admin view |
| `prepare_attempt` | Bind original input without claiming acceptance |
| `admit` | Return an input-bound accepted or rejected business receipt |
| `requester_page` | Recover original input/receipt using the operation identity, plus current request-owner status and eligibility |

Decisions, deadlines, approved plans and delivery progress belong to
[`InvitationRequest/<request ULID>`](request-lifecycle-v1.md).

Operation IDs are canonical Crockford ULIDs with ASCII-case normalization.
Overflow, aliases, missing IDs and arbitrary strings are invalid. Justification
trims outer whitespace and normalizes absent/empty to `None`. Reusing an identity
with different normalized input conflicts. Bounds are 100 repositories, 256 bytes
per repository full name, a nonempty single-line description up to 120 characters,
and 16 KiB internal note/normalized justification.

## State and recovery

The link checks the original receipt **before** current eligibility. Recorded
outcomes survive expiration, revocation and completed invocation cleanup. Replay
consumes no use and performs no new request handoff. Fresh accepted admission
consumes one use, even if subsequently declined or expired. Uses are never refunded.

Admission checks link guardrails and availability evidence. For an indexed request
it asks the request owner for an input-bound eligibility verdict. Pending/approved
requests block; terminal requests permit a fresh use subject to current guardrails.
A retained blocking verdict binds its original evaluation time across recovery.
Missing expected authority is retryable uncertainty. New request initialization
completes before admission acknowledgement. Requests never call back synchronously
to the link.

HTTP 400 means invalid input, 404 missing/inaccessible authority, and 409 input
conflict. HTTP 200 carries a retained business receipt. Unknown availability is
503 without a business rejection. Transport uncertainty requires the same ID and
input. An empty SQL view never overturns an acknowledged command.

## Durable query projection

`InvitationProjection/<link>/apply_transition` applies link snapshots and
link/admission audit facts. `RequestProjection/<request>/apply` alone projects
mutable request snapshots and lifecycle audit. `DeliveryProjection` alone projects
repository-delivery snapshots, invitation lifecycle and audit. Owners arrange
durable sends, then return; SQL retries run in independent queues.

SQLx transactions and D1 batches atomically validate parents, immutable identity,
revision/content and required audit. Missing parents yield `ProjectionDependency`;
conflicting identities/equal-revision content yield `ProjectionInvariant`. Older
snapshots cannot regress state, but still supply missing immutable audit facts.
Terminal request snapshots may initialize missing rows; late initialization cannot
reset them. Current snapshots alone are not a historical audit archive.

### Inspection, repair, and redrive

1. Inspect `sys_invocation` for the affected projection service/key and failure.
   Queue lag is not admission rejection, request expiry or delivery failure.
2. Restore parents through their real owners or compatible backup. Projectors
   never fabricate installation/user authority. Retained uninstalled installations
   remain valid historical parents.
3. Compare conflicting envelopes with authority and history. Repair incorrect
   records/encoding; do not invent a higher revision to win.
4. Let the original invocation retry, or privately resubmit the exact envelope
   to the same projector/key with original revisions and event IDs. Do not remint
   an admission ID to repair projection.

### Recovery and audit retention

Retain Restate business state, HTTP write fences, webhook routing receipts and
historical SQL audit together. Completed journals are disposable; business
receipts are independent. Preserve compatible backups/event envelopes before
removing history. Database restore requires closing writers and reconciling
original admissions, uses, decisions, manifests, external effects and audits.
A stale SQL snapshot cannot replace current authority.

Administrative kill/purge is not business cancellation or ordinary retry. Close
affected producers, inspect the original journal and retained obligations, and
restore compatible checkpoints or explicitly reconcile missing effects/sends
before reopening. Missing state never permits a new GitHub PUT. Use
[explicit delivery recovery](delivery-recovery.md) to resubmit original commands
and reconcile uncertain effects read-only.

### Projection tests

```sh
cargo test --locked -p ghinvite-storage-sqlx --test sqlx_suite
cargo test --locked -p ghinvite-storage-sqlx --test projection
bash scripts/test-restate.sh durable_projection
npm run test:storage --prefix tests/worker
npm run test:journey --prefix tests/worker
```

The shared storage contract exercises reordered/duplicate snapshots, late
initialization, equal-revision conflict, parent repair and acknowledgement-loss
replay on SQLx and actual D1. Worker gates also lose a real D1 batch acknowledgement
after commit. Focused deadline/concurrency and interruption cases remain in
`request_owner` and `authoritative_admission`.

## Native acceptance

```sh
bash scripts/test-restate.sh authoritative_admission
bash scripts/test-restate.sh request_owner
bash scripts/test-restate.sh canonical_link
```

Runners own isolated pinned Restate instances and fresh SQLite. See
[local testing](local-testing.md) for tools and bounds, and the
[clean prelaunch replacement](invitation-ownership.md#clean-prelaunch-replacement)
for setup. Automatic scheduling remains #86; rollout remains #61.
