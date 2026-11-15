# Token-free sign-in service and explicit route composition

> **Update (GM-09, 2026-10-21).** Token-free sign-in is now the only sign-in
> path: legacy OAuth, `GH_SIGNIN_ENABLED` and the token-free session policy
> switch (`with_token_free_sessions`) are removed, and the service runs on
> octologin (GM-08). Sessions written by legacy OAuth stay valid; the session
> store re-serializes every record it writes, which drops their unread token
> (ADR 0002, GM-09 amendment). `signin-cutover.mjs` keeps the keyring-lag
> callback proof and no longer seeds legacy records. The text below records
> the opt-in era and its evidence as it was.

`ghinvite-web::signin::SignInService` implements the M0 callback boundary in P1
§6.3 and §11.3 using `gh-toolkit::signin::OAuthConfig<SignIn>::from_registry`.
`GH_SIGNIN_ENABLED=true` explicitly wires it into `/login` and `/oauth/callback`
using the SQLx/D1 pending-state adapter. The default remains false and retains
the legacy flow. SP-19/SP-19a have not run: local composition does not establish
live permissionless private-email behavior or authorize deployment. Typed
sessions omit the legacy token field. Both production roots also select a
token-free protected-store policy that rejects credential-bearing legacy
records and stale writes without changing their original lifetime.

The service generates 32-byte random state and a separate PKCE S256 verifier,
issues an authorize URL without scopes, and persists the pending material before
returning the URL. State contains no link selection or application data. Pending
state expires after the anonymous session's fixed 30-minute maximum (ADR 0002);
future timestamps fail closed. Callback processing consumes the pending state
before checking state, provider denial, or exchanging a code. A returned token
is used for `GET /user` and, only for a selected restricted link, the complete
`GET /user/emails` walk, followed by one client-bound Basic-authenticated
`DELETE /applications/{client_id}/token`. Failed user reads and malformed identity
responses still reach revocation. Failed revocation records a fixed toolkit
error kind and preserves the user-read result. The return type contains only
`github_user_id` and `login`; neither a token nor refresh token can be returned.
Restricted callbacks return only normalized matching domains and a checked
timestamp bound to the selected link; never addresses or unmatched domains.
Failed identity, capability, or email reads still attempt token revocation.
Before reauthentication and again before callback reads, the route removes and
explicitly persists old evidence so a failed account switch cannot reuse it.

The `PendingSignInStore` interface is the activation seam. Its production
`SqlPendingSignInStore` adapter now uses a separate `PendingSignInStorage` port
implemented by SQLx and D1. Migration `0003_pending_signin.sql` stores a SHA-256
lookup key, opaque base64 envelope and fixed expiry. The lookup domain is
`p1:pending-signin:v1\0`; the canonical session ID comes from trusted session
middleware, never callback input. Neither the raw session ID nor OAuth material
is a plaintext column. The additive version-1 registered purpose is
`pending-signin; session_id=<trusted canonical ID>`; the lookup hash is not a
substitute for that encryption context. Existing context encodings are unchanged.

Consumption reads the authoritative row without deleting it (D1 explicitly uses
`FirstPrimary`), decrypts and validates context/issuance/expiry, then atomically
compares and deletes the exact key, ciphertext and expiry with `DELETE RETURNING`.
SQLx commits before acknowledging success. Pending material is returned only
after acknowledged deletion; uncertainty drops it, so an uncertain committed
consume cannot exchange a token on retry. Concurrent callbacks have one winner;
a replacement between decryption and deletion survives and the stale callback
gets no pending state. Generations above the high-water mark
(`VersionAboveHighWaterMark`) and other keyring failures (`KeyIdMismatch`,
removed generations) leave the row retryable; diagnostics carry only the fixed
kind, the kekring code and the nonsecret keyring fingerprint, and only a
configuration-class failure raises the operator alert. Corrupt/context-mismatched
records also remain until bounded expiry cleanup. Expired records can be removed
only by exact comparison. Each replacement cleans at most 100 expired rows;
there is one live record per session and no unbounded callback history.

Workers KV's eventual read/delete sequence is not used for consume authority.
The explicitly enabled normal routes use this adapter. Its bounded error type
separates unknown key generation (retryable503) from other unavailable state
(500); neither includes secret or backend error text. The route preserves the
original pending row on generation lag. Configuration failures keep the fixed
operator alert and return500.

The normal `/login` and `/oauth/callback` handlers already select this service
under the explicit composition flag. They preserve the existing five starts per
ten minutes budget, rate-budget history, session-ID rotation, fixed authenticated
lifetime, hash-only pending link and validated return path; upsert the returned
identity; then persist the §11.3 token-free session. Storage failure cannot report
successful login. Default legacy composition and its payload/API remain until
the coordinated live SP-19a cutover; no second public sign-in flow is introduced. GitHub's
exact token-limit error mapping and token-expiry setting require live SP-19a
observations; no guessed upstream error text is treated as evidence here.

Focused tests check exact PKCE exchange, exactly one revocation with the issued
token, identity-only serialization, replay and concurrent callback exclusion,
state mismatch, provider denial, expired/future/cross-session state, persistence
failure, token-exchange failure, failed/malformed user reads, and marker-free
revocation-failure diagnostics. These tests exercise the service with a scripted
HTTP transport; they do not prove production browser/session or deployed logs.

Local verification (2026-10-16):

- `cargo test -p ghinvite-web --lib signin::tests`: 7 passed. The concurrent
  callback fixture yields after atomic removal and before exchange, so the
  competing callback actually overlaps and observes consumed state.
- `cargo test -p ghinvite-web --lib session::tests`: 7 passed, including the
  separate legacy-session diagnostic redaction regression.
- `cargo clippy -p ghinvite-web --lib --tests -- -D warnings`: passed.
- `cargo clippy -p ghinvite-web-worker --lib --target wasm32-unknown-unknown -- -D warnings`:
  passed with the deployed Worker's feature set. A standalone `ghinvite-web`
  wasm check does not enable the transitive `getrandom 0.4` browser backend and
  is not a supported standalone deployment check.

These checks add no remote GitHub evidence. They do not replace the actual
Worker/session cutover acceptance or the live SP-19a gate.


Pending adapter verification (2026-10-16):

- Eight actual SQLite tests pass, covering independent concurrent consumers and full concurrent
  service callbacks, raw encrypted-row markers, exact expiry, authenticated expiry
  metadata, cross-session/purpose rejection, failed DELETE, lost committed ACK,
  lagging/wrong-key retries, exact-CAS replacement and bounded cleanup.
- `node tests/worker/pending-signin.mjs` passes on the ordinary runtime-test Worker.
  Two independent Worker isolates share real D1; exactly one callback exchanges
  the code, reads the user and revokes the token. Actual D1 DELETE abort and lost
  committed ACK prevent exchange; gen2-on-gen1 and wrong-root key id retry preserve
  rows; a deterministic replacement between decrypt and CAS survives; fixed
  expiry and cross-session/purpose failures release no authority.
- The Worker proof scans raw D1 rows and complete runtime logs (1 MiB cap, failing
  on overflow) for raw/base64/URL-encoded state, PKCE, OAuth-code, token and private
  database-error markers. It verifies the logged kekring codes and fingerprints.
  This is fixture-local evidence, not a deployed browser or platform-log claim.
- `ghinvite-core`'s `purpose` tests pin the exact canonical AAD bytes of every
  purpose, and bidirectional cross-purpose/cross-session rejection is checked. Native web strict
  Clippy and actual web-worker runtime-tests wasm strict Clippy pass.

Evidence logs: `/tmp/platform-pending-signin-sqlite-final.log`,
`/tmp/platform-pending-signin-workerd.log`, `/tmp/platform-pending-signin-context.log`,
`/tmp/platform-pending-signin-native-clippy-final.log`, and
`/tmp/platform-pending-signin-worker-clippy.log`. The Worker fixture endpoints are
compiled only with `runtime-tests`; production normal-route selection remains
controlled by `GH_SIGNIN_ENABLED`, default false.


## Domain restriction route verification (local opt-in)

The anonymous offer reads the separate capability-only `InvitationLink.offer`
command and shows target grants and required domains before sign-in. It does not
invent a requester identity. The authenticated callback resolves current rules
using the pending hash/generation and authenticated numeric user ID. Only verified,
non-noreply emails whose UTS46-normalized domain matches a current exact/subdomain
rule produce evidence. The complete email walk must succeed; a later-page failure
returns no identity/evidence and still attempts revocation.

Web forms preserve raw invalid input, normalize/deduplicate valid rules, enforce
32 unique rules plus a 4096-character input bound, and copy exact rules when
Duplicating a link. Disabled sign-in composition rejects restricted creation and
renders restricted offers unavailable. Direct admission still relies on the
owner's authoritative evidence check. Reauthentication clears prior evidence and
any transitional token with an explicit session save before reads; a failed
callback preserves the previous identity and capability without previous domain
authority. New account identity is installed only after the full callback succeeds.

Native validation so far: web library 144 passed (`/tmp/platform-domain-web-lib2.log`)
and startup 11, Console 114, requester 30, OAuth 15, document-shell 3 passed
(`/tmp/platform-domain-http-final.log`; six browser server fixtures ignored).
The HTTP tests use a declared authority response fixture, not Restate evidence.
They prove PKCE/SQL-adapter composition, token-free rotated sessions, verified-only
matched-domain payloads, failed reauthentication persistence, future/wrong-link
refusals, exact old refusal versus fresh generic navigation, and restricted
creation/Duplicate/default-disabled behavior. Actual Worker and enhanced-browser
checks are recorded below only after running.

The actual Worker route gate now passes (`node tests/worker/domain-signin.mjs`,
`/tmp/platform-domain-signin-workerd2.log`). It uses real Worker code, D1 atomic
pending-state consumption, encrypted KV sessions and an explicitly scripted HTTP
owner; it does not substitute this fixture for the separate real Restate admission
proof. It checks the anonymous capability offer, PKCE, matching-only evidence,
rotated token-free sessions, fresh submission, and failed account-switch reauth.
The latter retains an unresolved local attempt: both generic and operation-bound
fresh recovery must recheck current proof, while exact old input remains replayable.
A complete bounded log capture (overflow fails), every application D1 table,
pending rows captured before consumption, and both session/authority KV namespaces
contain none of the raw email, unmatched domain, token, code, state or verifier
markers. Token revocation occurs after both successful and failed email reads.

The UI/island/parity suite passes 192 tests (`/tmp/platform-domain-ui-tests3.log`),
including the rendered-control sanitiser contract for the new domain input.

Fresh-recovery native regression passes all three domain HTTP tests
(`/tmp/platform-domain-recovery-native2.log`); an interrupted `prepare_attempt`
retains recoverable immutable input but cannot bypass fresh proof checks. The
actual Worker startup matrix passes 26 cases (`/tmp/platform-domain-startup-workerd.log`),
including complete typed configuration without legacy OAuth credentials, missing
SignIn credentials, and invalid opt-in values. The rebuilt island is 466 KiB gzip
against the existing 600 KiB budget (`/tmp/platform-domain-island-build2.log`).

Final full web library validation passes 145 tests
(`/tmp/platform-domain-web-lib-final.log`). Native web/UI/island all-target
Clippy and the actual web-worker wasm feature-set Clippy both pass with warnings
denied (`/tmp/platform-domain-native-clippy2.log`,
`/tmp/platform-domain-wasm-clippy.log`). These local gates do not establish live
GitHub permission visibility or consent behavior; SP-19 and SP-19a remain open.

The full browser suite passes **352 tests** in 2.6 minutes
(`/tmp/platform-domain-browser.log`). Eight domain-control cases cover native and
enhanced forms across desktop/mobile and light/dark projects, preserving raw
submitted rules and displaying normalized IDNA subdomain previews. The other 344
existing form, takeover, accessibility, organization-selection and refresh cases
remain green. This browser run captured the domain artifact before the subsequent
advisory organization eligibility panel was added.

After adding the advisory eligibility panel, the combined browser suite passes
360 tests (`/tmp/platform-eligibility-browser-final.log`) and the refreshed actual
Worker repeats the domain/D1/KV gate (`/tmp/platform-domain-signin-workerd-final2.log`).
Complete runtime logs are scanned after Worker disposal, including shutdown
output. Both root and application CI workflows run `domain-signin.mjs` immediately
after pending-signin against the ordinary web fixture artifact; the package
`test:domain-signin` command builds that artifact when run independently.


## Token-free cutover and pending keyring retry (local, 2026-10-17)

Both deployment roots select `ProtectedStore::with_token_free_sessions(true)`
when the same configuration selects typed normal routes. Fresh records omit
`access_token`; an empty transitional field is removed on write without renewing
its lifetime. Credential-bearing or malformed credential snapshots cannot load,
save, or create under a different ID. Their encrypted records and revocation
markers are left untouched. Fresh sign-in is required. ADR0002 describes the
operator's full-fleet cutover/drain and rollback constraints; this is not an
upstream-token revocation mechanism.

`StateUnavailable::KeyringRetry` passes through `SignInError::KeyringRetry` to
HTTP503. The lagging isolate does not consume/recreate the pending row, exchange
a code, or replace the cookie. The original state/code pair retries against the
updated keyring and completes once. Configuration failures retain the fixed500
mapping and operator alert rather than becoming an anonymous session.

- Native focused46 pass: `invitation_resolution`33, `keyring_sessions`5,
  `pending_signin`8 (two preexisting invitation fixtures ignored). The new
  route proof uses real SQLite pending state and asserts exact ciphertext/expiry,
  no exchange on503, and successful same-operation retry. Session proof uses
  real protected SQLite records and checks fixed lifetime, moved-ID/stale-save
  rejection, no revocation/deletion and absent credential field.
- `node tests/worker/signin-cutover.mjs` passes with actual normal Worker
  routes, shared KV/D1 and two keyring generations. Real protected legacy
  records fail operator authorization; new token-free records succeed. The
  real normal callback returns503 for gen2 pending material on a gen1 isolate,
  preserves cookie/row and sends no GitHub request, then retries exactly once
  on the updated isolate. Successful callback rotates ID and omits the field.
  Runtime-only fixture endpoints seed/inspect bounded facts and exercise stale
  store writes; they are absent from production builds.
- All application D1 tables and both KV namespaces are scanned for raw,
  base64 and URL-encoded credential/state/PKCE markers. Complete runtime logs
  fail on2MiB overflow; application snapshots fail on16MiB/5000-record overflow.
  These are fixture-local evidence, not deployed log-retention guarantees.

Evidence: `/tmp/platform-signin-cutover-native.log`,
`/tmp/platform-signin-cutover-web-build2.log`,
`/tmp/platform-signin-cutover-workerd-final.log`. Both CI workflows run the actual
Worker check after domain sign-in; `npm run test:signin-cutover` builds its
ordinary runtime artifact first. Defaultfalse and the live SP19a gate remain.

Inherited verification after the cutover changes: native web library159,
OAuth HTTP15 and protected-session6 pass in
`/tmp/platform-signin-cutover-regressions.log`. Existing actual Worker
pending-state and typed domain-route suites pass in
`/tmp/platform-signin-cutover-pending-workerd.log` and
`/tmp/platform-signin-cutover-domain-workerd.log`.
Native web/server library and test strict Clippy passes in
`/tmp/platform-signin-cutover-native-strict.log`; owned-source formatting and
new Worker runner syntax checks pass.
Actual web-worker runtime-tests wasm strict Clippy also passes in
`/tmp/platform-signin-cutover-wasm-strict.log`.
