# Invitation ownership and acceptance

The accepted target is [ADR 0009](adr/0009-unified-repository-delivery.md), delivered
by #117–#122. One entity has one mutable owner:

| Owner/key | Retained authority | Independent SQL writer |
|---|---|---|
| `AccountInstallation/<account>` | Current installation, availability, tombstones, continuations | `InstallationProjection` |
| `Installation/<installation>` | Account routing and uninstall tombstone | Routes to account owner |
| `InvitationLink/<link ULID>` | Creation, guardrails, metadata, revocation, uses, attempts/receipts, latest-request index | `InvitationProjection` |
| `InvitationRequest/<request ULID>` | Initialization, decisions, deadlines, eligibility verdicts, approved manifest, submission IDs | `RequestProjection` |
| `RepositoryDelivery/<request ULID>:<repository>` | Bound command/internal invitation ID, original create receipt, current lifecycle, terminal winner | `DeliveryProjection` |

Web allocates one link ULID before submission and retains exact input and expiration
anchor for recovery. Admission consumes a use and completes request initialization
before acknowledgement. Approval retains the immutable manifest before dispatch.
Repository sends progress independently. Submission acknowledgement is not GitHub
success. Current numeric installation/account/repository/requester identities are
verified before effects. Signed member evidence triggers conservative correlation
and read-only verification of the known upstream invitation before settlement.

## Retained SQL is more than a read model

- Link/request/delivery snapshots and Console lists are eventually consistent
  projections. Absence is not proof of failed or nonexistent acknowledged work.
- `delivery_attempts` is an input-bound external-write fence. Unavailable storage
  prevents new PUTs. Lost claim acknowledgement or uncertain PUT permits only
  read-only reconciliation, never another PUT on absence alone.
- `member_webhook_receipts` retains original match **or no-match**, including
  historical ambiguity. Redelivery cannot bind to a newer invitation.
- `audit_events` retains required immutable history; current snapshots alone
  cannot replace it.
- Installation/user records support routing, authorization and historical parents.
  Installation projection is not installation authority.
- `attempt_continuations` retain sealed browser input across unknown results;
  session-scoped expiry does not expire independent Restate business receipts.
  Native sessions use encrypted SQLite; Worker sessions use KV.

**Read-projection outage:** owners can accept/decide/deliver while snapshots lag,
provided prerequisites and the write fence are available. Projectors retry
independently. **Fence-store outage:** no new GitHub write is safe. Do not reset
fences or reinterpret unknown results as failure. Browser continuations and
webhook correlation also have independent SQL dependencies.

Recover original attempts via retained URLs and admin Attempt pages. Privately
call `InvitationRequest/<id>/recover` or `DeliveryRecovery/recover` with
`{link_id, request_id, requester_id}` to resubmit the original manifest. Inspect
`RepositoryDelivery/<request>:<repo>/status` for create and settlement together.
See [request authority](request-lifecycle-v1.md), [delivery recovery](delivery-recovery.md)
and [projection repair](admission-v1.md#inspection-repair-and-redrive).

## Retired protocols

Prior slices removed the code registry/separate sharing identity, link-owned
decisions/deadlines/dispatch, request Workflow `run`/terminal promise/consumption
protocol, split create/settlement writers, SQL approval gates, SQL authority
adoption, and owner-held projection retries. `build_endpoint` registers current
owners, projectors, observation and recovery services. No importer/alias routes
support discarded prelaunch state. Installation routing/tombstones, business
receipts, fences, identity checks, webhook correlation and historical audit remain.

#122 deletes the remaining link-envelope request field and its SQL request writer
and request-audit branch. Both adapters now accept request snapshots only through
the request projection contract; old combined envelopes are rejected. Fixture
seeding uses the two production writers independently. Request audit replay also
validates the relational event columns against retained content after restore.

#122 removes obsolete native/Worker test command rewrites, confines the browser form suite
to its own fixture, and names the web HTTP response fixture's limited proof surface.
This does not complete [#114](https://github.com/sagikazarmark/ghinvite2.orig/issues/114):
the fake retains an independent response model; shared fake/real ingress conformance
remains separate work. Assembled acceptance bypasses that fake.

## Reproducible evidence

```sh
bash scripts/test-restate.sh canonical_link
bash scripts/test-restate.sh request_owner
bash scripts/test-restate.sh authoritative_admission
bash scripts/test-restate.sh retained_delivery
bash scripts/test-restate.sh approved_delivery
bash scripts/test-restate.sh durable_projection
npm run test:journey --prefix tests/worker
npm run test:admission --prefix tests/worker
npm run test:delivery --prefix tests/worker
npm run test:storage --prefix tests/worker
```

`canonical_link` drives authenticated native HTTP, real Restate owners and the
production GitHub HTTP client: distinct admin/requester sessions, ULID sharing,
lost creation/admission/decision responses, delayed projection, signed member
settlement, status/audit convergence, completed-execution cleanup and original
replay/recovery without a second confirmed PUT. It also covers decline followed
by fresh admission, auto-approval and independent two-repository progress. Focused
runtime suites retain deadline, concurrency, uncertain-PUT, retention, installation
and projection fault matrices.

`test:journey` runs actual web/workflow Wasm in workerd, shared actual D1 and real
Restate, including signed settlement and binding-level request commit-ack loss.
`test:storage` runs the shared SQLx/D1 contract on real D1. CI retains per-crate
Wasm checks, native/island tests and production-feature packaging. HTML form tests
use controlled HTTP responses and do not prove Restate behavior. Local stubs do
not establish live GitHub webhook availability or Cloudflare regional behavior.
See [local testing](local-testing.md) and [Worker tests](../tests/worker/README.md).

### Local verification record — 2026-09-24

Passed: workspace tests (including native/island parity), workspace/test Clippy,
integration-feature Clippy, formatting, per-crate Wasm checks for UI, island, D1,
web Worker and workflow Worker; native `canonical_link`, `request_owner`,
`authoritative_admission`, `retained_delivery`, `approved_delivery`,
`durable_projection` and `installation_availability`; actual Worker journey,
admission/settlement/deadline matrix, approved delivery and all 18 shared D1 storage
scenarios. Browser checks passed: all 296 form cases, eight admission cases, admin
mutation recovery, and the four initially failing accessibility reflow cases after
wrapping the link-detail action row (the other 24 accessibility cases passed on
the original run). Standards/spec review findings were addressed and re-reviewed.

Local tools: Nix Rust 1.98.1, Node 22.23.2, Docker 29.7.2, Compose 5.4.0 and
wasm-bindgen 0.2.120. CI retains Rust 1.92.0; this local run is not evidence of that
compiler or a remote production rollout. Runtime fixtures used pinned Restate
1.7.9 and the checked-in workerd/Miniflare lockfile.

## Clean prelaunch replacement

This is coordinated replacement, not in-place retained-state migration.
Environment replacement is explicit maintainer action; tests destroy only their
own disposable fixtures.

1. Close old web/webhook/scheduled producers. Record old Restate deployments,
   registrations, Worker URLs, database/KV bindings and credentials. Preserve a
   coordinated backup of state, fences, routing receipts and required audit.
   Isolate old endpoints from ingress, database and GitHub writes.
2. Provision **new** Restate and **new** SQL/D1 state from current migrations,
   with fresh sessions/KV. Never pair old authority with empty fences or fresh
   owners with old projections. Retire old registrations with their environment;
   changing an endpoint URL alone does not remove registered services.
3. Native: choose a new Compose project (`docker compose -p ghinvite-fresh up -d
   restate`, with the documented ports free), new `GHINVITE_DATABASE_PATH` and
   matching web/workflow builds. Both binaries use the same new database path;
   startup applies migrations. Register only the current endpoint in fresh Restate.
   Follow [native setup](../README.md#option-a--native-binaries-recommended-for-development).
4. Worker: create fresh D1/session KV, bind both Workers to the same database and
   apply migrations. Local Wrangler migrations and both dev commands must use one
   explicitly selected fresh `--persist-to` directory. Remote setup uses selected
   account/environment bindings per [deployment](deploy.md). Configure new ingress
   credentials/endpoint identity key and register the matching immutable build.
   Build/check Wasm per crate, never workspace-wide.
5. Inspect discovery: `InvitationRequest` must be a Virtual Object; request methods
   are request-scoped; old code-registry, Workflow and split-delivery registrations
   are absent. Onboard via verified setup; SQL inserts are not onboarding.
6. Run restricted acceptance before opening producers. Never reactivate old writers
   as rollback; forward repair preserves coordinated authority and external effects.
   Production opening remains the maintainer's #61 decision.

Automatic scheduler operation remains #86. Neither #86 nor #61 is completed by
this verification or by fixture changes.
