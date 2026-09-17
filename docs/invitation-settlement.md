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
late legacy writer rejection. Complete GitHub pagination remains separate work.
