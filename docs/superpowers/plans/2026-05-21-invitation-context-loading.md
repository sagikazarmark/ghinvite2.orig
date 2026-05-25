# Invitation Context Loading Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a Restate service Invitation Context loader and route existing workflow/reconciliation paths through it without changing behavior.

**Architecture:** Create `crates/restate-svc/src/invitation_context.rs` as the single lookup/classification boundary for Invitation Request and GitHub Invitation context chains. Existing Restate modules keep owning workflow behavior, GitHub calls, storage writes, and audit emission, but delegate repeated reads and invariant classification to the new module.

**Tech Stack:** Rust 2024, `restate-svc`, `domain`, `storage`, `github::InstallationClient`, in-memory `SqlxStorage` test fixtures, `tokio::test`.

---

## File Structure

- Create: `crates/restate-svc/src/invitation_context.rs` — context structs, loader functions, and focused classification tests.
- Modify: `crates/restate-svc/src/lib.rs` — declare the new module.
- Modify: `crates/restate-svc/src/invitation_request.rs` — use request context in `build_dispatch_inputs`.
- Modify: `crates/restate-svc/src/github_invitation.rs` — use account context in webhook, GitHub Invitation context in cancel/expire, and installation lookup in create.
- Modify: `crates/restate-svc/src/reconcile.rs` — use account-aware GitHub Invitation context in `reconcile_single`.

---

### Task 1: Request Context Loader

**Files:**
- Create: `crates/restate-svc/src/invitation_context.rs`
- Modify: `crates/restate-svc/src/lib.rs`

- [ ] **Step 1: Write failing request-context tests**

Create `crates/restate-svc/src/invitation_context.rs` with the module imports, placeholder function signatures, and tests:

```rust
use crate::error::{HandlerError, Result};
use crate::state::AppState;
use domain::{InvitationRequest, RequestId, InvitationLink, User};

#[derive(Clone, Debug)]
pub(crate) struct InvitationRequestContext {
    pub request: InvitationRequest,
    pub link: InvitationLink,
    pub requester: User,
}

pub(crate) async fn load_invitation_request_context(
    _state: &AppState,
    _request_id: RequestId,
) -> Result<InvitationRequestContext> {
    Err(HandlerError::Invariant("red test stub".into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::HandlerError;
    use crate::test_support::{dt, fixture_state};
    use domain::{AccountType, Permission, RequestState, SelectedRepos, InvitationLinkId, Slug};
    use rand::SeedableRng;

    async fn seed_request_chain(state: &AppState) -> (RequestId, InvitationLinkId) {
        state.storage.insert_installation(&domain::Account {
            installation_id: 9,
            account_id: 100,
            account_login: "acme".into(),
            account_type: AccountType::Organization,
            installed_at: dt("2026-05-04T12:00:00Z"),
            uninstalled_at: None,
            selected_repos: SelectedRepos::All,
        }).await.unwrap();
        state.storage.upsert_user(&domain::User {
            user_id: 7,
            login: "creator".into(),
            avatar_url: None,
            last_seen_at: dt("2026-05-04T12:00:00Z"),
        }).await.unwrap();
        state.storage.upsert_user(&domain::User {
            user_id: 8,
            login: "alice".into(),
            avatar_url: None,
            last_seen_at: dt("2026-05-04T12:00:00Z"),
        }).await.unwrap();

        let link = domain::InvitationLink {
            id: InvitationLinkId::new(),
            slug: Slug::generate(&mut rand_chacha::ChaCha8Rng::seed_from_u64(21)),
            installation_id: 9,
            account_id: 100,
            created_by: 7,
            created_at: dt("2026-05-04T12:00:00Z"),
            expires_at: None,
            max_uses: None,
            uses_count: 0,
            permission: Permission::Push,
            approval_required: false,
            internal_note: None,
            revoked_at: None,
            revoked_by: None,
            repos: vec![domain::InvitationLinkRepo {
                repo_id: 10,
                repo_full_name: "acme/api".into(),
            }],
        };
        state.storage.insert_invitation_link(&link).await.unwrap();

        let request_id = RequestId::new();
        state.storage.insert_invitation_request_and_increment_uses(&domain::InvitationRequest {
            id: request_id,
            invitation_link_id: link.id,
            requester_id: 8,
            justification: None,
            state: RequestState::Approved,
            decided_by: Some(7),
            decided_at: Some(dt("2026-05-04T13:00:00Z")),
            decline_reason: None,
            created_at: dt("2026-05-04T12:30:00Z"),
        }).await.unwrap();

        (request_id, link.id)
    }

    #[tokio::test]
    async fn request_context_loads_request_link_and_requester() {
        let state = fixture_state().await;
        let (request_id, link_id) = seed_request_chain(&state).await;

        let context = load_invitation_request_context(&state, request_id).await.unwrap();

        assert_eq!(context.request.id, request_id);
        assert_eq!(context.link.id, link_id);
        assert_eq!(context.requester.user_id, 8);
        assert_eq!(context.requester.login, "alice");
    }

    #[tokio::test]
    async fn missing_request_returns_not_found() {
        let state = fixture_state().await;

        let err = load_invitation_request_context(&state, RequestId::new()).await.unwrap_err();

        assert!(matches!(err, HandlerError::Storage(storage::Error::NotFound)));
    }
}
```

Add the module declaration to `crates/restate-svc/src/lib.rs`:

```rust
pub mod invitation_context;
```

- [ ] **Step 2: Run failing request-context tests**

Run: `cargo test -p restate-svc invitation_context::tests`

Expected: FAIL because `load_invitation_request_context` returns the placeholder invariant error.

- [ ] **Step 3: Implement request-context loader**

Replace the placeholder loader with:

```rust
pub(crate) async fn load_invitation_request_context(
    state: &AppState,
    request_id: RequestId,
) -> Result<InvitationRequestContext> {
    let request = state
        .storage
        .get_invitation_request(request_id)
        .await?
        .ok_or(HandlerError::Storage(storage::Error::NotFound))?;
    let link = state
        .storage
        .get_invitation_link_by_id(request.invitation_link_id)
        .await?
        .ok_or(HandlerError::Storage(storage::Error::NotFound))?;
    let requester = state
        .storage
        .get_user(request.requester_id)
        .await?
        .ok_or(HandlerError::Storage(storage::Error::NotFound))?;

    Ok(InvitationRequestContext {
        request,
        link,
        requester,
    })
}
```

- [ ] **Step 4: Run request-context tests green**

Run: `cargo test -p restate-svc invitation_context::tests`

Expected: PASS.

---

### Task 2: GitHub Invitation Context Loader

**Files:**
- Modify: `crates/restate-svc/src/invitation_context.rs`

- [ ] **Step 1: Add failing GitHub-context tests**

Add `GithubInvitationContext`, placeholder loader signatures, and tests to `crates/restate-svc/src/invitation_context.rs`:

```rust
use domain::{Account, GithubInvitation, GithubInvitationId, RepositoryIdentity, InvitationLinkRepo};

#[derive(Clone, Debug)]
pub(crate) struct GithubInvitationContext {
    pub invitation: GithubInvitation,
    pub request: InvitationRequest,
    pub link: InvitationLink,
    pub repo: InvitationLinkRepo,
    pub repository: RepositoryIdentity,
    pub requester: User,
    pub account: Account,
}

#[derive(Clone, Debug)]
pub(crate) struct GithubInvitationAccountContext {
    pub invitation: GithubInvitation,
    pub request: InvitationRequest,
    pub link: InvitationLink,
    pub requester: User,
    pub account: Account,
}

pub(crate) async fn load_installation_account(
    _state: &AppState,
    _installation_id: u64,
) -> Result<Account> {
    Err(HandlerError::Invariant("red test stub".into()))
}

pub(crate) async fn load_github_invitation_context(
    _state: &AppState,
    _invitation_id: GithubInvitationId,
    _expected_installation_id: u64,
) -> Result<GithubInvitationContext> {
    Err(HandlerError::Invariant("red test stub".into()))
}

pub(crate) async fn load_github_invitation_account_context(
    _state: &AppState,
    _invitation_id: GithubInvitationId,
) -> Result<GithubInvitationAccountContext> {
    Err(HandlerError::Invariant("red test stub".into()))
}

pub(crate) async fn load_github_invitation_context_for_account(
    _state: &AppState,
    _account: &Account,
    _invitation: &GithubInvitation,
) -> Result<GithubInvitationContext> {
    Err(HandlerError::Invariant("red test stub".into()))
}
```

Add these tests. Use private helper functions for impossible foreign-key states: `load_invitation_link_for_request` can be tested with a synthetic `InvitationRequest` whose `invitation_link_id` does not exist, and `load_requester_for_request` can be tested with a synthetic `InvitationRequest` whose `requester_id` does not exist.

```rust
#[tokio::test]
async fn github_context_loads_invitation_request_link_repo_requester_and_account() {
    let state = fixture_state().await;
    let (request_id, link_id) = seed_request_chain(&state).await;
    let invitation_id = seed_github_invitation(&state, request_id, 10).await;

    let context = load_github_invitation_context(&state, invitation_id, 9).await.unwrap();

    assert_eq!(context.invitation.id, invitation_id);
    assert_eq!(context.request.id, request_id);
    assert_eq!(context.link.id, link_id);
    assert_eq!(context.repo.repo_id, 10);
    assert_eq!(context.repository.owner(), "acme");
    assert_eq!(context.repository.name(), "api");
    assert_eq!(context.requester.login, "alice");
    assert_eq!(context.account.account_id, 100);
}

#[tokio::test]
async fn missing_invitation_link_returns_not_found() {
    let state = fixture_state().await;
    let request = domain::InvitationRequest {
        id: RequestId::new(),
        invitation_link_id: InvitationLinkId::new(),
        requester_id: 8,
        justification: None,
        state: domain::RequestState::Approved,
        decided_by: Some(7),
        decided_at: Some(dt("2026-05-04T13:00:00Z")),
        decline_reason: None,
        created_at: dt("2026-05-04T12:30:00Z"),
    };

    let err = load_invitation_link_for_request(&state, &request).await.unwrap_err();

    assert!(matches!(err, HandlerError::Storage(storage::Error::NotFound)));
}

#[tokio::test]
async fn missing_requester_returns_not_found() {
    let state = fixture_state().await;
    let request = domain::InvitationRequest {
        id: RequestId::new(),
        invitation_link_id: InvitationLinkId::new(),
        requester_id: 8,
        justification: None,
        state: domain::RequestState::Approved,
        decided_by: Some(7),
        decided_at: Some(dt("2026-05-04T13:00:00Z")),
        decline_reason: None,
        created_at: dt("2026-05-04T12:30:00Z"),
    };

    let err = load_requester_for_request(&state, &request).await.unwrap_err();

    assert!(matches!(err, HandlerError::Storage(storage::Error::NotFound)));
}

#[tokio::test]
async fn github_context_missing_repo_returns_invariant() {
    let state = fixture_state().await;
    let (request_id, _) = seed_request_chain_with_repos(&state, vec![]).await;
    let invitation_id = seed_github_invitation(&state, request_id, 10).await;

    let err = load_github_invitation_context(&state, invitation_id, 9).await.unwrap_err();

    assert!(matches!(err, HandlerError::Invariant(_)));
}

#[tokio::test]
async fn github_context_invalid_repo_name_returns_invariant() {
    let state = fixture_state().await;
    let (request_id, _) = seed_request_chain_with_repos(&state, vec![domain::InvitationLinkRepo {
        repo_id: 10,
        repo_full_name: "acme/team/api".into(),
    }]).await;
    let invitation_id = seed_github_invitation(&state, request_id, 10).await;

    let err = load_github_invitation_context(&state, invitation_id, 9).await.unwrap_err();

    assert!(matches!(err, HandlerError::Invariant(_)));
}

#[tokio::test]
async fn github_context_wrong_installation_returns_invariant() {
    let state = fixture_state().await;
    let (request_id, _) = seed_request_chain(&state).await;
    let invitation_id = seed_github_invitation(&state, request_id, 10).await;

    let err = load_github_invitation_context(&state, invitation_id, 10).await.unwrap_err();

    assert!(matches!(err, HandlerError::Invariant(_)));
}

#[tokio::test]
async fn github_account_context_does_not_require_matching_repo() {
    let state = fixture_state().await;
    let (request_id, link_id) = seed_request_chain_with_repos(&state, vec![]).await;
    let invitation_id = seed_github_invitation(&state, request_id, 10).await;

    let context = load_github_invitation_account_context(&state, invitation_id).await.unwrap();

    assert_eq!(context.invitation.id, invitation_id);
    assert_eq!(context.request.id, request_id);
    assert_eq!(context.link.id, link_id);
    assert_eq!(context.requester.login, "alice");
    assert_eq!(context.account.account_id, 100);
}

#[tokio::test]
async fn github_context_wrong_account_returns_invariant() {
    let state = fixture_state().await;
    let (request_id, _) = seed_request_chain(&state).await;
    let invitation_id = seed_github_invitation(&state, request_id, 10).await;
    let invitation = state.storage.get_github_invitation(invitation_id).await.unwrap().unwrap();
    let mut account = load_installation_account(&state, 9).await.unwrap();
    account.account_id = 101;

    let err = load_github_invitation_context_for_account(&state, &account, &invitation)
        .await
        .unwrap_err();

    assert!(matches!(err, HandlerError::Invariant(_)));
}
```

Add helper functions `seed_request_chain_with_repos` and `seed_github_invitation` in the test module so these tests compile independently.

- [ ] **Step 2: Run failing GitHub-context tests**

Run: `cargo test -p restate-svc invitation_context::tests::github_context`

Expected: FAIL because the GitHub-context loaders return placeholder invariant errors.

- [ ] **Step 3: Implement GitHub-context loaders**

Implement the loader helpers with these rules:

```rust
pub(crate) async fn load_installation_account(
    state: &AppState,
    installation_id: u64,
) -> Result<Account> {
    state
        .storage
        .get_installation(installation_id)
        .await?
        .ok_or(HandlerError::Storage(storage::Error::NotFound))
}

pub(crate) async fn load_github_invitation_context(
    state: &AppState,
    invitation_id: GithubInvitationId,
    expected_installation_id: u64,
) -> Result<GithubInvitationContext> {
    let invitation = state
        .storage
        .get_github_invitation(invitation_id)
        .await?
        .ok_or(HandlerError::Storage(storage::Error::NotFound))?;
    let context = load_github_invitation_context_for_loaded(state, &invitation).await?;
    if context.link.installation_id != expected_installation_id {
        return Err(HandlerError::Invariant(format!(
            "github_invitation {} expected installation {} but link uses installation {}",
            invitation_id, expected_installation_id, context.link.installation_id
        )));
    }
    Ok(context)
}
```

Use an internal helper that takes an already-loaded `GithubInvitation`, calls `load_invitation_request_context`, finds the matching repo by `row.repo_id`, parses `RepositoryIdentity`, loads the installation account from `link.installation_id`, and validates any expected account/installation rules before returning `GithubInvitationContext`.

Implement `load_github_invitation_account_context` by loading the GitHub Invitation row, request context, and installation account without resolving the matching repository.

- [ ] **Step 4: Run GitHub-context tests green**

Run: `cargo test -p restate-svc invitation_context`

Expected: PASS.

---

### Task 3: Wire Restate Consumers

**Files:**
- Modify: `crates/restate-svc/src/invitation_request.rs`
- Modify: `crates/restate-svc/src/github_invitation.rs`
- Modify: `crates/restate-svc/src/reconcile.rs`

- [ ] **Step 1: Run existing behavior tests before edits**

Run: `cargo test -p restate-svc github_invitation && cargo test -p restate-svc invitation_request && cargo test -p restate-svc reconcile`

Expected: PASS before consumer rewiring.

- [ ] **Step 2: Update `build_dispatch_inputs`**

Replace the manual request/link/requester lookup in `build_dispatch_inputs` with:

```rust
let context = crate::invitation_context::load_invitation_request_context(state, request_id).await?;

Ok(context
    .link
    .repos
    .into_iter()
    .map(|r| crate::github_invitation::CreateInvitationInput {
        invitation_id: domain::GithubInvitationId::new(),
        invitation_request_id: context.request.id,
        installation_id: context.link.installation_id,
        repo_id: r.repo_id,
        repo_full_name: r.repo_full_name,
        recipient_login: context.requester.login.clone(),
        permission: context.link.permission,
        now,
    })
    .collect())
```

- [ ] **Step 3: Update `github_invitation.rs` consumers**

Use `crate::invitation_context` helpers:

```rust
let account = crate::invitation_context::load_installation_account(state, input.installation_id).await?;
let account_id = account.account_id;
```

For webhook:

```rust
let context = crate::invitation_context::load_github_invitation_account_context(
    state,
    input.invitation_id,
).await?;
```

Use this account context for audit account id resolution only; do not require repository resolution in webhook handling.

For cancel and expire:

```rust
let context = crate::invitation_context::load_github_invitation_context(
    state,
    input.invitation_id,
    input.installation_id,
).await?;
```

Then replace `link.account_id`, `repo.repo_full_name`, `repository.owner()`, and `repository.name()` reads with `context.link.account_id`, `context.repo.repo_full_name`, `context.repository.owner()`, and `context.repository.name()`.

- [ ] **Step 4: Update `reconcile.rs` consumer**

Replace the manual lookup chain at the top of `reconcile_single` with:

```rust
let context = crate::invitation_context::load_github_invitation_context_for_account(
    state,
    acct,
    row,
).await?;
```

Then use `context.repository`, `context.requester.login`, and `context.link.account_id` in the existing GitHub calls and audit emission.

- [ ] **Step 5: Run behavior tests after rewiring**

Run: `cargo test -p restate-svc github_invitation && cargo test -p restate-svc invitation_request && cargo test -p restate-svc reconcile`

Expected: PASS.

---

### Task 4: Final Verification

**Files:**
- All changed files

- [ ] **Step 1: Format**

Run: `cargo fmt`

Expected: exit 0.

- [ ] **Step 2: Focused tests**

Run: `cargo test -p restate-svc invitation_context && cargo test -p restate-svc github_invitation && cargo test -p restate-svc invitation_request && cargo test -p restate-svc reconcile`

Expected: PASS.

- [ ] **Step 3: Crate tests**

Run: `cargo test -p restate-svc`

Expected: PASS.

- [ ] **Step 4: Review diff**

Run: `git diff --stat && git diff --check`

Expected: changed files match this plan and no whitespace errors.
