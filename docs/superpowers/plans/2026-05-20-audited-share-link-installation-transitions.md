# Audited Share Link And Installation Transitions Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Move Share Link and Installation audit-event construction behind transition-level behavior while preserving existing lifecycle semantics.

**Architecture:** Keep the existing Restate service modules and public handler payloads. Add small transition functions inside `share_link.rs` and `installation.rs` that own the state write plus audit event for each lifecycle transition, then keep the existing `*_logic` functions as thin delegates. Tests assert both persisted state and audit events through the existing in-memory SQLite storage fixture.

**Tech Stack:** Rust 2024 workspace, Restate SDK handler modules, `storage::SqlxStorage::in_memory`, `audit::AuditEvent`, `cargo test`, `cargo fmt`.

---

## Scope Check

This plan covers one coherent Restate-side architecture-deepening slice: audited Share Link transitions and audited Installation transitions. It does not include Invitation Request, GitHub Invitation, Reconcile, web routes, storage schema changes, or UI changes.

Do not commit unless the maintainer explicitly authorizes commits in the active session.

Execution note: checklist items below are reusable task instructions. The active agent session tracks execution status separately, so these boxes may remain unchecked after an implementation run.

## File Structure

- Modify `crates/restate-svc/src/test_support.rs`: add a test fixture that returns both `AppState` and the concrete `SqlxStorage` so tests can read audit events without changing production `AppState`.
- Modify `crates/restate-svc/src/share_link.rs`: add Share Link transition tests, add `create_share_link_transition`, `revoke_share_link_transition`, and `expire_share_link_transition`, then delegate existing logic functions to them.
- Modify `crates/restate-svc/src/installation.rs`: add Installation transition tests, add `onboard_installation_transition`, `change_installation_repos_transition`, and `uninstall_installation_transition`, then delegate existing logic functions to them.
- Verify `docs/superpowers/specs/2026-05-20-audited-share-link-installation-transitions-design.md`: no code changes needed, but use it as the source of acceptance criteria.

### Task 1: Concrete Audit-Readable Test Fixture

**Files:**
- Modify: `crates/restate-svc/src/test_support.rs`

- [ ] **Step 1: Add a fixture that preserves concrete storage access**

In `crates/restate-svc/src/test_support.rs`, add this function after `fixture_state()` and before `fixture_state_with_transport()`:

```rust
/// Convenience: an `AppState` plus concrete storage for tests that need
/// debug-only audit reads.
pub(crate) async fn fixture_state_with_storage() -> (AppState, Arc<storage::SqlxStorage>) {
    use github::mocks::MockTransport;

    let storage = Arc::new(storage::SqlxStorage::in_memory().await.unwrap());
    let storage_for_state: Arc<dyn storage::Storage> = storage.clone();
    let transport = Arc::new(MockTransport::scripted(vec![]));
    let state = AppState::new(storage_for_state, fixture_github_client(transport));

    (state, storage)
}
```

- [ ] **Step 2: Run the package check for the new helper**

Run: `cargo test -p restate-svc audit::tests::emit_writes_event_to_storage`

Expected: PASS. This proves the helper did not break existing test support or crate compilation.

- [ ] **Step 3: Inspect the fixture diff**

Run: `git diff -- crates/restate-svc/src/test_support.rs`

Expected: Diff contains only `fixture_state_with_storage` and no production code changes.

### Task 2: Share Link Transition Tests

**Files:**
- Modify: `crates/restate-svc/src/share_link.rs`

- [ ] **Step 1: Update test imports**

In the `#[cfg(test)] mod tests` block in `crates/restate-svc/src/share_link.rs`, replace the existing test support import:

```rust
use crate::test_support::{dt, fixture_state};
```

with:

```rust
use crate::test_support::{dt, fixture_state, fixture_state_with_storage};
use ::audit::{ActorKind, EventType, TargetKind};
```

- [ ] **Step 2: Add an audit-read helper to Share Link tests**

In the same test module, add this helper after `sample_input()`:

```rust
async fn audit_events(
    storage: &::storage::SqlxStorage,
    account_id: u64,
) -> Vec<::audit::AuditEvent> {
    storage.debug_list_audit(account_id).await.unwrap()
}
```

- [ ] **Step 3: Add the failing create transition audit test**

Add this test after `create_inserts_link_with_generated_slug`:

```rust
#[tokio::test]
async fn create_transition_inserts_link_and_audits() {
    let (state, storage) = fixture_state_with_storage().await;
    seed_installation_and_user(&state).await;

    let out = create_share_link_transition(&state, &sample_input(), Some("req-create".into()))
        .await
        .unwrap();

    let link = state
        .storage
        .get_share_link_by_id(out.link_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(link.slug.as_str(), out.slug);
    assert_eq!(link.repos.len(), 1);

    let audits = audit_events(&storage, 100).await;
    assert_eq!(audits.len(), 1);
    let event = &audits[0];
    assert_eq!(event.event_type, EventType::ShareLinkCreated);
    assert_eq!(event.actor_kind, ActorKind::User);
    assert_eq!(event.actor_id, Some(7));
    assert_eq!(event.target_kind, TargetKind::ShareLink);
    assert_eq!(event.target_id, out.link_id.to_string());
    assert_eq!(event.request_id.as_deref(), Some("req-create"));
    assert_eq!(event.metadata.get("permission"), Some(&serde_json::json!("pull")));
    assert_eq!(
        event.metadata.get("approval_required"),
        Some(&serde_json::json!(true))
    );
    assert_eq!(event.metadata.get("max_uses"), Some(&serde_json::json!(5)));
    assert_eq!(event.metadata.get("repo_count"), Some(&serde_json::json!(1)));
}
```

- [ ] **Step 4: Add the failing revoke transition audit test**

Add this test after `revoke_marks_and_audits`:

```rust
#[tokio::test]
async fn revoke_transition_marks_and_audits_once() {
    let (state, storage) = fixture_state_with_storage().await;
    seed_installation_and_user(&state).await;
    let out = create_share_link_transition(&state, &sample_input(), Some("req-create".into()))
        .await
        .unwrap();

    revoke_share_link_transition(
        &state,
        &RevokeLinkInput {
            link_id: out.link_id,
            by_user: 7,
            when: dt("2026-05-04T13:00:00Z"),
        },
        Some("req-revoke".into()),
    )
    .await
    .unwrap();

    let link = state
        .storage
        .get_share_link_by_id(out.link_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(link.revoked_by, Some(7));
    assert_eq!(link.revoked_at, Some(dt("2026-05-04T13:00:00Z")));

    revoke_share_link_transition(
        &state,
        &RevokeLinkInput {
            link_id: out.link_id,
            by_user: 7,
            when: dt("2026-05-04T14:00:00Z"),
        },
        Some("req-revoke-again".into()),
    )
    .await
    .unwrap();

    let audits = audit_events(&storage, 100).await;
    let revoked: Vec<_> = audits
        .iter()
        .filter(|event| event.event_type == EventType::ShareLinkRevoked)
        .collect();
    assert_eq!(revoked.len(), 1);
    let event = revoked[0];
    assert_eq!(event.actor_kind, ActorKind::User);
    assert_eq!(event.actor_id, Some(7));
    assert_eq!(event.target_kind, TargetKind::ShareLink);
    assert_eq!(event.target_id, out.link_id.to_string());
    assert_eq!(event.request_id.as_deref(), Some("req-revoke"));
    assert_eq!(event.metadata.get("by_user"), Some(&serde_json::json!(7)));
}
```

- [ ] **Step 5: Add the failing expiration transition audit test**

Add this test after `tick_expiration_emits_when_past_expiry`:

```rust
#[tokio::test]
async fn expire_transition_emits_audit_without_state_mutation() {
    let (state, storage) = fixture_state_with_storage().await;
    seed_installation_and_user(&state).await;
    let out = create_share_link_transition(&state, &sample_input(), Some("req-create".into()))
        .await
        .unwrap();
    let expired_at = dt("2026-06-03T12:00:00Z");

    expire_share_link_transition(
        &state,
        &TickExpirationInput {
            link_id: out.link_id,
            at: expired_at,
        },
        Some("req-expire".into()),
    )
    .await
    .unwrap();

    let link = state
        .storage
        .get_share_link_by_id(out.link_id)
        .await
        .unwrap()
        .unwrap();
    assert!(link.revoked_at.is_none());
    assert_eq!(link.uses_count, 0);
    assert!(!link.is_active(expired_at));

    let audits = audit_events(&storage, 100).await;
    let event = audits
        .iter()
        .find(|event| event.event_type == EventType::ShareLinkExpired)
        .expect("expiration should emit audit event");
    assert_eq!(event.actor_kind, ActorKind::System);
    assert_eq!(event.actor_id, None);
    assert_eq!(event.target_kind, TargetKind::ShareLink);
    assert_eq!(event.target_id, out.link_id.to_string());
    assert_eq!(event.request_id.as_deref(), Some("req-expire"));
    assert_eq!(
        event.metadata.get("expired_at"),
        Some(&serde_json::json!(expired_at.to_rfc3339()))
    );
}
```

- [ ] **Step 6: Add the failing expiration no-audit test**

Add this test after `tick_expiration_skips_revoked_link`:

```rust
#[tokio::test]
async fn expire_transition_skips_early_and_revoked_links_without_audit() {
    let (state, storage) = fixture_state_with_storage().await;
    seed_installation_and_user(&state).await;
    let out = create_share_link_transition(&state, &sample_input(), Some("req-create".into()))
        .await
        .unwrap();

    expire_share_link_transition(
        &state,
        &TickExpirationInput {
            link_id: out.link_id,
            at: dt("2026-06-03T11:59:59Z"),
        },
        Some("req-early".into()),
    )
    .await
    .unwrap();

    revoke_share_link_transition(
        &state,
        &RevokeLinkInput {
            link_id: out.link_id,
            by_user: 7,
            when: dt("2026-05-04T13:00:00Z"),
        },
        Some("req-revoke".into()),
    )
    .await
    .unwrap();

    expire_share_link_transition(
        &state,
        &TickExpirationInput {
            link_id: out.link_id,
            at: dt("2026-06-03T12:00:00Z"),
        },
        Some("req-expire-revoked".into()),
    )
    .await
    .unwrap();

    let audits = audit_events(&storage, 100).await;
    let expired_count = audits
        .iter()
        .filter(|event| event.event_type == EventType::ShareLinkExpired)
        .count();
    assert_eq!(expired_count, 0);
}
```

- [ ] **Step 7: Run Share Link tests and verify RED**

Run: `cargo test -p restate-svc share_link::tests::create_transition_inserts_link_and_audits`

Expected: FAIL because `create_share_link_transition` is not defined yet.

### Task 3: Share Link Transition Implementation

**Files:**
- Modify: `crates/restate-svc/src/share_link.rs`

- [ ] **Step 1: Add Share Link transition functions and delegate existing logic**

In `crates/restate-svc/src/share_link.rs`, replace the current `create_logic`, `revoke_logic`, and `tick_expiration_logic` function bodies with this transition section:

```rust
/// Pure logic: generate a slug, write the link + repos rows, emit audit.
pub async fn create_logic(
    state: &AppState,
    input: &CreateLinkInput,
    request_id: Option<String>,
) -> crate::error::Result<CreateLinkOutput> {
    create_share_link_transition(state, input, request_id).await
}

async fn create_share_link_transition(
    state: &AppState,
    input: &CreateLinkInput,
    request_id: Option<String>,
) -> crate::error::Result<CreateLinkOutput> {
    let mut rng = OsRng;
    let slug = Slug::generate(&mut rng);
    let link_id = ShareLinkId::new();

    let link = DomainShareLink {
        id: link_id,
        slug: slug.clone(),
        installation_id: input.installation_id,
        account_id: input.account_id,
        created_by: input.created_by,
        created_at: input.created_at,
        expires_at: input.expires_at,
        max_uses: input.max_uses,
        uses_count: 0,
        permission: input.permission,
        approval_required: input.approval_required,
        internal_note: input.internal_note.clone(),
        revoked_at: None,
        revoked_by: None,
        repos: input.repos.clone(),
    };

    state.storage.insert_share_link(&link).await?;

    crate::audit::emit(
        state,
        input.account_id,
        EventType::ShareLinkCreated,
        Actor::User(input.created_by),
        Target::share_link(link_id),
        serde_json::json!({
            "permission": input.permission.to_string(),
            "approval_required": input.approval_required,
            "max_uses": input.max_uses,
            "repo_count": input.repos.len(),
        }),
        request_id,
    )
    .await?;

    Ok(CreateLinkOutput {
        link_id,
        slug: slug.as_str().to_string(),
    })
}

/// Pure logic: stamp `revoked_at`/`revoked_by`; emit audit. Idempotent on
/// double-revoke (storage returns NotFound when no-op; we treat as Ok).
pub async fn revoke_logic(
    state: &AppState,
    input: &RevokeLinkInput,
    request_id: Option<String>,
) -> crate::error::Result<()> {
    revoke_share_link_transition(state, input, request_id).await
}

async fn revoke_share_link_transition(
    state: &AppState,
    input: &RevokeLinkInput,
    request_id: Option<String>,
) -> crate::error::Result<()> {
    match state
        .storage
        .mark_share_link_revoked(input.link_id, input.by_user, input.when)
        .await
    {
        Ok(()) => (),
        Err(storage::Error::NotFound) => return Ok(()),
        Err(e) => return Err(e.into()),
    }

    let link = state
        .storage
        .get_share_link_by_id(input.link_id)
        .await?
        .ok_or(crate::error::HandlerError::Storage(
            storage::Error::NotFound,
        ))?;

    crate::audit::emit(
        state,
        link.account_id,
        EventType::ShareLinkRevoked,
        Actor::User(input.by_user),
        Target::share_link(input.link_id),
        serde_json::json!({"by_user": input.by_user}),
        request_id,
    )
    .await
}

/// Pure logic: if the link is past its expiry and not already revoked, emit
/// `share_link.expired`. No state mutation — `is_active(now)` already reflects
/// the expiry. Idempotent under repeated calls.
pub async fn tick_expiration_logic(
    state: &AppState,
    input: &TickExpirationInput,
    request_id: Option<String>,
) -> crate::error::Result<()> {
    expire_share_link_transition(state, input, request_id).await
}

async fn expire_share_link_transition(
    state: &AppState,
    input: &TickExpirationInput,
    request_id: Option<String>,
) -> crate::error::Result<()> {
    let link = state
        .storage
        .get_share_link_by_id(input.link_id)
        .await?
        .ok_or(crate::error::HandlerError::Storage(
            storage::Error::NotFound,
        ))?;

    if link.revoked_at.is_some() {
        return Ok(());
    }
    let Some(expires) = link.expires_at else {
        return Err(crate::error::HandlerError::Invariant(format!(
            "tick_expiration fired for link {} which has no expires_at",
            input.link_id
        )));
    };
    if input.at < expires {
        return Ok(());
    }

    crate::audit::emit(
        state,
        link.account_id,
        EventType::ShareLinkExpired,
        Actor::System,
        Target::share_link(input.link_id),
        serde_json::json!({"expired_at": input.at.to_rfc3339()}),
        request_id,
    )
    .await
}
```

- [ ] **Step 2: Run focused Share Link transition tests**

Run: `cargo test -p restate-svc share_link::tests::create_transition_inserts_link_and_audits`

Expected: PASS.

Run: `cargo test -p restate-svc share_link::tests::revoke_transition_marks_and_audits_once`

Expected: PASS.

Run: `cargo test -p restate-svc share_link::tests::expire_transition_emits_audit_without_state_mutation`

Expected: PASS.

Run: `cargo test -p restate-svc share_link::tests::expire_transition_skips_early_and_revoked_links_without_audit`

Expected: PASS.

- [ ] **Step 3: Run all Share Link tests**

Run: `cargo test -p restate-svc share_link`

Expected: PASS. Existing Share Link behavior remains unchanged and new transition-level audit assertions pass.

- [ ] **Step 4: Inspect Share Link diff**

Run: `git diff -- crates/restate-svc/src/share_link.rs`

Expected: Diff adds transition tests, adds transition functions, and keeps existing `*_logic` functions as the public async seam.

### Task 4: Installation Transition Tests

**Files:**
- Modify: `crates/restate-svc/src/installation.rs`

- [ ] **Step 1: Update Installation test imports**

In the `#[cfg(test)] mod tests` block in `crates/restate-svc/src/installation.rs`, replace the existing test support import:

```rust
use crate::test_support::{dt, fixture_state};
```

with:

```rust
use crate::test_support::{dt, fixture_state, fixture_state_with_storage};
use ::audit::{ActorKind, EventType, TargetKind};
```

- [ ] **Step 2: Add an audit-read helper to Installation tests**

In the same test module, add this helper after `sample_input()`:

```rust
async fn audit_events(
    storage: &::storage::SqlxStorage,
    account_id: u64,
) -> Vec<::audit::AuditEvent> {
    storage.debug_list_audit(account_id).await.unwrap()
}
```

- [ ] **Step 3: Add the failing onboard transition audit test**

Add this test after `onboard_inserts_row_and_audits`:

```rust
#[tokio::test]
async fn onboard_transition_inserts_row_and_audits_once() {
    let (state, storage) = fixture_state_with_storage().await;

    onboard_installation_transition(&state, &sample_input(), Some("req-onboard".into()))
        .await
        .unwrap();

    let acct = state.storage.get_installation(1).await.unwrap().unwrap();
    assert_eq!(acct.account_login, "acme");
    assert_eq!(acct.account_id, 100);
    assert!(acct.uninstalled_at.is_none());

    onboard_installation_transition(&state, &sample_input(), Some("req-onboard-again".into()))
        .await
        .unwrap();

    let audits = audit_events(&storage, 100).await;
    let created: Vec<_> = audits
        .iter()
        .filter(|event| event.event_type == EventType::InstallationCreated)
        .collect();
    assert_eq!(created.len(), 1);
    let event = created[0];
    assert_eq!(event.actor_kind, ActorKind::User);
    assert_eq!(event.actor_id, Some(7));
    assert_eq!(event.target_kind, TargetKind::Installation);
    assert_eq!(event.target_id, "1");
    assert_eq!(event.request_id.as_deref(), Some("req-onboard"));
    assert_eq!(
        event.metadata.get("account_login"),
        Some(&serde_json::json!("acme"))
    );
    assert_eq!(
        event.metadata.get("account_type"),
        Some(&serde_json::json!("Organization"))
    );
}
```

- [ ] **Step 4: Add the failing repository-selection transition audit test**

Add this test after `repos_changed_updates_and_audits`:

```rust
#[tokio::test]
async fn repos_changed_transition_updates_and_audits() {
    let (state, storage) = fixture_state_with_storage().await;
    onboard_installation_transition(&state, &sample_input(), Some("req-onboard".into()))
        .await
        .unwrap();

    change_installation_repos_transition(
        &state,
        &ReposChangedInput {
            installation_id: 1,
            selected_repos: SelectedRepos::Subset(vec![10, 20]),
        },
        Some("req-repos".into()),
    )
    .await
    .unwrap();

    let acct = state.storage.get_installation(1).await.unwrap().unwrap();
    assert_eq!(acct.selected_repos, SelectedRepos::Subset(vec![10, 20]));

    let audits = audit_events(&storage, 100).await;
    let event = audits
        .iter()
        .find(|event| event.event_type == EventType::InstallationReposChanged)
        .expect("repository-selection change should emit audit event");
    assert_eq!(event.actor_kind, ActorKind::Github);
    assert_eq!(event.actor_id, None);
    assert_eq!(event.target_kind, TargetKind::Installation);
    assert_eq!(event.target_id, "1");
    assert_eq!(event.request_id.as_deref(), Some("req-repos"));
    assert_eq!(
        event.metadata.get("selected_repos_kind"),
        Some(&serde_json::json!("subset"))
    );
}
```

- [ ] **Step 5: Add the failing uninstall transition audit test**

Add this test after `uninstall_marks_and_audits`:

```rust
#[tokio::test]
async fn uninstall_transition_marks_and_audits_once() {
    let (state, storage) = fixture_state_with_storage().await;
    onboard_installation_transition(&state, &sample_input(), Some("req-onboard".into()))
        .await
        .unwrap();
    let uninstalled_at = dt("2026-05-05T00:00:00Z");

    uninstall_installation_transition(
        &state,
        &UninstallInput {
            installation_id: 1,
            uninstalled_at,
        },
        Some("req-uninstall".into()),
    )
    .await
    .unwrap();

    let acct = state.storage.get_installation(1).await.unwrap().unwrap();
    assert_eq!(acct.uninstalled_at, Some(uninstalled_at));

    uninstall_installation_transition(
        &state,
        &UninstallInput {
            installation_id: 1,
            uninstalled_at: dt("2026-05-06T00:00:00Z"),
        },
        Some("req-uninstall-again".into()),
    )
    .await
    .unwrap();

    let audits = audit_events(&storage, 100).await;
    let uninstalled: Vec<_> = audits
        .iter()
        .filter(|event| event.event_type == EventType::InstallationUninstalled)
        .collect();
    assert_eq!(uninstalled.len(), 1);
    let event = uninstalled[0];
    assert_eq!(event.actor_kind, ActorKind::Github);
    assert_eq!(event.actor_id, None);
    assert_eq!(event.target_kind, TargetKind::Installation);
    assert_eq!(event.target_id, "1");
    assert_eq!(event.request_id.as_deref(), Some("req-uninstall"));
    assert_eq!(
        event.metadata.get("uninstalled_at"),
        Some(&serde_json::json!(uninstalled_at.to_rfc3339()))
    );
}
```

- [ ] **Step 6: Add the failing unknown uninstall no-audit test**

Add this test after `uninstall_unknown_installation_is_idempotent`:

```rust
#[tokio::test]
async fn uninstall_transition_unknown_installation_does_not_audit() {
    let (state, storage) = fixture_state_with_storage().await;

    uninstall_installation_transition(
        &state,
        &UninstallInput {
            installation_id: 999,
            uninstalled_at: dt("2026-05-05T00:00:00Z"),
        },
        Some("req-uninstall-missing".into()),
    )
    .await
    .unwrap();

    let audits = audit_events(&storage, 100).await;
    assert!(audits.is_empty());
}
```

- [ ] **Step 7: Run Installation tests and verify RED**

Run: `cargo test -p restate-svc installation::tests::onboard_transition_inserts_row_and_audits_once`

Expected: FAIL because `onboard_installation_transition` is not defined yet.

### Task 5: Installation Transition Implementation

**Files:**
- Modify: `crates/restate-svc/src/installation.rs`

- [ ] **Step 1: Add Installation transition functions and delegate existing logic**

In `crates/restate-svc/src/installation.rs`, replace the current `onboard_logic`, `repos_changed_logic`, and `uninstall_logic` function bodies with this transition section:

```rust
/// Pure logic: persist the new installation row and emit the audit event.
/// Idempotent: a duplicate `installation_id` returns `Ok(())` after observing
/// the existing row matches the input.
pub async fn onboard_logic(
    state: &AppState,
    input: &OnboardInput,
    request_id: Option<String>,
) -> crate::error::Result<()> {
    onboard_installation_transition(state, input, request_id).await
}

async fn onboard_installation_transition(
    state: &AppState,
    input: &OnboardInput,
    request_id: Option<String>,
) -> crate::error::Result<()> {
    let account = Account {
        installation_id: input.installation_id,
        account_id: input.account_id,
        account_login: input.account_login.clone(),
        account_type: input.account_type,
        installed_at: input.installed_at,
        uninstalled_at: None,
        selected_repos: input.selected_repos.clone(),
    };

    match state.storage.insert_installation(&account).await {
        Ok(()) => (),
        Err(storage::Error::Conflict(storage::ConflictKind::DuplicateId)) => {
            return Ok(());
        }
        Err(e) => return Err(HandlerError::Storage(e)),
    }

    crate::audit::emit(
        state,
        input.account_id,
        EventType::InstallationCreated,
        Actor::User(input.actor_user_id),
        Target::installation(input.installation_id),
        serde_json::json!({
            "account_login": input.account_login,
            "account_type": input.account_type.to_string(),
        }),
        request_id,
    )
    .await
}

/// Pure logic: update the installation's selected repos and emit audit.
pub async fn repos_changed_logic(
    state: &AppState,
    input: &ReposChangedInput,
    request_id: Option<String>,
) -> crate::error::Result<()> {
    change_installation_repos_transition(state, input, request_id).await
}

async fn change_installation_repos_transition(
    state: &AppState,
    input: &ReposChangedInput,
    request_id: Option<String>,
) -> crate::error::Result<()> {
    state
        .storage
        .update_installation_repos(input.installation_id, &input.selected_repos)
        .await?;

    let acct = state
        .storage
        .get_installation(input.installation_id)
        .await?
        .ok_or_else(|| {
            HandlerError::Invariant(format!(
                "installation {} updated but not readable",
                input.installation_id
            ))
        })?;

    crate::audit::emit(
        state,
        acct.account_id,
        EventType::InstallationReposChanged,
        Actor::Github,
        Target::installation(input.installation_id),
        serde_json::json!({
            "selected_repos_kind": match &input.selected_repos {
                SelectedRepos::All => "all",
                SelectedRepos::Subset(_) => "subset",
            },
        }),
        request_id,
    )
    .await
}

/// Pure logic: stamp `uninstalled_at` and emit audit. Idempotent: if the
/// installation was already uninstalled, returns `Ok(())` without re-audit.
pub async fn uninstall_logic(
    state: &AppState,
    input: &UninstallInput,
    request_id: Option<String>,
) -> crate::error::Result<()> {
    uninstall_installation_transition(state, input, request_id).await
}

async fn uninstall_installation_transition(
    state: &AppState,
    input: &UninstallInput,
    request_id: Option<String>,
) -> crate::error::Result<()> {
    match state
        .storage
        .mark_installation_uninstalled(input.installation_id, input.uninstalled_at)
        .await
    {
        Ok(()) => (),
        Err(storage::Error::NotFound) => {
            return Ok(());
        }
        Err(e) => return Err(HandlerError::Storage(e)),
    }

    let acct = state
        .storage
        .get_installation(input.installation_id)
        .await?
        .ok_or_else(|| {
            HandlerError::Invariant(format!(
                "installation {} marked uninstalled but not readable",
                input.installation_id
            ))
        })?;

    crate::audit::emit(
        state,
        acct.account_id,
        EventType::InstallationUninstalled,
        Actor::Github,
        Target::installation(input.installation_id),
        serde_json::json!({
            "uninstalled_at": input.uninstalled_at.to_rfc3339(),
        }),
        request_id,
    )
    .await
}
```

- [ ] **Step 2: Run focused Installation transition tests**

Run: `cargo test -p restate-svc installation::tests::onboard_transition_inserts_row_and_audits_once`

Expected: PASS.

Run: `cargo test -p restate-svc installation::tests::repos_changed_transition_updates_and_audits`

Expected: PASS.

Run: `cargo test -p restate-svc installation::tests::uninstall_transition_marks_and_audits_once`

Expected: PASS.

Run: `cargo test -p restate-svc installation::tests::uninstall_transition_unknown_installation_does_not_audit`

Expected: PASS.

- [ ] **Step 3: Run all Installation tests**

Run: `cargo test -p restate-svc installation`

Expected: PASS. Existing Installation behavior remains unchanged and new transition-level audit assertions pass.

- [ ] **Step 4: Inspect Installation diff**

Run: `git diff -- crates/restate-svc/src/installation.rs`

Expected: Diff adds transition tests, adds transition functions, and keeps existing `*_logic` functions as the public async seam.

### Task 6: Full Verification And Diff Review

**Files:**
- Verify: `crates/restate-svc/src/test_support.rs`
- Verify: `crates/restate-svc/src/share_link.rs`
- Verify: `crates/restate-svc/src/installation.rs`
- Verify: `docs/superpowers/specs/2026-05-20-audited-share-link-installation-transitions-design.md`
- Verify: `docs/superpowers/plans/2026-05-20-audited-share-link-installation-transitions.md`

- [ ] **Step 1: Run targeted Share Link tests**

Run: `cargo test -p restate-svc share_link`

Expected: PASS.

- [ ] **Step 2: Run targeted Installation tests**

Run: `cargo test -p restate-svc installation`

Expected: PASS.

- [ ] **Step 3: Run affected package tests**

Run: `cargo test -p restate-svc`

Expected: PASS.

- [ ] **Step 4: Run format check**

Run: `cargo fmt --check`

Expected: PASS. If formatting fails, run `cargo fmt`, then rerun `cargo fmt --check`.

- [ ] **Step 5: Search for direct audit construction in the affected lifecycle functions**

Run: `rg "crate::audit::emit" crates/restate-svc/src/share_link.rs crates/restate-svc/src/installation.rs`

Expected: The remaining `crate::audit::emit` calls in these two files appear inside `*_transition` functions, not inside `*_logic` function bodies.

- [ ] **Step 6: Review final diff**

Run: `git diff -- crates/restate-svc/src/test_support.rs crates/restate-svc/src/share_link.rs crates/restate-svc/src/installation.rs docs/superpowers/specs/2026-05-20-audited-share-link-installation-transitions-design.md docs/superpowers/plans/2026-05-20-audited-share-link-installation-transitions.md`

Expected: Diff is limited to the spec, this plan, test support, Share Link transition tests and functions, and Installation transition tests and functions. No storage schema, migration, web, GitHub adapter, or UI files are changed.

## Self-Review Notes

- Spec coverage: Task 2 and Task 3 cover Share Link create, revoke, and expiration transitions with audit event assertions. Task 4 and Task 5 cover Installation onboarding, repository-selection changes, and uninstall transitions with audit event assertions. Task 6 verifies formatting, tests, and audit-call locality.
- Open-item scan: The plan contains no deferred implementation items. Every code-changing step includes the exact code block to add or replace.
- Type consistency: Transition names, input types, event types, actor kinds, target kinds, metadata keys, and request id assertions match the current `restate-svc`, `audit`, `domain`, and `storage` APIs.
