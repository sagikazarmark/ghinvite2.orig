---
status: accepted
date: 2026-09-22
---

# The web's LinkAuthority stays concrete, tested at the Restate ingress seam

`crates/ghinvite-web/src/link_authority.rs` is the web's only path to the
invitation link authority, the `InvitationLink` Restate object
([ADR 0003](0003-restate-authoritative-admission.md)). It is a concrete
module over Restate ingress. It maps the authority's documented terminal
statuses to a typed `AuthorityError`: 400 to `Invalid`, 404 to `Missing`, 409 to
`Conflict`, and every other failure to `Unknown`, which leaves the command's
outcome unknown. It never reads SQL eligibility.

## Decision

There is deliberately no `LinkAuthority` trait and no in-memory adapter. The
seam stays at ingress HTTP. Tests run the real `LinkAuthority` and its real
status mapping against `FakeLinkAuthority`
(`crates/ghinvite-web/tests/common/authority_http_fixture.rs`), an in-process
server that answers the same ingress paths as the Restate objects and aims at
their status codes. That fake is the second adapter.

The fake does not restate the authority's pure rules. Admission (acceptance,
open-request, domain and scope refusals, the offer), decisions, expiry, the
revocation cascade, requester views, metadata updates and secret rotation are
`ghinvite-core` functions (`admission.rs`, `request_lifecycle.rs`,
`invitation_link.rs`, `link_lifecycle.rs`) that the Restate objects and the
fake both call, so the rules themselves cannot drift apart. What surrounds
them still can: each adapter validates ingress itself, chooses the inputs it
passes the rules (the fake never forces approval on a qualifying short-lifetime
rerequest after a lapse, as the object does), and maps refusals to status codes
itself. The two still differ wherever nothing compares them. Nothing yet runs
the same ingress scenarios against both the fake and the real objects (#114).

## Rejected: a trait with an in-memory authority

An in-memory implementation behind a trait would bypass the status mapping, so
tests would no longer exercise how the web reads the authority's answers. It
would also drift from Restate semantics that the web depends on: `prepare` and
`admit` replaying the same attempt to the same receipt, and 409 for an identity
reused with different input. Keeping one concrete module and faking the wire
keeps those semantics on the tested path. Attempt continuations
([ADR 0007](0007-attempt-continuations-share-storage-not-outcomes.md)) triage the
typed errors this module returns.

## Amendment (2026-09-22): every web call into Restate follows this rule

The web's installation and webhook commands (`RestateCommands`,
`crates/ghinvite-web/src/commands.rs`) used to sit behind a
`GhinviteCommands` trait. That trait had one production adapter and four
hand-written test fakes, two of which only panicked. The fakes recorded
command structs, so tests never exercised how those commands reach Restate:
the service, key and handler names, call versus one-way send, or the rule
that a failed mutation is reported as outcome unknown.

The same decision now covers them: web calls into Restate are concrete
modules over ingress, with no trait and no in-memory adapter. Tests run the
real module against an in-process server that answers the ingress paths and
records the calls it received. `AppState` builds the link authority and the
commands from one `RestateClient`, as production already did.

## Amendment (2026-09-24): single link identity (#117)

The Invitation Code is now the canonical link ULID. Local parsing replaces the
`InvitationCode` registration/resolution protocol. The concrete HTTP seam and
error mapping remain; public lookup calls `InvitationLink` directly.

## P1 amendment (2026-10-16): hash-only public capability

The ULID remains the internal object key, but no longer grants public access.
The concrete requester client requires a typed hash and generation obtained from
the session's selected capability; it cannot look up a public page by ID alone.
Creation and rotation pass only hashes into Restate. The edge registers the hash
routing row after confirmation and before displaying the fragment URL once.
Routing is available before projection; the authority checks the current secret.
Lost responses retain hash-only commands and require rotation if the URL was lost.
The concrete HTTP test seam and definitive/unknown outcome mapping are unchanged.

## P1 amendment (2026-10-16): app-scoped installation ingress

Setup verifies `GET /app/installations/{id}` with the selected app's JWT and
checks the returned numeric App and installation IDs before sending canonical
`OnboardInput` to `Installation/repo:<id>` or `Installation/org:<id>`. Its facts
type has no installer field. A signed-in callback viewer therefore cannot become
the Installing Owner through the setup query.

`/webhooks/{app}` selects that app's rotated HMAC secrets. Only authenticated
installation facts and sender reach `onboard_webhook`; the operation receipt key
is the authenticated payload hash, not the unsigned delivery header. Signed
upstream update times fence delayed owner changes. The Installation schema
[requires creation and update timestamps](https://raw.githubusercontent.com/octokit/webhooks/main/payload-schemas/api.github.com/common/installation.schema.json),
and accepts RFC3339 or epoch seconds; equal timestamps do not establish ordering.
Repo member and repository-invitation events no longer dispatch delivery commands.
Repository settlement uses the reconciler's verified read evidence.

The new installation keys and required input/state shapes use the pinned endpoint
transition described in ADR0009. Console authority now uses installation read
credentials; these ingress changes alone do not complete its acceptance gates.

## P1 amendment (2026-10-16): Console authority and decision receipts

The web checks current authority before exposing account-scoped records or
dispatching mutations, including retained attempt retries. Installation-token
reads establish numeric repository/user identity and administrator permission;
the cache is independent of sessions and expires evidence after five minutes.
Delegated SQL scopes filter all target repositories before counts and pagination.
No user OAuth token supplies Console authority.

Delegation changes use concrete `AccountInstallation/repo:<account_id>` ingress
with a typed actor and operation ID. Tests exercise its real HTTP encoding,
typed refusals and unknown acknowledgements. There is no fake command trait.

Request decisions independently check retained account authority. Exact operation
replay precedes that fresh check, preserving a recorded decision or refusal.
`NotAuthorized` leaves the request unchanged and becomes a generic web 404.
This coarse retained-state check does not replace the web's per-target checks.
Repository availability remains a delivery prerequisite rather than a reason to
prevent a historical personal owner from deciding a request or stopping a link.
