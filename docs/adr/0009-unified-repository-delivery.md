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

The request-owner restructuring described elsewhere in #116 remains a separate
slice; this ADR does not claim the entire parent target is implemented.
