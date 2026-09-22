---
status: accepted
date: 2026-09-14
---

# Delivery identity, availability, and uncertain GitHub results

The maintainer approved this contract during implementation of #56, resolving
the policy blocker in #49. We favor preserving confirmed effects and stopping
uncertain writes over automatic retries that could send another invitation.

## Availability outcome matrix

| Access loss timing | Outcome |
|---|---|
| Before admission | Reject new requests if the installation or any repository in the fixed scope is known unavailable. |
| Pending approval | Preserve the original deadline; allow approval with an explanation that delivery can be blocked. |
| Between repository sends | Continue available repositories and block unavailable ones; never rewrite scope. |
| After a confirmed create | Preserve the create result and access history; do not revoke or cancel downstream access. |
| Reinstall/access restoration | Revalidate numeric account, requester, and repository identities; resume work blocked before an effect with its original IDs. |

Existing links regain eligibility when their whole scope is available and their
original guardrails permit admission. Restoration never resets uses, deadlines,
revocation, or history. Installation processing is serialized with authoritative
repository refreshes rather than stale snapshot deltas; events for an old
installation cannot mutate a replacement installation. GitHub remains the final
access-enforcement boundary; refreshes are observations, not atomic guarantees.

## Identity and ambiguity

Create commands bind the immutable numeric requester ID. Resolve its current
login and validate that login against the numeric ID immediately before a new
write. Failure to verify blocks delivery. GitHub's login-addressed API retains
a residual rename race; local durability does not remove it.

Retain attempt intent before the HTTP write. A 201 confirms a created invitation
with its upstream ID; 204 confirms already-collaborator; a definitive rejection
confirms failure. Timeouts, acknowledgement loss, and uncertain server failures
require read-only reconciliation against immutable requester identity. Pending
invitation/access evidence may resolve uncertainty; an absent list result never
proves the earlier write had no effect. Unresolved ambiguity is persisted as
outcome unknown and prohibits automatic repeat writes. Operational repair may
reconcile evidence; this introduces no manual resend UI or cancellation rights.

## Retention and reads

Retain input-bound create receipts in the GitHub invitation Restate object,
independently of workflow/invocation retention and invitation lifecycle. A later
decline, expiry, or cancellation never authorizes replay of the original create.
SQL is the read projection, not create-operation authority. Missing projected
parents are retryable dependencies outside link exclusivity.

Approval, planned delivery, durable submission, blocked delivery, confirmed
created/already-collaborator, definitive failure, and outcome unknown are distinct
facts. Current per-repository reads must preserve those distinctions. There is
no distributed exactly-once transaction between GitHub and Restate.

## Implementation scope

#56 implements retained dispatch/receipts and the delivery-side policy. Admission
availability enforcement and installation-event convergence need follow-up
implementation ticket #60 under #40; approving this decision does not assert those
features are already implemented. Verification seams approved by the maintainer:
public Restate commands/status, a real runtime with GitHub HTTP stub, and current
read interfaces exposing persisted outcomes.

#60's [installation convergence and admission integration](../installation-availability.md)
implements the remaining policy at the account and link command boundaries.

The receiving implementation also retains an input-bound SQL HTTP-attempt fence:
Restate run closures may re-execute if their result acknowledgement is lost. The
fence prevents a second PUT across that gap, while confirmed receipts remain in
invitation-object state. Fence acknowledgement loss can conservatively produce
outcome unknown without any GitHub effect. Positively rejected access/rate-limit
attempts release only their exact generation for a safe later retry; uncertain
attempts remain fenced. See [recovery operations](../delivery-recovery.md).

## Amendment (2026-09-22): an accept may rest on a recent observation, a rejection may not

The matrix row above rejects new requests when the installation or a repository
in the fixed scope is *known* unavailable. It never said how current that
knowledge must be, and admission read GitHub on every fresh attempt: a live
installation read plus a paged repository read, both inside the account's
exclusive object, so every admission for one account queued behind another's
GitHub I/O.

The two directions are not symmetric, so they no longer share one rule.

An acceptance may rest on an availability observation up to five minutes old.
When the retained observation is available and covers the whole fixed scope,
admission accepts on it without reading GitHub again. Access lost inside that
window is already this ADR's policy: an observation is never atomic with the
delivery that follows it, and the loss surfaces as blocked delivery, which
preserves approval and the original repository scope.

A rejection still rests on a live read. It is a permanent retained receipt that
restoration never revisits, so nothing weaker than a current reading may produce
one. Every path that could reject — an observation that is unavailable, unknown,
older than that window, or that does not cover the scope, and an account never
observed — reads GitHub under the account's exclusivity exactly as before, with
the same network deadlines, the same unknown-on-stall result, and the same 503
asking for the same attempt again.

Unchanged: uncertainty is never a rejection, the periodic recheck still drives
unavailable and unknown observations back to available, and an acceptance resting
on a recent observation consumes its use and retains its receipt like any other.
