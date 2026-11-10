# Link pause and admission holds

The link owner serializes admission, pause, resume and revoke. Its lifecycle
operation receipt binds the authenticated admin, link, action and operation ID.
An exact replay returns its original result even after a later transition;
changing the action or actor conflicts. Manual pause stops fresh admissions
without changing requests, decision deadlines or deliveries. An existing
admission receipt remains recoverable. Capability validation precedes pause
presentation, so an invalid or rotated secret cannot discover the pause state.

Resume follows the explicit `ExpiredOrExhausted` refusal in P1 Appendix A.2:
it cannot resume an expired or exhausted link. A refused resume sends no release
and leaves an abuse hold in place; revoke aborts those held deliveries. A
successful resume clears pause without changing immutable guardrails or uses.
Revocation is terminal. Its default cascade still aborts an active abuse hold.

## Automatic pause

Admission retains timestamps from the last rolling hour. Only an accepted new
request appends a timestamp and consumes a use; old operation replays and
refusals do neither. At the journaled decision time, admitting beyond the limit
instead freezes an abuse window beginning at the oldest retained timestamp and
pauses the link in the same exclusive invocation. A wall-clock regression behind
any retained admission returns a retryable failure without consuming a use or
creating an invalid future hold window. The default is 20 per hour,
or `max(20, ceil(max_uses / 10))` for a limited link. A positive custom limit is
an immutable creation guardrail. Resume does not reset the rolling window.

Each request receives a full link control snapshot with its immutable admission
time and invitation lifetime. The initial request inbox acknowledges the snapshot
before initialization can dispatch. Later controls use durable one-way sends;
request owners forward them to their deliveries without calling the link back.
The retained admission handoff includes the hold-epoch watermark from its own
decision, so a later pause cannot change an old initialization operation.

Control operation IDs derive from the link ID and authoritative revision in a
private hash namespace. They never reuse a requester-selected operation ID that
could collide with an earlier mailbox receipt. Every full snapshot carries the
greatest hold epoch, including released epochs. A revoke also carries the active
hold window so a delivery can abort even if the earlier hold message is delayed.
[Delivery control](delivery-control.md) specifies uncertainty, fence and ownership
rules; listing absence never proves no external write occurred.

## Immutable invitation lifetime

The domain accepts 3,600 through 604,800 seconds, defaulting to seven days. The
creation form offers whole hours from 1 through 168. Blank input retains the
default, and a blank abuse threshold selects the calculated limit. Native forms
and the browser island use the same validators and preserve raw invalid input.

The lifetime and threshold belong to the immutable creation snapshot and SQL
columns. Default values preserve older serialized creation content; SQL reads
interpret an absent lifetime as seven days. Equal-revision conflict checks and
future projections reject changing either guardrail. This does not waive the
pinned/drained deployment requirement for changed workflow journal paths.

## Immutable budget share

Creation accepts `budget_share_pct` from 1 through 100, defaulting to 50. The
server form and browser island use one validator; blank input selects the
default. Metadata edits cannot change the share. The immutable value travels
through request initialization and the approved delivery command, independent
of SQL projection availability.

Before S3 reserves a slot, the delivery owner calls `InviteBudget.set_link_share`.
An exact assignment replays; a changed percentage conflicts. This setter leaves
the organization cap and other links unchanged. Once a link policy is bound,
the legacy bulk fixture configuration cannot overwrite it. Each daily bucket
uses its own cap times the percentage, rounded down with a minimum of one;
untagged administrator reservations still use the remaining shared capacity.
This path is enabled only by the existing explicit budget composition. Share
assignments remain retained after the rolling charge window expires: deleting
them could allow a later retry to bind a different policy. There is no arbitrary
lifetime link-count refusal. The retained map grows with configured links; safe
compaction requires a future explicit retired-link/no-future-reservation proof,
not merely expiration of existing charges.

Migration 0007 backfills the already-existing nullable SQL column to 50. Readers
also coalesce old NULL values to 50, and the projector writes explicit values
and rejects changes at every revision. Default serialization omits the new
field so existing canonical creation and delivery-fence content stays unchanged
for the default policy. Added workflow calls still require the documented
pinned/drained deployment procedure; this is not a retained-journal compatibility
claim or SP-08 production activation.

## Budget-share verification

- Actual Restate: all five budget tests pass, including immutable 1%/100% shares,
  unchanged cap/other links, minimum-one rounding and administrator capacity;
  projection-paused link capability test preserves custom 17% through approval
  and refuses changed creation input (`/tmp/platform-budget-share-runtime.log`).
- Actual S3: policy registration precedes the first charge; 1% of the repository
  cap permits one PUT, then returns retained `LinkShare` without another PUT or
  attempt fence (`/tmp/platform-budget-share-s3-real2.log`).
- Core75/island37/UI133 and web form30 tests pass. The production island is394KB
  compressed; all320 Chromium tests pass, including mounted/no-JavaScript bounds,
  default50 and explicit17% submission (`/tmp/platform-budget-share-unit3.log`,
  `/tmp/platform-budget-share-forms-final.log`,
  `/tmp/platform-budget-share-island-build2.log`,
  `/tmp/platform-budget-share-browser.log`).
- Shared SSR/island parity14 and strict web/UI/island library+test Clippy pass
  (`/tmp/platform-budget-share-parity-final.log`,
  `/tmp/platform-budget-share-clippy-final.log`).
- Actual D1 shared33 plus audit, telemetry and digest guards pass
  (`/tmp/platform-budget-share-d1-final2.log`). Later organization-delivery schema
  work and its additional scenario have their own evidence.
- Signed Worker organization-cap confirmation, refusal, exact replay, 500/50
  projection and once-only audits pass in the separate budget-enabled artifact
  (`/tmp/platform-budget-cap-workerd-final.log`). Run
  `npm run test:budget-cap --prefix tests/worker`; both CI workflows run this
  gate after building the isolated composition. Ordinary production defaults
  remain disabled.

## Local verification

- `authoritative_admission`: all four actual Restate tests pass, including
  queued admission behind pause, interrupted transitions, operation replay,
  approvals while paused, automatic hold/release/revoke, unchanged request
  deadlines, exact rolling-hour expiry, wall-clock regression/recovery and a
  failed initialization replayed after later hold/release controls.
  `/tmp/p1-link-controls-clock-runtime.log`.
- `PAUSE_ONLY=1 node tests/worker/admission.mjs`: actual signed Worker and D1
  pass manual pause, auto-pause, approval while held, one write after release,
  abort on revoke, auto-approved blocked work held across installation recovery,
  immutable old receipts, once-only audits, and 21 concurrent
  admissions producing exactly 20 accepted requests plus one pause.
  `/tmp/p1-auto-pause-workerd-final.log`.
- Shared SQLx/D1 conformance includes pause epochs, stale/equal revision handling,
  projection state, guardrail immutability and terminal revocation. All 31
  scenarios, including request cancellation, pass on SQLite and actual D1.
  `/tmp/request-cancel-sqlx.log`, `/tmp/request-cancel-d1.log`.
- Web pause routes: 132 passing tests, five existing fixture tests ignored;
  native/enhanced pause browser tests: two pass. Creation controls: UI130,
  island37, parity13 and webforms29 pass; real Chromium form suite80 passes.
  Strict web/UI/island checks and the production island build pass.

These are local proofs. Full cascade execution/progress, invitation cancellation,
own-expiry execution, remaining M1 acceptance, remote CI and live release gates
remain open; this document does not mark B1-08 or M1 complete.

## Organization selection and exact Duplicate values

The existing new-link island now shares organization and team selection with
native form submission. Roles remain Member; repository grants remain separate
per target. The server resolves both installation IDs from current app facts,
checks numeric organization/user/team identities and the complete repository
selection, and refuses unavailable or positively identity-provider-managed
teams. Team listing is paginated with a bounded read budget and fails without a
partial picker. A missing org app has a separate `/install/org` destination;
`GH_ORG_APP_INSTALL_URL` must be a validated GitHub installation URL. Missing
configuration shows an unavailable recovery page, never a setup callback link.

`GH_ORG_DELIVERY_ENABLED` is false by default and accepts only `true` or `false`.
Enabling it requires `P1_APP_STAGE=repo-and-org` and complete organization app
configuration. This explicit local composition is separate from live GitHub
readiness gates. Creation-time IdP detection remains limited to positive
upstream facts; absence of an LDAP marker does not establish write eligibility.
The form explains that seats, account restrictions and IdP policy are rechecked
at delivery. No live seat or policy approval is inferred from the picker.

Duplicate creates a fresh link ID and capability while preserving Targets and
all immutable guardrails. Exact expiration (RFC3339, including fractional
seconds) and invitation lifetime in seconds are visible, editable controls.
They preserve values such as a 3601-second lifetime rather than rounding to
hours or starting a new relative expiration. An expired source expiration must
be changed before submission. The original link, receipt and secret are not
modified or copied. Creation replay still uses the original retained target and
installation bindings.

## Organization and team delivery presentation

Requester and authorized Console history use the scoped organization projection
port plus the requester-bound dispatch RPC. When dispatch is available, only its
exact generation and immutable target set supply the organization receipt and
its atomic team snapshot. If that RPC is unavailable, the latest projected
numeric generation remains visible with an explicit unavailable warning. Planned/submitted RPC results only describe dispatch; missing
projection never establishes an invitation or membership. Every requested team
has its own row, including missing, pending, unknown and blocked results.

Existing foreign invitations remain explicitly unowned. Pending team membership
is not called active membership; verified owner/maintainer roles remain intact.
Policy blocks have bounded actionable text. Read failures preserve available
projected history with an unavailable warning, and all observations are labeled
as potentially delayed rather than current access guarantees. Requester pages
still require the capability/session binding; Console history requires current
full-target authority before reading account/link/request-scoped data.

The requester has a native POST refresh form, progressively enhanced by the
existing external script. Refresh requires the current capability, authenticated
requester, approved request, authoritative dispatch generation and exact retained
organization command. It never accepts a caller-supplied owner key or target.
The owner durably throttles observation scheduling to once per minute; the web
route additionally shares the existing per-user admission edge bucket. Native
and JSON responses distinguish requested, throttled (with retry time), and not
tracked. A missing newer projection cannot authorize a stale-generation refresh
or show an older generation's successful result as the current outcome.

Local verification for this slice includes UI137, island37 and SSR/island parity16
tests (`/tmp/platform-org-form-ui-final.log`), web library140, Console112 and
requester25 tests (`/tmp/platform-org-form-web-final.log`; three Console and two
requester live tests remain ignored). Authority5 covers complete pagination and
numeric/app identity boundaries (`/tmp/platform-org-authority-tests2.log`).
Focused refresh HTTP tests2 pass (`/tmp/platform-org-refresh-http3.log`), including
CSRF, rotated capability, current-generation binding, native responses and typed
throttling. These are local authorization and rendering proofs, not live GitHub
seat, identity-provider or deployment activation evidence.

The current production island builds at 408 KB gzip
(`/tmp/platform-org-form-island-final2.log`). The full Chromium suite passes
344/344 across desktop/mobile and light/dark themes
(`/tmp/platform-org-form-browser-final2.log`), including no-JavaScript and mounted
organization-only/mixed submission, exact Duplicate fields, pre-hydration edits,
accessibility and enhanced refresh feedback. Native all-target strict Clippy and
web-worker runtime-tests Wasm strict Clippy pass
(`/tmp/platform-org-form-native-clippy-final.log`,
`/tmp/platform-org-form-wasm-clippy-final.log`).

## Lapsed request presentation

Lapsed is a terminal delivery result for an originally approved request. The
requester and Console keep the original approval and per-target delivery history.
The requester sees “Request again” only when the authoritative page allows a
fresh attempt; the new attempt has a new operation identity and consumes another
use. A retained `rerequest_after` or `RerequestLimited.retry_after` is shown as an
absolute retry time, with current link admissibility still required. Viewing the
page never submits another admission.

The actual admitted state controls the page: a re-request admitted as Pending
shows admin review even if the link normally auto-approves. Lapsed history does
not offer organization refresh, and the refresh route refuses a Lapsed request.
Console calls the retained decision timestamp the original approval time rather
than inventing a completion timestamp. The web tests script owner responses;
they do not prove the owner’s seven-day counter or delivery aggregation.

Current native web verification passes library140, Console113 (three ignored),
requester27 (two ignored), document-shell3 (one ignored) and concrete-client18
tests in `/tmp/platform-lapsed-web-final.log`. The Console case uses the real
SQLite atomic approval-plus-Lapsed envelope and checks the preserved actor and
time; requester cases cover the seven-day date, forbidden fresh form, actual
Pending state and refusal of refresh after Lapsed. Actual web-worker Wasm strict
Clippy passes `/tmp/platform-lapsed-wasm-clippy.log`. The earlier 344-browser gate
covered organization selection/refresh before this additive Lapsed presentation.

Native all-target strict Clippy also passes `/tmp/platform-lapsed-native-clippy.log`.

The actual requester-router Chromium gate now passes all six cases in
`/tmp/platform-lapsed-browser2.log`: desktop with JavaScript and mobile without
JavaScript each cover inherited mixed delivery/settlement, Lapsed Request again
with a fresh operation and CSRF POST followed by Pending review, and the durable
retry date with no fresh-request form. It reuses the native router fixture and
existing assets; private authority responses remain scripted.

The updated browser fixture also passes strict Clippy in `/tmp/platform-lapsed-browser-clippy.log`; both existing CI delivery-browser jobs include these cases.

## Requester two-factor notice

The authenticated requester page shows an acceptance notice when the organization
has a fresh known requirement for two-factor authentication. The notice describes
the organization policy; P1 does not infer whether the requester has enabled it.
It leaves ordinary request submission and existing request history available.

The link owner validates the capability and requester before reading the
organization facts cache. Only facts matching the link's account and organization
installation, within the one-hour window at the journaled page time, can supply
the requirement. Unknown, unavailable, future or misbound facts produce no
affirmative notice. Suspension invalidates the cache through the canonical
installation getter. The public page carries only the optional requirement;
seat counts and other administrative observations remain private.

This adds a cross-object read to organization requester pages. Endpoint pinning
or draining under ID-21 applies even though the optional wire field is compatible
with older page readers. Minimal-app field visibility and real sign-in acceptance
remain live gates.


Requester notice verification: the real Restate/cache/admission regression passes
in `/tmp/requester-two-factor-runtime3.log`; HTTP requester31 and document-shell3
pass in `/tmp/requester-two-factor-http-final.log` (existing fixture ignores are
unchanged). The production requester-router browser suite passes all eight cases
in `/tmp/requester-two-factor-browser.log`, with desktop JavaScript and mobile
without JavaScript covering known, false and unknown requirements alongside
inherited mixed delivery and Lapsed recovery. Browser OAuth and private authority
responses are scripted; they do not establish token-visible live organization
fields. Workflow/web all-target strict checks pass
`/tmp/requester-canonical-strict.log`.
