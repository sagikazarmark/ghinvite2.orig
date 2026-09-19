# crates/ghinvite-workflows

Restate handler services for ghinvite. Which services own durable state depends
on the endpoint mode (`GHINVITE_ADMISSION_MODE`):

- **`legacy` (the default)** — `build_endpoint` binds `InvitationLink`,
  `InvitationRequest`, `GithubInvitation` and `Reconcile` as their real
  implementations, and they still own their state. Installations are the
  exception: they cut over ahead of the rest, so `AccountInstallationV1` and
  `InstallationProjectionV1` own them in both modes.
- **`authoritative`** — `build_cutover_endpoint` gives the `_v1` services
  ownership of every durable state change: `InvitationRequestV1` and the request
  lifecycle, `GithubCreateV1` and settlement, and the projection service they all
  write through. The four writers above stay bound only for deployments pinned
  before the cutover.

Built on the upstream `restate-sdk = "0.12"` Rust SDK (patched; see the
workspace `Cargo.toml`).

The library contains handler logic shared by the native binary and the
`ghinvite-workflows-worker` entry point. Native tests use `SqlxStorage`; the
Worker uses `D1Storage`.

## Local dev (current)

Run unit tests without infrastructure:

```bash
cargo test -p ghinvite-workflows
```

Each handler module keeps its tests in the same file, in a `mod tests` at the
bottom; `grep -l 'mod tests' src/*.rs` lists them. They call the module's pure
functions directly, so none of them needs a Restate runtime.

The durable timer path requires the real Restate runtime. From the repository
root, run `bash scripts/test-restate.sh` to register the current endpoint and
verify request expiration and persisted audit outcomes. This feature-gated
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
durable step. Pure functions take `&AppState` (`Arc<dyn Storage>` +
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

Audit events go through `audit::emit(state, account_id, event_type, actor,
target, metadata, request_id)`, which constructs an `AuditEvent` and calls
`Storage::audit`. Every state-change handler emits exactly one audit event.

Metadata updates prepare and journal their changed field names, audit ID, and
timestamp before applying the write. They construct the event from that snapshot
and call `Storage::audit` directly so retries reuse the same identity. Storage
deduplicates this event type by ID; unchanged metadata produces no event. Audit
metadata includes field names only, never description or internal-note values.

See `docs/superpowers/plans/2026-05-04-ghinvite-restate-handlers.md` for the
implementation plan, and `docs/superpowers/specs/2026-05-04-ghinvite-v1-design.md`
§9 for the workflow specifications.

## Isolated authoritative admission

The opt-in `admission_v1` module and its native acceptance runner implement #53.
See [the command and recovery contract](../../docs/admission-v1.md).
