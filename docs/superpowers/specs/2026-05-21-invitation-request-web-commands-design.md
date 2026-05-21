# Invitation Request Web Commands Design

## Context

Issue #10 continues the web command facade work for Invitation Request flows. The existing facade already routes recipient request submission and Account Admin approve or decline actions through `GhinviteCommands`, but route callers still construct serialization-shaped command payloads and log Restate-style target names.

The completed Share Link command deepening in commit `3e70c24` established the local pattern: Restate service names, method names, object keys, send-versus-call choices, and payload serialization belong inside `crates/web/src/commands.rs`; routes should speak in ghinvite domain commands.

## Goals

- Recipient request submission uses a stable Invitation Request command interface.
- Account Admin approve and decline actions share one decision command interface.
- Routes no longer know Invitation Request Restate service names, method names, workflow keys, send-versus-call choices, or serialized enum shape.
- Existing duplicate-pending redirects, pending-page redirects, approve behavior, and decline behavior are preserved.
- Command tests verify Restate adapter behavior with fake or mock adapters, while route tests remain focused on HTTP, session, authorization, and view outcomes.

## Non-Goals

- No product behavior changes to recipient submission, pending pages, approval, or decline.
- No Restate service API changes in `crates/restate-svc`.
- No broad command facade redesign beyond Invitation Request commands.
- No decline reason UI work; the current route still sends no reason.

## Approach

Use a focused API-refinement pass in `crates/web/src/commands.rs`.

Keep the existing route-facing trait methods:

- `submit_invitation_request(SubmitInvitationRequest)`
- `decide_invitation_request(DecideInvitationRequest)`

Add route-facing constructors so routes build domain commands instead of serialization-shaped payloads:

- `SubmitInvitationRequest::new(request_id, share_link_id, requester_id, justification, created_at)`
- `DecideInvitationRequest::approve(request_id, decided_by, decided_at)`
- `DecideInvitationRequest::decline(request_id, decided_by, decided_at, reason)`

Move Restate payload concerns behind the command adapter:

- Add Invitation Request constants for service and method names.
- Add an Invitation Request workflow key helper.
- Serialize a private submit payload from `SubmitInvitationRequest` when sending `InvitationRequest/{request_id}/submit`.
- Serialize a private decision payload enum when calling `InvitationRequest/{request_id}/decide`.
- Keep `submit` as `send` and `decide` as `call`, preserving current behavior.

## Component Changes

### `crates/web/src/commands.rs`

The command module becomes the only web crate location that knows the Invitation Request Restate target details.

`SubmitInvitationRequest` remains a public, route-facing command type with public fields for lightweight fake-command tests. It should not derive `Serialize`; `RestateCommands` converts it to a private serializable submit payload. Routes should use `SubmitInvitationRequest::new` rather than a struct literal.

`DecideInvitationRequest` remains public with a public `request_id` and a private route-facing decision value. `InvitationRequestDecision` should become private to the command module. `RestateCommands` converts that private value to a private serializable Restate decision payload. Routes should use the approve and decline constructors instead of selecting serialized enum variants directly.

The existing generic `RestateCommandAdapter` is reused for fake-adapter tests, matching the Share Link command pattern.

### `crates/web/src/routes/invitation.rs`

The submit route keeps current responsibilities: session loading, auth redirect, share-link resolution, duplicate-pending redirect, justification trimming, request id parsing, command dispatch, flash on failure, and pending redirect on success.

The route changes only its command construction and warning log message:

- Construct `SubmitInvitationRequest` via `SubmitInvitationRequest::new`.
- Log `submit invitation request command failed` on command failure.

### `crates/web/src/routes/dashboard.rs`

The Account Admin routes keep current responsibilities: admin authorization, request ownership lookup, command dispatch, flash messages, and redirect back to the request queue.

The routes change only their command construction and warning log messages:

- Construct approvals via `DecideInvitationRequest::approve`.
- Construct declines via `DecideInvitationRequest::decline`.
- Log `approve invitation request command failed` and `decline invitation request command failed`.

## Data Flow

Recipient submission:

1. `POST /i/{slug}/request` resolves an active share link and checks pending duplicate state.
2. The route constructs `SubmitInvitationRequest` with the domain request id, share link id, requester id, justification, and timestamp.
3. `RestateCommands::submit_invitation_request` converts it to a private Restate submit payload.
4. The adapter sends `InvitationRequest/{request_id}/submit`.
5. The route redirects to `/i/{slug}/pending/{request_id}`.

Account Admin decision:

1. `POST /accounts/{login}/requests/{request_id}/approve` or `/decline` validates Account Admin ownership of the request.
2. The route constructs a `DecideInvitationRequest` through the approve or decline constructor.
3. `RestateCommands::decide_invitation_request` converts it to a private Restate decision payload.
4. The adapter calls `InvitationRequest/{request_id}/decide`.
5. The route sets a success or error flash and redirects to the request queue.

## Error Handling

Route error behavior is unchanged.

- Recipient submission command failure sets the existing error flash and redirects back to the request form.
- Approval failure sets the existing approval error flash and redirects to the request queue.
- Decline failure sets the existing decline error flash and redirects to the request queue.
- Invalid ids and failed Account Admin request lookup keep their current responses.

Command adapter errors continue to propagate as `web::Result` values. The command module does not reinterpret Restate failures.

## Testing

Command tests in `crates/web/src/commands.rs` verify Invitation Request mapping through the fake `RestateCommandAdapter`:

- `submit_invitation_request` sends service `InvitationRequest`, key `{request_id}`, method `submit`, and the expected submit payload.
- `decide_invitation_request` for approve calls service `InvitationRequest`, key `{request_id}`, method `decide`, and the expected private approve payload.
- `decide_invitation_request` for decline calls the same service, key, and method with the expected private decline payload.

Route tests stay focused on visible behavior:

- Existing duplicate-pending request tests continue to assert redirects and no command dispatch.
- Existing submission tests continue to assert command intent without raw Restate payloads.
- Existing dashboard route tests remain focused on authentication and route responses unless there is already coverage for approve or decline flows.

Verification commands:

- `cargo fmt --all --check`
- `cargo test -p web commands::tests`
- `cargo test -p web`

## Risks

The main risk is accidentally changing the serialized decision shape expected by `crates/restate-svc/src/invitation_request.rs`, which currently deserializes `Decision::Approve` and `Decision::Decline` with Rust's default enum representation. Command tests should assert that shape directly.

Another risk is over-hiding fields and breaking lightweight route tests that inspect fake command values. The implementation should prefer route constructor usage while keeping public command inspection simple where tests already rely on it.
