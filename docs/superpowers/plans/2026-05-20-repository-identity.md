# Repository Identity Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a domain `RepositoryIdentity` value type and use it in invitation workflow GitHub-call paths instead of ad hoc repository full-name splitting.

**Architecture:** `domain` owns repository identity parsing and validation. `restate-svc` maps invalid repository identities into existing invariant handler errors at GitHub-call boundaries. Storage records, schemas, Restate payload JSON, and web display keep using the existing `repo_full_name` string.

**Tech Stack:** Rust 2024 workspace, `thiserror`, `serde`, `schemars`, Restate SDK, existing `github::InstallationClient` URL path encoding.

---

## File Structure

- Create `crates/domain/src/repository_identity.rs`: focused value type for parsing and exposing GitHub repository owner/name while preserving display full name.
- Modify `crates/domain/src/lib.rs`: publish the new module and re-export the value type and error.
- Modify `crates/restate-svc/src/github_invitation.rs`: replace the local `split_full_name` helper and its call sites with `RepositoryIdentity`.
- Modify `crates/restate-svc/src/reconcile.rs`: replace direct `split_once('/')` parsing with `RepositoryIdentity`.
- Existing tests in `crates/github/src/installation.rs` already cover URL path encoding separation through `get_repo_uses_path_encoded_segments`.

## Important Constraints

- Do not change `ShareLinkRepo.repo_full_name: String`.
- Do not change storage schemas, migrations, storage adapters, Restate payload shapes, or UI display behavior.
- Do not move GitHub URL path encoding into `domain`; keep encoding inside the GitHub adapter.
- Do not commit unless the maintainer explicitly authorizes commits in the active session.

### Task 1: Add Domain Repository Identity

**Files:**
- Create: `crates/domain/src/repository_identity.rs`
- Modify: `crates/domain/src/lib.rs`
- Test: `crates/domain/src/repository_identity.rs`

- [ ] **Step 1: Write the failing domain tests and value-type skeleton**

Create `crates/domain/src/repository_identity.rs` with this complete initial content:

```rust
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RepositoryIdentity {
    full_name: String,
    separator: usize,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum RepositoryIdentityError {
    #[error("repository full name must be in owner/name form")]
    MissingSeparator,
    #[error("repository owner must not be empty")]
    EmptyOwner,
    #[error("repository name must not be empty")]
    EmptyName,
    #[error("repository full name must contain exactly one separator")]
    TooManySeparators,
}

impl RepositoryIdentity {
    pub fn parse(full_name: impl Into<String>) -> Result<Self, RepositoryIdentityError> {
        let full_name = full_name.into();
        let separator = full_name.find('/').ok_or(RepositoryIdentityError::MissingSeparator)?;
        Ok(Self {
            full_name,
            separator,
        })
    }

    pub fn owner(&self) -> &str {
        &self.full_name[..self.separator]
    }

    pub fn name(&self) -> &str {
        &self.full_name[self.separator + 1..]
    }

    pub fn full_name(&self) -> &str {
        &self.full_name
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_identity_exposes_owner_name_and_full_name() {
        let identity = RepositoryIdentity::parse("acme/api").unwrap();

        assert_eq!(identity.owner(), "acme");
        assert_eq!(identity.name(), "api");
        assert_eq!(identity.full_name(), "acme/api");
    }

    #[test]
    fn missing_slash_is_rejected() {
        assert_eq!(
            RepositoryIdentity::parse("acme-api").unwrap_err(),
            RepositoryIdentityError::MissingSeparator
        );
    }

    #[test]
    fn empty_owner_is_rejected() {
        assert_eq!(
            RepositoryIdentity::parse("/api").unwrap_err(),
            RepositoryIdentityError::EmptyOwner
        );
    }

    #[test]
    fn empty_repository_name_is_rejected() {
        assert_eq!(
            RepositoryIdentity::parse("acme/").unwrap_err(),
            RepositoryIdentityError::EmptyName
        );
    }

    #[test]
    fn multiple_slashes_are_rejected() {
        assert_eq!(
            RepositoryIdentity::parse("acme/team/api").unwrap_err(),
            RepositoryIdentityError::TooManySeparators
        );
    }

    #[test]
    fn display_full_name_is_preserved_exactly() {
        let identity = RepositoryIdentity::parse("Acme-Inc/api gateway").unwrap();

        assert_eq!(identity.owner(), "Acme-Inc");
        assert_eq!(identity.name(), "api gateway");
        assert_eq!(identity.full_name(), "Acme-Inc/api gateway");
    }
}
```

Modify `crates/domain/src/lib.rs` to expose the module and public types:

```rust
//! Pure domain types and state derivations. No I/O.

pub mod account;
pub mod github_invitation;
pub mod ids;
pub mod invitation_request;
pub mod permission;
pub mod repository_identity;
pub mod share_link;
pub mod slug;
pub mod user;

// Re-exports filled in as each module gains its public types:
pub use account::{Account, AccountType, SelectedRepos};
pub use github_invitation::{GithubInvitation, InvitationState};
pub use ids::{AuditEventId, GithubInvitationId, RequestId, ShareLinkId};
pub use invitation_request::{InvitationRequest, RequestState};
pub use permission::Permission;
pub use repository_identity::{RepositoryIdentity, RepositoryIdentityError};
pub use share_link::{ShareLink, ShareLinkRepo};
pub use slug::Slug;
pub use user::User;
```

- [ ] **Step 2: Run the focused domain tests to verify they fail**

Run: `cargo test -p domain repository_identity`

Expected: FAIL. The first failure should show `empty_owner_is_rejected`, `empty_repository_name_is_rejected`, or `multiple_slashes_are_rejected` returning `Ok(...)` instead of the expected error.

- [ ] **Step 3: Implement full validation**

Replace the `parse` method in `crates/domain/src/repository_identity.rs` with:

```rust
    pub fn parse(full_name: impl Into<String>) -> Result<Self, RepositoryIdentityError> {
        let full_name = full_name.into();
        let separator = full_name
            .find('/')
            .ok_or(RepositoryIdentityError::MissingSeparator)?;
        if separator == 0 {
            return Err(RepositoryIdentityError::EmptyOwner);
        }
        if separator + 1 == full_name.len() {
            return Err(RepositoryIdentityError::EmptyName);
        }
        if full_name[separator + 1..].contains('/') {
            return Err(RepositoryIdentityError::TooManySeparators);
        }
        Ok(Self {
            full_name,
            separator,
        })
    }
```

- [ ] **Step 4: Run the focused domain tests to verify they pass**

Run: `cargo test -p domain repository_identity`

Expected: PASS. All six `repository_identity` tests pass.

- [ ] **Step 5: Inspect diff checkpoint**

Run: `git diff -- crates/domain/src/lib.rs crates/domain/src/repository_identity.rs`

Expected: Diff only contains the new domain module and the `domain::lib` export. Do not commit unless the maintainer explicitly authorizes commits.

### Task 2: Adopt Repository Identity In GitHub Invitation Workflow Paths

**Files:**
- Modify: `crates/restate-svc/src/github_invitation.rs`
- Test: `crates/restate-svc/src/github_invitation.rs`

- [ ] **Step 1: Extend the invalid input test to cover empty pieces and multiple slashes**

In `crates/restate-svc/src/github_invitation.rs`, replace the existing `create_with_bad_repo_full_name_is_invariant` test with:

```rust
    #[tokio::test]
    async fn create_with_bad_repo_full_name_is_invariant() {
        for bad_full_name in ["no-slash", "/api", "acme/", "acme/team/api"] {
            let storage = fixture_storage().await;
            let mock = MockTransport::scripted(vec![]);
            let github = fixture_github_client(Arc::new(mock));
            let state = AppState::new(storage, github);
            let req_id = seed_chain(&state).await;

            let inv_id = GithubInvitationId::new();
            let mut input = sample_input(inv_id, req_id);
            input.repo_full_name = bad_full_name.into();
            let err = create_logic(&state, &input, None).await.unwrap_err();

            assert!(
                matches!(err, HandlerError::Invariant(_)),
                "{bad_full_name:?} should be an invariant error, got {err:?}"
            );
        }
    }
```

- [ ] **Step 2: Run the focused test to verify it fails before adoption**

Run: `cargo test -p restate-svc create_with_bad_repo_full_name_is_invariant`

Expected: FAIL. `"/api"`, `"acme/"`, or `"acme/team/api"` should not yet be classified as invalid by the old helper.

- [ ] **Step 3: Add a local conversion helper and replace `create_logic` parsing**

In `crates/restate-svc/src/github_invitation.rs`, update the domain imports near the top from:

```rust
use domain::{GithubInvitationId, InvitationState, RequestId};
```

to:

```rust
use domain::{GithubInvitationId, InvitationState, RepositoryIdentity, RequestId};
```

Delete the local `split_full_name` function and replace it with:

```rust
fn repository_identity(full_name: &str) -> crate::error::Result<RepositoryIdentity> {
    RepositoryIdentity::parse(full_name.to_owned()).map_err(|err| {
        crate::error::HandlerError::Invariant(format!(
            "invalid repo_full_name {full_name:?}: {err}"
        ))
    })
}
```

In `create_logic`, replace:

```rust
    let (owner, repo) = split_full_name(&input.repo_full_name)?;

    let outcome = state
        .github
        .add_collaborator(
            input.installation_id,
            owner,
            repo,
            &input.recipient_login,
            input.permission,
        )
        .await;
```

with:

```rust
    let repository = repository_identity(&input.repo_full_name)?;

    let outcome = state
        .github
        .add_collaborator(
            input.installation_id,
            repository.owner(),
            repository.name(),
            &input.recipient_login,
            input.permission,
        )
        .await;
```

- [ ] **Step 4: Replace `cancel_logic` parsing**

In `cancel_logic`, replace:

```rust
    let (owner, repo_name) = split_full_name(&repo.repo_full_name)?;
```

with:

```rust
    let repository = repository_identity(&repo.repo_full_name)?;
```

Then replace the `delete_invitation` call arguments:

```rust
            .github
            .delete_invitation(input.installation_id, owner, repo_name, github_id)
            .await
```

with:

```rust
            .github
            .delete_invitation(
                input.installation_id,
                repository.owner(),
                repository.name(),
                github_id,
            )
            .await
```

- [ ] **Step 5: Replace `tick_expire_logic` parsing**

In `tick_expire_logic`, replace:

```rust
    let (owner, repo_name) = split_full_name(&repo.repo_full_name)?;

    let pending = state
        .github
        .list_invitations(input.installation_id, owner, repo_name)
        .await?;
```

with:

```rust
    let repository = repository_identity(&repo.repo_full_name)?;

    let pending = state
        .github
        .list_invitations(
            input.installation_id,
            repository.owner(),
            repository.name(),
        )
        .await?;
```

- [ ] **Step 6: Run the focused Restate service tests**

Run: `cargo test -p restate-svc github_invitation`

Expected: PASS. Existing invitation workflow behavior remains unchanged except stricter invalid full-name invariant validation.

- [ ] **Step 7: Inspect diff checkpoint**

Run: `git diff -- crates/restate-svc/src/github_invitation.rs`

Expected: Diff removes `split_full_name`, adds `RepositoryIdentity`, and updates only GitHub-call owner/name extraction. Do not commit unless the maintainer explicitly authorizes commits.

### Task 3: Adopt Repository Identity In Reconciliation

**Files:**
- Modify: `crates/restate-svc/src/reconcile.rs`
- Test: `crates/restate-svc/src/reconcile.rs`

- [ ] **Step 1: Add a reconciliation fixture parameter for stored repo full names**

In `crates/restate-svc/src/reconcile.rs`, rename the existing test helper:

```rust
    async fn seed_one_pending(state: &AppState) -> GithubInvitationId {
```

to:

```rust
    async fn seed_one_pending_with_repo_full_name(
        state: &AppState,
        repo_full_name: &str,
    ) -> GithubInvitationId {
```

Inside that helper, replace the share-link repository literal:

```rust
                repo_full_name: "acme/api".into(),
```

with:

```rust
                repo_full_name: repo_full_name.into(),
```

After the renamed helper, add this wrapper so existing tests keep using the valid repository fixture:

```rust
    async fn seed_one_pending(state: &AppState) -> GithubInvitationId {
        seed_one_pending_with_repo_full_name(state, "acme/api").await
    }
```

- [ ] **Step 2: Add a reconciliation invariant test for invalid stored repo names**

In `crates/restate-svc/src/reconcile.rs`, add this test inside the existing `#[cfg(test)] mod tests` block after `seed_one_pending`:

```rust
    #[tokio::test]
    async fn daily_run_continues_when_repo_full_name_is_invalid() {
        let storage = fixture_storage().await;
        let mock = MockTransport::scripted(vec![]);
        let github = fixture_github_client(Arc::new(mock));
        let state = AppState::new(storage, github);
        let inv_id = seed_one_pending_with_repo_full_name(&state, "acme/team/api").await;

        daily_run_logic(
            &state,
            &DailyRunInput {
                at: dt("2026-05-05T13:00:00Z"),
            },
            None,
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
    }
```

- [ ] **Step 3: Run the focused reconciliation test to verify it fails before adoption**

Run: `cargo test -p restate-svc daily_run_continues_when_repo_full_name_is_invalid`

Expected: FAIL. The old `split_once('/')` parsing treats `"acme/team/api"` as owner `"acme"` and repo `"team/api"`, so the mock GitHub transport receives an unexpected request instead of the row being skipped as a terminal invariant.

- [ ] **Step 4: Replace direct splitting with `RepositoryIdentity`**

In `crates/restate-svc/src/reconcile.rs`, update imports from:

```rust
use domain::InvitationState;
```

to:

```rust
use domain::{InvitationState, RepositoryIdentity};
```

Replace:

```rust
    let (owner, repo_name) = repo.repo_full_name.split_once('/').ok_or_else(|| {
        HandlerError::Invariant(format!("bad repo full name: {}", repo.repo_full_name))
    })?;
```

with:

```rust
    let repository = RepositoryIdentity::parse(repo.repo_full_name.clone()).map_err(|err| {
        HandlerError::Invariant(format!(
            "invalid repo_full_name {:?}: {err}",
            repo.repo_full_name
        ))
    })?;
```

Replace the `list_invitations` call:

```rust
        .list_invitations(acct.installation_id, owner, repo_name)
```

with:

```rust
        .list_invitations(acct.installation_id, repository.owner(), repository.name())
```

Replace the `is_collaborator` call:

```rust
        .is_collaborator(acct.installation_id, owner, repo_name, &recipient.login)
```

with:

```rust
        .is_collaborator(
            acct.installation_id,
            repository.owner(),
            repository.name(),
            &recipient.login,
        )
```

- [ ] **Step 5: Run reconciliation tests**

Run: `cargo test -p restate-svc reconcile`

Expected: PASS. Existing reconciliation behavior is unchanged for valid names, and invalid repository identities are classified as terminal invariants for the affected row.

- [ ] **Step 6: Inspect diff checkpoint**

Run: `git diff -- crates/restate-svc/src/reconcile.rs`

Expected: Diff replaces direct `split_once('/')` parsing with `RepositoryIdentity` and does not alter reconciliation state transitions. Do not commit unless the maintainer explicitly authorizes commits.

### Task 4: Verify Separation From GitHub URL Encoding And Full Workspace Behavior

**Files:**
- Test existing: `crates/github/src/installation.rs`
- Verify: workspace packages `domain`, `restate-svc`, `github`

- [ ] **Step 1: Run GitHub adapter URL path encoding test**

Run: `cargo test -p github get_repo_uses_path_encoded_segments`

Expected: PASS. This confirms owner/name path segment encoding remains in the GitHub adapter, not in `domain::RepositoryIdentity`.

- [ ] **Step 2: Run package verification from the spec**

Run: `cargo test -p domain -p restate-svc -p github`

Expected: PASS. Domain identity tests, Restate service invitation/reconciliation tests, and GitHub adapter tests pass.

- [ ] **Step 3: Format check**

Run: `cargo fmt --check`

Expected: PASS. If it fails, run `cargo fmt`, then rerun `cargo fmt --check`.

- [ ] **Step 4: Search for removed ad hoc parsing**

Run: `rg "split_full_name|repo_full_name\.split_once|split_once\('/'\)" crates/restate-svc/src`

Expected: No matches in invitation or reconciliation workflow paths. Any remaining match must be unrelated to repository full-name parsing and should be explained before finishing.

- [ ] **Step 5: Final diff review**

Run: `git diff -- crates/domain/src/lib.rs crates/domain/src/repository_identity.rs crates/restate-svc/src/github_invitation.rs crates/restate-svc/src/reconcile.rs docs/superpowers/specs/2026-05-20-repository-identity-design.md docs/superpowers/plans/2026-05-20-repository-identity.md`

Expected: Diff matches the spec: one new domain type, workflow adoption, tests, design doc, and plan doc. No storage schema, migration, web UI, or GitHub adapter behavior changes.

## Self-Review Notes

- Spec coverage: domain validation, display preservation, workflow adoption, storage-shape preservation, GitHub URL encoding separation, and verification commands are covered by Tasks 1-4.
- Placeholder scan: no open-ended implementation placeholders remain.
- Type consistency: the plan consistently uses `RepositoryIdentity::parse`, `owner()`, `name()`, `full_name()`, and `RepositoryIdentityError`.
