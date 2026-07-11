# crates/ghinvite-workflows

Restate handler services for ghinvite. Five services own every durable state
change in the system: `Installation`, `InvitationLink`, `InvitationRequest`
(workflow), `GithubInvitation`, `Reconcile`. Built on the upstream
`restate-sdk = "0.10"` Rust SDK.

This crate is **library-only in Plan 3**: handler logic + unit tests, with
`SqlxStorage::in_memory` and `crates/ghinvite-github`'s `MockTransport` driving every
test. Plan 7 wraps this library in a Workers `#[event(fetch)]` entry point
backed by `D1Storage`, plus the `wrangler.toml` and Workers secrets.

## Local dev (current)

The crate ships no binary in Plan 3. Run unit tests:

```bash
cargo test -p ghinvite-workflows
```

Each handler module has tests in the same file:

- `crates/ghinvite-workflows/src/installation.rs::tests`
- `crates/ghinvite-workflows/src/invitation_link.rs::tests`
- `crates/ghinvite-workflows/src/github_invitation.rs::tests`
- `crates/ghinvite-workflows/src/invitation_request.rs::tests`
- `crates/ghinvite-workflows/src/reconcile.rs::tests`

The `InvitationRequest` workflow's awakeable + timer race is **not** unit-
tested in Plan 3 — that requires the Restate runtime. Plan 8 covers it via
integration tests against the local Restate server.

## Local dev (after Plan 7)

Restate's platform server runs in `compose.yaml` at the repo root:

```bash
docker compose up -d restate
```

This binds:
- `127.0.0.1:8080` — ingress (where you POST invocations).
- `127.0.0.1:9070` — admin (where deployments are registered).
- `127.0.0.1:9071` — internal.

Plan 7 will wire the Workers entry point. Until then, the crate is library-
only and registers nothing with the Restate platform.

## Architecture notes

Each handler delegates to a pure-async function (e.g. `onboard_logic`,
`create_logic`). The `#[restate_sdk::object|service|workflow]` impl wraps the
pure function inside `ctx.run(|| async {...}).name("step_name").await` so
Restate captures it as a durable step. Pure functions take `&AppState`
(`Arc<dyn Storage>` + `Arc<InstallationClient>`) and return
`Result<T, HandlerError>`.

`HandlerError::is_terminal()` distinguishes failures Restate should NOT retry
(constraint violations, 4xx) from transient ones (5xx, network) — the
`crate::error::to_sdk_handler_error` bridge maps the result to Restate's
`TerminalError` accordingly.

Audit events go through `audit::emit(state, account_id, event_type, actor,
target, metadata, request_id)`, which constructs an `AuditEvent` and calls
`Storage::audit`. Every state-change handler emits exactly one audit event.

See `docs/superpowers/plans/2026-05-04-ghinvite-restate-handlers.md` for the
implementation plan, and `docs/superpowers/specs/2026-05-04-ghinvite-v1-design.md`
§9 for the workflow specifications.
