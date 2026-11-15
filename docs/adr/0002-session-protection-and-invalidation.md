---
status: accepted
date: 2026-09-12
---

# Application-key session protection with eventual individual sign-out

## P1 amendment (2026-10-16)

P1 spec §11.3 replaces the single XChaCha key and strict hex variable below with
an immutable keyring loaded from the `P1_KEYRING` descriptor (kekring v1 since
2026-11-15, `libs/kekring/docs/kekring-format.md`). Sessions use anykms envelope
v1 with the canonical AAD of the `session` purpose derived from the requested KV
ID. All session KV writes have TTL ≤30 days. Add/read/promote/retire generation
rollouts preserve old sessions until expiry; a record under a version above the
reader's high-water mark (code `VersionAboveHighWaterMark`) returns503 without
sign-out, removed/corrupt ciphertext or another root's key id invalidates, and a
malformed descriptor fails startup with500 and an operator alert. Fingerprints are logged once/isolate
and exposed only to numeric `P1_OPERATOR_IDS` members at `/console/keyring`.
The historical single-key/global-rotation text below is superseded only on these
points; fixed lifetimes, independent revocations, nondestructive reads, and explicit
sign-out failure reporting remain binding. No legacy format fallback is provided.
The explicit token-free composition and its remaining live gate are described in
the cutover amendment below.

## Legacy OAuth removal (GM-09, 2026-10-21)

Supersedes the cutover amendment below. Typed, token-free sign-in is the only
sign-in path; there is no `GH_SIGNIN_ENABLED` and no token-free store policy to
select. Sessions signed in through legacy OAuth **remain valid** until their
original fixed deadline: they carry the same GitHub identity, CSRF authority
and lifetime as typed sessions, and nothing depends on the user token they
hold. The `Session` type no longer has `access_token` or `oauth_csrf`; unknown
fields are ignored when a record loads, and the protected store re-serializes
the session payload on every write, so the token leaves storage with the
record's next write, whatever that write changes, and otherwise at expiry. A
legacy record is not rewritten merely because it was read (KV write limits).
No upstream revocation of tokens issued by legacy OAuth is performed. Rollback
to a pre-GM-09 build in legacy mode would again accept those records until
their deadline.

## Explicit token-free session cutover (2026-10-17)

`GH_SIGNIN_ENABLED=true` selects both the typed OAuth routes and token-free
session storage in native and Worker composition. The default remains false;
SP19a remains a hard live-activation gate. New typed session records omit the
legacy credential field entirely. Credential-bearing records from a previous
legacy deployment confer no identity on a typed deployment. Create/save rejects
those snapshots even if a stale caller assigns a fresh ID. Empty transitional
fields can be removed without changing the original fixed deadline.

Rejection is nondestructive: the typed reader does not delete ciphertext or
write a revocation marker. Existing records expire at their original deadline;
there is no automatic identity migration, lifetime renewal, or legacy fallback.
This follows the existing overlap rule and avoids an old or misconfigured
reader destroying another deployment's valid record. A successful fresh OAuth
callback rotates the session ID and establishes a new authenticated lifetime.

An operator cutover must deploy the typed policy to every serving instance and
drain legacy instances and in-flight requests before declaring old credentials
unusable application-wide. Rollback must retain typed session enforcement; a
rollback to legacy mode would accept still-live old records. This is not GitHub
token revocation: existing upstream authorizations need their own retirement
procedure. No production activation or upstream revocation is implied by local
verification. Pending-sign-in generation lag returns503, keeps the exact
ciphertext/expiry and cookie, and allows the original callback to retry after
keyring rollout; configuration failures remain500 with the existing fixed alert.

During the design interview for [#42](https://github.com/sagikazarmark/ghinvite2.orig/issues/42), we chose application-key protection for complete stored session records and accepted eventual individual sign-out in exchange for retaining Workers KV, using separate revocation markers and fixed lifetimes to prevent lasting session resurrection. The maintainer confirmed the complete contract and concluded the design session on 2026-09-12. This records the approved decisions, not an assertion that they are implemented.

## Protection and lifetime

Use XChaCha20-Poly1305 with a 32-byte application key and a fresh cryptographically random 24-byte nonce on every encryption. Protect the complete session record, including OAuth tokens, identity, OAuth state, and cached authorization information; authenticate the format version, requested session ID, and lifetime metadata so substitution and deadline alteration cannot grant authority. Native and Worker deployments share the protection implementation and strict key interpretation.

Use RustCrypto directly for the session encryption implementation. Cryptbox was considered but is not required: its released binding API does not directly accept arbitrary per-session associated authenticated data. Verify the chosen dependency configuration on both native and Worker targets. Randomness failure aborts encryption and persistence rather than falling back to insecure nonce generation.

`GHINVITE_SESSION_SECRET` must contain exactly 64 hexadecimal characters decoding to 32 bytes. Missing, malformed, or oversized values are configuration errors, including in local development; secrets must be redacted from debug output and errors. Local development generates a secret explicitly and tests supply explicit test keys. Native hosting remains scoped to development and testing.

Authenticated sessions have a fixed maximum lifetime of 30 days from successful GitHub sign-in, subject to any earlier inactivity expiry. Ordinary requests cannot extend that maximum; a new successful sign-in establishes a new lifetime. Existing unprotected sessions are rejected without a plaintext compatibility period, including restarting pending OAuth flows.

Anonymous sessions, including pending OAuth sign-in, expire 30 minutes after their creation without sliding renewal. Successful sign-in establishes the authenticated lifetime instead. Preserve authenticated lifetime metadata through ordinary loads and saves; neither a storage write nor an ID change may silently reset it. Enforce logical expiry in the application rather than relying only on storage expiration or cookie expiry.

## Sign-out and emergency invalidation

Individual sign-out targets the current session only. Other browsers, GitHub authorization, and repository access are unaffected. The browser cookie is cleared immediately; server-side rejection may propagate eventually, with no promised short global propagation bound. Concurrent ordinary requests must not restore lasting authority to the signed-out session. Already-authorized operations may finish, and an independently completing OAuth callback may establish a new session.

Persist a deny-only revocation marker separately from the encrypted session record and check it on every session load. Ordinary session writes must never erase the marker or transfer stale authority to a fresh ID to bypass it. Successful eventual sign-out requires an acknowledged marker write; ciphertext deletion is subsequent cleanup. A stale request may rewrite ciphertext while the marker is propagating, but once visible the marker denies that session independently of the ciphertext.

Retain markers for 31 days after sign-out: the maximum authenticated lifetime plus a one-day clock margin. This assumes normally synchronized deployment clocks within that margin; it is not a KV propagation bound. Never shorten protection during duplicate revocations. Once marker retention ends, the fixed authenticated deadline must independently reject every old snapshot. A marker-read failure is a storage failure, never evidence that the session has not been revoked.

Global session invalidation replaces the single active key and requires fresh sign-in. A maintenance window is acceptable: invalidation is complete only once old-key deployments stop serving and their in-flight requests have drained. Rollbacks retain the new key and never restore a retired key. We accept interrupted sessions instead of rolling multi-key migration and uninterrupted key rotation.

Unreadable records, including unknown formats and wrong-key ciphertext, confer no authority but must not be deleted, overwritten, or revoked merely because they could not be read. During deployment overlap, they may be valid records written by a new-key deployment. Any new anonymous session uses a fresh ID; old records are left for expiration cleanup.

## Failure contract

- Invalid, tampered, expired, or retired-key records confer no authenticated session; users must sign in again.
- Storage failures produce generic temporary errors on affected session-dependent requests, without exposing internal error details.
- Failure to persist a protected login session must not report successful login.
- If sign-out cannot persist revocation, clear the browser cookie but explicitly report that server-side sign-out could not be confirmed.
- Once revocation is persisted, failure to clean up the session record does not turn eventual sign-out into failure; record the cleanup failure operationally.

The implementation and deployment runbook must demonstrate these guarantees before #42 is considered complete. Verification must cover raw stored records, tampering and cross-session substitution, invalid keys and formats, old-cookie rejection and new login after rotation, fixed deadlines, revocation with concurrent stale writes, and persistence failures. The design-approval gate is satisfied; implementation remains a separate step.
