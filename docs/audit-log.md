# Console Audit Log

`GET /console/accounts/{login}/audit` shows recorded events for the authorized
account ID, including previous installations. Current Console authorization is
required on every request. Audit responses use `Cache-Control: private, no-store`.

## URLs and live history

- No `event` means All events. Otherwise use one exact persisted event type.
  Unknown values normalize to All events.
- `before` seeks older than a row; `after` seeks the nearest newer rows. Both
  use exclusive timestamp/ID boundaries. Pages contain at most 25 rows and are
  displayed newest first. Navigation is based on bounded existence probes.
- Cursors are version-1 URL-safe base64 JSON containing the full UTC timestamp,
  audit event ID, and normalized event filter. They carry no account authority.
  Invalid, unsupported, over-512-byte, filter-mismatched, or conflicting cursors
  normalize to latest. Duplicate recognized keys use the last value; unrelated
  parameters are ignored. Applying the native GET filter drops both cursors.
- Back to latest and retry preserve the normalized filter. An empty cursor range
  is distinct from empty account history, no filter matches, and a load error.

This is live operational history, not a snapshot or a completeness guarantee.
Front arrivals do not shift older pages; delayed events can appear behind a
cursor. Names are locally last known, not historical identity snapshots.

## Delegated Bulk history

Delegated readers see a repository-only batch only when they administer every
repository in its immutable manifest. This predicate applies to the batch's own
audit events and its retained request, repository-invitation and access-grant
events before event filtering, page limits and both navigation probes. An
organization or team target excludes the whole batch from delegated scope.
Account administrators retain account-wide history. Event metadata cannot grant
visibility; the query follows relational batch/request/delivery/grant identities.

Authority discovery reads distinct repository identities from both invitation
links and retained batch manifests through `distinct_console_repo_targets`.
Its bounded cursor and current numeric GitHub evidence rules are unchanged.
Consequently an account with only batches can still authorize its repository
administrators' audit reads. This discovery supplies candidates, never authority
by itself, and browsing sends no mutation command.

The shared SQLite scenario verifies zero invitation links, real immutable batch
creation records across two pages, full/partial authority, both seek/probe
directions, independent request/acceptance/removal events and unchanged receipts.
The HTTP regression uses current installation-token GitHub observations and
proves a two-repository batch appears only with authority over both repositories
(`/tmp/m2-bulk-delegated-audit-http3.log`). The combined shared suite passes
all 49 scenarios on SQLite (`/tmp/email-transition-shared2.log`) and actual D1
(`/tmp/member-facade-d1.log`), including this delegated Bulk scenario and the
email transition audit cases.

## Read and presentation boundaries

The storage port owns bounded reads and indexed timestamp/ID pagination on both
SQLx and D1 (see `migrations/README.md`). No debug full-history reader is enabled
for production. Up to 25 distinct actors and 25 distinct invitation-link targets
are enriched through local point reads, deduplicated within the visible page.
Target enrichment reads only identity/account membership, not repository lists.
Optional enrichment failures fall back to IDs; core read/decoding failures fail
the whole page with HTTP 500.

Only safe event-specific metadata summaries enter UI rows. Private metadata
values, arbitrary JSON, invitation codes, upstream errors, and correlation IDs
are not rendered. Browsing calls no workflow commands and does not emit events.
GitHub calls are limited to existing authorization. Controls, table, and
navigation are server-rendered and work without JavaScript.

## Verification

- Shared storage conformance covers account isolation, exact filtering, bounded
  pages, exclusive forward/backward seeks, equal-time IDs, nanoseconds, and new
  arrivals. SQLite fixtures cover historical mixed encodings and corrupt rows.
- Console HTTP tests cover authorization, reinstall history, URL normalization,
  navigation, safe summaries/resources, privacy, enrichment failures, and errors.
- The D1 storage gate (`npm run test:storage --prefix tests/worker`) runs the
  shared conformance suite on the actual Worker adapter in workerd, and reads
  historical mixed encodings through the adapter in both directions with exact
  page boundaries, filtering/account isolation, and expression-index seek plans.

## Organization delivery transitions

Each organization delivery retains its snapshot, per-step semantic cursor and
pending audit facts in one state value. An invocation interrupted before that
write exposes none of the new transition; interruption afterward preserves the
original fact time and transition revision alongside the receipt. Before a later
mutation replaces this value, the owner durably enqueues any pending facts with
the durable predecessor snapshot. SQL acknowledgement is handled independently
by the projector, so an unavailable database cannot turn a recorded transient
state into a missing event.

Organization and team steps emit `delivery.blocked`, `delivery.outcome_unknown`,
`delivery.held` and `delivery.released`. Repeated observations of the same state
and reason do not produce polling noise. A new hold epoch advances the snapshot
revision even when the Held receipt itself is unchanged. Replacing one held
epoch with another emits the new Held fact; returning to runnable work emits
Released for the current epoch. Revocation that aborts held work never claims
that the hold was released. Confirmed creation receipts remain immutable.

The bounded projection envelope contains typed facts, never upstream prose.
Historical facts remain insertable with stale or equal snapshots, but cannot
roll back delivery state. Equal audit identities with changed payloads fail the
whole atomic projection. This is prospective recording, not a backfill of
transitions that previous owners never retained. Producer and projector changes
must use the endpoint pinning or draining procedure in ID-21; an old projector
would otherwise ignore the newly added audit field.

The organization transition producer was verified with 36 native actual-Restate
cases, including four audit scenarios, plus two semantic unit tests. The audit
cases retain facts through projector outage and lost SQL acknowledgement, kill
an invocation after observing its atomic receipt and before enqueueing, preserve
per-team hold epochs and confirmed peers, and distinguish revoke-abort from
release. The signed Worker standard organization pipeline verifies the original
NoSeat audit identity, reason and timestamp through repeated blocked recovery
and later successful invitation creation in actual D1. These checks precede the
separate canonical organization success-event version migration.

Run the native gate with `cargo test -p ghinvite-workflows --features integration
--test organization_delivery -- --test-threads=1` and a configured
`RESTATE_SERVER_BIN`. After the ordinary workflow Worker build, run
`ORG_DELIVERY_ONLY=1 node tests/worker/admission.mjs`. Native checks also scan the
owner's retained journals for the fixture installation-token and LDAP markers.

## Confirmed event versions

A delivery's first retained receipt fixes its audit version. Receipts without
`audit_version` keep `Legacy` behavior, including the previous event IDs, types,
metadata and omitted-field serialization. CanonicalV1 retains its original
invitation compatibility identity. Under ADR0011, new dispatch/reissue commands
omit that identity and owners select `canonical_v2`; old commands with the exact
compatibility value continue to initialize V1 when required by retained inputs.
Retries, recovery and later settlements preserve the retained selection.
A projection cannot upgrade or downgrade a delivery, even before confirmation or
when an older snapshot arrives. This does not backfill events from older owners.

Canonical organization writes use `org_invitation.sent`, and positive acceptance
uses `org_invitation.accepted`. Direct team writes use `team_membership.added`
with an explicit `membership_state` of `active` or `pending`. An invitation that
includes a team first records its pending attachment as `invitation.sent`; the
team's `team_membership.added` fact follows positively observed organization
acceptance. Pending never means the user already has access.

Canonical `delivery.aborted` events retain the bounded abort reason. A team
cancelled with an owned organization invitation retains its exact
`own_expiry`, `link_revoked` or `admin_reissue` reason. The team cannot invent a
reason or cancellation ownership from a foreign membership. Confirmed events
use a versioned identity namespace; transient transition IDs from ID-55 remain
unchanged. The installation event migration is separate.

V2 repository facts target `repository_delivery` with the physical delivery key;
confirmation and settlement IDs use distinct V2 phases. Organization/team V2
facts retain their kind-qualified keys with a V2 namespace. The transition fact
ID algorithm is unchanged. Migration0024 rejects mismatched version/identity
pairs and version changes, while the exact retained Legacy/V1 identity remains
unique and available for historical Audit Log authorization. No old event is
regenerated or rewritten. Immutable endpoint routing/draining and the shared
schema migration remain a coordinated deployment boundary.

Canonical organization owner verification passed all 41 native Restate scenarios
(`/tmp/org-canonical-telemetry-runtime.log`), including distinct sent/accepted
identities, pending versus active team facts, immutable create receipts, exact
replay, and typed own-expiry reasons on both organization and attached-team
cancellation events. Native workflow/integration strict and Worker Wasm strict
checks passed. The combined Worker capture
(`/tmp/org-canonical-telemetry-worker-build.log`) passed signed organization
creation and cancellation/reconciliation suites
(`/tmp/org-canonical-telemetry-workerd.log`,
`/tmp/org-canonical-cancellation-workerd.log`). The creation gate reads the actual
D1 canonical event and step clocks; the cancellation gate retains the earlier
foreign-ownership, unknown DELETE and own-expiry assertions. These results do
not establish the later installation event contract or live GitHub contracts.

## Organization journal inventory

`tests/organization_delivery/journal.rs` checks 30 fixed, source-reviewed named-run
paths against all 19 deployed handlers of `OrganizationDelivery`,
`OrganizationEligibility`, `OrganizationProjection` and `OrganizationReconcile`.
The gate uses the standard production endpoint composition, not the fault-injection
binder. It captures actual create/cancel operations, a capacity recovery with an
independent real budget, delayed recheck and own-expiry invocations, advisory
cache reads, reconciliation, and the receipt and per-step telemetry projectors.
Empty paths require actual nonempty retained journals; deleting a captured
journal is a negative control. The handler catalog also detects an added handler
without an inventory entry.

The native gate passed (`/tmp/org-fixed-journal3.log`, 16.68 seconds). Both CI
workflows already run the enclosing `organization_delivery` integration target.
The table describes current declared paths rather than every possible branch,
and is never generated from observed journal names. It complements the existing
repository/link inventory; it does not establish replay compatibility across a
change to production handler code. A deployment with retained invocations still
needs review of the old invocation prefix and wire/state contracts, or draining
those invocations before deploying incompatible changes.

Installation audit facts use the explicit V1 contract in ID-59. A verified suspension records GitHub's suspension time; a signed accepted-permissions transition records its original update time, payload digest and numeric sender. App identity and retained revision distinguish equal numeric installation IDs. Legacy generic installation event IDs and payloads stay intact. Recovery drains the original fact from the compound owner state before a newer installation or delegation snapshot can replace it. The SQL transaction applies the snapshot and retained facts together, including delayed historical facts; an audit conflict prevents the accompanying state change. Cross-version endpoint pinning/draining remains required because the projector's two former writes are now one named atomic run.

Installation verification: core3, projection7, shared SQLite44 and actual D144 pass. The real Restate installation gate passes4, including two arbitrary-kill recoveries and 29 fixed named-run paths across all18 account/projection handlers. A separate actual committed-SQL-acknowledgement-loss test passes. The signed Worker installation gate verifies the canonical source facts against actual D1, repeated-event suppression, pagination/replacement recovery, and loss of an already committed atomic batch acknowledgement. Logs: `/tmp/installation-audit-{core-tests,projection-tests,shared,d1,authority-full,lost-ack,workerd2}.log`. Core/SQLx and workflow focused strict checks pass. These are current-version local runtime proofs; ID-21 deployment and live-event gates remain open.
