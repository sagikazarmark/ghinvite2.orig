---
status: accepted
date: 2026-10-17
---

# Canonical delivery identity with retained audit compatibility

P1 §12 and ID-23 require canonical delivery identities. At acceptance, the random
local invitation ULID remained in write fences, repository read facades and
immutable audit payloads, despite each owner already having a canonical key.
Use the request, target kind, numeric target and delivery generation as the
operational identity. Preserve old audit facts through explicit retained-version
compatibility, rather than retaining a second routing identity for new work.
This decision is approved for implementation; the inventory below is not a claim
that the cutover or any deployed migration has completed.

The repository owner key remains `{request}:{repo}:{generation}`; organization
owners retain their normative key and kind-qualified SQL storage ID
`org:{request}:{org}:{generation}`. Numeric upstream GitHub invitation IDs remain
required evidence for ownership, cancellation and settlement. They are neither
local delivery identities nor requester IDs.

New generations use `CanonicalV2` audit facts and allocate no local invitation
ULID, including administrator reissues and Bulk dispatch. Repository audit targets
use a new `RepositoryDelivery` target kind with the canonical physical key;
confirmation and settlement event IDs derive from the key and bounded phase.
Existing transition-fact ID derivation already includes key, step and transition
revision and stays unchanged. The retained receipt's audit version is immutable.

Legacy and CanonicalV1 receipts keep their exact original event IDs, target IDs,
metadata, timestamps and serialization. Their original `invitation_id` remains
an optional, explicitly retained compatibility value at the same serialized
property and struct order, omitted for new V2 commands. Never generate a fallback
ULID or infer an old identity from a newer key. Validate the audit-version/legacy-ID
pairing before projection. Compatibility decoding does not authorize replaying
old journal histories through changed handlers or starting legacy work through
an unsafe alias.

Fences address `(kind-qualified delivery key, step)` and bind the complete
immutable command. Rejecting a write additionally binds its exact attempt
generation. A changed command, target kind or delivery generation cannot release
another fence. A missing or unreadable fence cannot prove an uncertain write did
not happen. Old command JSON remains byte-stable; new commands omit only the
legacy identifier under the new contract.

Requester/history readers move to canonical delivery snapshots, retaining
current-generation filtering, unknown/blocked/held distinctions and settlement
history. Remove the `GithubInvitation` read facade and the SQL
`deliveries.invitation_id` routing column only after these consumers migrate.
Historical delegated Audit Log visibility must still resolve Legacy/V1 target
IDs through their exact retained receipt JSON. New V2 events join canonical keys.
Neither old events nor their authorization scope may disappear during removal.

The additive SQL cutover rebuilds `deliveries` without the bridge column and its
unique index, preserving all other column values, canonical IDs, child foreign
keys, receipt JSON, fences, triggers and audit rows. It must prove complete raw
row preservation and foreign-key/trigger behavior on both SQLite and actual D1.
Previously recorded migrations and checksums stay unchanged. No deployed schema
or retained state is assumed disposable.

The endpoint and shared-schema rollout are one compatibility boundary under
ID-21/ID-24. Inventory old serving endpoints, pinned invocations, delayed messages,
producer versions and shared database users. Route old producers and invocations
to their compatible immutable version or drain them before replacing the
protocol; remove shared SQL dependencies only after every consumer has stopped
using them. Pinning an invocation alone does not protect a dropped shared column.
The registered legacy `on_webhook`, `cancel` and `tick_expire` handlers must not
silently lose semantics on a serving endpoint. Their eventual removal belongs to
this explicit version cutover, with current observation/control paths replacing
tests of obsolete ingress behavior. No remote drain, migration, reset or release
is authorized by a local test result.

## Implementation inventory and acceptance

- Core: repository/organization create commands, retained audit version and
  target kinds, repository snapshot read shape, version-bound settlement facts.
- Workflow producers: approved Request dispatch, reissue generation allocation,
  repo/org receipt initialization, cancellation/control and retained settlement.
  Preserve existing Legacy/V1 receipts; update actual fixed journal inventories
  when an allocation run or handler path changes.
- Core SQL and adapters: command-bound create/cancel fences, delivery projection
  identity guards, canonical readers, historical audit scope, additive table
  rebuild. Preserve upstream numeric IDs and original historical timestamps.
- Web/UI: requester and Console history use current retained generations and
  canonical snapshots, with no synthetic invitation row or success inference.
- Proofs: old Legacy/V1 audit bytes and visibility; V2 create/settlement/reissue
  identities; changed-input and stale-generation refusals; uncertain-write fence
  retention; delayed/stale projections; raw migration/FK/trigger preservation;
  native real-owner and signed Worker/D1 flows. These are required gates, not
  evidence claimed by this ADR's existence.

The separate member-webhook facade removal uses migration0022 and did not alter
the ULID bridge or legacy private handler wires. The subsequent canonical cutover
uses0023/0024. Test-only unscoped installation ports are removed independently; `Account` and the
app-keyed retained `AccountInstallation.current_installation` remain valid domain
contracts and are not targets of that cleanup.

## Local verification (2026-10-17)

The local implementation now uses canonical full-command fences/readers and
CanonicalV2 producers, with Legacy/V1 compatibility retained as described above.
Core129, shared SQLite49 and actual D149 pass. The D1 run also executes the
populated0023 migration/preflight proof and historical missing-send-time adapter
check, plus raw audit/operational/digest guards
(`/tmp/canonical-delivery-shared-d1-2.log`). SQLx lib35, projection10, settlement4
and the two native migration cases pass. Real Request reissue3, installation2,
canonical link and retained delivery suites pass; signed Worker reissue confirms
distinct generation keys, no new local identity and exact original receipts.
The migrated ordinary signed ingress suite also passes
(`/tmp/platform-canonical-ordinary-workerd-final.log`), including actual deadline,
D1 fault and installation-outage paths. These
local proofs do not establish a remote endpoint inventory, drain or deployment.

A subsequent actual Bulk cancellation regression found one remaining V1-only
initializer in `delivery_batch_cancel`: a canonical command without a legacy ID
was refused before the no-write barrier. The failing real-owner case is retained
in `/tmp/canonical-batch-cancel-red-runtime.log`. Validation and a new cancellation
receipt now choose the audit version from the supplied command; an existing
receipt preserves its original version. Two focused model controls cover
Legacy/V1/V2 identity and immutable prior receipts. All seven actual Restate
cancellation cases pass (`/tmp/canonical-batch-cancel-green-runtime3.log`), including
pre-initialization V2 and V1 stops, an initialized V2 budget-held delivery,
delayed create/recheck after that hold expires, canonical SQL/audit convergence,
uncertain/written preservation and old-operation replay after explicit reissue.
Focused workflow library/integration strict Clippy also passes
(`/tmp/canonical-batch-cancel-strict.log`). No new journal path or remote cutover
is introduced by this correction.
