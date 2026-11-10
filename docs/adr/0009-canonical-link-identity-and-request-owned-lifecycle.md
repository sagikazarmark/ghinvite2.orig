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


## P1 capability and target amendment (2026-10-16)

The initial public-ULID decision is superseded: Link ID is internal, and `/i#secret`
is an independent capability. The edge immediately erases the fragment, submits
it with anonymous-session CSRF, and stores only its hash/generation for `/join`.
Sign-in rotation preserves that selection while clearing identity-bound state.
Old public-ID paths return neutral 404 without echo or sign-in redirects.

Link creation, projections, requester pages and request initialization now carry
canonical Targets, including each repository's own grant. Delivery commands still
carry one local permission. Organization/team delivery remains a separate M1 gate;
M0 refuses those targets instead of silently dropping grants.

Fresh admission validates current hash/generation before eligibility, and consumes
caller-supplied scope evidence without GitHub or installation-authority I/O. Fresh
negative evidence may refuse; stale/missing evidence does not. Exact attempt
replays preserve their original input and result across rotation and revocation.
These changed journal paths and payloads require the immutable deployment policy
above; no cross-release replay compatibility is claimed.


## P1 Lapsed request amendment (2026-10-16)

The rule suppressing repeat requests after every approval is superseded narrowly
by P1 §5.2. The request owner derives Lapsed from its exact approved target and
generation set only when all targets are final, at least one invitation lapsed,
and none was satisfied. Delivery notifications carry identity only; the owner
reads retained snapshots and verifies their commands before accepting final facts.
Original approval decisions, create receipts and admission results remain immutable.

The link permits a fresh request after Lapsed under current guardrails. Accepted
re-requests consume a use and establish a durable seven-day limit. Declining or
expiring the new request does not erase that limit. An invitation lifetime shorter
than seven days forces re-requests to Pending even on an auto-approve link.
Operation replay preserves the earlier result and never consumes another use.

A Lapsed projection carries its retained original approval as predecessor proof.
Both snapshots and their audit events apply in one SQLx/D1 transaction. Requiring
a separate earlier approval invocation would risk deadlock in the exclusive
projector if the later snapshot arrived first. Raw SQL still cannot transition
a Pending row directly to Lapsed; the transaction first applies the proved approval.
Migration0010 and immutable endpoint versioning precede activation. These journal
and schema changes make no cross-release replay or flag-only rollback claim.


## P1 email-domain admission amendment (2026-10-17)

Email rules are immutable link guardrails. Domain evidence is part of the exact
admission attempt, so a changed proof under the same operation ID conflicts.
The owner checks link binding, matching domains and an inclusive ten-minute
window against `admission_time` before use consumption. Preparing an attempt
confers no freshness lease. A retained successful admission carries the original
evidence through initialization and projection without checking it against a
later wall clock; retries cannot turn the committed acceptance into a stale-proof
refusal. Unrestricted request snapshots omit evidence.

Public default-empty/default-absent fields preserve legacy unrestricted wire
content, but changed handler paths still require endpoint pinning or draining
under ID-21. The explicit sign-in composition collects minimal verified evidence
and replaces it on account changes; deployment activation remains separate from
local implementation evidence.
