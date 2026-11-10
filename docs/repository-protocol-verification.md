# Repository protocol verification

> **Note (2026-10-21):** gh-toolkit, its scripts (`check-toolkit-boundary`, `verify-toolkit-features`, `check-toolkit-error-codes`) and the `implementation/gh-toolkit` workspace were removed (GM-10, `plans/ghinvite-toolkit-migration.md`). ghinvite now uses `crates/ghinvite-github` and the octoauth crates. The dated records below describe the toolkit as it was.

The B0-10 acceptance suite runs the production RepositoryDelivery handler through
an actual Restate server, SQLite, and a real HTTP GitHub fixture. It lives in
`crates/ghinvite-workflows/tests/repository_protocol.rs` and runs in the P1 CI
Restate job. It does not replace live GitHub acceptance.

Verified on 2026-10-16: seven tests passed, none ignored (7.22 seconds), using
`/tmp/p1-restate-bin/restate-server`. Latest runtime log:
`/tmp/p1-policy-protocol-runtime.log`. Nine focused protocol tests and 207
toolkit library tests also pass (`/tmp/p1-policy-protocol-unit.log`,
`/tmp/p1-policy-toolkit.log`). Toolkit, protocol and UI strict Clippy pass
(`/tmp/p1-policy-toolkit-clippy.log`, `/tmp/p1-policy-protocol-clippy.log`,
`/tmp/p1-policy-ui-clippy.log`). The focused UI policy-guidance test passes
(`/tmp/p1-policy-ui.log`).

| Scenario | Observed result |
| --- | --- |
| Existing maintain role, target write | AlreadySatisfied; no write or fence |
| Existing custom role | SkippedCustomRole; no write or fence |
| Existing read role, PUT 204 | Upgraded read→write; generation 1 fence |
| Existing pending admin invitation | AlreadyPending admin; no write or fence |
| Existing pending read invitation, target write | Exactly one PATCH; AlreadyPending write |
| S1 read role changes to maintain before S4 | AlreadySatisfied; no mutation or fence |
| PUT 401, remint, PUT 201 | Generation 1 released before remint; generation 2 claimed; Created; two WRITE mints plus the initial READ mint |
| PUT 401, remint, PUT 401 | Both generations released; Blocked InstallationUnavailable; durable `Installation/repo:1/recheck_permissions` dispatch after the guarded write/release completion; actual permission verification sees generation 2 released |
| WRITE mint 422 | Blocked InstallationPermissions; no PUT, PATCH or fence; app-scoped permission recheck executes |
| Successful PUT followed by three forced journal acknowledgement losses | Three actual guarded run errors; positive read-only invitation recovery; Created; exactly one PUT |
| HTTP timeout with no invitation evidence, followed by three create calls | OutcomeUnknown on every call; generation 1 retained; exactly one PUT |
| Positive outside-policy 403/422 and IP-denial 403 | Typed blocked reason; generation 1 released after retained rejection; later recheck uses generation 2 and one PUT per invocation; no upstream policy text or marker in create journals |

The fixture records nonsecret HTTP operations, profile/generation identities,
and the SQL fence visible during each request. Confirmed replay adds no GitHub
calls. The bounded scan of each retained create invocation journal and reply
finds no fixture token marker. This is not the full AC-JRN-1 `sys_*` proof.

The recheck proof retains both installation owners' journals. It queries actual
`sys_invocation` metadata for the exact `Installation/repo:1` handler, waits for
successful completion, and inspects the wrapper's call to
`AccountInstallation/repo:100`. The create journal orders the durable one-way
send after the guarded stage completion; the real GitHub verification request
observes the released SQL fence. The double-401 path still has exactly two PUTs
and two write-attempt mints. One additional WRITE mint belongs to permission
refresh after release. The mint-422 path has one refused mint and one refresh
mint, with no write fence at any point.

The acknowledgement fault is compiled only with the `integration` feature. A
counter makes the run closure return an error three times after a confirmed
write or positive recovery; it does not issue three writes. The timeout case
separately checks three later public handler invocations with absent evidence.

## Positive policy refusals

Repository writes now retain a typed `OutsideCollaboratorsRestricted` blocked
reason for complete positive policy messages on 403 or 422. IP-denial 403s with
the recognized complete template retain `IpAllowList`. Both return the rejected
fence generation from S4; the owner journals that result before releasing the
exact generation in S5. A later invocation may recheck a blocked outcome, using
a newly claimed generation; blocked outcomes are not terminal receipts.

The toolkit inspects at most 64 KiB of JSON and 32 nested error entries, and
policy messages must be at most 1024 bytes. It retains only enum values and
validation message lengths. The outside-collaborator classifier accepts whole
messages, case-insensitively, with an optional final period:

- `Outside collaborators are not allowed in this organization`
- `You are not allowed to invite outside collaborators to this organization`
- `Inviting outside collaborators is restricted by organization policy`
- `Inviting outside collaborators is restricted by enterprise policy`

These are explicit conservative accepted shapes, not a claim that live GitHub
currently emits all of them. The local research and GitHub's
[collaborator endpoint documentation](https://docs.github.com/en/rest/collaborators/collaborators#add-a-repository-collaborator)
establish the policy and status family, without specifying an exact error-body
contract. SP-07's live installation-token policy behavior and wording remain
unverified. Generic, conditional, negated, quoted and appended text does not
match. Rate-limit evidence and explicit anti-spam codes take precedence.

For IP restrictions, the classifier recognizes the complete denial template
reported first-hand in
[github/github-mcp-server issue 748](https://github.com/github/github-mcp-server/issues/748),
requiring a bounded GitHub-compatible organization name between the fixed
prefix and suffix. This is observed API evidence, not a verified IAT contract.
Names and upstream prose never reach the retained error or UI. The UI offers
fixed policy guidance and explains the service's lack of fixed outbound IPs.

`ip_allow_list` is an additive serialized `BlockedReason` tag; existing tags
are unchanged. Readers built before this variant do not understand new IP
refusal receipts, so rollback compatibility requires keeping readers capable
of decoding the new tag while such receipts are retained.

Run from `implementation/ghinvite`:

```sh
RESTATE_SERVER_BIN=/path/to/restate-server CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=2 \
  ../../scripts/dev.sh cargo test --locked -p ghinvite-workflows \
  --features integration --test repository_protocol -- --test-threads=1
CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=2 ../../scripts/dev.sh cargo clippy \
  -p ghinvite-workflows --features integration --test repository_protocol -- -D warnings
```

The local double-401 release/recheck clause of AC-DLV-5 is now verified through
the production app-scoped handlers. The M1 budget reservation, hold/cascade
extensions, complete journal proof, inherited suites, Worker and live GitHub
gates have their own evidence requirements and are not claimed here.

## L2 extraction audit (2026-10-17)

This source inventory is preparatory, not EX-01 acceptance. The current toolkit
provides multi-app registries, opaque broker credentials, read-only profiles,
generic write-budget ledgers, typed REST endpoints and complete/streaming
pagination. These are suitable foundations for P2's viewer/merger/sign-in apps;
application availability and workflow identities remain outside L2.

| Gate | Concrete remaining work |
| --- | --- |
| EX-01 | Production readiness and seven-day SD-L2-a observations; local tests do not substitute for live evidence. Shared token cache remains conditional on BR-17's measured trigger. |
| EX-02 | Remove transitional installation/raw-token facades after migrating consumers; reconcile feature/wasm contract, conditional requests, partial listings and remaining API deltas before moving code. Local quota/preflight/OAuth-budget and open-enum evidence is recorded below; it does not satisfy the complete breaking-delta or extraction gates. The typed sign-in cutover remains default-off and separately gated. |
| EX-03 | Move both toolkit and Worker transport crates into an independent workspace; preserve provenance/history and licenses, replace inherited workspace metadata/dependencies, add LICENSE, deny/advisory rationale, recorded stub contract, error-code/public-API snapshots and requirement coverage. Current uncommitted in-tree source is not yet preserved extraction history. |
| EX-04 | Expand CI beyond the passing default dependency/source boundary and injected-dependency negative control: native/wasm feature matrix, public API/re-export/P2 vocabulary checks, wasm dependency constraints, snapshot diffs, disclosure/trybuild, cargo-deny and standalone scratch builds. |
| EX-05–07 | Release artifact/tag, P1 dependency switch without duplicate code, complete post-switch CI and staging acceptance remain unverified. No remote publication has occurred. |
| EX-08 | Add acting-user token/GrantTokens exchange, GraphQL/search/PR/check APIs and events, CustomProperties permission, check-token/delete-grant/SSO support and the L1 refresher after L1 exists. Current SignIn-only exchange does not provide the acting-user API. |
| EX-09 | Test rollback of the dependency/artifact switch. |

The current Worker adapter uses actual Fetch with manual redirects and deadline
handling; the reqwest wasm implementation fails closed. TR-17 expressly permits
this safe fallback, so Fetch itself is not a readiness defect. The TR-03 public
`wasm::{assert_send, SendFut}` contract is now implemented and used by the adapter;
its focused native/actual-wasm evidence and remaining combined runtime boundary
are recorded below. No current vulnerability/advisory claim was made during the
initial read-only inventory.

The P1 workflow dependency cleanup is implemented locally: `AppState`, native
startup and Worker startup no longer construct or retain the old toolkit
`InstallationClient`. HTTP tests now exercise production typed protocol stages;
the parallel test-only delivery implementation and its legacy fixture builder
were removed. The current library suite passes 118 tests, and actual Restate
`canonical_link` and `retained_delivery` both pass with canonical snapshot reads
(`/tmp/platform-typed-canonical-unit.log`,
`/tmp/platform-canonical-retained-runtime.log`). Toolkit compatibility exports
and legacy web OAuth remain; typed sign-in activation stays default-off. This is
one EX-02 delta, not the full
extraction or sign-in activation gate. Workflow/server all-target native strict and workflow Worker runtime-tests wasm
strict pass (`/tmp/platform-typed-canonical-strict3.log`,
`/tmp/platform-typed-canonical-wasm-strict.log`). Signed actual Worker reissue
also passes on the new artifact, proving distinct canonical generation keys,
no newly allocated local invitation ULIDs, exact replay and D1 proof/audit
(`/tmp/platform-canonical-reissue-workerd.log`). The broader ordinary Worker
regression subsequently passed on the captured canonical artifacts
(`/tmp/platform-canonical-ordinary-workerd-final.log`): all migrated snapshot
reads and lifecycle evidence, D1 rollback/lost acknowledgements, replacement
installation recovery, both real GitHub header/body timeouts, both browser
mutation-timeout recovery paths and projection-outage convergence.

Separate signed Worker gates passed for reissue, approved delivery with every
projected parent withheld, retained sweep scheduling, authenticated journey and
seven cancellation cases (`/tmp/platform-canonical-{reissue,approved,sweep,journey,cancellation}-workerd.log`).
The journey forced three actual guarded-run failures with one PUT, then scanned
17 public system tables, 1,021 raw journal entries, 32 state rows, all 30 D1
application tables, session and authority-cache KV, and complete captured logs;
persisted negative controls were detected. The isolated cancellation build
remains explicitly enabled only for its fixture composition.

Two earlier ordinary-run failures are retained: one unexpected Unknown instead
of the scripted 204 upgrade (`/tmp/platform-canonical-ordinary-workerd.log`),
which did not reproduce in isolated/prefix tests or subsequent full runs; and a
later deadline-replay/cleanup timeout during severe workspace disk pressure
(`/tmp/platform-canonical-ordinary-workerd2.log`). No production fix, relaxed
assertion or increased watchdog was used. Temporary diagnostics were removed
before the clean passing run. Disk pressure is observed context, not a proven
cause of the first failure.

These artifacts include the canonical identity/read cutover and the corrected
None/misbound owner-status display. They predate the subsequent toolkit quota
observation and non-exhaustive enum edits; those changes require their own
current-source verification. No deployed endpoint drain or schema migration
is implied by these local results.

### Endpoint API-version plumbing (local EX-02 delta)

L2 TR-05 is implemented in the in-tree transport: typed endpoint calls pass
`EndpointMeta.api_version` to request construction, with `2022-11-28` for `None`.
This includes every validated pagination page, metered repository/team pages,
broker mint and installation confirmation, installation/user-token revocation,
authenticated-user reads, and the email invitation wrapper. Public generic
`Rest::call` keeps its existing default and signature. Existing endpoint version
values remain unchanged; the synthetic override in tests makes no claim about a
live GitHub version. OAuth exchange keeps its distinct header set without the
REST version header. Credential checks, redirect handling and retry limits are
unchanged.

Three focused regressions exercise default → override → default through actual
typed/generic dispatch, the same override on both pagination pages, and rejection
of incorrect credentials before sending. Existing broker and sign-in header
assertions cover mint/confirmation and OAuth/read/revoke paths, including the
opaque authorization and exact OAuth header set. These tests use the existing
mock HTTP transport; live version support is outside this proof.

Verification is green:

- `cargo test --locked -p gh-toolkit --lib --tests`: 249 unit and 40 integration tests, including ten compile-fail credential cases, exact OAuth headers, disclosure checks, manual redirects and actual native deadline bounds (`/tmp/toolkit-api-version-native2.log`). The three new override tests also pass independently (`/tmp/toolkit-api-version-tests.log`).
- `cargo clippy --locked -p gh-toolkit --all-targets -- -D warnings` (`/tmp/toolkit-api-version-native-strict2.log`). An inherited test-module placement warning was corrected by moving that module after production items; no lint allowance was added. The sealed-credential compiler snapshot was updated only for the compiler's ordering of the same four permitted implementation types.
- `cargo clippy --locked -p gh-toolkit --lib --target wasm32-unknown-unknown -- -D warnings` (`/tmp/toolkit-api-version-wasm-strict.log`).

The final ordinary web Worker capture includes the changed transport
(`/tmp/platform-canonical-web-capture2.log`). This closes only endpoint version
plumbing, not EX-02 as a whole or extraction/release acceptance. No publication,
remote release, deployment, or live GitHub version claim is made.

### Extensible role and result enums (local EX-02 delta)

The toolkit's `OrgRole`, `TeamRole`, `MembershipState`, organization
`BasePermission`, observed `InvitationRole`, outbound `CreateInvitationRole`,
`CollaboratorWrite`, and `RateLimitScope` now use `#[non_exhaustive]`. Existing
variants and serialized wire values are unchanged. `RepoRole`, endpoint and
permission enums, and the specified typed API/broker/authentication errors were
already open. `CallError` remains the explicitly specified closed `Api`/`App`
wrapper; this change does not remove or redesign the legacy `Error` facade.
Other extraction compatibility work remains in the EX-02 inventory.

P1 mappings now refuse unknown future roles/states with bounded decode errors
or unavailable evidence. Organization/team access removal no longer interprets
“not admin/maintainer” as “member.” Organization observations validate roles and
states before treating pending membership as an invitation; team writes validate
the returned role and state before confirming a grant. An unknown collaborator
write result becomes uncertain while preserving its write fence and charge.
Complete organization list walks refuse unknown role conversions, rather than
using an incomplete list as absence evidence. Unknown organization base-role
facts remain advisory `None`.

A downstream compile-fail fixture requires wildcard handling for all eight newly
open enums. The typed HTTP regression rejects unknown organization/team roles
and membership states without retaining their marker text. Known membership
wire values continue to decode as before. The all-target web/workflow integration
compile passes (`/tmp/toolkit-open-enums-consumer-check2.log`); toolkit, web and
workflow native lib/tests strict Clippy passes
(`/tmp/toolkit-open-enums-native-strict.log`). The shared current toolkit suite
passes 257 unit tests, including the new wire regression and the concurrent
quota-observation tests (`/tmp/toolkit-quota-all-unit.log`).

Final credential/public-API tests pass with no snapshot overwrite: three tests
including eleven compile-fail fixtures (the new fixture checks eight independent
open-enum errors), `/tmp/toolkit-open-enums-ui-final.log`. Combined toolkit,
workflow Worker and web Worker wasm strict Clippy also passes on the changed
caller graph (`/tmp/toolkit-quota-consumers-wasm-strict.log`). No extraction,
release or deployment gate is claimed by these local checks.


### Quota observations and OAuth-token volume (local EX-02 delta)

The in-tree `observations` module now stores complete numeric primary-quota
headers per credential bucket and known resource, alongside the shared signing
clock estimate and an isolate-local sliding one-hour OAuth-token counter. Typed
REST, complete pagination, metered pages, installation mints and typed sign-in
exchange use the same response observer before body decoding. Production
constructors share observations; `test-mock` constructors and explicit test
stores are isolated. Unknown resource text and incomplete/invalid quota headers
are discarded rather than retained as fail-fast evidence.

An exhausted attributed bucket/resource fails before transport until its reset,
using the observed server clock. Another app, installation, user, client or
resource remains independent. `Rest::no_preflight()` bypasses only that local
check, including the whole pagination walk; responses remain observed. Fresh
user tokens are `Unattributed` until bound to a numeric user and never fail fast.
Manifest conversions, code search, ordinary search and GraphQL select separate
resource families; the mapping follows GitHub's [rate-limit resource documentation](https://docs.github.com/en/rest/rate-limit/rate-limit).
The retained compatibility installation facade and the rest of the L2 target
API still require their separate EX-02 inventory; these checks do not claim all
GitHub API resource families or final extraction readiness.

Every dispatched installation mint and typed or legacy OAuth code exchange
increments the app/client counter, including refused responses or transport
failures. Cache hits, invalid local input and pre-flight refusals do not increment
it. OAuth-only configuration without a numeric App ID joins the same known
client budget in either initialization order. Counts at 1,000 and 1,600 requests
emit secret-free `OAuthBudget` Warn/High events at most once per rolling hour per
level. These signals neither reserve capacity nor refuse requests; totals across
isolates still require host metrics. Future L1 refreshes must use the same seam.
The default-off typed sign-in flag remains unchanged. The active legacy callback
now counts exchanges and observes responses; compatibility user GETs/list pages
observe as `Unattributed` without inventing a numeric identity.

Current native evidence: 264 toolkit unit tests pass after the final read-only
metrics correction (`/tmp/toolkit-quota-readonly-final2.log`). The complete prior
run passes 263 units plus all 40 integration tests, including privacy, the eleven
compile-fail fixtures and the actual 30-second transport deadline
(`/tmp/toolkit-quota-full-native.log`); the only subsequent production edit makes
metric lookup nonbinding and entirely immutable, including expiry calculation,
and has a dedicated regression. Seven typed sign-in
contract tests also pass (`/tmp/toolkit-quota-native-final2.log`). The focused first observer
suite passed six cases (`/tmp/toolkit-quota-focused.log`); subsequent additions
cover numeric/OAuth-only counter identity in both orders, independent public
mock environments, manifest/code-search scope, and pre-flight refusal preserving
the exact 401-remint cause without recording a mint attempt. A metrics read with
a different numeric App ID cannot bind or split an OAuth-only counter; only an
actual token request may establish that association. Three additional
legacy transport tests pass within the 23-case OAuth suite
(`/tmp/toolkit-legacy-oauth-observations.log`). They cover shared counter identity,
503/transport failure counts, unchanged response privacy, failed user reads and
multi-page observation. Combined toolkit/web/workflow native strict passes
(`/tmp/toolkit-final-native-strict.log`), as does toolkit plus both real Worker
crates on wasm/runtime-tests (`/tmp/toolkit-final-worker-wasm-strict.log`). After
the private metrics-accessor correction, toolkit native lib/tests and wasm strict
pass again (`/tmp/toolkit-quota-immutable-{native,wasm}-strict.log`). Fresh Worker
artifacts include that final correction. The complete ordinary signed Worker
runner and privacy journey passed (`/tmp/platform-quota-ordinary-workerd.log`,
`/tmp/platform-quota-journey-workerd.log`). The latter forced three real guarded-run
failures with one PUT and scanned 17 system tables, 1,037 raw journal entries,
32 state rows, all 30 application tables, session/authority KV and full logs,
including persisted negative controls. The full serial web refresh was interrupted
by ENOSPC after four passing targets; it is not a complete web regression claim.


### Remaining target and P2 fit, after the local EX-02 increments

This is an inventory of the in-tree source, not a reduced L2 contract. The first
implementation priority identified was the typed transport seam: the old
`transport::HttpTransport` required a legacy `Error` result and supplied
`send_typed` through a pessimistic compatibility default, alongside a separate
`rest::HttpTransportDyn`. The local migration described below now implements
TR-20's typed async method and handwritten boxed-future bridge. Migrating adapters
in place preserves dispatch certainty before freezing the extracted public API. Compatibility conversion remains necessary
for P1's explicitly enabled `legacy-p1-oauth` route until its existing activation gate is met.

| Priority / gate | Current fit and remaining implementation |
| --- | --- |
| EX-02, transport and features | Typed transport/dyn migration, optional/default-on `reqwest`, explicit legacy OAuth isolation and the TR-03 public wasm wrapper are implemented below. After P2 REST and Acting integration, before the subsequent HTTP/TLS security patch updates, all256 native/wasm production configurations and their dependency checks pass (`/tmp/l2-acting-matrix-final3.log`). The first run exposed a missing Wasm getrandom0.4 backend; the correction is optional, target-specific, and activated only by `rotating-credential`. Earlier combined ordinary/privacy/email runtime gates require fresh captures/consumers. Preserve manual redirects and native/Worker deadline semantics throughout migration. |
| EX-02 / D-11, user reads | `list_user_installations` and `list_user_installation_repos` have User-only metadata and typed wrappers with exhaustive-count/identity checks. P1 alone explicitly enables legacy OAuth until its rollout gate; these reads no longer depend on that raw-token facade. |
| Additive TR-11 / PG-07 | Conditional GET and optional ETag-store list revalidation are implemented below; default remains off and live rotating-token behavior remains unverified. Complete typed list walks fail closed on SSO partial results and retain up to 100 numeric organization IDs. The REST opt-in `allow_partial` contract is now implemented below, including explicit collected/streamed completeness; GraphQL is implemented in the later section. Generic `Rest::call` now preserves those IDs through the shared parser. Live rotating-token ETag and user-token SSO behavior remain separate spike questions. |
| P2 M0 / EX-08 | Registry slots/purposes, HMAC router, profiles, `Workflows`, REST walks and write-ledger primitives fit. GraphQL transport/cursor/error/cost handling, search cap, installation-repository filter and ten additional PR/review/check/status/push/repository/authorization event decoders are implemented and tested. PR/activity/property REST reads and `CustomProperties` are now registered with ten focused P2 REST cases passing; full post-addition checks follow. Query documents and merge policy stay in P2. |
| P2 M1 / EX-08 | Async merge submission/polling and review/comment endpoints, operator credentials, and Basic-bound check/delete-grant APIs are implemented. `OAuthConfig<Acting>::exchange_grant`, opaque `GrantTokens`, conversion into the actual L1 `TokenPair`, exact credential/context mapping, and `GithubUserTokenRefresher` are registered. The real15-case refresher contract, transport deadline cases, and maximal410/minimal303/default349 toolkit suites pass (`/tmp/l2-acting-verification.json`). These are provider-adapter proofs, not full executor or deployed conformance. |
| EX-03/04 | The71-code append-only snapshot and negative-control CI are implemented. The current256-configuration production/dependency matrix, trybuild and disclosure suites pass. MIT/Apache licenses and scoped GA-06 advisory rationale are present. Compiler-rendered native minimal/default/extended, wasm-extended and fixture API snapshots were generated and passed the consumer-vocabulary checker (`/tmp/l2-api-snapshots-final.log`). Actual Rust1.91.1 minimal/default/extended library checks pass. Documentation for813 compiler-reported gaps and four rustdoc links is registered; strict validation and helper-surface completion are underway. A portable53-endpoint/five-family provenance contract and93-ID requirement inventory (85 test anchors/eight explicit external or conditional gates) now pass their scripts and negative controls; four new metadata/wire assertions await compilation. API/docs/MSRV/dependency and inventory CI is registered, with hosted execution unverified. Current five-flavor API comparison and actual package-scoped cargo-deny pass after narrow h2/TLS security patches (`/tmp/l2-public-api-verification.json`, `/tmp/l2-dependency-audit.json`). The fresh temporary eight-package bundle `/tmp/l2-portable-bundle-20261017-4` passes four actual locked library builds: native and wasm32 minimal (`--no-default-features`) and both toolkit/Worker packages with `--all-features`. The preceding bundle exposed native fixture-server dependencies leaking into wasm; optional axum/wiremock and the stub module are now native-only, preserving every applicable client feature. All 251 source/transformed-file hashes match at capture; the resolved lock and all six resolved internal package paths are verified (`verification.json`, `/tmp/l2-portable4-build-results.json`). This is scratch portability evidence with internal paths; full no-path independence, history/release/switch and live gates remain open. |

Legacy facade code still hard-codes the `ghinvite` user-agent and contains P1/
Restate commentary; typed calls use the configured app user-agent. The source
boundary test currently finds no forbidden application imports, but this is not
a complete extraction review by itself. The compiler-rendered API vocabulary
check described above adds exported paths and signatures. Shared-cache storage and
WebCrypto signing remain conditional on their specified measured triggers;
absence alone is not an unconditional implementation failure. EX-01 production
observations and EX-05–09 publication/switch/staging/rollback remain unverified.


### Typed transport and object-safe bridge (local EX-02 increment)

The migration identified above is now implemented in source. `HttpTransport`
uses the specified `trait_variant` Send contract and one typed `send` method;
`HttpTransportDyn` is a handwritten boxed-future trait with a blanket adapter.
There is no default that converts arbitrary legacy errors into dispatch evidence.
Native reqwest, Worker Fetch and mocks implement the same typed method. P1 keeps
`Arc<dyn HttpTransportDyn>` in its web roots and state; three redundant production
web transport wrappers were removed. Existing `rest::HttpTransportDyn` imports
remain valid. A private legacy adapter reduces typed errors only for the retained
OAuth/installation compatibility facades, preserving the default-off sign-in
cutover and its live gate.

Toolkit all-target compilation and strict Clippy pass
(`/tmp/typed-transport-toolkit-check2.log`, `/tmp/typed-transport-toolkit-strict.log`).
All 264 toolkit unit tests pass (`/tmp/typed-transport-toolkit-unit.log`). Focused
integration tests pass 24 cases, including eleven compile-fail privacy fixtures,
actual connection refusal, both post-dispatch timeout phases, streamed/advertised
oversize bodies, the real 30-second deadline, no redirect repeat, legacy disclosure
and typed sign-in (`/tmp/typed-transport-focused-tests.log`). A new blanket-adapter
regression proves one dispatch and exact preservation of all four failure values
through a Send boxed future. Toolkit plus the real Fetch adapter pass wasm strict
(`/tmp/typed-transport-wasm-strict.log`). Dependent P1 native all-target strict also passes for web, its native server and
workflow integration (`/tmp/typed-transport-consumer-strict2.log`). The actual web
Worker runtime-tests graph compiles and passes wasm strict
(`/tmp/typed-transport-web-worker-check.log`,
`/tmp/typed-transport-web-worker-strict.log`). Workflow Worker runtime-tests wasm strict also passes
(`/tmp/typed-transport-workflows-worker-strict.log`). Fresh ordinary workflow and
web captures then passed the full signed ordinary runner, privacy journey and
email recovery gate (`/tmp/p1-final-gap-{ordinary,journey,email}-workerd.log`).
The journey includes real requester-status/refresh authorization under delayed
projection, three actual guarded-run losses and one PUT, plus all 17 discovered
system tables, 1265 raw journals, 33 states, all 30 application D1 tables, session
and authority KV and full logs. Captured binary hashes are retained in
`/tmp/p1-final-gap-artifact-sha256.txt`; two later Console rustfmt-only expression
changes do not alter semantics (boundary record `/tmp/p1-final-gap-capture-boundary.json`).
Actual D151 and signed organization timing also pass on these immutable artifacts.
These focused and runtime gates do not claim complete EX-02 closure or a complete
current-source native aggregate.


### Typed user-installation reads (local D-11 / EX-02 increment)

`Endpoint::ListUserInstallations` and `ListUserInstallationRepos` now declare
User-only authentication, GET pagination, safe retries and an explicit
no-additional-permission explanation. Public functions in `endpoints` (also
re-exported from `signin` when OAuth is enabled) accept only credential-scoped
`Rest`; the repository lookup takes `InstallationId`. Non-exhaustive
`UserInstallation` and `RepoRef` results retain exact typed installation/repository
identities and minimal display/ownership facts. The paths and credential contract
follow [GitHub's installation API documentation](https://docs.github.com/en/rest/apps/installations#list-app-installations-accessible-to-the-user-access-token).

Both reuse the existing bounded complete-page walker and endpoint API-version
metadata. Each envelope must preserve its reported total; final counts must
match, and zero/duplicate numeric identities fail closed. A later HTTP/shape
failure, cycle, foreign origin or different endpoint cannot return a successful
prefix. SSO partial responses preserve the existing typed `Partial` error with
at most 100 numeric organization IDs and no upstream text. The generic REST `allow_partial` / `Listing.partial` contract is implemented
below. These typed exhaustive functions deliberately keep returning an error
for SSO omissions; this does not claim live user-token SSO behavior.

New public-interface tests pass both 101-item walks, wrong-credential/zero-target
no-send controls, and 20 negative walks across both endpoints
(`/tmp/typed-user-installation-tests.log`). The full sign-in contract suite passes
all ten tests (`/tmp/typed-user-installation-signin-full.log`), preserving opaque
exchange/revocation/privacy behavior. Native library plus sign-in test strict and
wasm32 library strict pass (`/tmp/typed-user-installation-{strict,wasm-strict}.log`).
No P1 route changes, raw token accessor, extra sign-in permission or live rollout
are introduced. The earlier captured P1 runtime evidence predates this additive
slice; final extraction/switch verification remains separate.

### Production feature separation (local EX-02 increment)

The source now makes `reqwest` optional and default-on; with it disabled the
transport traits, typed REST client and externally supplied Fetch adapter remain
available. Typed sign-in modules/exports follow `oauth`; the later compatibility migration places legacy OAuth behind explicit `legacy-p1-oauth` (which implies `oauth`).
The wasm compile smoke separates its optional OAuth references. The self
dev-dependency disables defaults, so no-default test runs exercise the selected
feature set. Feature-specific HTTP, OAuth and webhook integration targets name
their required features; core disclosure and credential compile-fail contracts
remain available without defaults, with the OAuth-only credential case enabled
only when its type exists.

`scripts/verify-toolkit-features.sh` compiles all eight combinations of the
currently implemented `reqwest`, `oauth` and `webhooks` features independently on
native and wasm32 with strict Clippy. Each combination checks its actual production dependency
tree: reqwest and octoevents must follow their feature, application/runtime/store
dependencies are forbidden, and wasm32 must exclude Tokio. Root P1 CI now runs
the matrix and no-default tests. All16 strict production combinations and their
dependency checks pass (`/tmp/toolkit-feature-matrix-strict.log`). Current
no-default runtime proof passes261 native tests across all6 available targets,
including10 nested compile-fail credential contracts
(`/tmp/toolkit-no-default-final.json`). The extra OAuth credential case runs
with defaults. The sealed-trait diagnostic has a distinct no-OAuth snapshot
because rustc correctly omits OAuth-only implementors; the error remains the
same inaccessible sealed-trait requirement. Original failed logs are retained:
one stale global endpoint inventory was corrected for both newly added User
reads, one concurrent partial-list edit needed restored imports, and the first
temporary direct runner needed Cargo's package cwd/manifest environment. The
serial runner now supplies both. No contract test was removed. This did
not itself implement GraphQL, conditional shared-cache or L1 features; later GraphQL and conditional-cache slices are recorded separately. The subsequent explicit `repo-admin` capability is verified below; P1 leaves it disabled.

### Explicit partial REST listings (local PG-07 increment)

The opt-in spelling is
`rest.allow_partial().list_all::<T>(endpoint, path, query).await`.
`Rest::allow_partial` returns a distinct borrowing `PartialRest`, leaving the
original client and all exhaustive typed endpoint signatures unchanged. Success
returns `Listing<T> { items, partial: Option<PartialInfo> }`; `complete()` derives
its answer from `partial` so no independent boolean can contradict the metadata.
`PartialInfo` retains only `PartialReason` and at most 100 unique positive numeric
organization IDs across the walk, never upstream header text. An SSO header with
no usable IDs still means incomplete.

The same view offers `for_each_page` with final `PageCompletion` metadata and a
`complete()` method. Callbacks can see earlier pages before a later failure;
callers must await final success and inspect completeness before treating absence
as evidence. Explicit opt-in includes items from SSO-partial pages and follows
all remaining links. A later transport, HTTP, decoding, callback, cyclic,
malformed/off-origin/different-endpoint link, or page-limit failure remains an
error, never a successful prefix. The existing default APIs still return typed
`Partial` immediately on an SSO page, before absorbing it or fetching the next
page. Both modes use the same sole bounded REST page walker, version dispatch,
credential check, quota observation and preflight setting.

Six focused tests pass without default features
(`/tmp/partial-listing-tests2.log`): collected and streaming partial/complete
results, default fail-closed behavior, multi-page bounded ID union/redaction,
later failure categories, callback error preservation, Send futures, credential
and endpoint guards, and a real 1,000-page limit walk. Native library/test strict
and wasm32 library strict pass without default features
(`/tmp/partial-listing-{native,wasm}-strict.log`).
This is the REST PG-07 slice; GraphQL partial-data/error semantics are implemented in the later section, and live user-token SSO remains SP-15 evidence.

### Conditional typed list requests (local TR-11 increment)

`Request::if_none_match(etag)` validates a single strong or weak entity tag and
rejects non-GET methods without echoing input. `Response::is_not_modified()`
recognizes HTTP 304. `rest.with_etag_store(&store)` opts the existing typed list
helpers, collected lists and streamed page walker into conditional requests;
`no_preflight()` preserves that choice. The default client, unattributed
credentials, single generic calls and writes do not use the store. P1 does not
opt in.

The application supplies the asynchronous `rest::EtagStore` (`get`, `put`,
`remove`) and owns its bounded retention, capacity, private-data protection and
backend acknowledgements. `EtagKey` contains the credential class and exact
nonsecret app/installation or OAuth client/user principal, a SHA-256 digest of the
complete URL/query, effective API version, Accept and User-Agent. It contains
neither the URL nor authorization/token bytes or a token-derived key. Opaque
serializable `EtagRecord` values contain the exact key, validator, body and only
ETag/Link/SSO/Vary/Cache-Control/content-type headers. Debug output omits contents;
records that echo the current credential in their body or retained headers are
not retained or reused. Cache records can still contain private GitHub data;
serialization is an explicit storage adapter seam, not permission to log it.

Every page still makes an authorized HTTP request. The real response, including
304 Date and quota evidence, passes through the normal observer exactly once.
A 304 can supply a body only from the exact locally held, validated record used
to send that request. Omitted Link and SSO metadata survive revalidation; supplied
representation metadata updates the record. Cached rate-limit or Date fields are
never replayed into observations. The common bounded walker still checks every
page and link, decodes the full shape, and fails on partial SSO evidence unless
`allow_partial()` was explicitly selected. A later HTTP/transport/decode/cache
miss/link failure never returns a complete successful prefix.

Invalid/misbound records and failed cache reads cause unconditional HTTP fetches.
Unexpected 304s, mismatched validators, nonempty 304 bodies and acknowledged-store
failures return a fixed error, without stale fallback. Fresh 200 responses replace
old representations; responses without usable validators clear the old entry.
`no-store`, `Vary: Authorization`, `Vary: *` and unsupported Vary dimensions are
not retained. A 304 that changes to such a policy clears the record and fails
closed. Supported Vary dimensions are included in the key. This is an
always-revalidated private response cache, not a general shared HTTP cache.
The response-update rule follows [RFC 9111 §4.3.4](https://www.rfc-editor.org/rfc/rfc9111.html#section-4.3.4).

Live reuse across rotating installation tokens and quota exemption remain
**unverified SD-L2-c**; the local rotating-credential test verifies isolation and
conditional-dispatch mechanics with scripted responses, not GitHub behavior.

Verification: all twelve ETag cases pass inside the complete 290-test toolkit
library run (`/tmp/requester-profile-gh-toolkit-runtime.log`). They exercise
actual typed repository and counted user-installation list helpers, two-page
200→304 walks, updated Link and fresh 200 replacement, partial SSO preservation,
fresh 304 quota observation, default-off/unattributed behavior, principal/class/
URL/version isolation, cache read/write failures, misbound records, later-page
transport/decode/304/link failures, unsupported Vary/no-store, malformed tags and
credential-echo rejection. A serialized memory adapter exercises the same opaque
record round trip offered to KV adapters. No live GitHub request is used.
No-default native library/test strict and wasm32 library strict also pass
(`/tmp/etag-{native,wasm}-strict.log`).

### Public wasm host-future adaptation (local TR-03 increment)

The toolkit exports `wasm::{assert_send, SendFut}` only on wasm32. The actual
Worker Fetch transport wraps its complete host operation through this public
seam. Its inner future remains pinned, every poll uses the current caller's
waker, cancellation drops it on its constructing thread, and the result is
unchanged, including a non-Send result. Debug output omits the future.

The only unsafe implementation is the audited `Send` implementation in
`src/wasm_send.rs`. Poll checks the constructing thread before touching the
future. Drop takes the pinned future into ManuallyDrop before consulting thread
identity, then destroys it only after positive owner identification. A foreign
drop or identity-check unwind leaks the future; foreign drop also avoids a
second panic during unwinding. This deliberately leaks a misused host
operation rather than executing a non-Send destructor on another thread. There
is no unsafe Sync implementation or public extraction of the inner value.
Production native builds exclude this module. The boundary checker permits
unsafe only in this exact toolkit file and rejects unsafe in the Worker adapter.

An initial compile-time atomics guard failed its negative control: stable rustc
hides unstable wasm target features from cfg even when explicitly enabled. That
failed proof remains in `/tmp/toolkit-wasm-send-isolated.log`; the final design
uses the runtime owner-thread checks described above instead.

The exact module passes seven native tests for pinning, current wakers,
cancellation, foreign-thread poll/drop, unwind and identity-check failure. The
same final module also passes actual Node wasm construction, Send-bound Rc
capture, polling and destruction
(`/tmp/toolkit-wasm-send-native7-actual-wasm.log`). The identity-failure test
rejects a compiled mutation restoring the earlier destructor ordering
(`/tmp/toolkit-wasm-send-unwind-negative.log`). This ordering matters because
[Rust's thread lookup](https://doc.rust-lang.org/src/std/thread/current.rs.html)
can panic after thread-local destruction. The real toolkit Worker transport,
web Worker and workflow Worker each pass wasm strict, the latter two with
runtime-tests (`/tmp/tr03-gh-toolkit-worker-wasm-strict.log`,
`/tmp/tr03-ghinvite-web-worker-wasm-strict.log`,
`/tmp/tr03-ghinvite-workflows-worker-wasm-strict.log`). These compile the combined
profile/ETag production source before the final destructor-order correction.
The complete native workspace subsequently passed1,400 tests with9 named browser
fixture servers ignored and zero filtered cases (`/tmp/p1-workspace-tests2.log`),
before the later operator/admin/webhook changes and destructor correction.
Fresh signed Worker consumers and final combined checks remain separate gates;
standalone wasm execution and per-crate compilation do not prove those gates.


### Operator user credentials and explicit repository administration

`UserAccessToken::from_operator(Zeroizing<String>, Option<ClientId>,
Option<UserId>)` is available without OAuth or native HTTP features. It owns and
zeroizes the credential, has redacted Debug, and provides no Clone, Serialize,
Display or raw-token accessor. Incomplete caller-supplied identity remains
Unattributed; construction performs no discovery, validation or refresh. Typed
user reads accept this credential through the existing sealed Credential seam.

With OAuth enabled, sealed `UserTokenRef` accepts both operator and ephemeral
credentials for `revoke_token`, `delete_grant` and `check_token`. App-level calls
require Basic authentication bound to the token's declared ClientId before any
send. JSON token bodies use SecretBody; response bodies are zeroized and decoded
into only positive numeric user identity and optional nonnegative Unix expiry.
Only GitHub's documented 404 token-check result becomes Invalid. Other failures
retain typed API/transport errors; mismatched client/user identity, malformed expiry
and negative timestamps cannot become Valid. Missing expiry is not a promise of
indefinite validity, and a positive past expiry is not locally reclassified as
revocation. These helpers do not implement Acting exchange, retained rotating
credentials, L1 refresh or the live sign-in cutover.

`repo-admin` is an explicit, default-off feature. It exposes eight metadata-backed
settings, repository destruction and repository/organization ruleset mutation
helpers; these accept only User or Installation credentials. The shared endpoint
macro preserves cfg attributes in both the enum and its full inventory. Without
the feature, those variants are absent and generic REST calls refuse the TR-18
repository PATCH/DELETE, ruleset mutations and branch-protection PUT/DELETE before
sending. With the feature, that gate is lifted deliberately; credential checks,
path escaping, quota observation and error mapping remain in force. Exact-resource
PUT/DELETE have documented retry-safety reasons; settings PATCH and ruleset POST
remain unsafe to replay automatically. The helpers do not initiate retries.

Repository mutations use `administration:write`. Organization ruleset mutations
use the distinct `organization_administration:write` permission; existing
permission discriminants are preserved. These follow the GitHub
[repository ruleset](https://docs.github.com/en/rest/repos/rules#create-a-repository-ruleset)
and [organization ruleset](https://docs.github.com/en/rest/orgs/rules#create-an-organization-repository-ruleset)
contracts, with the canonical organization permission key recorded in GitHub's
[App permission inventory](https://github.com/actions/create-github-app-token/blob/main/scripts/generated/app-permissions.json).
P1 enables neither the feature nor those App permissions. Its boundary check now
inspects actual native and wasm consumer feature trees for accidental activation.

Local verification after applying these APIs and the accompanying generic SSO,
error-code and webhook slices:

- Operator six cases and enabled-admin four cases pass. They exercise all eight
  helpers for both allowed credential kinds, exact Basic/body/path binding,
  no-send refusal, all 23 generic deny-list cases and inherited typed user reads.
- Full default toolkit: 348 tests across all 11 executable targets, with 13 nested
  compile-fail contracts; minimal: 283 tests across all six available targets,
  with 12 contracts; explicit repo-admin: 349 tests across all 11 targets, with
  12 contracts (only the feature-absence fixture is inapplicable). All have zero
  ignored or filtered tests. Logs: `/tmp/l2-toolkit-{default,minimal,admin}-final.log`.
- Compiler-generated snapshots prove operator Serialize/Clone/Display/private-field
  refusal and absence of all eight admin variants. Default and minimal sealed-trait
  snapshots include the additional opaque credential without opening the trait.
  The first diagnostic runs are retained as `/tmp/l2-privacy-{default,minimal}-initial.log`.
- All six strict combinations pass: native all-targets and wasm library for
  default, minimal and explicit repo-admin features
  (`/tmp/l2-operator-admin-strict.json`, `/tmp/l2-{default,minimal,admin}-{native,wasm}-strict.log`).
  These checks include the subsequent TR-03 drop-guard correction. Actual P1
  native/wasm resolved feature trees, source boundaries and existing injected
  dependency negative controls pass (`/tmp/l2-operator-admin-boundary{,-tests}.log`).
- The initial operator fixture expected an unescaped client ID. The existing path
  contract encodes its dot as `%2E`; only the fixture was corrected, preserving
  exact request assertions (`/tmp/l2-operator-focused{,2}.log`).

This evidence is local to the in-tree toolkit. The preceding full-workspace
1,400-test baseline predates these additions; it is not a combined P1 claim for
this new source. Extraction, packaging, remote permissions and release remain
separate gates.

### Stable error-code registration and generic SSO evidence

The crate now records71 current error codes in `error-codes.txt`. Every existing
public error family supplies code(), including configuration, avatar, email,
ledger/refund, cache backend and the explicitly retained compatibility error.
Compiler-exhaustive matches keep variant classification explicit; delegated
Call/Broker/OAuth errors preserve the underlying code. New code methods return
fixed literals without incorporating values, status numbers or diagnostics.

`scripts/check-toolkit-error-codes.py` scans code methods with a Rust-aware
lexical reader, enforces the ER01 grammar and registers their literals against
the sorted unique snapshot. Historical codes remain in the snapshot even after
a source removal. CI fetches history and compares against the actual PR base or
push predecessor, refusing any removed/renamed historical line. Local mutation
controls prove unregistered source codes, invalid grammar, duplicates and
historical removals/renames are rejected; source comments, raw strings and
escaped quotes do not create registrations. This is the error-code gate, not a
substitute for the still-required generated public API snapshot. Verification:
`/tmp/toolkit-error-codes-verification.log` (initial51), plus current71 checker and
negative controls. Full default/minimal/admin toolkit suites pass after the new
methods (`/tmp/l2-toolkit-{default,minimal,admin}-final.log`).

Generic Rest::call now uses the same SSO parser as list walks. A partial response
fails before data decode, preserving up to100 unique positive numeric
organization IDs, including case-insensitive header directives, while dropping
all untrusted header prose. Its regression deliberately supplies incompatible
response data to prove that partial evidence retains priority. This does not
turn a generic single call into an exhaustive pagination walk.

### Additional authenticated event decoders

`webhook::events` now includes typed pull request, review, check suite/run, status,
push, repository, organization/team membership and app-authorization decoders.
Every decoder validates its enumerated action and required payload fields,
uses exact positive numeric identities, and refuses mutually exclusive event
objects. Required repository/organization scope comes from the authenticated
body. Unknown non-discriminating JSON fields are ignored. The decoders own no
application merge, reconciliation or authorization policy.

Status context labels and push refs are available to callers but omitted from
Debug. Titles, comment/review bodies, descriptions, URLs, commit authors and email
addresses are not retained in these event values. An email-only organization
invitation has no invented numeric member; a sender is not substituted for the
affected team member. Object IDs above JavaScript's exact-number range survive
without floating-point conversion. Checks and PRs retain bounded validated
object hashes, not arbitrary upstream strings.

The in-source discriminator table also rejects known foreign event objects. A
real signed issue_comment body with action edited is refused after changing only
its unsigned event header to repository; action vocabulary overlap cannot bypass
the body checks. PR label and organization invitation objects have explicit
exceptions for their legitimate parent events. These contracts follow
[GitHub's event payload documentation](https://docs.github.com/en/webhooks/webhook-events-and-payloads)
and WH04, without claiming live GitHub acceptance or token revocation evidence.

All12 webhook integration cases pass in both complete default and explicit-admin
suite runs (`/tmp/l2-toolkit-{default,admin}-final.log`). Five new matrix cases
cover every supported action, signature-preserving cross-kind routing, missing
and mixed discriminators, unknown actions, exact large identities, protected
Debug output, scope and affected-user distinctions. P1's existing authenticated
installation decoder tests remain in the same suite; new event APIs do not
activate new P1 subscriptions or handlers.

### GraphQL transport, cost and complete connection walks

The default-off `graphql` feature implements L2 §10.2 without application query
schemas. `Rest::graphql` sends one request; `GraphQlRequest.kind` supplies query
versus mutation retry/content metadata. An unknown mutation outcome never causes
a hidden retry. The public `assert_kind_matches` helper checks an application's
explicit kind in its tests. No application merge or reconciliation policy is
included.

Responses retain typed data, bounded recognized error codes, optional numeric
cost and explicit SSO omission metadata. Unknown upstream types collapse to a
fixed label; error messages, paths and locations are discarded. Default calls
reject any errors or SSO omissions. `allow_partial().graphql` and its cursor walk
expose incomplete results explicitly; rate limits and later-page failures still
return errors. GraphQL RATE_LIMITED on HTTP 200/403 feeds the existing rate-limit
error/event contract without treating the envelope as success. Exact live error
shapes and installation-token search scope remain SP-05 evidence, not inferred
from local fixtures.

Cost opt-in inserts a reserved root alias into one query and removes it before
typed application decoding. It handles comments, ordinary/block strings,
variable-default objects and fragments, and rejects alias collisions or multiple
operations. This option is query-only because GitHub exposes `rateLimit` on
[Query](https://docs.github.com/en/graphql/reference/meta#ratelimit); mutations
with cost opt-in fail locally rather than causing an unrequested second read.
Response quota headers remain observed for mutations. Cost is retained even if
application-data decoding fails, scoped to the exact credential bucket and
GraphQL resource. Within one reset window, later observations cannot reopen an
exhausted quota; explicit preflight bypass still records observations.

`graphql_walk` owns `first` and `after` variables, accepts first 1–100 and follows
at most 1,000 pages. It rejects absent, empty, oversized or cyclic continuation
cursors. Search walks require the selected `issueCount` and reject more than
1,000 results. Numeric installation repository filtering requires the caller's
complete authoritative repository set; missing/zero/foreign IDs are removed.
These bounds follow [GitHub pagination](https://docs.github.com/en/graphql/guides/using-pagination-in-the-graphql-api)
and [search](https://docs.github.com/en/graphql/reference/search#search).

The focused sixteen-case native suite passes (`/tmp/graphql-focused-tests.log`),
covering these behaviors, metadata, resource isolation and privacy. Full default, minimal and explicit compatibility suites also pass: respectively
333, 293 and 381 reported tests (including doctests), with nine, eight and ten
nested privacy compile-fail cases. Explicit compatibility enables GraphQL,
repo-admin, legacy-p1-oauth and test-stub together. Logs are
`/tmp/toolkit-post-feature-{default3,minimal,compat2}.log`; all-target strict passes
in `/tmp/toolkit-post-feature-alltarget-strict.log`. Original failed runs remain
recorded: two fixture PathTemplate placeholders were corrected, the numeric
alias behavior below was restored, and compiler-verified privacy snapshots were
updated only for canonical module qualification/line positions. A separate
legacy-enabled ephemeral-token snapshot preserves identical privacy/Acting
denials when rustc qualifies the two OAuthConfig types differently. These
results do not imply extraction, live proof or release.

### Preserved repository invitation pagination during facade migration

The preserved legacy tests exposed an actual typed-walker regression: GitHub can
return a `/repositories/{id}/invitations` Link after a named repository request.
The typed invitation endpoint now binds this alias to authenticated first-page
`repository.id` fields. Every first-page row must provide the same positive ID;
provided full names must match the requested repository. Missing, empty,
conflicting or misbound evidence refuses the alias before dispatch. The numeric
alias remains pinned on later pages. Other endpoint walkers retain exact-path
checks, and no additional GitHub identity request or budget charge is added.
The minimal fixture without repository facts remains a refusal control; the
positive fixture uses GitHub's documented [invitation response fields](https://docs.github.com/en/rest/collaborators/invitations).
Default full toolkit tests pass both alias forms, mismatches and explicit SSO
partial-list metadata (`/tmp/toolkit-post-feature-default3.log`).


The integrated production feature matrix now covers all 64 combinations of
`reqwest`, `oauth`, `webhooks`, `graphql`, `repo-admin` and `legacy-p1-oauth`, on
both native and wasm32: all128 strict library compilations and all128 production
dependency checks pass (`/tmp/toolkit-six-feature-matrix.log`). Each actual tree
requires reqwest/octoevents exactly when enabled and excludes application,
workflow, UI and database dependencies; wasm trees exclude Tokio. Explicit
legacy OAuth implies typed OAuth, but never enables reqwest or webhooks. The
source hash manifest is `/tmp/toolkit-post-feature-source.json`. This supersedes
the earlier three-feature matrix for the current source; it does not replace the
pending extracted-crate public API, coverage, live spike or downstream P1 gates.

### Current native reconciliation and Worker fixture corrections (2026-10-17)

The fresh full integration targets exposed two fixture defects. The requester
refresh test waited only for `Reconcile.target`, although it durably queues
`observe_next` and then repository reconciliation. The corrected barrier waits
for the entire dispatched chain before asserting that a throttled retry causes
zero additional HTTP reads. The sweep fixture omitted the real
`InvitationRequest.on_delivery_settled` handler, so notification dispatch failed
before untracking could finish. Binding that production handler fixes both sweep
timeouts without changing their limits, SQL-outage checks or production code.
Full repository66 and sweep3 targets pass. Original failed runs and new exact
source/binary/log hashes are retained in
`/tmp/p1-native-integration-binaries/corrected-reconciliation.json`.

The fresh captured Worker also exposed old fixture assumptions. The separate
browser database now applies all ordered migrations before signing in. Foreign
history and queue rows use distinct app-scoped installation/account parents.
The queue test observes the exact six-statement inventory: two app-login reads,
two app-account authority reads, one scoped navigation count and one queue page.
It retains the page's row-work bound and separately accounts for the full scoped
badge count; additional or per-row queries fail. The test observer imports the
unchanged captured Worker. Remaining positional user inserts name their columns
explicitly so additive advisory profile columns preserve the fixture contract.

Ordinary signed Worker, decision queue, request history, organization cancellation
and access removal modes all pass after these test-only corrections. Original
red logs remain alongside the passing reruns. Exact test-source/log hashes and
verified unchanged capture artifact hashes are in
`/tmp/p1-worker-current-20261017/fixture-corrections.json`; the original capture
manifest is `/tmp/p1-worker-current-20261017/manifest.json`. These are complete
individual target/mode results, not a new whole-workspace command or deployed
verification claim.

The remaining delivery-control mode's old V1 assertion is now migrated to the
current V2 contract, including absent compatibility ULIDs, exact physical
request/repository/generation keys, retained repository-step fences and canonical
transition-audit target identity. Full signed `DELIVERY_CONTROL_ONLY` passes on
the same immutable capture. The combined32 selected modes now individually pass:
26 original successes plus six complete corrected reruns. Original failures and
per-result provenance remain in
`/tmp/p1-worker-current-20261017/consumers/current-results.json`. Remaining
full-CI cancellation/link-secret/production/browser modes were outside this
capture's scope; the following sections record their later verification. These
results do not claim complete P1 acceptance.

### Remaining local packaging and browser gates (2026-10-17)

The current island build uses the official SHA-verified Dioxus CLI 0.7.9 and
measures 475 KB gzipped against the 600 KB limit. Island 16 unit tests and Wasm
Clippy pass. All 14 browser configurations pass 457 cases: 372 link-form cases,
10 repository-selection cases and 75 cases across 12 native-router configurations.
The native fixtures were rebuilt with the final stylesheet; temporary Playwright
configs change only the Cargo launch into its exact compiled executable, keeping
the CI projects, assertions and timeouts. Exact binaries, logs and source hashes
are in `/tmp/p1-final-browser-native/{manifest,results}.json` and
`/tmp/p1-current-browser/island-results.json`.

The full run exposed obsolete admission-test expectations for `/i/{id}`, the
permission label and direct login redirect. Tests now exercise the selected
capability's `/join` surface and anonymous sign-in offer, retaining two-tab,
lost-acknowledgement, revocation and account-switch checks. The sidebar test now
checks the added Apps tab stop. Reaching its final assertion exposed a real
keyboard-scroll defect: the last control extended 0.3125 px below a 240 px viewport.
Half-rem sidebar scroll padding keeps it fully visible, including the focus
outline. The strict viewport-ratio 1 assertion is unchanged and passes in both
themes. A rebuilt-router geometry probe and inspected screenshot are retained
in `/tmp/p1-final-browser-native/sidebar-{probe.log,keyboard.png}`. Original
failed logs and the exact earlier native executables remain preserved.

Both production Workers build with the exact Wrangler `worker-build 0.8.1`
release commands and locked wasm-bindgen 0.2.120, without runtime-test features.
Actual workerd checks pass scheduled failure/recovery and frozen digest replay,
both generated runtime entrypoints, fixture-route exclusion, and invalid
identity/host/HTTPS/entropy/retry-binding refusals. The final artifacts include
the SDK W03 private channel implementation and final CSS. The 1,111-file source
manifest is unchanged across capture; exact artifact and runtime-log hashes are
in `/tmp/p1-final-production-manifest.json`. Subsequent three SDK import-order
formatting edits and the later L1 Wasm dependency correction are outside this
capture boundary. No release, deployment or live-provider proof is claimed.

The final supplemental refresh also passes all five modes: guarded-run startup,
seven cancellation scenarios, budget cap, entropy startup and the full dedicated
entropy journey. It uses the final default workflow/web/stub capture after SDK
formatting and the L1 Wasm dependency correction, plus a freshly built isolated
cancellation variant. Default and feature artifact hashes remain unchanged after
runtime. The secret web variant is reused explicitly: it has no SDK dependency,
and its later product difference is the separately verified sidebar CSS padding.
Exact source, artifact, runner and log hashes are retained in
`/tmp/p1-worker-final-supplemental-20261017/{manifest,results}.json`; the parent
capture is `/tmp/p1-worker-final-20261017/manifest.json`. The
[entropy verification record](link-secret-entropy-verification.md) records the
final scan scope. Original failures and earlier successful captures remain
preserved; this does not relabel the earlier 32-mode capture or claim complete
P1/live/deployment acceptance.

### Current toolkit matrix and fixture API (2026-10-17)

The literal registered `scripts/verify-toolkit-features.sh all` completes all
256 strict library checks: seven production feature flags, 128 combinations on
native and 128 on wasm32. Each combination also checks its actual production
Cargo dependency tree, including optional transport/webhook/rotating-credential
edges, no executor/consumer dependencies, no production probe/conformance, and
Wasm random-backend binding. All 199 captured source files remain unchanged.
This is Clippy/metadata and dependency verification, not 256 codegen builds or
runtime executions. Exact flags, commands, source and log hashes are retained in
`/tmp/l2-final-feature-matrix/verification.json` (original terminal handle57528).

The post-cfg native fixture API is generated by Rust1.98.0 and
cargo-public-api0.52.0 and matches the existing `api/fixtures.txt` byte-for-byte.
No baseline update was made. `/tmp/l2-final-fixture-api/verification.json` records
the actual compiler JSON/rendered API and exact sources. This supplements the
four real scratch native/Wasm minimal/all-feature builds in
`/tmp/l2-portable4-build-results.json`; it does not claim extraction, history,
publication or a P1 dependency switch.

### Final default Worker capture and reconciliation (2026-10-17)

The final workflow/web/stub capture includes the formatted W03 SDK, the explicit
core Wasm std dependency/lock correction and the sidebar CSS fix. All 32 complete
individual consumer modes now pass on that single unchanged capture: 31 pass in
the initial batch, and the full journey passes after its fixture correction.
The initial batch returned nonzero and remains preserved; this is not a claim
that its original aggregate command passed. The earlier pre-W03 32-mode evidence
is a separate immutable proof.

The journey intentionally holds SQL projection failures across lost responses
and three guarded-run failures. Default automatic Restate backoff could then
outlast its 25-second convergence poll after SQL restoration. The journey-only
`journey-compose.yaml` caps automatic retry intervals at one second; the actual
pinned server's `--dump-config` and merged Compose configuration confirm the
setting. The 25-second deadline, real automatic retries and all assertions remain
unchanged. There is no manual invocation resume, SQL bypass or production change.

`/tmp/p1-worker-final-20261017/consumers/current-results.json` binds the complete
per-mode exit/log hashes and corrected rerun to capture manifest SHA-256
`007c3def10ddc9bc9a732f345286bc8347e3f2852fee8e78ea1a2a66533f7d63`.
`/tmp/p1-worker-final-20261017/journey-fixture-recovery/fixture-provenance.json` records
the fixture sources and actual server configuration. Workflow/web/stub hashes
remain unchanged after all consumers. These local modes complement the separate
five supplemental feature gates, browser and production-package records; they
do not establish milestone deployments, live GitHub behavior or a release.


### Portable bundle CI closure (2026-10-17)

The extraction preparer now supplies a runnable standalone CI workflow and a
seven-phase verification runner alongside the eight actual copied packages.
The runner checks inventoried source bytes before and after execution, resolves
and records the copied lock seed explicitly, and rejects any local Cargo package
outside the bundle. Generated workspace manifests are included in the integrity
inventory. Negative controls prove that changed Rust source is refused before
execution and that a real external path package is refused after successful Cargo
metadata resolution, before any build or test. The ordinary in-tree boundary
mode still checks P1 consumer restrictions; only the explicit bundle mode omits
those absent-consumer checks.

All seven phases have passing local evidence:

- Complete native minimal/default/all-feature Cargo tests: 307/353/416 passed,
  zero ignored, including doctests and the actual privacy compile cases.
- Two actual Wasm builds: minimal toolkit and all-feature toolkit plus Worker.
- The literal standalone 256-combination native/Wasm strict library matrix,
  with all 256 production dependency checks.
- Five compiler-owned API snapshot comparisons, three strict documentation
  builds, package cargo-deny, and ten contract/negative-control commands.

`/tmp/l2-portable8-verification.json` binds the command and log hashes, final
source checks and exact phase provenance. Bundle8 completed native tests, Wasm
builds and the matrix before its orchestration parent was deliberately stopped
to avoid repeating API/documentation work. Its original exit143 and partial
report are preserved; no whole-command success is claimed. API/documentation
results are imported explicitly from bundle7: all 194 Rust/manifests/resolved
lock inputs are byte-identical, and the only inventoried differences are the
verification/provenance helpers. The dependency audit and new integrity controls
ran on bundle8. Initial bundle5/6 negative-control failures remain recorded;
parsed TOML mutation fixed the test portability issue without changing product
Rust or weakening the forbidden-dependency assertion.

The workflow is runnable scaffolding, not a remotely executed CI claim. Real
internal path dependencies remain. EX-03 history, the no-path/release dependency
boundary, publication, the actual P1 dependency switch and live acceptance remain
separate open requirements. No commits, tags or deployments were made.


### Isolated P1 dependency-switch and rollback rehearsal (2026-10-17)

`crates/gh-toolkit/scripts/rehearse-consumer-switch.py` now exercises the real
consumer layout without touching the original checkout. It copies the complete
27-package local P1 dependency closure, verifies toolkit Rust bytes against the
accepted portable bundle, resolves each package's own inherited manifest and
retains a single canonical identity for each shared local dependency. It does
not blindly combine P1's original anykms identities with a second copied set.

The actual rehearsal passes baseline, switched and rolled-back Cargo metadata
and native/Wasm production feature trees for web, workflows and both Workers.
Only toolkit and its Worker adapter move to a sibling workspace; exact dependency
paths are rebased. Production graphs match after normalizing source locations.
Toolkit's own dev dependencies correctly disappear from the P1 workspace graph
when toolkit becomes external. The first rehearsal's over-broad graph comparison
failed on that distinction and is retained; the final check compares production
graphs for the switch and still requires the full original graph on rollback.

The final run restores all 996 baseline files, including manifests and the lock,
byte-for-byte, and confirms that all 993 inventoried original source files stayed
unchanged. Negative controls reject an in-tree toolkit directory, actual Cargo
graphs with `repo-admin` or `rotating-credential` injected into the real web
manifest, and a corrupted rollback lock. All 13 metadata/tree commands complete;
there is no codegen, target filtering or repeated positive test suite.

Evidence and helper hashes are in
`/tmp/l2-p1-switch-rehearsal-3/verification.json`, with exact commands, logs,
source inventory and restored-file inventory under its `evidence/` directory.
The original failure and intermediate proof are preserved. This is executable
local path/layout acceptance only: the real EX-06 tagged/versioned dependency
switch, EX-03 history preservation, EX-07 post-switch full CI/live delivery and
EX-09 revert of the actual switch PR remain open. No history, release or deployed
artifact was changed.

The expanded rehearsal has now also passed with `--compile-consumers` and
`--runtime-contract`. Both native server binaries and both Wasm Workers built
while toolkit was external. The exact
`unauthorized_write_releases_its_generation_and_remints_once` integration case
then passed against real local Restate 1.8.0, SQLite and the GitHub HTTP fixture;
the runner refuses a zero-test result. Both Restate reuse URLs were unset, so
this run used its own local server. The production graph comparison includes
the native entry-point packages too.

All 17 Cargo/compiler commands passed. The same four negative controls were
refused, all 1,070 baseline files were restored byte-for-byte, all 1,067 original
source inputs remained unchanged, and both production graphs and the full
rollback graph matched. The first compilation attempt exposed a copier omission
of SQLx's actual root migrations; its failed layout and all 1,092 file hashes
remain at `/tmp/l2-p1-switch-rehearsal-codegen-1-missing-migrations/`. The helper
now copies those real migrations and required out-of-package documentation and
public test key, preserving production Rust include paths unchanged.

The successful run and an independent verification are captured at
`/tmp/l2-p1-switch-codegen-capture/verification.json` (SHA-256
`6fd629a37876059a3d89cfe9eb5dc939dc65561c080c0cf50f9d4a89917eefc8`).
The capture retains all original inputs, all command logs, the four compiled
consumer artifacts and the integration-test executable with hashes. It uses
bundle10's accepted toolkit Rust bytes; the copier-only helper correction is
captured separately. The earlier metadata-only proof keeps its original scope.
This adds actual local codegen and one runtime contract; the real released
dependency switch, complete post-switch CI/live acceptance, history preservation
and actual switch-PR revert remain open.


The portable preparation now also exports a root README, CHANGELOG, both license
files and dependency-denial policy as exact provenance-tracked copies of their
contracts. `check-portable-root.py` checks all five artifacts, byte identity and
local documentation links; `test-portable-root.py` rejects an actually missing
license, divergent changelog and broken local link. Both are registered in the
portable contracts phase. Bundle11 retains 319 file hashes across eight packages;
all source-integrity and artifact/control checks pass at
`/tmp/l2-portable-root-contract/verification.json`. Earlier bundle10/MSRV and
relocation proofs are unchanged. No compiler result is inferred from these file
checks. Canonical extraction still follows ADR0005 and EX-01 readiness gates.


### TS-12 Node Wasm execution (2026-10-17)

The previously unverified Node runner now executes four public-toolkit contract
tests successfully. This is actual `wasm-bindgen-test` execution in Node22.23.2,
using the ABI-matched official runner0.2.120 and Rust1.98.0, not a Wasm compile
reported as a runtime pass. The isolated `tests/wasm-node` package has its own
locked test dependency graph; no P1 production manifest or lock changed.

The tests exercise full App JWT creation and claims with independent RSA
signature verification and tamper refusal; opaque credential, request and
provider-echo redaction; an actual two-page numeric repository listing plus
cross-origin next-page refusal before dispatch; and ledger multi-bucket
atomicity, retained receipt replay, serialization and exact window expiry.
The public toolkit source is unchanged. All four execute in Wasm (0.27 seconds),
with no ignored or filtered tests. Cargo's separate zero-doctest phase is not
counted as proof.

`/tmp/l2-node-wasm-1/results.json` records the compiler/runner commands, isolated
lock, runner binary and source/log hashes (result SHA-256
`15a5a9ed151fc321e608095ab2540cee1a91ee75a5ae184f7a62a7bbb5a19af7`).
`crates/gh-toolkit/scripts/verify-node-wasm.py` requires the exact runner ABI and
all four runtime successes. The in-tree L2 workflow and portable CI template
now have a dedicated Node job using that command; both parse successfully
(`/tmp/l2-node-wasm-ci.json`). Those jobs have not been run remotely. A fresh portable bundle also resolves
the isolated test package with `cargo metadata --offline --locked`; both local
packages remain inside that bundle (`/tmp/l2-node-portable-containment.json`).
This is containment evidence, not another runtime execution. The local SD-L2-d
Node choice is supported; its literal remote-CI condition remains unverified. No workerd fallback or live-provider success is inferred.


### CF-15 retry-delay overflow correction — actual red/green (2026-10-17)

Source review found that the optional L1 refresher adapter capped both provider
and quota-preflight delays at `u32::MAX`. L1 CF-15 requires overflow rejection;
a shorter manufactured delay is not the provider's instruction. The
L2-only correction returns bounded `Retryable(Malformed)` with no delay when
numeric/date advice cannot fit L1's seconds representation, including a decimal
header larger than u64. Unknown textual advice remains absent guidance under
RL-01. Known any-status provider verdicts still take precedence, and irrelevant
retry headers cannot discard an issued rotating pair or suppress the required
nonrotating-token revoke.

Five targeted tests cover response/header boundaries, actual exhausted quota
preflight without a second dispatch, verdict/issued-pair precedence,
nonrotating cleanup despite irrelevant advice, and timeout constructor bounds.
After the crash-runtime hold released, the preserved original adapter failed
both response and preflight overflow assertions with capped `4294967295`
seconds instead of `Malformed` with no delay. The first corrected run passed21
and failed the new preflight fixture: its missing `x-ratelimit-used` meant the
observer correctly ignored an incomplete quota snapshot. Adding exact used5000
made the original adapter fail at the intended preflight overflow assertion;
no production behavior or assertion was weakened. All original logs remain.
The corrected entire `rotating::` module then passed22/22, including all nine
new tests across both fixes, the actual reusable refresher contract, and real
native socket timeout/revocation tests. Native library plus signin-contract
strict checks and Wasm library strict checks both pass. No tests were ignored;
18 unrelated signin tests were filtered by the explicit module selection.

`/tmp/l2-adapter-red-green/verification.json` records all seven command outcomes,
source/test/binary/log hashes and the22 executed names (SHA256
`2d2903c40f31c28292ce0a32cced71a7992f87d1d23208d7b2dc2ce3c4cf493f`).
The prepared manifests and original source copies remain historical evidence.
Earlier complete matrix/test proofs retain their pre-correction source scope;
this focused proof does not relabel them or claim MSRV/deployed verification.

The timing audit found existing source/tests for RF-03 request-start expiry,
checked u32 lifetime parsing and i64 expiry addition, zero lifetime preservation,
real native header/body timeout and one-budget exchange plus stalled revocation.
`rotating_conformance.rs` advances the injected clock during a successful
exchange and tests expiry overflow; `rotating_deadlines.rs` uses actual sockets
and checks exact dispatch counts. Default 15 seconds splits into 14 seconds for
refresh and one for cleanup; the configurable 15ms–30s bounds keep cleanup
nonzero. No further production timing defect was established. A combined
refresh/revoke-specific Worker host-deadline proof is not established by these
native tests or by generic Worker transport deadline evidence.


### OA-08 nonrotating cleanup ordering — actual red/green (2026-10-17)

L1 RF-02/§8.5 and L2 OA-08 distinguish an accepted access token without a
refresh token from an unknown outcome. The previous adapter decoded optional
`u32` lifetimes before making that distinction: a malformed or overflowing
lifetime prevented both the required best-effort revocation attempt and the
`ProviderChangedGrantType` verdict. The correction first decodes a
bounded, zeroizing credential-identity envelope. A valid access token without
a valid refresh token takes the existing bounded revoke path before optional
lifetime decoding. A rotating pair still undergoes full lifetime validation;
provider errors and non-success statuses retain their previous precedence.
Malformed JSON, ambiguous identity, absent access and oversized bodies remain
retryable without revocation.

Four additional focused tests cover malformed/overflowed lifetimes with both
successful and failed cleanup, malformed rotating pairs, missing access,
provider/status precedence, and malformed/duplicate/oversized identity. The
preserved pre-cleanup source reproduced `Retryable(Malformed)` instead of
`LostGrant(ProviderChangedGrantType)` for valid access/no refresh plus malformed
`expires_in`. The exact corrected source passed the full22-test adapter module
and both strict checks described above. `/tmp/l2-nonrotating-cleanup-regression/`
preserves before/after copies and the original prepared patches; the final
verification manifest records the single subsequent test-fixture correction.
No L1/core/conformance source or public API changed. Portable MSRV checks and
combined refresh/revoke-specific Worker host-deadline evidence remain separate.


### Portable MSRV gate — six actual checks pass (2026-10-17)

The portable CI template carries a separate six-combination production-library
gate: minimal/default/extended on native and Wasm, exact Rust1.91.0.
All six actual `cargo check --locked --lib` invocations pass against the unchanged
Rust/manifests in `/tmp/l2-portable-bundle-20261017-10`, including the corrected
optional refresher. Every invocation records rustc1.91.0 (`f8297e351`) and
cargo1.91.0, actual dependency containment, source/provenance hashes and an
unchanged resolved lock during compilation. No newer-compiler fallback,
`--ignore-rust-version`, downgrade or skipped failure was used. The copied parent
lock was explicitly resolved once before the first check; all six used the same
resolved hash `81a04c3182ccac388a0f0f50ef32994f6eda20b1705849ef796fcb8f6d8b429c`.
All296 captured non-lock files stayed unchanged; the original repository lock
was not modified.

The exact official compiler and Wasm standard library were installed into
isolated `/tmp` homes, without changing the host default toolchain. The first
installation completed its four components but failed at a post-install rustup
self-update lookup using the original Cargo home. The corrected isolated-home
invocation passed; the initial log is retained, and the isolated default was
reset to none. This was a tooling setup failure, not a compiler incompatibility.

`/tmp/l2-portable-msrv-proposal/verification.json` records all six results,
actual compiler/command/log hashes and installation provenance (SHA256
`d499fa68697a7e1e0bc881d8ba83c52d17237a79d469677006732b4b7733bd1a`).
Bundle10 contains eight actual packages and297 captured entries, including its
lock seed; original provenance SHA256 is
`43bf16bb21ad55c8b0968f52f90f381982dc802db1d290179a43494baec605a5`.
The six literal commands remain in `commands.sh` and `bundle10.json` beside the
verification report. This proves production-library MSRV compilation, not
Rust1.91 dev-test/trybuild compatibility, full artifact codegen, remote CI or a
switched-P1 runtime. Bundle8's earlier suites and Rust1.91.1 native evidence retain
their original scopes; no whole-matrix or broad test suite was repeated here.


### Complete persistent portable verification after execution recovery (2026-10-17)

The latest source-hashed eight-package bundle has now passed all seven phases in
one actual `verify-extraction.py --phase all` invocation. All 24 commands exit
zero: metadata, twelve contract/control commands, complete minimal307/default353/
all-feature425 native tests, both Wasm library builds, all256strictfeature and
production-dependency combinations, five compiler API comparisons, three strict
documentation builds and the package dependency audit. Native suites have no
ignored or filtered tests. This includes the current optional-adapter regression
fixes; the earlier416-test and imported-phase evidence keeps its original scope.

The 318 non-lock file hashes and explicitly resolved lock are unchanged. Eight
source packages travel; the actual graph resolves six local packages because
excluded dependencies' own dev dependencies do not join the consumer graph.
The separate exactRust1.91.0 six-check native/Wasm production-library gate also
passes, preserving its original repository lock and the same resolved lock.
No dev-test MSRV compatibility is inferred.

Logs, source/provenance, compiler JSON and rendered snapshots now persist under
`.verification/20261017-recovery/root/`. Independent command/log/hash/count and
API-baseline checks pass in `portable-whole-verification.json` (SHA-256
`61b71723db1673c6c59dd4807eeab0136328d8de14fc1f7c38f16e795d4d7b0a`)
and `msrv-verification.json`. The exact whole-run result is `whole/results.json`;
its SHA-256 is `c49107dab6b2086177d4168427f3269e03dd697a236d0b086d3dc1a3b6611037`.
These generated artifacts are ignored by Git. Earlier `/tmp` artifacts were lost
in the execution reset and are not claimed to remain inspectable. The portable
internal paths, canonical extraction/history, no-path released dependencies,
real P1 switch, hosted CI and live acceptance remain separate open gates.

## Persistent current Worker recovery (2026-10-17)

The recovered current ordinary CI inventory contains **36 complete runtime
cases**. All 36 pass in one run against fresh, unchanged workflow/web/stub
artifacts. This includes shared D1 and raw migration guards, installed-app and
sign-in/startup/privacy surfaces, all signed organization/access/reissue/control
modes, the assembled journey, approved delivery, and the full ordinary browser,
30-second upstream deadlines and projection-outage recovery. The historical 32
inventory remains a separate, currently unavailable `/tmp` scope.

Evidence is retained under
`.verification/20261017-recovery/p1-audit/worker-runtime/` at the repository root:
`capture-1/manifest.json` binds 2,516 unchanged inputs and 9 copied artifacts;
`runtime-inputs.json` and `runtime-source/` bind the final fixture closure;
`runtime-helper-delta.json` records only the two JavaScript files changed after
capture to reject premature exit 0. The Rust/artifact capture remains unchanged.
`modes-1/results.json` contains every terminal command, environment selector,
log hash and completion hash. `ordinary-verification.json` independently checks
all retained source/artifact/raw-log bytes, 184 owned children, 382 complete
streams and 8,946,097 bytes. Node 22.23.2, Rust 1.98.0, wasm-bindgen 0.2.120, actual
Miniflare/workerd binaries and the pinned Compose image are recorded separately.

The fixture launcher now guards Miniflare's clean pre-EOF pipe destruction at
child creation, including replacement runtimes. It awaits actual child close
and raw EOF, then scans each concatenated byte stream separately with the
existing exact privacy callbacks. Admission stops its owned Restate service,
retrieves full shutdown logs, then removes it; its GitHub stub also drains.
Ten controls cover late output, interleaved/split markers, overflow, I/O errors,
premature closure, bounded missing EOF, owned-process selection, and caught or
uncaught early process exit. A real Worker control passes before the full run.
Thirteen cases register exact final marker scans. Cases with zero registered
scanners still retain full raw logs but do not gain an additional privacy claim.
Existing raw D1/KV/journal assertions and all behavior/time limits are preserved.

All five isolated supplemental cases now pass: cancellation, budget cap, Link
Secret startup, guarded-run startup, and the complete deterministic Link Secret
journey. Fresh isolated feature artifacts and default artifacts remain unchanged.
`supplemental-verification.json` independently binds 17 owned children, 37 full
raw streams/1,453,110 bytes, five completion records and all 17 default/isolated
artifacts; only the journey adds an exact final privacy-marker scan.

Exact production packaging and generated-shim runtime now also pass.
`production-1` runs both literal Wrangler release commands using worker-build
0.8.1, official wasm-bindgen 0.2.120 and the standard wasm-opt/esbuild pipeline,
without fixture features or packaging overrides. `production-closure-1` captures
the complete build directories, including the `../index.js` and Wasm imported
by each shim; its actual runtime is bound to all ten files/25,060,030 bytes.
This complete closure supersedes the first driver's worker-directory-only copy
for artifact completeness, without rebuilding. The final runtime proves
scheduled-event failure/recovery/frozen daily snapshot, production fixture-route
exclusion, identity/configuration refusals and zero outbound requests. Sixteen
owned children and 32 complete raw streams pass the exact final marker scan.
`production-verification.json` independently verifies all 2,516 source inputs,
artifact/log bytes and four retained packaging-tool binaries; its SHA-256 is
`3da3e1f9af28b438cbe081915481fd2121737050f3e8e414580fbfb98337b469`.
The two native email fixture changes and two launcher changes since ordinary
capture are explicit copied source deltas; no production Rust difference is
inferred. Later native fixture pagination changes remain a separate scope.
These local results do not establish deployment, live GitHub behavior or
cross-release replay.
