# Audited Invitation Link And Installation Transitions Design

## Goal

Deepen Invitation Link and Installation lifecycle behavior so each transition owns both its state write and its audit-event construction.

This preserves current product behavior while localizing the event type, actor, target, metadata, and request id rules for each audited transition.

## Current State

`crates/restate-svc/src/invitation_link.rs` and `crates/restate-svc/src/installation.rs` already keep Restate SDK-facing handlers thin and delegate to async logic functions.

Those logic functions currently perform both storage writes and `crate::audit::emit` calls inline. The behavior is correct, but the transition boundary is implicit: callers and tests must read the whole function to see which state change and audit event belong together.

Existing tests cover the state outcome for create, revoke, expire, onboard, repository-selection changes, and uninstall. They do not consistently assert the emitted audit event shape.

## Chosen Approach

Keep the existing Restate modules and public handler input/output types. Add small transition-level functions inside the same module as the Restate service:

- Invitation Link transitions stay in `crates/restate-svc/src/invitation_link.rs`.
- Installation transitions stay in `crates/restate-svc/src/installation.rs`.
- Existing `*_logic` functions remain the Restate-handler seam and delegate to the new transition functions.

This gives the desired locality with the least churn. It avoids a new generic audit abstraction and avoids moving Restate-owned audit behavior into `domain` or `storage`.

## Detailed Design

Add transition functions that perform the state write and emit the corresponding audit event before returning:

- `create_invitation_link_transition` generates the slug and link id, inserts the invitation link, and emits `invitation_link.created`.
- `revoke_invitation_link_transition` marks the link revoked, handles the existing idempotent double-revoke behavior, reads the link for its account id, and emits `invitation_link.revoked` only when a state write occurred.
- `expire_invitation_link_transition` reads the link, preserves the existing no-state-mutation expiration model, and emits `invitation_link.expired` only when the link is expired and not revoked.
- `onboard_installation_transition` inserts the installation row, preserves duplicate-installation idempotency, and emits `installation.created` only for the first insert.
- `change_installation_repos_transition` updates selected repositories, reads the installation for its account id, and emits `installation.repos_changed`.
- `uninstall_installation_transition` marks the installation uninstalled, preserves missing/already-uninstalled idempotency, reads the installation for its account id, and emits `installation.uninstalled` only when a state write occurred.

The transition functions should own audit construction details directly:

- Event type comes from the transition name.
- Actor comes from the transition trigger: user, GitHub, or system.
- Target is the transitioned entity.
- Metadata is built in the transition from the input and resulting state.
- Request id is passed through from the Restate handler and forwarded unchanged to audit emission.

The existing `create_logic`, `revoke_logic`, `tick_expiration_logic`, `onboard_logic`, `repos_changed_logic`, and `uninstall_logic` functions should become thin delegates to these transitions. Existing callers and tests can keep using the logic functions unless direct transition tests are clearer.

## Error Handling And Idempotency

Preserve existing error semantics:

- Unknown invitation link on expiration remains terminal `NotFound`.
- Expiration without `expires_at` remains a terminal invariant error.
- Early expiration timer remains a no-op.
- Revoked invitation-link expiration remains a no-op.
- Double revoke remains idempotent and does not emit a second audit event.
- Duplicate `installation_id` on onboard remains idempotent and does not emit a second audit event.
- Duplicate active installation for a different installation id remains terminal.
- Repository-selection change for an unknown installation remains terminal.
- Unknown or already-uninstalled installation on uninstall remains idempotent and does not emit audit.

No new backward-compatibility layer is required because public payloads, storage rows, and external behavior remain unchanged.

## Testing

Extend Restate service tests to assert both persisted state and audit outcomes for the affected transitions.

For Invitation Link:

- Create inserts the link and emits one `invitation_link.created` event with user actor, invitation-link target, request id, and metadata for permission, approval requirement, max uses, and repository count.
- Revoke marks `revoked_at` and `revoked_by`, emits one `invitation_link.revoked` event with user actor and invitation-link target, and does not emit a second revoke event on double revoke.
- Expiration emits `invitation_link.expired` with system actor, invitation-link target, request id, and `expired_at` metadata while preserving the no-state-mutation expiration model.
- Revoked links and early timers do not emit expiration audit events.

For Installation:

- Onboard inserts the installation and emits `installation.created` with user actor, installation target, request id, and account metadata.
- Duplicate onboard by installation id returns successfully without a second audit event.
- Repository-selection change persists the selected repos and emits `installation.repos_changed` with GitHub actor, installation target, request id, and selected-repos kind metadata.
- Uninstall stamps `uninstalled_at` and emits `installation.uninstalled` with GitHub actor, installation target, request id, and uninstall timestamp metadata.
- Unknown or already-uninstalled uninstall paths do not emit audit events.

Use the existing in-memory `SqlxStorage` test fixture and `debug_list_audit` helper for audit assertions. If the fixture currently erases the concrete storage type behind `Arc<dyn Storage>`, adjust test support minimally so tests that need audit reads can keep access to the concrete `SqlxStorage` without changing production `AppState`.

Run:

```bash
cargo test -p restate-svc invitation_link installation
cargo fmt --check
```

## Non-Goals

This change does not modify database schemas, migrations, storage trait methods, Restate payload shapes, web routes, UI text, GitHub API behavior, invitation-link active-state derivation, or cascade revoke behavior.

This change also does not introduce a generic audit-event builder. The goal is transition-level locality for the current Invitation Link and Installation lifecycle slice, not a cross-workflow audit framework.
