---
status: accepted
date: 2026-09-22
---

# The web's LinkAuthority stays concrete, tested at the Restate ingress seam

`crates/ghinvite-web/src/link_authority.rs` is the web's only path to the
invitation link authority, the `InvitationLink` and `InvitationCode` Restate
objects ([ADR 0003](0003-restate-authoritative-admission.md)). It is a concrete
module over Restate ingress. It maps the authority's documented terminal
statuses to a typed `AuthorityError`: 400 to `Invalid`, 404 to `Missing`, 409 to
`Conflict`, and every other failure to `Unknown`, which leaves the command's
outcome unknown. It never reads SQL eligibility.

## Decision

There is deliberately no `LinkAuthority` trait and no in-memory adapter. The
seam stays at ingress HTTP. Tests run the real `LinkAuthority` and its real
status mapping against `FakeLinkAuthority`
(`crates/ghinvite-web/tests/common/link_authority.rs`), an in-process server
that answers the same ingress paths with the same status codes as the Restate
objects. That fake is the second adapter.

## Rejected: a trait with an in-memory authority

An in-memory implementation behind a trait would bypass the status mapping, so
tests would no longer exercise how the web reads the authority's answers. It
would also drift from Restate semantics that the web depends on: `prepare` and
`admit` replaying the same attempt to the same receipt, and 409 for an identity
reused with different input. Keeping one concrete module and faking the wire
keeps those semantics on the tested path. Attempt continuations
([ADR 0007](0007-attempt-continuations-share-storage-not-outcomes.md)) triage the
typed errors this module returns.
