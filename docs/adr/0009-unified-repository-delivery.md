---
status: accepted
date: 2026-09-24
---

# One owner for repository creation and settlement

The accepted target in #116 assigns a single owner to each mutable entity.
Issue #120 implements its repository-delivery slice: `RepositoryDelivery` is keyed
by canonical request ULID and numeric repository ID, separated by `:`. Its approved
command binds the internal invitation ID once. Create outcomes remain historical
facts after upstream invitation settlement; the same object retains the terminal
winner and immutable audit event.

This supersedes the split-owner and SQL-context portions of ADRs 0004 and 0006.
Their distinct evidence rules still apply: absent evidence cannot authorize an
unknown create retry; settlement observes a known upstream identity, complete
pagination and verified numeric identities. Current installation credentials come
from account-owned retained context. A SQL row cannot reconstruct missing authority.

One delivery revision lineage carries the original create receipt and optional
terminal settlement to independent projection execution. SQLx and actual D1 apply
snapshot, lifecycle and required audits atomically. Missing parents retry outside
the owner. Owner reads expose create history and current lifecycle together;
dispatch acknowledgement remains distinct from GitHub success.

The HTTP-attempt fence and historical member-webhook match/no-match receipts remain
retained SQL records. This is read-projection independence, not independence from
the safety-fence database. Cancellation rights and expiry evidence are unchanged;
request decision deadlines are not invitation expiry timers. Lifecycle scheduling
policy remains in #86.

This clean prelaunch switch removes the old create/settlement registrations and
SQL winner table. Use fresh migrations and a fresh Restate environment with matching
web and workflow builds on both native and Worker deployments; no legacy retained
state importer is provided. Environment reset is an explicit operator action.

Issue #121 implements the request-owner slice. `InvitationRequest` is a persistent
Virtual Object with short exclusive initialization, decision, eligibility and
expiry handlers. It retains request-scoped operation receipts, the immutable
approved manifest and repository submission progress. Delayed expiry messages
replace workflow sleeps and terminal promises. `RequestProjection` is its sole
mutable SQL snapshot producer; link projection carries only link/admission facts.

The link retains original admission receipts and a latest-request index. Before a
fresh admission it asks that request for an input-bound eligibility verdict.
Blocking verdicts retain their evaluation time and bind rejection even when
publication resumes after deadline. Definitive terminal verdicts permit a fresh
use-consuming decision under current link guardrails and availability evidence.
Expected missing authority yields retryable uncertainty. Initialization completes
before acceptance is acknowledged; admission replay performs no new handoff.
Calls remain acyclic: request handlers never synchronously wait on the link.

## P1 implementation amendment (2026-10-16)

Repository delivery identity now includes delivery generation:
`{request_id}:{repo_id}:{delivery_generation}`. Generation is an explicit part of
commands, observations, projection rows and object addresses. Attempt generation
is separate: it fences an individual external write within that delivery. A
positive rejection can release only its exact attempt generation; a later
attempt cannot be released by a delayed acknowledgement for an earlier attempt.
The fresh schema retains an internal invitation ULID mapping while legacy
consumers are migrated; it is not an alternative delivery identity.

Creation now journals a typed pre-read, makes the rank decision in the handler,
and journals the guarded write separately. The guarded stage checks the role
again before claiming a write fence. Built-in roles are ordered read, triage,
write, maintain and admin; higher roles and pending invitation grants are never
lowered. Custom roles are unranked and cause a skip. A successful 204 records
Upgraded; preexisting sufficient access records Already Satisfied. A sufficient
pending invitation records Already Pending and is tracked as foreign.

An existing attempt fence changes replay into read-only recovery. A complete
invitation list containing a unique, nonexpired invitation for the immutable
requester, with sufficient grant and a verified repository ID, can recover a
lost successful result. Absence and current access alone leave creation unknown;
they never authorize another PUT or PATCH. This narrows ADR 0004's original
permission-evidence wording to P1's explicit uncertainty rule.

Settlement observations bind delivery key and expected revision. Acceptance
requires absence of the known invitation and verified effective permission at
least equal to the target. Merely receiving an acceptance webhook or observing
a lower base role is insufficient. Cancellation/own-expiry of a foreign pending
invitation stops local tracking without DELETE or a fictitious upstream
Cancelled/Expired fact. P1's conflicting cascade-cancellation wording remains an
explicit M1 decision gate in the implementation ledger.

The changed object keys, journal entries and serialized observations require new
immutable endpoint versions with old invocations pinned, or a drained/fresh
environment. These local changes do not establish compatibility with an existing
deployment or authorize resetting one.

## P1 operational timing amendment (2026-10-16)

The delivery owner separately retains the first time creation became unknown and
its latest complete, identity-bound observation time. These are monitoring facts,
not create receipts or lifecycle transitions. A no-change reconciliation can
advance the observation time without changing the snapshot or its revision.
Stale revision, wrong upstream identity and incomplete read evidence cannot make
an owner appear freshly observed. Evidence predating confirmation or the retained
observation watermark is rejected before the settlement reducer too: a newer
no-change listing protects its unchanged revision from delayed older absence. Older unknown owners without instrumentation
remain undated rather than borrowing an approval timestamp.

`DeliveryProjection.telemetry` is queued after snapshot application on the same
exclusive projection object. The owner never waits for projection storage. Both
SQL adapters reject account/identity/chronology conflicts, preserve the initial
unknown time and apply observation times monotonically without changing receipt,
settlement or audit fields. Operator reports expose missing and future timing and
cannot establish complete health when projection activity is delayed.

The extra named clock and state/send commands change journal paths. ID-21's
immutable endpoint pinning/draining policy applies; fresh schema additions are
not an upgrade or automatic import of a deployed environment. The fixed real
journal-path gate includes newly unknown create/recheck transitions, including
pending rejection-release acknowledgement paths.


## Retained transient audit amendment (2026-10-17)

Repository delivery stores its current snapshot, semantic audit cursor and bounded
pending audit facts in one authoritative object-state record. Reads of creation
history and settlement use that record. The old snapshot/receipt keys are a
read-only fallback until the first complete local record is written; they are not
separately updated authorities. Organization delivery applies the same atomic
snapshot-plus-audit boundary in its retained record.

Before replacing that record, the owner drains any older pending batch through a
durable one-way send. It journals the time of a new semantic episode before the
single state write, and clears only pending facts after the durable send is
acknowledged. The independent projector owns SQL retry. Thus killing an invocation
before the write exposes none of the new snapshot, and killing it afterward
retains both the snapshot and the original audit facts for later recovery.

Repeated Blocked with the same bounded reason or OutcomeUnknown polling emits no
new event. A later distinct episode uses a new snapshot revision. A new hold epoch
also advances the delivery revision, and release retains that hold's epoch;
revocation abort does not claim release. Delayed or equal projection snapshots
still insert missing historical facts without regressing current state. Audit ID
conflicts reject changed payloads and roll back the batch atomically.

The new state layout, named clocks and producer/projector envelope require ID-21
endpoint pinning or draining. Old snapshot payloads decode in the new projectors;
old projectors ignore the new event field and must not receive new producers.
This implementation cannot reconstruct transient events that were never retained.
