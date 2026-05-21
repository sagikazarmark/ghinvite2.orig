# Audited GitHub Invitation Transitions Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Refactor GitHub Invitation lifecycle outcomes so transition helpers own state writes and audit-event construction while preserving existing GitHub workflow behavior.

**Architecture:** Keep Restate handler logic functions as orchestration seams for lookups, GitHub API calls, branching, and idempotency. Add private transition helpers in `github_invitation.rs` and `reconcile.rs` that each perform one storage update and emit one audit event.

**Tech Stack:** Rust, Tokio tests, Restate SDK handlers, `storage::SqlxStorage` in-memory fixtures, GitHub mock transport, `audit` event enums.

---

## File Structure

- Modify `crates/restate-svc/src/github_invitation.rs`: add transition helper tests, add private transition helpers, and replace inline update-plus-audit blocks in `create_logic`, `on_webhook_logic`, `cancel_logic`, and `tick_expire_logic`.
- Modify `crates/restate-svc/src/reconcile.rs`: add transition helper tests, add private reconciliation transition helpers, and replace inline update-plus-audit in `reconcile_single`.
- Do not create new production modules. The current codebase already keeps lifecycle transition behavior beside the Restate workflow module that chooses the transition.
- Do not modify storage traits, database migrations, domain structs, Restate payloads, GitHub client behavior, web routes, UI code, or audit event enum names.

## Task 1: Add GitHub Invitation Create Transition Tests

**Files:**
- Modify: `crates/restate-svc/src/github_invitation.rs`
- Test: `crates/restate-svc/src/github_invitation.rs`

- [ ] **Step 1: Extend test imports**

In `crates/restate-svc/src/github_invitation.rs`, inside `#[cfg(test)] mod tests`, replace the current `test_support` import with this version and add the audit enum import:

```rust
use crate::test_support::{dt, fixture_github_client, fixture_state_with_storage, fixture_storage};
use ::audit::{ActorKind, EventType, TargetKind};
```

- [ ] **Step 2: Add audit and row helpers for transition tests**

In the same test module, add these helpers after `sample_input`:

```rust
async fn audit_events(
    storage: &::storage::SqlxStorage,
    account_id: u64,
) -> Vec<::audit::AuditEvent> {
    storage.debug_list_audit(account_id).await.unwrap()
}

async fn seed_sending_invitation(state: &AppState, input: &CreateInvitationInput) {
    state
        .storage
        .insert_github_invitation(&DomainGithubInvitation {
            id: input.invitation_id,
            invitation_request_id: input.invitation_request_id,
            repo_id: input.repo_id,
            github_invitation_id: None,
            state: InvitationState::Sending,
            error_message: None,
            created_at: input.now,
            updated_at: input.now,
        })
        .await
        .unwrap();
}
```

- [ ] **Step 3: Add failing tests for create transition helpers**

Add these tests after `create_502_propagates_transient_for_retry`:

```rust
#[tokio::test]
async fn sent_transition_updates_row_and_audits() {
    let (state, storage) = fixture_state_with_storage().await;
    let req_id = seed_chain(&state).await;
    let inv_id = GithubInvitationId::new();
    let input = sample_input(inv_id, req_id);
    seed_sending_invitation(&state, &input).await;

    mark_invitation_sent_transition(&state, &input, 100, 9988, Some("req-sent".into()))
        .await
        .unwrap();

    let row = state
        .storage
        .get_github_invitation(inv_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.state, InvitationState::Sent);
    assert_eq!(row.github_invitation_id, Some(9988));
    assert_eq!(row.error_message, None);

    let audits = audit_events(&storage, 100).await;
    assert_eq!(audits.len(), 1);
    let event = &audits[0];
    assert_eq!(event.event_type, EventType::InvitationSent);
    assert_eq!(event.actor_kind, ActorKind::System);
    assert_eq!(event.actor_id, None);
    assert_eq!(event.target_kind, TargetKind::GithubInvitation);
    assert_eq!(event.target_id, inv_id.to_string());
    assert_eq!(event.request_id.as_deref(), Some("req-sent"));
    assert_eq!(
        event.metadata.get("github_invitation_id"),
        Some(&serde_json::json!(9988))
    );
    assert_eq!(
        event.metadata.get("repo_full_name"),
        Some(&serde_json::json!("acme/api"))
    );
    assert_eq!(
        event.metadata.get("recipient"),
        Some(&serde_json::json!("alice"))
    );
}

#[tokio::test]
async fn already_accepted_transition_updates_row_and_audits() {
    let (state, storage) = fixture_state_with_storage().await;
    let req_id = seed_chain(&state).await;
    let inv_id = GithubInvitationId::new();
    let input = sample_input(inv_id, req_id);
    seed_sending_invitation(&state, &input).await;

    mark_invitation_already_accepted_transition(
        &state,
        &input,
        100,
        Some("req-already".into()),
    )
    .await
    .unwrap();

    let row = state
        .storage
        .get_github_invitation(inv_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.state, InvitationState::Accepted);
    assert_eq!(row.github_invitation_id, None);
    assert_eq!(row.error_message, None);

    let audits = audit_events(&storage, 100).await;
    assert_eq!(audits.len(), 1);
    let event = &audits[0];
    assert_eq!(event.event_type, EventType::InvitationAccepted);
    assert_eq!(event.actor_kind, ActorKind::Github);
    assert_eq!(event.actor_id, None);
    assert_eq!(event.target_kind, TargetKind::GithubInvitation);
    assert_eq!(event.target_id, inv_id.to_string());
    assert_eq!(event.request_id.as_deref(), Some("req-already"));
    assert_eq!(
        event.metadata.get("reason"),
        Some(&serde_json::json!("already_collaborator"))
    );
    assert_eq!(
        event.metadata.get("repo_full_name"),
        Some(&serde_json::json!("acme/api"))
    );
    assert_eq!(
        event.metadata.get("recipient"),
        Some(&serde_json::json!("alice"))
    );
}

#[tokio::test]
async fn send_failed_transition_updates_row_and_audits() {
    let (state, storage) = fixture_state_with_storage().await;
    let req_id = seed_chain(&state).await;
    let inv_id = GithubInvitationId::new();
    let input = sample_input(inv_id, req_id);
    seed_sending_invitation(&state, &input).await;

    mark_invitation_send_failed_transition(
        &state,
        &input,
        100,
        "terminal failure".into(),
        Some("req-failed".into()),
    )
    .await
    .unwrap();

    let row = state
        .storage
        .get_github_invitation(inv_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.state, InvitationState::Failed);
    assert_eq!(row.github_invitation_id, None);
    assert_eq!(row.error_message.as_deref(), Some("terminal failure"));

    let audits = audit_events(&storage, 100).await;
    assert_eq!(audits.len(), 1);
    let event = &audits[0];
    assert_eq!(event.event_type, EventType::InvitationSendFailed);
    assert_eq!(event.actor_kind, ActorKind::System);
    assert_eq!(event.actor_id, None);
    assert_eq!(event.target_kind, TargetKind::GithubInvitation);
    assert_eq!(event.target_id, inv_id.to_string());
    assert_eq!(event.request_id.as_deref(), Some("req-failed"));
    assert_eq!(
        event.metadata.get("error"),
        Some(&serde_json::json!("terminal failure"))
    );
    assert_eq!(
        event.metadata.get("repo_full_name"),
        Some(&serde_json::json!("acme/api"))
    );
    assert_eq!(
        event.metadata.get("recipient"),
        Some(&serde_json::json!("alice"))
    );
}
```

- [ ] **Step 4: Run tests to verify the transition helpers do not exist yet**

Run: `cargo test -p restate-svc github_invitation::tests::sent_transition_updates_row_and_audits github_invitation::tests::already_accepted_transition_updates_row_and_audits github_invitation::tests::send_failed_transition_updates_row_and_audits`

Expected: FAIL with compile errors naming `mark_invitation_sent_transition`, `mark_invitation_already_accepted_transition`, and `mark_invitation_send_failed_transition`.

- [ ] **Step 5: Keep the failing create tests in the working tree**

Do not commit while the test target is failing. Leave the test changes unstaged for Task 2, where the implementation will make them pass and both tests plus implementation will be committed together.

## Task 2: Implement GitHub Invitation Create Transitions

**Files:**
- Modify: `crates/restate-svc/src/github_invitation.rs`
- Test: `crates/restate-svc/src/github_invitation.rs`

- [ ] **Step 1: Add create transition helpers**

Add these private functions after `create_logic` and before `on_webhook_logic`:

```rust
async fn mark_invitation_sent_transition(
    state: &AppState,
    input: &CreateInvitationInput,
    account_id: u64,
    github_invitation_id: u64,
    request_id: Option<String>,
) -> crate::error::Result<()> {
    state
        .storage
        .update_github_invitation(&storage::GithubInvitationUpdate {
            id: input.invitation_id,
            state: InvitationState::Sent,
            github_invitation_id: Some(github_invitation_id),
            error_message: None,
            updated_at: input.now,
        })
        .await?;

    crate::audit::emit(
        state,
        account_id,
        EventType::InvitationSent,
        Actor::System,
        Target::github_invitation(input.invitation_id),
        serde_json::json!({
            "github_invitation_id": github_invitation_id,
            "repo_full_name": input.repo_full_name,
            "recipient": input.recipient_login,
        }),
        request_id,
    )
    .await
}

async fn mark_invitation_already_accepted_transition(
    state: &AppState,
    input: &CreateInvitationInput,
    account_id: u64,
    request_id: Option<String>,
) -> crate::error::Result<()> {
    state
        .storage
        .update_github_invitation(&storage::GithubInvitationUpdate {
            id: input.invitation_id,
            state: InvitationState::Accepted,
            github_invitation_id: None,
            error_message: None,
            updated_at: input.now,
        })
        .await?;

    crate::audit::emit(
        state,
        account_id,
        EventType::InvitationAccepted,
        Actor::Github,
        Target::github_invitation(input.invitation_id),
        serde_json::json!({
            "reason": "already_collaborator",
            "repo_full_name": input.repo_full_name,
            "recipient": input.recipient_login,
        }),
        request_id,
    )
    .await
}

async fn mark_invitation_send_failed_transition(
    state: &AppState,
    input: &CreateInvitationInput,
    account_id: u64,
    error_message: String,
    request_id: Option<String>,
) -> crate::error::Result<()> {
    state
        .storage
        .update_github_invitation(&storage::GithubInvitationUpdate {
            id: input.invitation_id,
            state: InvitationState::Failed,
            github_invitation_id: None,
            error_message: Some(error_message.clone()),
            updated_at: input.now,
        })
        .await?;

    crate::audit::emit(
        state,
        account_id,
        EventType::InvitationSendFailed,
        Actor::System,
        Target::github_invitation(input.invitation_id),
        serde_json::json!({
            "error": error_message,
            "repo_full_name": input.repo_full_name,
            "recipient": input.recipient_login,
        }),
        request_id,
    )
    .await
}
```

- [ ] **Step 2: Delegate create outcomes to the transition helpers**

In `create_logic`, replace the entire `match outcome { ... }` body with this version:

```rust
match outcome {
    Ok(Some(github_id)) => {
        // 201 - invitation pending.
        mark_invitation_sent_transition(state, input, account_id, github_id, request_id).await?;
    }
    Ok(None) => {
        // 204 - already a collaborator.
        mark_invitation_already_accepted_transition(state, input, account_id, request_id).await?;
    }
    Err(e) => {
        let h: crate::error::HandlerError = e.into();
        if !h.is_terminal() {
            // Transient - let Restate retry.
            return Err(h);
        }
        // Terminal 4xx - mark failed, audit, return Ok so Restate does not retry.
        mark_invitation_send_failed_transition(
            state,
            input,
            account_id,
            h.to_string(),
            request_id,
        )
        .await?;
    }
}
```

- [ ] **Step 3: Run the focused create transition tests**

Run: `cargo test -p restate-svc github_invitation::tests::sent_transition_updates_row_and_audits github_invitation::tests::already_accepted_transition_updates_row_and_audits github_invitation::tests::send_failed_transition_updates_row_and_audits`

Expected: PASS.

- [ ] **Step 4: Run existing create behavior tests**

Run: `cargo test -p restate-svc github_invitation::tests::create_201_marks_sent_and_audits github_invitation::tests::create_204_marks_accepted github_invitation::tests::create_422_marks_failed_and_returns_ok github_invitation::tests::create_502_propagates_transient_for_retry`

Expected: PASS.

- [ ] **Step 5: Commit create transition tests and implementation**

Run:

```bash
git add crates/restate-svc/src/github_invitation.rs
git commit -m "refactor(restate): deepen github invitation create transitions"
```

## Task 3: Add Webhook, Cancel, And Expire Transition Tests

**Files:**
- Modify: `crates/restate-svc/src/github_invitation.rs`
- Test: `crates/restate-svc/src/github_invitation.rs`

- [ ] **Step 1: Add direct transition tests for webhook, cancel, and expire**

Add these tests after `webhook_unknown_invitation_is_terminal`:

```rust
#[tokio::test]
async fn webhook_accept_transition_updates_row_and_audits() {
    let (state, storage) = fixture_state_with_storage().await;
    let req_id = seed_chain(&state).await;
    let inv_id = seed_to_sent(&state, req_id).await;

    accept_invitation_from_webhook_transition(
        &state,
        inv_id,
        100,
        dt("2026-05-04T14:00:00Z"),
        Some("req-webhook-accepted".into()),
    )
    .await
    .unwrap();

    let row = state
        .storage
        .get_github_invitation(inv_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.state, InvitationState::Accepted);
    assert_eq!(row.github_invitation_id, None);
    assert_eq!(row.error_message, None);

    let audits = audit_events(&storage, 100).await;
    assert_eq!(audits.len(), 1);
    let event = &audits[0];
    assert_eq!(event.event_type, EventType::InvitationAccepted);
    assert_eq!(event.actor_kind, ActorKind::Github);
    assert_eq!(event.actor_id, None);
    assert_eq!(event.target_kind, TargetKind::GithubInvitation);
    assert_eq!(event.target_id, inv_id.to_string());
    assert_eq!(event.request_id.as_deref(), Some("req-webhook-accepted"));
    assert_eq!(
        event.metadata.get("action"),
        Some(&serde_json::json!("accepted"))
    );
}

#[tokio::test]
async fn webhook_decline_transition_updates_row_and_audits() {
    let (state, storage) = fixture_state_with_storage().await;
    let req_id = seed_chain(&state).await;
    let inv_id = seed_to_sent(&state, req_id).await;

    decline_invitation_from_webhook_transition(
        &state,
        inv_id,
        100,
        dt("2026-05-04T14:00:00Z"),
        Some("req-webhook-declined".into()),
    )
    .await
    .unwrap();

    let row = state
        .storage
        .get_github_invitation(inv_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.state, InvitationState::Declined);
    assert_eq!(row.github_invitation_id, None);
    assert_eq!(row.error_message, None);

    let audits = audit_events(&storage, 100).await;
    assert_eq!(audits.len(), 1);
    let event = &audits[0];
    assert_eq!(event.event_type, EventType::InvitationDeclined);
    assert_eq!(event.actor_kind, ActorKind::Github);
    assert_eq!(event.actor_id, None);
    assert_eq!(event.target_kind, TargetKind::GithubInvitation);
    assert_eq!(event.target_id, inv_id.to_string());
    assert_eq!(event.request_id.as_deref(), Some("req-webhook-declined"));
    assert_eq!(
        event.metadata.get("action"),
        Some(&serde_json::json!("declined"))
    );
}

#[tokio::test]
async fn cancel_transition_updates_row_and_audits_user_actor() {
    let (state, storage) = fixture_state_with_storage().await;
    let (_req_id, inv_id) = seed_chain_with_repos(&state).await;

    cancel_invitation_transition(
        &state,
        inv_id,
        100,
        Some(7),
        dt("2026-05-04T15:00:00Z"),
        Some("req-cancel-user".into()),
    )
    .await
    .unwrap();

    let row = state
        .storage
        .get_github_invitation(inv_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.state, InvitationState::Cancelled);
    assert_eq!(row.github_invitation_id, None);
    assert_eq!(row.error_message, None);

    let audits = audit_events(&storage, 100).await;
    assert_eq!(audits.len(), 1);
    let event = &audits[0];
    assert_eq!(event.event_type, EventType::InvitationCancelled);
    assert_eq!(event.actor_kind, ActorKind::User);
    assert_eq!(event.actor_id, Some(7));
    assert_eq!(event.target_kind, TargetKind::GithubInvitation);
    assert_eq!(event.target_id, inv_id.to_string());
    assert_eq!(event.request_id.as_deref(), Some("req-cancel-user"));
    assert_eq!(event.metadata.get("by_user"), Some(&serde_json::json!(7)));
}

#[tokio::test]
async fn cancel_transition_updates_row_and_audits_system_actor() {
    let (state, storage) = fixture_state_with_storage().await;
    let (_req_id, inv_id) = seed_chain_with_repos(&state).await;

    cancel_invitation_transition(
        &state,
        inv_id,
        100,
        None,
        dt("2026-05-04T15:00:00Z"),
        Some("req-cancel-system".into()),
    )
    .await
    .unwrap();

    let row = state
        .storage
        .get_github_invitation(inv_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.state, InvitationState::Cancelled);
    assert_eq!(row.github_invitation_id, None);
    assert_eq!(row.error_message, None);

    let audits = audit_events(&storage, 100).await;
    assert_eq!(audits.len(), 1);
    let event = &audits[0];
    assert_eq!(event.event_type, EventType::InvitationCancelled);
    assert_eq!(event.actor_kind, ActorKind::System);
    assert_eq!(event.actor_id, None);
    assert_eq!(event.target_kind, TargetKind::GithubInvitation);
    assert_eq!(event.target_id, inv_id.to_string());
    assert_eq!(event.request_id.as_deref(), Some("req-cancel-system"));
    assert_eq!(event.metadata.get("by_user"), Some(&serde_json::Value::Null));
}

#[tokio::test]
async fn expire_transition_updates_row_and_audits() {
    let (state, storage) = fixture_state_with_storage().await;
    let (_req_id, inv_id) = seed_chain_with_repos(&state).await;

    expire_invitation_transition(
        &state,
        inv_id,
        100,
        dt("2026-05-11T13:00:00Z"),
        Some("req-expire".into()),
    )
    .await
    .unwrap();

    let row = state
        .storage
        .get_github_invitation(inv_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.state, InvitationState::Expired);
    assert_eq!(row.github_invitation_id, None);
    assert_eq!(row.error_message, None);

    let audits = audit_events(&storage, 100).await;
    assert_eq!(audits.len(), 1);
    let event = &audits[0];
    assert_eq!(event.event_type, EventType::InvitationExpired);
    assert_eq!(event.actor_kind, ActorKind::System);
    assert_eq!(event.actor_id, None);
    assert_eq!(event.target_kind, TargetKind::GithubInvitation);
    assert_eq!(event.target_id, inv_id.to_string());
    assert_eq!(event.request_id.as_deref(), Some("req-expire"));
    assert_eq!(
        event.metadata.get("reason"),
        Some(&serde_json::json!("tick_expire_no_longer_pending"))
    );
}
```

- [ ] **Step 2: Run tests to verify the transition helpers do not exist yet**

Run: `cargo test -p restate-svc github_invitation::tests::webhook_accept_transition_updates_row_and_audits github_invitation::tests::webhook_decline_transition_updates_row_and_audits github_invitation::tests::cancel_transition_updates_row_and_audits_user_actor github_invitation::tests::cancel_transition_updates_row_and_audits_system_actor github_invitation::tests::expire_transition_updates_row_and_audits`

Expected: FAIL with compile errors naming `accept_invitation_from_webhook_transition`, `decline_invitation_from_webhook_transition`, `cancel_invitation_transition`, and `expire_invitation_transition`.

- [ ] **Step 3: Keep the failing settlement tests in the working tree**

Do not commit while the test target is failing. Leave the test changes unstaged for Task 4, where the implementation will make them pass and both tests plus implementation will be committed together.

## Task 4: Implement Webhook, Cancel, And Expire Transitions

**Files:**
- Modify: `crates/restate-svc/src/github_invitation.rs`
- Test: `crates/restate-svc/src/github_invitation.rs`

- [ ] **Step 1: Add webhook transition helpers**

Add these private functions before `cancel_logic`:

```rust
async fn accept_invitation_from_webhook_transition(
    state: &AppState,
    invitation_id: GithubInvitationId,
    account_id: u64,
    at: DateTime<Utc>,
    request_id: Option<String>,
) -> crate::error::Result<()> {
    state
        .storage
        .update_github_invitation(&storage::GithubInvitationUpdate {
            id: invitation_id,
            state: InvitationState::Accepted,
            github_invitation_id: None,
            error_message: None,
            updated_at: at,
        })
        .await?;

    crate::audit::emit(
        state,
        account_id,
        EventType::InvitationAccepted,
        Actor::Github,
        Target::github_invitation(invitation_id),
        serde_json::json!({"action": "accepted"}),
        request_id,
    )
    .await
}

async fn decline_invitation_from_webhook_transition(
    state: &AppState,
    invitation_id: GithubInvitationId,
    account_id: u64,
    at: DateTime<Utc>,
    request_id: Option<String>,
) -> crate::error::Result<()> {
    state
        .storage
        .update_github_invitation(&storage::GithubInvitationUpdate {
            id: invitation_id,
            state: InvitationState::Declined,
            github_invitation_id: None,
            error_message: None,
            updated_at: at,
        })
        .await?;

    crate::audit::emit(
        state,
        account_id,
        EventType::InvitationDeclined,
        Actor::Github,
        Target::github_invitation(invitation_id),
        serde_json::json!({"action": "declined"}),
        request_id,
    )
    .await
}
```

- [ ] **Step 2: Delegate webhook outcomes to transition helpers**

In `on_webhook_logic`, replace everything after the terminal-state guard with this code:

```rust
let context = crate::invitation_context::load_github_invitation_account_context(
    state,
    input.invitation_id,
)
.await?;
debug_assert_eq!(context.invitation.id, input.invitation_id);
debug_assert_eq!(context.request.id, row.invitation_request_id);
debug_assert_eq!(context.link.account_id, context.account.account_id);
debug_assert_eq!(context.requester.user_id, context.request.requester_id);

match input.action {
    WebhookAction::Accepted => {
        accept_invitation_from_webhook_transition(
            state,
            input.invitation_id,
            context.account.account_id,
            input.at,
            request_id,
        )
        .await
    }
    WebhookAction::Declined => {
        decline_invitation_from_webhook_transition(
            state,
            input.invitation_id,
            context.account.account_id,
            input.at,
            request_id,
        )
        .await
    }
}
```

- [ ] **Step 3: Add cancel and expire transition helpers**

Add these private functions before `tick_expire_logic`:

```rust
async fn cancel_invitation_transition(
    state: &AppState,
    invitation_id: GithubInvitationId,
    account_id: u64,
    by_user: Option<u64>,
    at: DateTime<Utc>,
    request_id: Option<String>,
) -> crate::error::Result<()> {
    state
        .storage
        .update_github_invitation(&storage::GithubInvitationUpdate {
            id: invitation_id,
            state: InvitationState::Cancelled,
            github_invitation_id: None,
            error_message: None,
            updated_at: at,
        })
        .await?;

    let actor = match by_user {
        Some(uid) => Actor::User(uid),
        None => Actor::System,
    };

    crate::audit::emit(
        state,
        account_id,
        EventType::InvitationCancelled,
        actor,
        Target::github_invitation(invitation_id),
        serde_json::json!({"by_user": by_user}),
        request_id,
    )
    .await
}

async fn expire_invitation_transition(
    state: &AppState,
    invitation_id: GithubInvitationId,
    account_id: u64,
    at: DateTime<Utc>,
    request_id: Option<String>,
) -> crate::error::Result<()> {
    state
        .storage
        .update_github_invitation(&storage::GithubInvitationUpdate {
            id: invitation_id,
            state: InvitationState::Expired,
            github_invitation_id: None,
            error_message: None,
            updated_at: at,
        })
        .await?;

    crate::audit::emit(
        state,
        account_id,
        EventType::InvitationExpired,
        Actor::System,
        Target::github_invitation(invitation_id),
        serde_json::json!({"reason": "tick_expire_no_longer_pending"}),
        request_id,
    )
    .await
}
```

- [ ] **Step 4: Delegate cancellation outcome to the transition helper**

In `cancel_logic`, replace the final state update, actor construction, metadata construction, and audit emit with this code:

```rust
cancel_invitation_transition(
    state,
    input.invitation_id,
    context.account.account_id,
    input.by_user,
    input.at,
    request_id,
)
.await
```

- [ ] **Step 5: Delegate expiration outcome to the transition helper**

In `tick_expire_logic`, replace the final state update, metadata construction, and audit emit with this code:

```rust
expire_invitation_transition(
    state,
    input.invitation_id,
    context.account.account_id,
    input.at,
    request_id,
)
.await
```

- [ ] **Step 6: Run focused transition tests**

Run: `cargo test -p restate-svc github_invitation::tests::webhook_accept_transition_updates_row_and_audits github_invitation::tests::webhook_decline_transition_updates_row_and_audits github_invitation::tests::cancel_transition_updates_row_and_audits_user_actor github_invitation::tests::cancel_transition_updates_row_and_audits_system_actor github_invitation::tests::expire_transition_updates_row_and_audits`

Expected: PASS.

- [ ] **Step 7: Run existing webhook, cancel, and expire behavior tests**

Run: `cargo test -p restate-svc github_invitation::tests::webhook_accepted_transitions_and_audits github_invitation::tests::webhook_declined_transitions_and_audits github_invitation::tests::webhook_on_terminal_row_is_idempotent github_invitation::tests::cancel_calls_delete_and_marks_cancelled github_invitation::tests::cancel_404_proceeds_to_mark_cancelled github_invitation::tests::tick_expire_marks_expired_when_still_pending github_invitation::tests::tick_expire_skips_when_no_longer_pending`

Expected: PASS.

- [ ] **Step 8: Commit settlement transition tests and implementation**

Run:

```bash
git add crates/restate-svc/src/github_invitation.rs
git commit -m "refactor(restate): deepen github invitation settlement transitions"
```

## Task 5: Add Reconciliation Transition Tests

**Files:**
- Modify: `crates/restate-svc/src/reconcile.rs`
- Test: `crates/restate-svc/src/reconcile.rs`

- [ ] **Step 1: Extend reconcile test imports**

In `crates/restate-svc/src/reconcile.rs`, inside `#[cfg(test)] mod tests`, replace the current `test_support` import with this version and add the audit enum import:

```rust
use crate::test_support::{dt, fixture_github_client, fixture_state_with_storage, fixture_storage};
use ::audit::{ActorKind, EventType, TargetKind};
```

- [ ] **Step 2: Add reconcile audit helper**

Add this helper after `seed_one_pending`:

```rust
async fn audit_events(
    storage: &::storage::SqlxStorage,
    account_id: u64,
) -> Vec<::audit::AuditEvent> {
    storage.debug_list_audit(account_id).await.unwrap()
}
```

- [ ] **Step 3: Add failing tests for reconciliation transition helpers**

Add these tests after `daily_run_continues_when_repo_full_name_is_invalid`:

```rust
#[tokio::test]
async fn reconciled_accept_transition_updates_row_and_audits() {
    let (state, storage) = fixture_state_with_storage().await;
    let inv_id = seed_one_pending(&state).await;
    let row = state
        .storage
        .get_github_invitation(inv_id)
        .await
        .unwrap()
        .unwrap();

    accept_reconciled_invitation_transition(
        &state,
        &row,
        100,
        dt("2026-05-05T13:00:00Z"),
        Some("req-reconcile-accept".into()),
    )
    .await
    .unwrap();

    let row = state
        .storage
        .get_github_invitation(inv_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.state, InvitationState::Accepted);
    assert_eq!(row.github_invitation_id, None);
    assert_eq!(row.error_message, None);

    let audits = audit_events(&storage, 100).await;
    assert_eq!(audits.len(), 1);
    let event = &audits[0];
    assert_eq!(event.event_type, EventType::InvitationAccepted);
    assert_eq!(event.actor_kind, ActorKind::System);
    assert_eq!(event.actor_id, None);
    assert_eq!(event.target_kind, TargetKind::GithubInvitation);
    assert_eq!(event.target_id, inv_id.to_string());
    assert_eq!(event.request_id.as_deref(), Some("req-reconcile-accept"));
    assert_eq!(
        event.metadata.get("reconciled"),
        Some(&serde_json::json!(true))
    );
}

#[tokio::test]
async fn reconciled_cancel_transition_updates_row_and_audits() {
    let (state, storage) = fixture_state_with_storage().await;
    let inv_id = seed_one_pending(&state).await;
    let row = state
        .storage
        .get_github_invitation(inv_id)
        .await
        .unwrap()
        .unwrap();

    cancel_reconciled_invitation_transition(
        &state,
        &row,
        100,
        dt("2026-05-05T13:00:00Z"),
        Some("req-reconcile-cancel".into()),
    )
    .await
    .unwrap();

    let row = state
        .storage
        .get_github_invitation(inv_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.state, InvitationState::Cancelled);
    assert_eq!(row.github_invitation_id, None);
    assert_eq!(row.error_message, None);

    let audits = audit_events(&storage, 100).await;
    assert_eq!(audits.len(), 1);
    let event = &audits[0];
    assert_eq!(event.event_type, EventType::InvitationCancelled);
    assert_eq!(event.actor_kind, ActorKind::System);
    assert_eq!(event.actor_id, None);
    assert_eq!(event.target_kind, TargetKind::GithubInvitation);
    assert_eq!(event.target_id, inv_id.to_string());
    assert_eq!(event.request_id.as_deref(), Some("req-reconcile-cancel"));
    assert_eq!(
        event.metadata.get("reconciled"),
        Some(&serde_json::json!(true))
    );
}
```

- [ ] **Step 4: Run tests to verify the reconciliation transition helpers do not exist yet**

Run: `cargo test -p restate-svc reconcile::tests::reconciled_accept_transition_updates_row_and_audits reconcile::tests::reconciled_cancel_transition_updates_row_and_audits`

Expected: FAIL with compile errors naming `accept_reconciled_invitation_transition` and `cancel_reconciled_invitation_transition`.

- [ ] **Step 5: Keep the failing reconciliation tests in the working tree**

Do not commit while the test target is failing. Leave the test changes unstaged for Task 6, where the implementation will make them pass and both tests plus implementation will be committed together.

## Task 6: Implement Reconciliation Transitions

**Files:**
- Modify: `crates/restate-svc/src/reconcile.rs`
- Test: `crates/restate-svc/src/reconcile.rs`

- [ ] **Step 1: Add reconciliation transition helpers**

Add these private functions after `reconcile_single`:

```rust
async fn accept_reconciled_invitation_transition(
    state: &AppState,
    row: &domain::GithubInvitation,
    account_id: u64,
    at: DateTime<Utc>,
    request_id_for_audit: Option<String>,
) -> crate::error::Result<()> {
    state
        .storage
        .update_github_invitation(&storage::GithubInvitationUpdate {
            id: row.id,
            state: InvitationState::Accepted,
            github_invitation_id: None,
            error_message: None,
            updated_at: at,
        })
        .await?;

    crate::audit::emit(
        state,
        account_id,
        EventType::InvitationAccepted,
        Actor::System,
        Target::github_invitation(row.id),
        serde_json::json!({"reconciled": true}),
        request_id_for_audit,
    )
    .await
}

async fn cancel_reconciled_invitation_transition(
    state: &AppState,
    row: &domain::GithubInvitation,
    account_id: u64,
    at: DateTime<Utc>,
    request_id_for_audit: Option<String>,
) -> crate::error::Result<()> {
    state
        .storage
        .update_github_invitation(&storage::GithubInvitationUpdate {
            id: row.id,
            state: InvitationState::Cancelled,
            github_invitation_id: None,
            error_message: None,
            updated_at: at,
        })
        .await?;

    crate::audit::emit(
        state,
        account_id,
        EventType::InvitationCancelled,
        Actor::System,
        Target::github_invitation(row.id),
        serde_json::json!({"reconciled": true}),
        request_id_for_audit,
    )
    .await
}
```

- [ ] **Step 2: Delegate reconciliation outcomes to transition helpers**

In `reconcile_single`, replace this block:

```rust
let (new_state, event) = if is_member {
    (InvitationState::Accepted, EventType::InvitationAccepted)
} else {
    (InvitationState::Cancelled, EventType::InvitationCancelled)
};

state
    .storage
    .update_github_invitation(&storage::GithubInvitationUpdate {
        id: row.id,
        state: new_state,
        github_invitation_id: None,
        error_message: None,
        updated_at: at,
    })
    .await?;

crate::audit::emit(
    state,
    context.account.account_id,
    event,
    Actor::System,
    Target::github_invitation(row.id),
    serde_json::json!({"reconciled": true}),
    request_id_for_audit,
)
.await
```

with this code:

```rust
if is_member {
    accept_reconciled_invitation_transition(
        state,
        row,
        context.account.account_id,
        at,
        request_id_for_audit,
    )
    .await
} else {
    cancel_reconciled_invitation_transition(
        state,
        row,
        context.account.account_id,
        at,
        request_id_for_audit,
    )
    .await
}
```

- [ ] **Step 3: Run focused reconciliation transition tests**

Run: `cargo test -p restate-svc reconcile::tests::reconciled_accept_transition_updates_row_and_audits reconcile::tests::reconciled_cancel_transition_updates_row_and_audits`

Expected: PASS.

- [ ] **Step 4: Run existing reconciliation behavior tests**

Run: `cargo test -p restate-svc reconcile::tests::daily_run_no_change_when_still_pending reconcile::tests::daily_run_continues_when_repo_full_name_is_invalid reconcile::tests::daily_run_marks_accepted_when_user_is_collaborator reconcile::tests::daily_run_marks_cancelled_when_user_is_not_collaborator`

Expected: PASS.

- [ ] **Step 5: Commit reconciliation tests and implementation**

Run:

```bash
git add crates/restate-svc/src/reconcile.rs
git commit -m "refactor(restate): deepen reconciled invitation transitions"
```

## Task 7: Add No-Op Audit Regression Assertions

**Files:**
- Modify: `crates/restate-svc/src/github_invitation.rs`
- Modify: `crates/restate-svc/src/reconcile.rs`
- Test: `crates/restate-svc/src/github_invitation.rs`
- Test: `crates/restate-svc/src/reconcile.rs`

- [ ] **Step 1: Add a GitHub Invitation test helper for concrete storage with scripted GitHub transport**

In `github_invitation.rs` tests, add this helper after `audit_events`:

```rust
async fn state_with_storage_and_mock(
    mock: MockTransport,
) -> (AppState, Arc<::storage::SqlxStorage>) {
    let storage = Arc::new(::storage::SqlxStorage::in_memory().await.unwrap());
    let storage_for_state: Arc<dyn storage::Storage> = storage.clone();
    let github = fixture_github_client(Arc::new(mock));
    (AppState::new(storage_for_state, github), storage)
}
```

- [ ] **Step 2: Replace the transient create test with a no-audit assertion**

Replace `create_502_propagates_transient_for_retry` with this version:

```rust
#[tokio::test]
async fn create_502_propagates_transient_for_retry() {
    let mock = MockTransport::scripted(vec![
        token_mint(9),
        Expectation::status(
            Method::Put,
            "https://api.github.test/repos/acme/api/collaborators/alice",
            502,
        ),
    ]);
    let (state, storage) = state_with_storage_and_mock(mock).await;
    let req_id = seed_chain(&state).await;

    let inv_id = GithubInvitationId::new();
    let err = create_logic(&state, &sample_input(inv_id, req_id), None)
        .await
        .unwrap_err();
    assert!(!err.is_terminal(), "5xx should be transient: {err:?}");

    let row = state
        .storage
        .get_github_invitation(inv_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.state, InvitationState::Sending);

    let audits = audit_events(&storage, 100).await;
    assert!(audits.is_empty());
}
```

- [ ] **Step 3: Replace the terminal webhook no-op test with a duplicate-audit assertion**

Replace `webhook_on_terminal_row_is_idempotent` with this version:

```rust
#[tokio::test]
async fn webhook_on_terminal_row_is_idempotent() {
    let (state, storage) = fixture_state_with_storage().await;
    let req_id = seed_chain(&state).await;
    let inv_id = seed_to_sent(&state, req_id).await;

    on_webhook_logic(
        &state,
        &OnWebhookInput {
            invitation_id: inv_id,
            action: WebhookAction::Accepted,
            at: dt("2026-05-04T14:00:00Z"),
        },
        Some("req-first-webhook".into()),
    )
    .await
    .unwrap();

    on_webhook_logic(
        &state,
        &OnWebhookInput {
            invitation_id: inv_id,
            action: WebhookAction::Declined,
            at: dt("2026-05-04T15:00:00Z"),
        },
        Some("req-second-webhook".into()),
    )
    .await
    .unwrap();

    let row = state
        .storage
        .get_github_invitation(inv_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        row.state,
        InvitationState::Accepted,
        "terminal state preserved"
    );

    let audits = audit_events(&storage, 100).await;
    let invitation_events: Vec<_> = audits
        .iter()
        .filter(|event| event.target_id == inv_id.to_string())
        .collect();
    assert_eq!(invitation_events.len(), 1);
    assert_eq!(invitation_events[0].event_type, EventType::InvitationAccepted);
    assert_eq!(
        invitation_events[0].request_id.as_deref(),
        Some("req-first-webhook")
    );
}
```

- [ ] **Step 4: Replace the expiration skip test with a no-audit assertion**

Replace `tick_expire_skips_when_no_longer_pending` with this version:

```rust
#[tokio::test]
async fn tick_expire_skips_when_no_longer_pending() {
    let mock = MockTransport::scripted(vec![
        token_mint(9),
        Expectation::ok_json(
            Method::Get,
            "https://api.github.test/repos/acme/api/invitations?per_page=100",
            serde_json::json!([]),
        ),
    ]);
    let (state, storage) = state_with_storage_and_mock(mock).await;
    let (_req_id, inv_id) = seed_chain_with_repos(&state).await;

    tick_expire_logic(
        &state,
        &TickExpireInput {
            invitation_id: inv_id,
            installation_id: 9,
            at: dt("2026-05-11T13:00:00Z"),
        },
        Some("req-expire-skip".into()),
    )
    .await
    .unwrap();

    let row = state
        .storage
        .get_github_invitation(inv_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        row.state,
        InvitationState::Sent,
        "row left for webhook to settle"
    );

    let audits = audit_events(&storage, 100).await;
    assert!(audits.is_empty());
}
```

- [ ] **Step 5: Add a reconcile test helper for concrete storage with scripted GitHub transport**

In `reconcile.rs` tests, add this helper after `audit_events`:

```rust
async fn state_with_storage_and_mock(
    mock: MockTransport,
) -> (AppState, Arc<::storage::SqlxStorage>) {
    let storage = Arc::new(::storage::SqlxStorage::in_memory().await.unwrap());
    let storage_for_state: Arc<dyn storage::Storage> = storage.clone();
    let github = fixture_github_client(Arc::new(mock));
    (AppState::new(storage_for_state, github), storage)
}
```

- [ ] **Step 6: Replace the still-pending reconcile test with a no-audit assertion**

Replace `daily_run_no_change_when_still_pending` with this version:

```rust
#[tokio::test]
async fn daily_run_no_change_when_still_pending() {
    let mock = MockTransport::scripted(vec![
        token_mint(9),
        Expectation::ok_json(
            Method::Get,
            "https://api.github.test/repos/acme/api/invitations?per_page=100",
            serde_json::json!([
                {
                    "id": 9988,
                    "invitee": {"id": 42, "login": "alice"},
                    "permissions": "write",
                    "created_at": "2026-05-04T13:00:00Z"
                }
            ]),
        ),
    ]);
    let (state, storage) = state_with_storage_and_mock(mock).await;
    let inv_id = seed_one_pending(&state).await;

    daily_run_logic(
        &state,
        &DailyRunInput {
            at: dt("2026-05-05T13:00:00Z"),
        },
        Some("req-reconcile-pending".into()),
    )
    .await
    .unwrap();

    let row = state
        .storage
        .get_github_invitation(inv_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.state, InvitationState::Sent);

    let audits = audit_events(&storage, 100).await;
    assert!(audits.is_empty());
}
```

- [ ] **Step 7: Run no-op regression tests**

Run: `cargo test -p restate-svc github_invitation::tests::create_502_propagates_transient_for_retry github_invitation::tests::webhook_on_terminal_row_is_idempotent github_invitation::tests::tick_expire_skips_when_no_longer_pending reconcile::tests::daily_run_no_change_when_still_pending`

Expected: PASS.

- [ ] **Step 8: Commit no-op audit regression assertions**

Run:

```bash
git add crates/restate-svc/src/github_invitation.rs crates/restate-svc/src/reconcile.rs
git commit -m "test(restate): assert github invitation no-op audit behavior"
```

## Task 8: Final Verification

**Files:**
- Verify: `crates/restate-svc/src/github_invitation.rs`
- Verify: `crates/restate-svc/src/reconcile.rs`

- [ ] **Step 1: Run focused GitHub Invitation and reconciliation tests**

Run: `cargo test -p restate-svc github_invitation reconcile`

Expected: PASS.

- [ ] **Step 2: Run the full Restate service suite**

Run: `cargo test -p restate-svc`

Expected: PASS.

- [ ] **Step 3: Run formatting check**

Run: `cargo fmt --check`

Expected: PASS.

- [ ] **Step 4: Inspect final diff**

Run: `git status --short`

Expected: only intentional changes, or no changes if each task was committed.

Run: `git diff --stat HEAD`

Expected: changes are limited to `crates/restate-svc/src/github_invitation.rs` and `crates/restate-svc/src/reconcile.rs` unless task commits already made the working tree clean.

- [ ] **Step 5: Commit final cleanup if formatting changed files**

If `cargo fmt --check` required a formatting fix, run:

```bash
cargo fmt
git add crates/restate-svc/src/github_invitation.rs crates/restate-svc/src/reconcile.rs
git commit -m "style(restate): format github invitation transitions"
```

If formatting did not change files, do not create an extra commit.
