# Audited GitHub Invitation Transitions Design

## Goal

Deepen GitHub Invitation lifecycle behavior so each outcome owns both its state mutation and its audit-event construction.

This preserves current GitHub API behavior, retry semantics, and idempotency while localizing event type, actor, target, metadata, request id, and GitHub invitation id clearing or preservation rules for each transition.

## Current State

`crates/restate-svc/src/github_invitation.rs` already keeps Restate SDK-facing handlers thin and delegates to async logic functions for create, webhook settlement, cancellation, and expiration.

Those logic functions currently perform storage writes and `crate::audit::emit` calls inline. The behavior is correct, but the transition boundary is implicit: callers and tests must read the whole function to understand which state write and audit event belong together.

`crates/restate-svc/src/reconcile.rs` has the same pattern for reconciled accepted and cancelled outcomes after it checks GitHub's current state.

Recent Invitation Link and Installation work established a local transition-function pattern in `invitation_link.rs` and `installation.rs`: logic functions remain the handler seam, while private transition functions perform one state write and emit the corresponding audit event.

## Chosen Approach

Use private transition functions in the existing Restate modules.

- GitHub Invitation create, webhook, cancel, and expire transitions stay in `crates/restate-svc/src/github_invitation.rs`.
- Reconciled GitHub Invitation transitions stay in `crates/restate-svc/src/reconcile.rs` because reconciliation owns the GitHub probing that chooses accepted versus cancelled.
- Existing public handler input and output types remain unchanged.
- Existing `*_logic` functions remain the testable handler seam and delegate only the storage-plus-audit outcome to transition helpers.

This mirrors the current Invitation Link and Installation pattern with the least churn. It avoids a generic audited-transition abstraction because GitHub Invitation outcomes have different GitHub API, idempotency, and metadata rules.

## Detailed Design

Add transition helpers for the create outcomes in `github_invitation.rs`:

- `mark_invitation_sent_transition` updates the row to `Sent`, preserves the upstream `github_invitation_id`, clears any error, and emits `invitation.sent` with system actor, GitHub Invitation target, request id, and metadata containing the upstream GitHub invitation id, repository full name, and recipient login.
- `mark_invitation_already_accepted_transition` updates the row to `Accepted`, clears `github_invitation_id`, clears any error, and emits `invitation.accepted` with GitHub actor, GitHub Invitation target, request id, and metadata containing `reason: "already_collaborator"`, repository full name, and recipient login.
- `mark_invitation_send_failed_transition` updates the row to `Failed`, clears `github_invitation_id`, stores the terminal error message, and emits `invitation.send_failed` with system actor, GitHub Invitation target, request id, and metadata containing the error, repository full name, and recipient login.

Add transition helpers for webhook settlement in `github_invitation.rs`:

- `accept_invitation_from_webhook_transition` updates the row to `Accepted`, clears `github_invitation_id`, clears any error, and emits `invitation.accepted` with GitHub actor, GitHub Invitation target, request id, and metadata containing `action: "accepted"`.
- `decline_invitation_from_webhook_transition` updates the row to `Declined`, clears `github_invitation_id`, clears any error, and emits `invitation.declined` with GitHub actor, GitHub Invitation target, request id, and metadata containing `action: "declined"`.

Add transition helpers for cancellation and expiration in `github_invitation.rs`:

- `cancel_invitation_transition` updates the row to `Cancelled`, clears `github_invitation_id`, clears any error, and emits `invitation.cancelled` with user actor when `by_user` is present or system actor otherwise, GitHub Invitation target, request id, and metadata containing `by_user`.
- `expire_invitation_transition` updates the row to `Expired`, clears `github_invitation_id`, clears any error, and emits `invitation.expired` with system actor, GitHub Invitation target, request id, and metadata containing the existing `reason: "tick_expire_no_longer_pending"` value.

Add reconciliation transition helpers in `reconcile.rs`:

- `accept_reconciled_invitation_transition` updates the row to `Accepted`, clears `github_invitation_id`, clears any error, and emits `invitation.accepted` with system actor, GitHub Invitation target, request id, and metadata containing `reconciled: true`.
- `cancel_reconciled_invitation_transition` updates the row to `Cancelled`, clears `github_invitation_id`, clears any error, and emits `invitation.cancelled` with system actor, GitHub Invitation target, request id, and metadata containing `reconciled: true`.

The logic functions keep responsibility for lookup, GitHub API calls, idempotency checks, and branching:

- `create_logic` still inserts a `Sending` row idempotently, parses `RepositoryIdentity`, calls `add_collaborator`, loads the installation account, delegates the create outcome to a transition helper, and preserves 5xx retry versus terminal 4xx behavior.
- `on_webhook_logic` still no-ops when the row is already terminal and uses Invitation Context loading for account resolution before delegating the webhook outcome to a transition helper.
- `cancel_logic` still no-ops when the row is terminal, loads full Invitation Context, calls `delete_invitation` only when an upstream id exists, tolerates GitHub 404, retries non-terminal errors, and proceeds on terminal 4xx responses.
- `tick_expire_logic` still no-ops when the row is terminal, lists pending GitHub invitations, leaves the row unchanged when GitHub no longer lists it, and only expires when the known upstream id is still pending.
- `reconcile_single` still lists pending GitHub invitations, no-ops when the upstream id is still pending, checks collaborator status otherwise, and chooses reconciled accepted or cancelled.

## Error Handling And Idempotency

Preserve existing semantics:

- A duplicate create insert reads the existing row and returns `Ok(())` if it is already terminal.
- Create 5xx GitHub errors remain transient and leave the row in `Sending` for retry.
- Create terminal 4xx GitHub errors mark the row `Failed`, emit `invitation.send_failed`, and return `Ok(())` so Restate does not retry.
- Webhook events on terminal rows remain no-ops and do not emit duplicate audit events.
- Cancellation of terminal rows remains a no-op.
- Cancellation with missing upstream id skips the GitHub delete and still marks the row cancelled.
- Cancellation tolerates GitHub 404 and proceeds to mark the row cancelled.
- Expiration leaves the row unchanged when GitHub no longer lists the upstream invitation.
- Reconciliation terminal per-row errors are logged and skipped by the daily sweep; transient errors still fail the whole sweep for retry.

Transition helpers should assume the caller already decided the transition is valid. They should not repeat idempotency guards that belong to the workflow branch decision.

Transition helpers receive the account id from the existing context-loading path rather than performing additional account lookups. This keeps account classification in `invitation_context.rs` and keeps transition helpers focused on state writes plus audit construction.

## Testing

Extend Restate service tests to assert both persisted state and audit outcomes for each transition-level outcome.

Create outcome tests should assert:

- `Sent` stores the upstream GitHub invitation id, clears errors, and emits one `invitation.sent` event with system actor, GitHub Invitation target, request id, and expected metadata.
- `Accepted` from already-collaborator clears the upstream id, clears errors, and emits `invitation.accepted` with GitHub actor and `reason: "already_collaborator"` metadata.
- `Failed` clears the upstream id, stores the terminal error message, and emits `invitation.send_failed` with system actor and error metadata.
- 5xx retry behavior still leaves the row in `Sending` and emits no terminal outcome event.

Webhook tests should assert:

- Accepted and declined webhooks transition state, clear the upstream id, and emit the corresponding audit event with GitHub actor and action metadata.
- A terminal row webhook remains a no-op and emits no second audit event.

Cancellation and expiration tests should assert:

- Cancellation marks the row `Cancelled`, clears the upstream id, emits `invitation.cancelled`, and uses user or system actor based on `by_user`.
- GitHub 404 cancellation still produces the same cancelled transition.
- Expiration marks the row `Expired`, clears the upstream id, and emits `invitation.expired` only when the upstream invitation is still pending.
- Expiration when GitHub no longer lists the invitation leaves state and audit unchanged.

Reconciliation tests should assert:

- Still-pending invitations remain unchanged and emit no audit event.
- Collaborator reconciliation marks `Accepted`, clears the upstream id, and emits `invitation.accepted` with system actor and `reconciled: true` metadata.
- Non-collaborator reconciliation marks `Cancelled`, clears the upstream id, and emits `invitation.cancelled` with system actor and `reconciled: true` metadata.

Use the existing in-memory `SqlxStorage` fixture and `debug_list_audit` helper for audit assertions, following the Invitation Link and Installation transition tests.

Run focused tests first:

```bash
cargo test -p restate-svc github_invitation reconcile
cargo fmt --check
```

Before completion, run the wider relevant suite:

```bash
cargo test -p restate-svc
```

## Non-Goals

This change does not modify database schemas, storage trait methods, domain structs, Restate payload shapes, GitHub client behavior, web routes, UI behavior, audit event type names, or existing GitHub URL path encoding.

This change also does not introduce a generic audit-event builder or cross-workflow transition framework. The scope is transition-level locality for GitHub Invitation lifecycle outcomes only.
