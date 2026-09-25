# crates/ghinvite-workflows

Restate handler services for ghinvite. `build_endpoint` binds every service on
one endpoint, and these services own every durable state change:
`InvitationLink` and `InvitationRequest` own admission and the request
lifecycle, `RepositoryDelivery/<request>:<repository>` owns create and settlement,
and `AccountInstallation` owns installations.
`Reconcile` only `daily_run`.

SQL projections have four independent writers: `InvitationProjection` for links,
`RequestProjection` for requests, `DeliveryProjection` for repository deliveries,
and `InstallationProjection` for installations. Business owners durably send
projection work without waiting for SQL. Each writer's per-key queue applies
snapshots in send order; a failing projection holds back only that key's later
projections, not the business owner.

Built on the upstream `restate-sdk = "0.12"` Rust SDK (patched; see the
workspace `Cargo.toml`).

The library contains handler logic shared by the two entry points —
`ghinvite-workflows-server` (native: tokio + `SqlxStorage` + restate-sdk's
hyper server) and `ghinvite-workflows-worker` (Workers: `D1Storage`). Neither
runtime is named here, so this crate has no `cfg(target_arch)` wiring; tests
use `SqlxStorage` as a dev-dependency.

## Local dev (current)

Run unit tests without infrastructure:

```bash
cargo test -p ghinvite-workflows
```

Each handler module keeps its tests in the same file, in a `mod tests` at the
bottom; `grep -l 'mod tests' src/*.rs` lists them. They call the module's pure
functions directly, so none of them needs a Restate runtime.

The durable timer path requires the real Restate runtime. From the repository
root, run `bash scripts/test-restate.sh [target]` to register the current endpoint and
run that target (default `authoritative_admission`) against it. This feature-gated
test is excluded from normal workspace tests; when enabled it fails if the
runtime is absent. See [local testing](../../docs/local-testing.md) for the
host/container topology, prerequisites, deadlines, and cleanup.

## Local runtime

Restate's platform server runs in `compose.yaml` at the repo root:

```bash
docker compose up -d restate
```

This binds:
- `127.0.0.1:8080` — ingress (where you POST invocations).
- `127.0.0.1:9070` — admin (where deployments are registered).
- `127.0.0.1:9071` — internal.

See the root [README](../../README.md#running-locally-full-stack) for starting
the native endpoint and registering it with Restate.

## Architecture notes

A handler that owns a storage effect delegates it to a pure-async function
(e.g. `create_logic`, `project_installation`). The
`#[restate_sdk::object|service|workflow]` impl wraps that function inside
`ctx.run(|| async {...}).name("step_name").await` so Restate captures it as a
durable step. Pure functions take `&AppState` (`Arc<dyn WorkflowStorage>` +
`Arc<InstallationClient>`) and return a failure their caller can classify —
usually `Result<T, HandlerError>`. Unit tests call them directly, without a
Restate runtime.

Handlers that only route do not follow this shape: `Installation` works on its
`ObjectContext` directly (`ctx.get`/`ctx.set`, then a call to the account
object) and is covered by the integration tests rather than by unit tests.

`HandlerError::is_terminal()` distinguishes failures Restate should NOT retry
(constraint violations, 4xx) from transient ones (5xx, network) — the
`crate::error::to_sdk_handler_error` bridge maps the result to Restate's
`TerminalError` accordingly.

Every audit event has an identity fixed before it is written, so a retry
replays it rather than duplicating it. Which write carries it depends on what
changed:

- Invitation links (`admission`) attach `AuditIntent`s to the `ProjectionEnvelope` they send to `projection`,
  and `ProjectionStorage::apply_transition` inserts them in the same
  transaction as the records. A metadata update journals its event
  (`link/{id}/metadata/{revision}`) before sending it; unchanged metadata
  produces no event, and the event carries no description or internal-note
  values.
- Invitation requests (`request_owner`) send `RequestProjectionEnvelope` to
  `RequestProjection`; `ProjectionStorage::apply_request` atomically applies
  the request snapshot and its immutable `AuditIntent`s.
- GitHub invitation settlement (`settlement`) retains its `AuditEvent` in the
  delivery snapshot that `DeliveryProjection` applies; the event ID derives
  from the invitation ID, so an invitation settles with one event.
- Installation changes (`availability`) journal their `AuditEvent` first,
  project the installation, then call `AuditStorage::audit`, which accepts an
  identical replay.

See [the request lifecycle](../../docs/request-lifecycle-v1.md) and
[ADR 0003](../../docs/adr/0003-restate-authoritative-admission.md) for the
workflow contracts.

## Authoritative admission

The `admission` module and its native acceptance runner implement #53.
See [the command and recovery contract](../../docs/admission-v1.md).
