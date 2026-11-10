# GitHub invitation settlement

Issue #120 unifies creation and settlement in
`RepositoryDelivery/<request_id>:<repo_id>:<generation>`. The internal invitation ID is bound
once by the approved command; another ID cannot create another writer.

- `on_webhook`, `reconcile`, `cancel`, and `tick_expire` share object
  exclusivity with creation and retain one terminal winner before projecting it.
- `SweepSchedule/repo:<account_id>` retains the sweep set independently of SQL.
  Created, AlreadyPending and OutcomeUnknown owners send registration hints;
  terminal/stopped owners send removal hints. The schedule verifies each hint
  against the current shared owner snapshot, so stale or reordered hints cannot
  remove live work or resurrect terminal work.
- `Reconcile/sweep` queues one repository walk per tracked numeric target. A walk
  completes all invitation pages once, then separate `observe_next` invocations
  inspect one delivery each and send evidence to its owner. Every absent known
  invitation requires current installation and numeric repository verification
  in that observation. Unknown creation resolves only from positive presence.
  Failed/partial reads supply no evidence and are retried by the next sweep.
- The owner retains the winning terminal transition. Independent `DeliveryProjection`
  execution applies one revision lineage. The SQLx transaction / D1 batch commits
  the complete snapshot, create audit and settlement audit together.
  There is no state-committed/audit-missing window. An acknowledgement lost after
  commit is safe to retry. The invitation ID supplies the stable terminal audit
  ID; the retained winner supplies immutable event content on later evidence.
  Conflicting audit IDs fail the atomic application instead of dropping the event.
- The upstream ID is retained after settlement. Delayed create projection and
  later lifecycle commands cannot resurrect or contradict a terminal result.
  A database trigger additionally rejects incompatible writes after settlement.

The expected `Sent` state and exact upstream/request/repository identity are the
fence: this lifecycle has no transition back to `Sent`. It does not require a
timestamp comparison. Projected `Sent` rows cannot reconstruct absent authority.

`cancel` settles Cancelled on GitHub's 204. GitHub answers the DELETE with the
same 404 whether the invitation is gone or the installation no longer sees the
repository, so a 404 settles only after the current account installation verifies it
still reaches the scoped numeric repository ID. An unavailable repository is
"cannot observe now" ([ADR 0006](adr/0006-delivery-and-settlement-checks-stay-distinct.md)):
nothing is settled and the row stays `Sent` for reconciliation. A failed
repository read leaves lifecycle unchanged. Installation/account/repository identity
is verified before deletion and rechecked after a 404. A withdrawal naming a retired
installation cannot borrow its replacement's credentials. `tick_expire` remains an
explicit operation with its existing pending-invitation evidence requirement; request
decision deadlines do not schedule it. Automatic lifecycle cadence remains in #86.

The post-settlement SQL fence is defense in depth over the projected snapshot.
Retain owner state, HTTP fences, webhook routing receipts and audit history together.

## Verification

Approved seams: public Restate create/lifecycle commands with a GitHub HTTP stub,
and Storage transition/read interfaces on SQLx and actual Worker/D1.

```sh
cargo test --locked -p ghinvite-storage-sqlx --features test-util --test settlement
bash scripts/test-restate.sh retained_delivery
WASM_BINDGEN=/path/to/wasm-bindgen npm run test:admission --prefix tests/worker
```

The native and Worker runtime scenarios cover a blocked create, intervening sweep,
successful retry and preserved upstream ID; accepted webhook versus delayed
reconciliation evidence; concurrent reconciliation; atomic rollback on audit
failure; and one terminal audit. The D1 test executes the real batch and loses its
acknowledgement before retry. The native approved-delivery test races supported
withdrawal with webhook settlement while parent projections are withheld.
The SQLx boundary also verifies audit-ID conflict rollback and
rejection of a late write after settlement.

## Retired member-added webhook protocol

The following #84 section records a retired prelaunch protocol. Current signed
`/webhooks/repo` ingress acknowledges `member` and `repository_invitation` events
without dispatch. Repository settlement uses retained scheduling and verified
invitation observations. Migration0022 removes the unused `webhook_receipts`
table and its storage/fixture matching API; prior migrations stay unchanged.
The separately retained organization webhook bindings and their seven-day
retention are unaffected. The private legacy delivery handlers below remain
registered pending their separate endpoint-version cutover; removing this SQL
facade does not rewrite their commands or audit identities.

### Historical #84 behavior

Issue [#84](https://github.com/sagikazarmark/ghinvite2.orig/issues/84) supports
`POST /webhooks/github` with `X-GitHub-Event: member`, action `added`, JSON content,
`X-GitHub-Delivery`, and a valid `X-Hub-Signature-256` over the exact request body
using `GHINVITE_WEBHOOK_SECRET`. Only authenticated deliveries reach matching.

The supported GitHub App payload requires numeric `installation.id`,
`repository.id`, `repository.owner.id`, and `member.id`. The installation's stored
account must equal the repository owner. Matching uses immutable account,
repository, and requester IDs, including links from historical installations;
repository names, user logins, and `sender` do not select a request. The lookup
returns at most two historical candidates. Exactly one candidate in `Sent` with
an upstream invitation ID schedules exact-invitation observation through
`RepositoryDelivery/<request>:<repository>/on_webhook`. Web ingress retains routing receipts in SQL;
only the delivery owner settles invitation state and arranges audit projection.
Detected missing request/delivery candidates cause conservative no-match routing.
Even apparently complete projections cannot prove association: a matched member
event only requests read-only verification by `DeliveryObservation`. Its payload
alone cannot settle an invitation. A complete listing must show the exact upstream
ID absent, followed by verified numeric membership evidence. Observation runs outside
owner exclusivity and applies identity/phase-bound evidence through the owner.

Before dispatch, `member_webhook_receipts` retains the first match or no-match
under the SHA-256 of the exact authenticated body. The delivery header is not
signed and cannot replace this identity. Concurrent retries recover the retained
target, including after dispatch acknowledgement loss; a no-match cannot later
bind to a new invitation. Receipts have no automatic expiry and must be backed up
with invitation history. Byte-identical subsequent provider events conservatively
reuse the receipt; invitation-specific reconciliation remains the fallback.

Member events contain neither an invitation ID nor an acceptance timestamp.
Multiple historical candidates are therefore ambiguous, even when only one is
still pending: acknowledging an old event must not accept a newer request. These
cases remain for invitation-specific reconciliation. Untracked events, unknown
installations, account mismatches, unsupported actions (`edited`/`removed`), and
team `membership` events receive an empty acknowledgement without settlement.
Malformed identity payloads fail with the receiver's generic error response;
tampered signatures receive 401. No private invitation/request state is returned.
`Sending`/unknown creates remain with the retained create owner. A delivery before
the Sent projection exists retains no-match and needs reconciliation.

### GitHub configuration and evidence

GitHub's [event documentation](https://docs.github.com/en/webhooks/webhook-events-and-payloads#member)
(consulted 2026-09-17) describes `member.added` as a GitHub user accepting a
repository invitation. It requires at least **organization Members: read** for
a GitHub App subscription. Enable that permission, obtain installation approval
for changed permissions, subscribe to **Member**, and ensure the repository is
available to the installation. Keep repository **Administration: write** and
**Metadata: read** for invitation delivery. The subscription is `member`, not
team `membership` or `organization.member_added`.

Organization permissions are not applicable to personal-account installations;
do not assume the App can subscribe to or receive Member there without checking
the provider's current configuration and actual delivery. Reconciliation remains
the fallback. The legacy `repository_invitation` accepted/declined adapter remains
for compatibility, but the current GitHub event catalog does not document it as
a subscribable event; deployment must not depend on that checkbox existing.

The signed HTTP tests use synthetic, schema-shaped payloads and a Restate HTTP
recorder. The native `retained_delivery` gate uses the actual web route, production
command adapter, real Restate, and SQLx invitation/audit reads. A proxy forwards
the durable send then loses its acknowledgement; redelivery, an audit write
outage, and concurrent delayed reconciliation leave one accepted transition and
one reconciliation audit. The Worker gate also exercises the new identity lookup and
retained match/no-match bindings against actual D1. These are local runtime proofs, **not captured live GitHub
deliveries or proof of GitHub subscription availability**. Existing #63 D1
settlement verification now exercises the unified owner and atomic projection boundary.

```sh
cargo test --locked -p ghinvite-web --lib signed_member
bash scripts/test-restate.sh retained_delivery
WASM_BINDGEN=/path/to/wasm-bindgen npm run test:settlement --prefix tests/worker
```

Retain both routing and settlement receipts. The routing receipt guarantees
replay association from deployment onward; it cannot establish when a never
previously observed delayed provider event occurred.

## Recovery after reinstall

Issue [#65](https://github.com/sagikazarmark/ghinvite2.orig/issues/65) makes
the retained account schedule discover pending work by immutable account ID
across historical installations. Each observation reloads the current active
installation, verifies its installation/account IDs and suspension status with
GitHub, and validates the scoped numeric repository ID before reading invitation
or requester access evidence. Another account or a reused repository name cannot
supply settlement authority. Missing installations, unavailable credentials, or
unverified identities leave the invitation recoverable for a later sweep;
failed GitHub reads leave the work registered for the next sweep. Projection retries remain independent.

The original link installation ID, request, fixed scope, create receipt and audit
history are preserved. Settlement runs through the request/repository-keyed owner.
The native runtime test covers A approval → blocked delivery → B delivery →
credential outage/identity mismatch → missed-webhook settlement. The Worker gate
covers replacement discovery and account mismatch using actual D1 and Restate.
Account-based discovery and authority verification happen inside the
the sweep's retained-owner reads and GitHub observation run closures.

## Complete pending-invitation observation

Settlement and delivery recovery read GitHub's pending-invitation listing and
treat a tracked invitation's absence from it as evidence. That evidence is only
sound over a complete observation, so the listing follows every
`Link: rel="next"` page. An incomplete walk — a failed later page, a malformed
body or `Link` header, a link off the API base, or a return to a page already
read — fails the read instead of returning a shorter list. Reconciliation then leaves the invitation pending and
retries; retained-create recovery keeps the outcome unknown. GitHub's offset
pages are still not a consistent snapshot: an invitation cancelled on an earlier
page mid-walk can shift a later one out of view. That residual race is unchanged
by this work and is bounded by the settlement fence, which admits one terminal
transition per invitation. Native tests cover
lifecycle reconciliation (`Reconcile/sweep` and `settlement::observe`) and
retained-create evidence with multi-page results and a failing later page:

```sh
cargo test --locked -p ghinvite-github --lib
cargo test --locked -p ghinvite-workflows --lib
```

## One scheduled chain per account (B0-18)

Onboarding queues `ensure_scheduled`. Empty accounts run every 24 hours and
request an installation drift recheck. Registering work pulls the deadline to
`min(next_run, now + 1 hour)`, advancing a bounded monotonic schedule token only
when a new delayed send is required. Duplicate registrations do not fork timers.
`run` ignores stale tokens and premature manual invocations. The current due
run calls the fan-out service, then advances the token and schedules one successor.
After the work set empties, the next run returns to the 24-hour cadence.
Old delayed sends can remain queued, but their obsolete tokens are inert.

There is no D1 pending-row discovery and no synchronous loop over delivery owners
in a sweep invocation. A repository observation continuation handles one delivery
and queues the next continuation before its external read. Listings retain only
numeric invitation/invitee identities, typed role and expiry evidence. HTTP errors
do not become empty listings, and failed reads do not accumulate a second retry
chain alongside the account schedule.

`tests/sweep_schedule.rs` exercises actual Restate with a journaled fixture clock:
24-hour cadence, pull-forward at t, acceptance by t+1 hour without a webhook,
exactly one successor token, stale/early/duplicate runs, late track/stale untrack,
a shared two-page walk for two deliveries, unavailable projection and later
convergence, same-sweep repository 404, insufficient base role, uncertain-create
presence-only recovery, and explicit upstream expiry. Its fixed named-run table
covers every current SweepSchedule handler, including all three run paths.
`SWEEP_ONLY=1 node tests/worker/admission.mjs` repeats cadence, shared listing,
stale-token/hint and acceptance behavior against actual workerd, Restate and D1
with a failing delivery-projection trigger, then proves convergence after repair.

Deployment remains gated by ID-21/ID-32. These new calls and retired `daily_run`
wire are incompatible with old retained journals. Pin old invocations to their
original endpoint or drain them. Existing pending owners must finish on that
endpoint or be explicitly enrolled through validated registration before relying
on the new schedule; merely deploying this code does not migrate that work.
Organization sweeps and requester-triggered refresh remain M1 work. Direct
collaborator acceptance checks remain behind SP-07; current evidence records
verified effective role. No live GitHub or production rollout is claimed.

### Migration0022 rollout boundary

The removed table contains obsolete repository member-event deduplication
bindings, not Audit Log rows. Existing organization bindings and audit events
remain. Before a deployed database drop, inventory and drain or route away every
older serving/pinned artifact that still calls this SQL; current source's lack
of callers cannot establish deployed compatibility. Legacy private delivery
handlers remain registered independently of this table removal. Local tests do
not claim remote migration or release.
