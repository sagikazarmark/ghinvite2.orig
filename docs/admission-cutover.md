# Authoritative admission writer cutover (#58)

This is a **native maintenance procedure**, rehearsed with disposable SQLite and
Restate 1.7.9. It does not authorize a live deployment. #58/#59 are complete for
native tooling and local actual Worker/D1 bindings. Production rollout remains
blocked on #61: remote identity verification, routing, capacity, live D1
export/fence/adoption, backup/restore, and deployment isolation. Native binaries
retain `legacy` as their default mode. Follow the [deployment order](deploy.md#rollout-status-and-order)
for the remote rehearsal and rollout evidence.

The [#59 Worker/D1 gate](worker-admission-gate.md) now rehearses the adopted
checkpoint/import path against actual local bindings. Its remote deployment and
live D1 checkpoint/adoption limitations remain rollout blockers. Both Workers
accept `GHINVITE_ADMISSION_MODE=authoritative`; web also accepts `maintenance`.

## Safety boundary

Only trusted operators can reach migration handlers. Private ingress and the
maintenance window are mandatory. Migration IDs/checksums detect accidental
conflicts; they are not credentials or proof of infrastructure isolation.

`GHINVITE_ADMISSION_MODE=maintenance` on web closes dynamic routes other than
`/health`, including automatic webhook ingress, with 503/Retry-After. Stop scheduled
reconciliation and other producers too. Let short commands complete. Inventory
the still-pending workflows and every pinned deployment before stopping endpoints.

**A runtime pause, cancellation, kill, deployment replacement, or empty queue does
not fence an already-issued database/GitHub write.** Stop/isolate every old endpoint
process and its restart controller; remove its DB access and outbound GitHub
connectivity. Wait for issued calls to settle or record them as uncertain. Keep
all old deployment URLs inaccessible permanently after cutover. A queued invocation
pinned to an old deployment must never regain independent mutation capability.
Do not redirect an old pinned URL to incompatible new journal code.

The SQL `fence` command installs persistent triggers in one `BEGIN IMMEDIATE`
transaction. Old link/request inserts, deletes, updates without a revision advance,
repository mutations, and legacy link/request audit inserts abort. SQLite serializes
this fence behind existing write transactions. The triggers survive endpoint restart.
They do not fence GitHub or independent invitation lifecycle writes: endpoint/egress
isolation is still required. They are not a defense against an operator editing SQL.

## Inventory and coordinated checkpoint

Run commands from the repository root. Files contain private business input and
invitation codes; retain them with the backups, outside public logs and Git.

```sh
python3 scripts/admission-cutover.py runtime-inventory --admin http://127.0.0.1:9070 --output runtime-before.json
python3 scripts/admission-cutover.py fence --database legacy.db --migration-id cutover-2026-09
python3 scripts/admission-cutover.py inventory --database legacy.db --output inventory.json
```

The read-only inventory captures full owned parent rows, links/repositories, uses,
all request states, known deadlines, invitation IDs/results, delivery fences/outcomes,
and audit history in one database read transaction. It reports counter mismatch,
pending/approved conflicts, and missing parents/input/deadlines per link.

After fencing, capture a consistent SQLite backup including committed WAL content
(SQLite backup API, not a bare copy of an open `.db` file). Stop the disposable
Restate instance and checkpoint its **entire persistent data volume**, metadata,
logs, state, journals, pinned deployment artifacts and runtime configuration together.
Archive SHA-256 digests and an identity for that paired recovery point. Production
Restate/Cloud restore requires the platform-supported coordinated export procedure;
the native volume procedure is not a Cloud backup claim.

Export immutable workflow inputs and journaled decision deadlines/dispatch identities
before retiring long-lived invocations. SQL `created_at` is not evidence of the
original workflow timeout; preserve the established historical cap when present.
Record each invocation's ID, `pinned_deployment_id`, service/key/handler/status,
handoff obligation, and uncertain HTTP effects. Parent identities come from existing
owners/backups; never synthesize profile/installation stubs.

Validated Restate 1.7.9 management endpoints in the native rehearsal:

- `GET /deployments` inventories deployment identities.
- `POST /query`, JSON `{"query":"SELECT id, target_service_name,
  target_service_key, target_handler_name, pinned_deployment_id, status FROM
  sys_invocation"}`, with `Accept: application/json`, returns `rows`.
- `POST /deployments`, JSON `{"uri":"http://host:port"}` registers a new endpoint;
  `force:true` is used only for the disposable fixture's replacement deployment.
- `DELETE /invocations/<id>?mode=kill` retires an accounted-for invocation. Kill
  was exercised on a waiting workflow; authoritative status remained pending.

Use kill only after the old process/egress fence and obligation capture. Do not
purge journals to make inspection easier. Pausing is unnecessary for this procedure;
no unverified pause command is prescribed. Keep the exact runtime version pinned.

## Evidence and preparation

Create an evidence JSON with this shape (hashes are actual archive digests):

```json
{
  "version": 1,
  "migration_id": "cutover-2026-09",
  "manifest_checksum": "<inventory checksum>",
  "checkpoint": {"id":"paired-checkpoint", "database_sha256":"<64 hex>", "restate_sha256":"<64 hex>"},
  "runtime": {
    "deployments": [{"id":"dp_...", "artifact_sha256":"..."}],
    "invocations": [{"id":"inv_...", "deployment_id":"dp_...", "handoff":"request ID and journal evidence reference"}],
    "old_endpoints_isolated": true,
    "issued_effects_accounted": true,
    "new_ingress_closed": true
  },
  "requests": {
    "<request ULID>": {
      "input": {"request_id":"<request ULID>", "invitation_link_id":"<link ULID>", "requester_id":123, "justification":null, "created_at":"<original SQL text>"},
      "deadline":"<original journaled absolute deadline>",
      "evidence_ref":"journal archive/path/checksum",
      "deliveries": {"456":{"invitation_id":"<journaled invitation ULID>"}}
    }
  }
}
```

`input` is copied from retained legacy workflow evidence, then checked against SQL.
Missing/ambiguous input stays unresolved. Legacy accepted replay uses command version
**0**, operation ID equal to the historical request ID, and normalized original input.
It returns the original admission policy/deadline, even if the current request is
terminal. New admissions use version 1 and separate server-generated request IDs.
No unrecorded legacy rejection is fabricated. Old browser POSTs are not resubmitted
as fresh admissions; trusted recovery can use version 0 and authoritative status.
Noncanonical historical justification needs explicit reconciliation, not silent edits.

Approved requests require approval evidence and a complete per-repository plan.
Existing invitation IDs win; missing IDs must come from retained fanout/journal
evidence. `Sent` (or later lifecycle) with an upstream ID imports a confirmed create.
`Sending` without confirmation imports `outcome_unknown` and a SQL HTTP-attempt
fence. Evidence can supply a confirmed `outcome` or a blocked-before-effect outcome
only after positive reconciliation under ADR 0004. Absence in GitHub lists is not
proof of no effect. Never allocate replacement IDs to resolve missing fanout.

```sh
python3 scripts/admission-cutover.py prepare --manifest inventory.json --evidence evidence.json --output prepared.json
python3 scripts/admission-cutover.py verify --manifest prepared.json
```

Encoding v1 uses SHA-256 over sorted-key, compact UTF-8 JSON (no ASCII escaping),
without the top-level `checksum` field. Per-link source and per-request record hashes
are retained. The prepared manifest leaves embedded `begin.manifest_checksum` empty;
the runner inserts the prepared top-level digest into transport commands. Identical
manifests resume; changing evidence creates a different identity and cannot overwrite
an initialized link. Reconcile unresolved links **before** beginning their import.

## Import, verify, hand off, open

Start the **new native** workflows binary with
`GHINVITE_ADMISSION_MODE=authoritative` at a new URL and register it. This binds
the v1 authority/projector/lifecycle/delivery services and legacy contract tombstones.
New legacy link commands, request submit/decision, GitHub create, and unversioned
GitHub settlement/sweep calls return 410. Before lifting maintenance, complete
the [GitHub invitation settlement cutover](invitation-settlement.md#deployment-and-pinned-invocations):
drain pinned legacy work on its original deployment or isolate and reconcile it,
apply migration 0006, deploy the `on_webhook_v1` caller, and move cancellation,
expiration, and scheduled reconciliation to their versioned handlers. Installation
ownership remains available through its current contracts.

```sh
python3 scripts/admission-cutover.py adopt-projections --database legacy.db --manifest prepared.json
python3 scripts/admission-cutover.py import --database legacy.db --manifest prepared.json --ingress http://127.0.0.1:8080
python3 scripts/admission-cutover.py import --database legacy.db --manifest prepared.json --ingress http://127.0.0.1:8080 --activate
```

Adoption checks the fenced manifest and exact legacy link/request/repository source
before assigning revision/content/identity columns. It preserves audit rows and
request history and never refunds uses. Per-link progress lives in
`admission_import_progress`; the remote durable cursor is authoritative for import
resume. Unresolved links are skipped and have no authoritative route; do not introduce
a SQL fallback for them. Source changes, missing parents, or malformed data must be
reconciled while closed. A link's first `begin_import` refuses nonempty authority.

Import writes one request at a time, with immutable checksum binding, exact blocker
checks, replay input, dispatch plan, and receiving receipts. The small migration
marker gates every ordinary link command/read until activation. Exclusive handlers
prevent readers from observing a half-applied request. Ordinary invocation replay
finishes the original writes; a fresh identical item is a no-op. Imported receipts
cannot overwrite already-initialized receiving objects.

Before activation the runner reads back full link/request facts, uses/revisions,
deadline, exact blocker, stored legacy replay, dispatch and receiving outcome identities.
It applies the original snapshots through the production projector against the adopted
database and projects receiving receipts independently of lifecycle handoff. The activation
assertion is private operator input, after SQL adoption verification. A conservative
`activation-started` marker is committed locally before calling remote activation,
including when its acknowledgement is lost. Activation registers the original code.
Handoff starts only pending workflows or approved work with unconfirmed deliveries,
once per imported request; terminal historical states and confirmed creates do not
start new work. Receipts and plans protect later explicit delivery recovery.

After verifying all intended links and transferred work, start native web with
`GHINVITE_ADMISSION_MODE=authoritative`. Link creation, revoke, metadata, admission,
decisions, replay and immediate status use canonical link-ID objects. Reopen producers
only against the new deployment. Keep the SQL fence and old endpoint isolation.

## Recovery, restoration, and audit retention

Ordinary replay requires the original Restate journal, compatible deployed code,
and retained authoritative keys. It is not a distributed rollback transaction.

- **Before activation/new-authority writes:** under continued maintenance, restore
  the coordinated pre-cutover Restate/database checkpoint and its original binaries,
  verify counts/IDs/effects, then restore old routing. Account for external effects
  since that checkpoint. The restored pre-adoption checkpoint still contains SQL
  fences. With all new endpoints stopped and the paired Restate checkpoint restored,
  run `restore-legacy --database restored.db --restored-coordinated-checkpoint <id>`.
  This explicitly asserts coordinated restoration and refuses adopted/revisioned or
  activation-started databases. It removes only the cutover triggers, allowing original
  writers to resume. Never use this assertion to bypass reverse reconciliation after
  new-authority writes; a stale restored database cannot prove those writes absent.
- **After activation or any new authoritative write:** use compatible forward repair,
  or an explicitly reviewed reverse reconciliation of every outcome, use, blocker,
  deadline, decision, invitation receipt, audit event and external effect. Never switch
  back to a stale SQL snapshot. An absent SQL row does not mean unused eligibility.
- **Killed/purged partial transitions:** close the link and all relevant receiving
  commands. Compare the journaled complete decision against every touched key and
  send identity. Restore the original journal/checkpoint if possible; otherwise retain
  explicit repair evidence and reconcile missing sends/receipts before reopening.
  A stored outcome or imported-item marker alone is not proof a killed invocation
  finished. Do not clear blockers or replay admission with a new ID as a repair.
- **Missing parents:** restore complete user/installation rows from their owners or
  matching backup before projection redrive. Leave dependency work visibly retrying;
  never fabricate identity, approval, or refund a use.
- **Projection rebuild:** current Restate link/request snapshots rebuild current
  projections; plans and receiving receipts rebuild delivery views. Immutable legacy
  audit rows come from the coordinated database archive. New historical events need
  retained envelopes/event archives or a compatible database backup: snapshots and
  expired runtime journals cannot reconstruct historical audit. Retain both archives
  before cleanup. Replay envelopes with their original event IDs and revisions.
- **Orphan notifications:** enumerate retained workflow keys against link-owned
  terminal/consumed records and outstanding send invocations. Quiesce publishers and
  verify no lifecycle/dispatch obligation remains before runtime-supported workflow
  cleanup. A late notify can recreate a promise but cannot authorize dispatch; never
  delete link decisions/receipts along with orphan promise state.
- **Uncertain external results:** use `DeliveryRecoveryV1/recover` on retained plans
  and read-only reconciliation per [delivery recovery](delivery-recovery.md). A kill
  is operational, never request cancellation or permission for another GitHub PUT.

## Native rehearsal

```sh
python3 -m unittest discover -s tests/migration -v
bash scripts/test-restate.sh writer_cutover
cargo test -p ghinvite-web --test route_smoke
```

The runtime rehearsal invokes the real CLI and import/activation/verification
handlers, tests partial/resumed/conflicting import, legacy deadline/replay, receipt
import, unknown-effect fencing, management query/kill, and obsolete legacy routing.
SQLite fixtures cover pending/approved/cancelled/expired import and status, counter/blocker
and parent problems, late SQL revoke/decision, history preservation and rollback
refusal after a new admission. The CLI and runtime share the same adopted database;
overdue expiry/readmission projects five uses without refund, and confirmed Sent
receipts appear in delivery reads. A restored pre-adoption checkpoint resumes legacy
SQL writes after explicit coordinated-restore assertion. Existing authoritative-admission and retained-delivery suites cover new
admissions after terminal states, actual retention recovery, and confirmed-create
replay without repeated PUT. No live state is modified by the rehearsal runner.
