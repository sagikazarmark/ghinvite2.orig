---
status: accepted
date: 2026-09-22
---

# Attempt continuations share storage, not outcome semantics

## P1 amendment (2026-10-16)

P1 spec §11.3 replaces the XChaCha/session-secret mechanism below with anykms
v1 envelopes using `P1_KEYRING`. The registered purpose is `attempt-continuation`;
AAD scope and binding come from the stored plaintext `scope` and `binding` columns.
The ID remains the first-writer-wins identity; the binding remains its separately
persisted logical subject (defaulting to ID). Only the payload is sealed. Existing
scope isolation, expiry, conflict/Bound recovery, release, and flow-specific outcome
triage remain unchanged. No legacy format decoder or fallback key exists.

Console mutations by an account admin and invitation request admission by a
requester both retain their exact submitted input before calling the authority,
so an uncertain outcome can be recovered without replaying different input.
They share the mechanism, not what an outcome means.

## Decision

`crates/ghinvite-web/src/attempt_continuations.rs` owns the mechanism:

- scope derivation per flow, browser session, user and subject (`Scope::console`,
  `Scope::invitation`), so nothing retained in one scope is visible from another;
- sealing payloads with XChaCha20-Poly1305 under the session secret, with the
  scope and attempt identity as associated data
  ([ADR 0002](0002-session-protection-and-invalidation.md));
- expiry bound to the browser session that retained the attempt;
- first-writer-wins binding by attempt identity and by logical subject: a
  different payload under a retained identity is a `Conflict`, and a subject
  already held by another identity is `Bound`;
- release.

Each flow keeps its own outcome triage
(`crates/ghinvite-web/src/routes/console/attempts.rs`,
`crates/ghinvite-web/src/routes/invitation/attempt.rs`), because the same shape
of answer means different things:

- A requester's `Rejected` admission is a final business receipt from the
  authority ([ADR 0003](0003-restate-authoritative-admission.md)); replaying the
  attempt must show the same receipt.
- A console `Invalid` is an input error. Nothing was applied, so the
  continuation is released and the admin starts again from a fresh form.
- An invitation request decision can come back `Incompatible`, which neither
  other outcome has.

Bulk Create and Edit receipts confirm the original operation, while their review
snapshots remain immutable. After an acknowledged operation, the Console reads
the current batch owner and rechecks full-target authority before rendering
review choices or submission status. The current review must have the same
manifest and a revision at least as new as the receipt; equal revisions must
agree. If the owner cannot be read, the original continuation stays available
with an explicit recorded-operation/current-state-unavailable message. An old
receipt cannot establish that a batch remains unsubmitted.

## Rejected: a unified retain → execute → outcome module

One module that retains, calls the authority and maps the result to a shared
outcome type would have to either flatten these meanings or grow a per-flow
switch inside it. Either way a change to one flow's semantics would risk the
other. The retained storage is the part with no per-flow meaning, so the seam
sits there.
