# GitHub invitation settlement

Issue [#63](https://github.com/sagikazarmark/ghinvite2.orig/issues/63) adds
versioned settlement handlers on the existing `GithubInvitation/<invitation_id>`
Virtual Object:

- `on_webhook_v1`, `reconcile_v1`, `cancel_v1`, and `tick_expire_v1` share object
  exclusivity and one `Storage::settle_github_invitation` transition boundary.
- `Reconcile/daily_run_v1` journals read-only GitHub evidence, then calls the
  invitation owner with the observed invitation identity, state, and upstream ID.
  Concurrent sweeps can observe the same row; only the first valid settlement wins.
- Only `Sent` rows with an upstream ID qualify. `Sending` placeholders, including
  blocked and outcome-unknown creates, remain with `GithubCreateV1`. A missing
  GitHub list entry cannot resolve an uncertain create.
- A conditional insert retains the winning terminal transition. The SQLx
  transaction / D1 batch commits this receipt, its audit, and its state together.
  There is no state-committed/audit-missing window. An acknowledgement lost after
  commit is safe to retry. The invitation ID supplies the stable terminal audit
  ID; the retained winner supplies immutable event content on later evidence.
  Conflicting audit IDs fail the atomic application instead of dropping the event.
- The upstream ID is retained after settlement. Delayed create projection and
  later lifecycle commands cannot resurrect or contradict a terminal result.
  A database trigger additionally rejects incompatible writes after settlement.

The expected `Sent` state and exact upstream/request/repository identity are the
fence: this lifecycle has no transition back to `Sent`. It does not require a
timestamp comparison or a revision backfill for historical invitations. Existing
terminal rows retain their state and historical audit. Historical `Sent` rows
with upstream IDs use the new boundary directly, without a retained create receipt.

## Deployment and pinned invocations

This is an explicit lifecycle cutover, extending the deployment discipline in
[writer cutover](admission-cutover.md) and ADRs 0003/0004:

1. Pause webhook/lifecycle ingress and the reconciliation scheduler. Inventory
   outstanding legacy `GithubInvitation` handlers and `Reconcile/daily_run`
   invocations, including retrying invocations and delayed timers.
2. Drain legacy work on its **original pinned deployment**, or isolate its
   endpoint/database credentials and reconcile its outcomes before resubmitting
   under the new handlers. Do not repin an existing journal to a new handler or
   replay an uncertain create as a fresh create. Preserve coordinated backups.
3. Apply migration `0006_invitation_settlement.sql` to SQLx/D1. Retain the
   settlement table with invitation and audit records in backups/restores.
4. Register a new immutable deployment using the authoritative/cutover endpoint.
   It returns 410 for new legacy settlement/sweep calls. The legacy endpoint
   implementation retains its original journal sequence for draining; registering
   a deployment alone does not fence old code that still has database access.
5. Deploy the web caller (`on_webhook_v1`), change scheduled sweeps to
   `Reconcile/daily_run_v1`, and replace queued operational cancellation/expiration
   calls with `cancel_v1`/`tick_expire_v1` after checking their current state.
   Reopen ingress only after old writers are drained or isolated.

Confirmed create ownership and `GithubCreateV1` journal sequences are unchanged.
The post-settlement SQL fence is defense in depth, not permission to run an
unfenced old sweep concurrently during cutover. Independently restoring SQL or
manually deleting settlement receipts requires coordinated recovery before
resuming writes. Existing already-corrupted terminal rows need evidence-based
operator repair; the migration does not guess their original outcome.

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
acknowledgement before retry, and exercises historical Sent rows, cancellation,
and expiration. The SQLx boundary also verifies audit-ID conflict rollback and
late legacy writer rejection.

## Member-added webhooks

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
an upstream invitation ID is dispatched as acceptance through
`GithubInvitation/<id>/on_webhook_v1`. Web ingress retains routing receipts in SQL;
only the lifecycle owner settles invitation state and publishes its audit.

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
one GitHub-actor audit. The Worker gate also exercises the new identity lookup and
retained match/no-match bindings against actual D1. These are local runtime proofs, **not captured live GitHub
deliveries or proof of GitHub subscription availability**. Existing #63 D1
settlement verification applies to the unchanged atomic lifecycle boundary.

```sh
cargo test --locked -p ghinvite-web --lib signed_member
bash scripts/test-restate.sh retained_delivery
WASM_BINDGEN=/path/to/wasm-bindgen npm run test:settlement --prefix tests/worker
```

Apply `0007_member_webhook_receipts.sql` before deploying the new web/storage code
against the #63 lifecycle cutover. Retain both routing and settlement receipts.
No lifecycle handler journal sequence is changed. The routing receipt guarantees
replay association from this deployment onward; it cannot establish when a never
previously observed delayed provider event occurred.

## Recovery after reinstall

Issue [#65](https://github.com/sagikazarmark/ghinvite2.orig/issues/65) makes
`Reconcile/daily_run_v1` discover pending work by the link's immutable account ID
across historical installations. Each observation reloads the current active
installation, verifies its installation/account IDs and suspension status with
GitHub, and validates the scoped numeric repository ID before reading invitation
or requester access evidence. Another account or a reused repository name cannot
supply settlement authority. Missing installations, unavailable credentials, or
unverified identities leave the invitation recoverable for a later sweep;
transient API/storage failures retain the existing retry behavior.

The original link installation ID, request, fixed scope, create receipt and audit
history are preserved. Settlement still runs through the invitation-keyed owner.
The native runtime test covers A approval → blocked delivery → B delivery →
credential outage/identity mismatch → missed-webhook settlement. The Worker gate
covers replacement discovery and account mismatch using actual D1 and Restate.

Deploy as a new immutable deployment; keep in-flight sweeps pinned to their
original deployment. The `daily_run_v1` journal entry shapes and ordering are
unchanged: account-based discovery and authority verification happen inside the
existing candidate/observation run closures. A completed old candidate snapshot
may omit historical rows; a fresh scheduled sweep discovers them. No migration
or create-handler journal change is required.

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
lifecycle reconciliation (`Reconcile/daily_run` and `settlement_v1::observe`) and
retained-create evidence with multi-page results and a failing later page:

```sh
cargo test --locked -p ghinvite-github --lib
cargo test --locked -p ghinvite-workflows --lib
```
