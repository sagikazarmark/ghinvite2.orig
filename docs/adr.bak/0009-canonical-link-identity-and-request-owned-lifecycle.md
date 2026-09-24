---
status: accepted
date: 2026-09-24
---

# Canonical link identity, request-owned lifecycle, and repository delivery

The architecture review found that admission, approval, and delivery expose too much coordination to callers. Use one application-generated ULID as the invitation link ID, public invitation code, and `InvitationLink` object key; remove the separate code registry. Keep admission and uses on the link, move retained request decisions and dispatch to an `InvitationRequest` Virtual Object, and give each request/repository delivery one `RepositoryDelivery` Virtual Object owning both its create receipt and subsequent GitHub invitation lifecycle.

The implementation contract and acceptance scenarios are in [spec #116](https://github.com/sagikazarmark/ghinvite2.orig/issues/116).

## Why these owners

- A request owns approval, decline, its absolute decision deadline, decision receipts, and its immutable delivery plan. Short exclusive handlers and delayed expiry messages replace workflow promise coordination. Business records survive invocation cleanup; a completed Workflow's retention is not the lifetime of the request.
- The link owns immutable admission receipts, guardrails, uses, and the pointer to each requester's latest admitted request. That pointer is not a second lifecycle record. Fresh admission consults the request owner for deadline-aware eligibility; uncertainty is retryable, never a permanent rejection from a stale reservation. The request does not synchronously call back into a waiting link.
- Approval issues immutable repository-delivery commands. Delivery does not wait for SQL to say `Approved` or repeatedly fetch the whole link plan. It still verifies current GitHub prerequisites and retains the input-bound HTTP-attempt fence protecting the GitHub-response/journal-acknowledgement gap.
- Create outcome and invitation lifecycle remain different facts inside one delivery owner. Missing observation is not failure, absence is not permission to repeat an uncertain write, and later settlement never reopens the original create.
- Each owner is the only producer of its mutable projected snapshots. Projection retries run separately from owner execution. SQL's HTTP fences, browser continuations, webhook routing receipts, and historical audit remain explicitly retained records, not an expendable cache.

## Protocol and rollout constraints

The implementation spec must define the request eligibility verdict as an operation-bound arbitration result: an old `Pending` observation cannot become a new rejection after recovery. Preserve immediate readmission after a definitive terminal result, original admission/decision replay, processing-time deadline arbitration, no use refunds, and approved-request repeat suppression regardless of delivery outcome. Initialize the request idempotently before acknowledging admission; initialization and eligibility calls perform no GitHub or SQL work. Auto-approval remains a policy fact established at admission and adopted by the request owner.

The maintainer confirmed that nothing runs in production and accepted a clean prelaunch replacement. No old public-code aliases, live-state import, or legacy handler compatibility layer is required. This records the rollout strategy; it does not itself reset local state or authorize production deployment. Native/Worker support and SQLx/D1 verification remain required.

## Relationship to earlier decisions

This supersedes the link-owned request lifecycle/dispatch and workflow-notification arrangement in ADR 0003, the split create/settlement ownership in ADR 0004 and ADR 0006, and the `InvitationCode` routing described in ADR 0008. It preserves their replay, external-effect ambiguity, distinct delivery/settlement evidence, and concrete HTTP ingress principles. Earlier ADRs describe the existing implementation until the replacement tickets land.

Verification uses authenticated web routes through real Restate owners with a GitHub HTTP stub, focused real-Restate replay/concurrency scenarios, and the shared conformance suites against SQLx and actual D1. The maintainer confirmed these seams.
