# Account Admin Request Queue Reads Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a focused Account Admin read module for pending request queues and ownership-sensitive dashboard lookups.

**Architecture:** Keep the read module inside `crates/web` because the dashboard routes are its only current consumers. The module depends on `storage::Storage` and domain records, returns route-ready read models, and maps ownership-sensitive misses to the existing generic `WebError::NotFound` policy.

**Tech Stack:** Rust 2024, axum, Dioxus SSR, async-trait, in-memory `storage::SqlxStorage` tests, existing `storage::Storage` trait.

---

## File Structure

- Create `crates/web/src/account_admin_reads.rs`: Account Admin read models, ownership-sensitive lookup functions, pending request queue assembly, and focused unit tests.
- Modify `crates/web/src/lib.rs`: declare the new internal module so routes can use it.
- Modify `crates/web/src/routes/dashboard.rs`: replace inline Account Admin lookup chains with calls to `account_admin_reads`.
- No changes to `storage::Storage`, D1, SQLite migrations, Restate commands, views, or route paths.

---

### Task 1: Invitation Link Ownership Read

**Files:**
- Create: `crates/web/src/account_admin_reads.rs`
- Modify: `crates/web/src/lib.rs`

- [ ] **Step 1: Declare the module and write failing Invitation Link ownership tests**

In `crates/web/src/lib.rs`, add the module declaration before `pub mod commands;`:

```rust
pub(crate) mod account_admin_reads;
```

Create `crates/web/src/account_admin_reads.rs` with this test scaffold:

```rust
#[cfg(test)]
mod tests {
    use chrono::{DateTime, Utc};
    use domain::{
        Account, AccountType, Permission, SelectedRepos, InvitationLink, InvitationLinkId, InvitationLinkRepo,
        Slug, User,
    };
    use storage::Storage;

    fn dt(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn sample_account(installation_id: u64, account_id: u64, login: &str) -> Account {
        Account {
            installation_id,
            account_id,
            account_login: login.into(),
            account_type: AccountType::Organization,
            installed_at: dt("2026-05-04T12:00:00Z"),
            uninstalled_at: None,
            selected_repos: SelectedRepos::All,
        }
    }

    fn sample_user(user_id: u64, login: &str) -> User {
        User {
            user_id,
            login: login.into(),
            avatar_url: None,
            last_seen_at: dt("2026-05-04T12:00:00Z"),
        }
    }

    fn sample_link(
        account_id: u64,
        installation_id: u64,
        created_by: u64,
        slug: &str,
    ) -> InvitationLink {
        InvitationLink {
            id: InvitationLinkId::new(),
            slug: Slug::from_string(slug.to_string()).unwrap(),
            installation_id,
            account_id,
            created_by,
            created_at: dt("2026-05-04T12:00:00Z"),
            expires_at: None,
            max_uses: None,
            uses_count: 0,
            permission: Permission::Pull,
            approval_required: true,
            internal_note: None,
            revoked_at: None,
            revoked_by: None,
            repos: vec![InvitationLinkRepo {
                repo_id: 10,
                repo_full_name: "acme/api".into(),
            }],
        }
    }

    async fn storage_with_accounts() -> storage::SqlxStorage {
        let storage = storage::SqlxStorage::in_memory().await.unwrap();
        storage
            .insert_installation(&sample_account(1, 9001, "acme"))
            .await
            .unwrap();
        storage
            .insert_installation(&sample_account(2, 9002, "other"))
            .await
            .unwrap();
        storage.upsert_user(&sample_user(701, "creator")).await.unwrap();
        storage
    }

    #[tokio::test]
    async fn account_admin_invitation_link_lookup_returns_owning_accounts_link() {
        let storage = storage_with_accounts().await;
        let link = sample_link(9001, 1, 701, "QueueSlug0000001");
        storage.insert_invitation_link(&link).await.unwrap();

        let found = super::find_account_admin_invitation_link(&storage, 9001, link.id)
            .await
            .unwrap();

        assert_eq!(found.id, link.id);
        assert_eq!(found.account_id, 9001);
        assert_eq!(found.slug.as_str(), "QueueSlug0000001");
    }

    #[tokio::test]
    async fn account_admin_invitation_link_lookup_hides_wrong_account_link() {
        let storage = storage_with_accounts().await;
        let link = sample_link(9002, 2, 701, "QueueSlug0000002");
        storage.insert_invitation_link(&link).await.unwrap();

        let err = super::find_account_admin_invitation_link(&storage, 9001, link.id)
            .await
            .unwrap_err();

        assert!(matches!(err, crate::WebError::NotFound));
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p web account_admin_invitation_link_lookup`

Expected: FAIL because `find_account_admin_invitation_link` is not defined.

- [ ] **Step 3: Implement the Invitation Link ownership read**

Insert this implementation above the existing `#[cfg(test)] mod tests` block in `crates/web/src/account_admin_reads.rs`:

```rust
use crate::error::{Result, WebError};
use chrono::{DateTime, Utc};
use domain::{InvitationRequest, RequestId, InvitationLink, InvitationLinkId};
use storage::Storage;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AccountAdminPendingRequestRow {
    pub(crate) request_id: RequestId,
    pub(crate) link_slug: String,
    pub(crate) link_id: Option<InvitationLinkId>,
    pub(crate) requester_login: String,
    pub(crate) justification: Option<String>,
    pub(crate) created_at: DateTime<Utc>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AccountAdminInvitationRequest {
    pub(crate) request: InvitationRequest,
    pub(crate) invitation_link: InvitationLink,
}

pub(crate) async fn find_account_admin_invitation_link(
    storage: &dyn Storage,
    account_id: u64,
    link_id: InvitationLinkId,
) -> Result<InvitationLink> {
    let link = storage
        .get_invitation_link_by_id(link_id)
        .await?
        .ok_or(WebError::NotFound)?;

    if link.account_id != account_id {
        return Err(WebError::NotFound);
    }

    Ok(link)
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p web account_admin_invitation_link_lookup`

Expected: PASS. The read function returns the owning account's Invitation Link and hides wrong-account access behind `NotFound`.

- [ ] **Step 5: Commit**

```bash
git add crates/web/src/lib.rs crates/web/src/account_admin_reads.rs
git commit -m "feat(web): add account admin invitation link reads"
```

---

### Task 2: Invitation Request Ownership Read

**Files:**
- Modify: `crates/web/src/account_admin_reads.rs`

- [ ] **Step 1: Add failing request ownership tests**

Inside the `#[cfg(test)] mod tests` block in `crates/web/src/account_admin_reads.rs`, extend the domain import:

```rust
use domain::{
    Account, AccountType, InvitationRequest, Permission, RequestId, RequestState, SelectedRepos,
    InvitationLink, InvitationLinkId, InvitationLinkRepo, Slug, User,
};
```

Add this helper below `sample_link`:

```rust
fn sample_request(
    id: RequestId,
    invitation_link_id: InvitationLinkId,
    requester_id: u64,
    created_at: &str,
) -> InvitationRequest {
    InvitationRequest {
        id,
        invitation_link_id,
        requester_id,
        justification: Some("need repository access".into()),
        state: RequestState::Pending,
        decided_by: None,
        decided_at: None,
        decline_reason: None,
        created_at: dt(created_at),
    }
}
```

Add these tests below the Invitation Link tests:

```rust
#[tokio::test]
async fn account_admin_request_lookup_returns_request_and_link_for_owner() {
    let storage = storage_with_accounts().await;
    storage.upsert_user(&sample_user(802, "requester")).await.unwrap();
    let link = sample_link(9001, 1, 701, "QueueSlug0000003");
    storage.insert_invitation_link(&link).await.unwrap();
    let request_id = RequestId::new();
    let request = sample_request(request_id, link.id, 802, "2026-05-04T12:30:00Z");
    storage
        .insert_invitation_request_and_increment_uses(&request)
        .await
        .unwrap();

    let found = super::find_account_admin_request(&storage, 9001, request_id)
        .await
        .unwrap();

    assert_eq!(found.request.id, request_id);
    assert_eq!(found.request.invitation_link_id, link.id);
    assert_eq!(found.invitation_link.id, link.id);
    assert_eq!(found.invitation_link.account_id, 9001);
}

#[tokio::test]
async fn account_admin_request_lookup_hides_wrong_account_request() {
    let storage = storage_with_accounts().await;
    storage.upsert_user(&sample_user(802, "requester")).await.unwrap();
    let link = sample_link(9002, 2, 701, "QueueSlug0000004");
    storage.insert_invitation_link(&link).await.unwrap();
    let request_id = RequestId::new();
    let request = sample_request(request_id, link.id, 802, "2026-05-04T12:30:00Z");
    storage
        .insert_invitation_request_and_increment_uses(&request)
        .await
        .unwrap();

    let err = super::find_account_admin_request(&storage, 9001, request_id)
        .await
        .unwrap_err();

    assert!(matches!(err, crate::WebError::NotFound));
}

#[tokio::test]
async fn account_admin_request_lookup_hides_missing_request() {
    let storage = storage_with_accounts().await;

    let err = super::find_account_admin_request(&storage, 9001, RequestId::new())
        .await
        .unwrap_err();

    assert!(matches!(err, crate::WebError::NotFound));
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p web account_admin_request_lookup`

Expected: FAIL because `find_account_admin_request` is not defined.

- [ ] **Step 3: Implement the request ownership read**

Insert this function below `find_account_admin_invitation_link`:

```rust
pub(crate) async fn find_account_admin_request(
    storage: &dyn Storage,
    account_id: u64,
    request_id: RequestId,
) -> Result<AccountAdminInvitationRequest> {
    let request = storage
        .get_invitation_request(request_id)
        .await?
        .ok_or(WebError::NotFound)?;
    let invitation_link = find_account_admin_invitation_link(storage, account_id, request.invitation_link_id).await?;

    Ok(AccountAdminInvitationRequest {
        request,
        invitation_link,
    })
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p web account_admin_request_lookup`

Expected: PASS. The read function returns owner-visible request context and hides missing or wrong-account requests.

- [ ] **Step 5: Commit**

```bash
git add crates/web/src/account_admin_reads.rs
git commit -m "feat(web): add account admin request reads"
```

---

### Task 3: Pending Request Queue Read Model

**Files:**
- Modify: `crates/web/src/account_admin_reads.rs`

- [ ] **Step 1: Add failing pending queue tests**

Inside the `#[cfg(test)] mod tests` block in `crates/web/src/account_admin_reads.rs`, add these imports:

```rust
use audit::AuditEvent;
use domain::{GithubInvitation, GithubInvitationId};
use storage::{GithubInvitationUpdate, RequestDecision};
```

Add this fake storage below `storage_with_accounts`:

```rust
#[derive(Default)]
struct FakeStorage {
    pending_requests: Vec<InvitationRequest>,
    request: Option<InvitationRequest>,
    invitation_link: Option<InvitationLink>,
    user: Option<User>,
}

#[async_trait::async_trait]
impl Storage for FakeStorage {
    async fn insert_installation(&self, _account: &Account) -> storage::Result<()> {
        panic!("insert_installation is not used by Account Admin read tests")
    }

    async fn mark_installation_uninstalled(
        &self,
        _installation_id: u64,
        _when: DateTime<Utc>,
    ) -> storage::Result<()> {
        panic!("mark_installation_uninstalled is not used by Account Admin read tests")
    }

    async fn update_installation_repos(
        &self,
        _installation_id: u64,
        _selected: &SelectedRepos,
    ) -> storage::Result<()> {
        panic!("update_installation_repos is not used by Account Admin read tests")
    }

    async fn get_installation(&self, _installation_id: u64) -> storage::Result<Option<Account>> {
        panic!("get_installation is not used by Account Admin read tests")
    }

    async fn get_active_installation_by_account_id(
        &self,
        _account_id: u64,
    ) -> storage::Result<Option<Account>> {
        panic!("get_active_installation_by_account_id is not used by Account Admin read tests")
    }

    async fn get_active_installation_by_login(
        &self,
        _login: &str,
    ) -> storage::Result<Option<Account>> {
        panic!("get_active_installation_by_login is not used by Account Admin read tests")
    }

    async fn list_active_installations(&self) -> storage::Result<Vec<Account>> {
        panic!("list_active_installations is not used by Account Admin read tests")
    }

    async fn upsert_user(&self, _user: &User) -> storage::Result<()> {
        panic!("upsert_user is not used by Account Admin read tests")
    }

    async fn get_user(&self, _user_id: u64) -> storage::Result<Option<User>> {
        Ok(self.user.clone())
    }

    async fn insert_invitation_link(&self, _link: &InvitationLink) -> storage::Result<()> {
        panic!("insert_invitation_link is not used by Account Admin read tests")
    }

    async fn mark_invitation_link_revoked(
        &self,
        _id: InvitationLinkId,
        _by_user: u64,
        _when: DateTime<Utc>,
    ) -> storage::Result<()> {
        panic!("mark_invitation_link_revoked is not used by Account Admin read tests")
    }

    async fn get_invitation_link_by_id(
        &self,
        _id: InvitationLinkId,
    ) -> storage::Result<Option<InvitationLink>> {
        Ok(self.invitation_link.clone())
    }

    async fn get_invitation_link_by_slug(&self, _slug: &str) -> storage::Result<Option<InvitationLink>> {
        panic!("get_invitation_link_by_slug is not used by Account Admin read tests")
    }

    async fn list_invitation_links_for_account(
        &self,
        _account_id: u64,
    ) -> storage::Result<Vec<InvitationLink>> {
        panic!("list_invitation_links_for_account is not used by Account Admin read tests")
    }

    async fn insert_invitation_request_and_increment_uses(
        &self,
        _request: &InvitationRequest,
    ) -> storage::Result<()> {
        panic!("insert_invitation_request_and_increment_uses is not used by Account Admin read tests")
    }

    async fn record_request_decision(&self, _decision: &RequestDecision) -> storage::Result<()> {
        panic!("record_request_decision is not used by Account Admin read tests")
    }

    async fn get_invitation_request(
        &self,
        _id: RequestId,
    ) -> storage::Result<Option<InvitationRequest>> {
        Ok(self.request.clone())
    }

    async fn list_pending_requests_for_account(
        &self,
        _account_id: u64,
    ) -> storage::Result<Vec<InvitationRequest>> {
        Ok(self.pending_requests.clone())
    }

    async fn list_requests_for_link(
        &self,
        _link_id: InvitationLinkId,
    ) -> storage::Result<Vec<InvitationRequest>> {
        panic!("list_requests_for_link is not used by Account Admin read tests")
    }

    async fn insert_github_invitation(
        &self,
        _invitation: &GithubInvitation,
    ) -> storage::Result<()> {
        panic!("insert_github_invitation is not used by Account Admin read tests")
    }

    async fn update_github_invitation(
        &self,
        _update: &GithubInvitationUpdate,
    ) -> storage::Result<()> {
        panic!("update_github_invitation is not used by Account Admin read tests")
    }

    async fn get_github_invitation(
        &self,
        _id: GithubInvitationId,
    ) -> storage::Result<Option<GithubInvitation>> {
        panic!("get_github_invitation is not used by Account Admin read tests")
    }

    async fn get_github_invitation_by_github_id(
        &self,
        _github_id: u64,
    ) -> storage::Result<Option<GithubInvitation>> {
        panic!("get_github_invitation_by_github_id is not used by Account Admin read tests")
    }

    async fn list_pending_github_invitations_for_installation(
        &self,
        _installation_id: u64,
    ) -> storage::Result<Vec<GithubInvitation>> {
        panic!("list_pending_github_invitations_for_installation is not used by Account Admin read tests")
    }

    async fn audit(&self, _event: &AuditEvent) -> storage::Result<()> {
        panic!("audit is not used by Account Admin read tests")
    }
}
```

Add these tests below the request lookup tests:

```rust
#[tokio::test]
async fn account_admin_pending_request_queue_returns_hydrated_rows_oldest_first() {
    let storage = storage_with_accounts().await;
    storage.upsert_user(&sample_user(802, "alice")).await.unwrap();
    storage.upsert_user(&sample_user(803, "bob")).await.unwrap();
    let link_a = sample_link(9001, 1, 701, "QueueSlug0000005");
    let link_b = sample_link(9001, 1, 701, "QueueSlug0000006");
    storage.insert_invitation_link(&link_a).await.unwrap();
    storage.insert_invitation_link(&link_b).await.unwrap();
    let newer = sample_request(
        RequestId::new(),
        link_a.id,
        802,
        "2026-05-04T12:40:00Z",
    );
    let older = sample_request(
        RequestId::new(),
        link_b.id,
        803,
        "2026-05-04T12:30:00Z",
    );
    storage
        .insert_invitation_request_and_increment_uses(&newer)
        .await
        .unwrap();
    storage
        .insert_invitation_request_and_increment_uses(&older)
        .await
        .unwrap();

    let rows = super::pending_request_queue(&storage, 9001).await.unwrap();

    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].request_id, older.id);
    assert_eq!(rows[0].link_slug, "QueueSlug0000006");
    assert_eq!(rows[0].link_id, Some(link_b.id));
    assert_eq!(rows[0].requester_login, "bob");
    assert_eq!(rows[0].justification.as_deref(), Some("need repository access"));
    assert_eq!(rows[1].request_id, newer.id);
    assert_eq!(rows[1].requester_login, "alice");
}

#[tokio::test]
async fn account_admin_pending_request_queue_uses_missing_related_record_fallbacks() {
    let missing_link_id = InvitationLinkId::new();
    let request = sample_request(
        RequestId::new(),
        missing_link_id,
        999_999,
        "2026-05-04T12:30:00Z",
    );
    let storage = FakeStorage {
        pending_requests: vec![request.clone()],
        request: None,
        invitation_link: None,
        user: None,
    };

    let rows = super::pending_request_queue(&storage, 9001).await.unwrap();

    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].request_id, request.id);
    assert_eq!(rows[0].link_slug, "(deleted link)");
    assert_eq!(rows[0].link_id, None);
    assert_eq!(rows[0].requester_login, "user-999999");
}

#[tokio::test]
async fn account_admin_request_lookup_hides_missing_related_invitation_link() {
    let request = sample_request(
        RequestId::new(),
        InvitationLinkId::new(),
        802,
        "2026-05-04T12:30:00Z",
    );
    let storage = FakeStorage {
        pending_requests: Vec::new(),
        request: Some(request.clone()),
        invitation_link: None,
        user: None,
    };

    let err = super::find_account_admin_request(&storage, 9001, request.id)
        .await
        .unwrap_err();

    assert!(matches!(err, crate::WebError::NotFound));
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p web account_admin_pending_request_queue`

Expected: FAIL because `pending_request_queue` is not defined.

- [ ] **Step 3: Implement pending queue assembly**

Insert these functions below `find_account_admin_request`:

```rust
pub(crate) async fn pending_request_queue(
    storage: &dyn Storage,
    account_id: u64,
) -> Result<Vec<AccountAdminPendingRequestRow>> {
    let pending = storage.list_pending_requests_for_account(account_id).await?;
    let mut rows = Vec::with_capacity(pending.len());

    for request in pending {
        let invitation_link = storage
            .get_invitation_link_by_id(request.invitation_link_id)
            .await
            .ok()
            .flatten();
        let requester = storage.get_user(request.requester_id).await.ok().flatten();
        rows.push(pending_request_row(request, invitation_link, requester));
    }

    rows.sort_by(|a, b| a.created_at.cmp(&b.created_at));
    Ok(rows)
}

fn pending_request_row(
    request: InvitationRequest,
    invitation_link: Option<InvitationLink>,
    requester: Option<domain::User>,
) -> AccountAdminPendingRequestRow {
    let (link_slug, link_id) = match invitation_link {
        Some(link) => (link.slug.as_str().to_string(), Some(link.id)),
        None => ("(deleted link)".to_string(), None),
    };
    let requester_login = requester
        .map(|user| user.login)
        .unwrap_or_else(|| format!("user-{}", request.requester_id));

    AccountAdminPendingRequestRow {
        request_id: request.id,
        link_slug,
        link_id,
        requester_login,
        justification: request.justification,
        created_at: request.created_at,
    }
}
```

- [ ] **Step 4: Run pending queue tests**

Run: `cargo test -p web account_admin_pending_request_queue`

Expected: PASS. Queue rows are hydrated, ordered oldest-first, and use explicit missing-record fallbacks.

- [ ] **Step 5: Run all Account Admin read tests**

Run: `cargo test -p web account_admin`

Expected: PASS. All tests in `account_admin_reads` pass.

- [ ] **Step 6: Commit**

```bash
git add crates/web/src/account_admin_reads.rs
git commit -m "feat(web): add account admin pending queue reads"
```

---

### Task 4: Dashboard Route Integration

**Files:**
- Modify: `crates/web/src/routes/dashboard.rs`

- [ ] **Step 1: Import the Account Admin read functions**

In `crates/web/src/routes/dashboard.rs`, add this import after the existing `use crate::commands::{...};` block:

```rust
use crate::account_admin_reads::{
    find_account_admin_request, find_account_admin_invitation_link, pending_request_queue,
};
```

- [ ] **Step 2: Wire `link_detail` to the shared Invitation Link lookup**

Replace the inline storage lookup in `link_detail` with:

```rust
let link = match find_account_admin_invitation_link(
    state.storage.as_ref(),
    admin.account.account_id,
    link_id,
)
.await
{
    Ok(link) => link,
    Err(e) => return e.into_response(),
};
```

- [ ] **Step 3: Wire `revoke_link` to the shared Invitation Link lookup**

Replace the inline storage lookup in `revoke_link` with:

```rust
if let Err(e) = find_account_admin_invitation_link(
    state.storage.as_ref(),
    admin.account.account_id,
    link_id,
)
.await
{
    return e.into_response();
}
```

- [ ] **Step 4: Wire `requests_queue` to the shared queue read model**

Replace the pending-request assembly block at the start of `requests_queue` with:

```rust
let rows = match pending_request_queue(state.storage.as_ref(), admin.account.account_id).await {
    Ok(rows) => rows
        .into_iter()
        .map(|row| crate::views::requests::PendingRequestRow {
            request_id: row.request_id.to_string(),
            link_slug: row.link_slug,
            link_id: row.link_id.map(|id| id.to_string()).unwrap_or_default(),
            requester_login: row.requester_login,
            justification: row.justification,
            created_at: row.created_at,
        })
        .collect::<Vec<_>>(),
    Err(e) => return e.into_response(),
};
```

- [ ] **Step 5: Wire `approve_request` to the shared request lookup**

Replace the inline request/link ownership verification in `approve_request` with:

```rust
let _account_request = match find_account_admin_request(
    state.storage.as_ref(),
    admin.account.account_id,
    request_id,
)
.await
{
    Ok(account_request) => account_request,
    Err(e) => return e.into_response(),
};
```

- [ ] **Step 6: Wire `decline_request` to the shared request lookup**

Replace the inline request/link ownership verification in `decline_request` with:

```rust
let _account_request = match find_account_admin_request(
    state.storage.as_ref(),
    admin.account.account_id,
    request_id,
)
.await
{
    Ok(account_request) => account_request,
    Err(e) => return e.into_response(),
};
```

- [ ] **Step 7: Run compile check**

Run: `cargo check -p web`

Expected: PASS. Dashboard routes compile against the new read module.

- [ ] **Step 8: Run route smoke test filter**

Run: `cargo test -p web dashboard`

Expected: PASS. Existing dashboard route tests still pass.

- [ ] **Step 9: Commit**

```bash
git add crates/web/src/routes/dashboard.rs
git commit -m "refactor(web): use account admin dashboard reads"
```

---

### Task 5: Final Verification

**Files:**
- Verify: `crates/web/src/account_admin_reads.rs`
- Verify: `crates/web/src/routes/dashboard.rs`
- Verify: `crates/web/src/lib.rs`

- [ ] **Step 1: Run focused Account Admin tests**

Run: `cargo test -p web account_admin`

Expected: PASS. Queue rows, wrong-account access, missing related records, and successful Account Admin lookups are covered.

- [ ] **Step 2: Run dashboard tests**

Run: `cargo test -p web dashboard`

Expected: PASS. Existing dashboard route behavior is not regressed.

- [ ] **Step 3: Run web crate tests**

Run: `cargo test -p web`

Expected: PASS. The full web crate test suite passes.

- [ ] **Step 4: Run formatting check**

Run: `cargo fmt --check`

Expected: PASS. Formatting is unchanged after `cargo fmt` or already clean.

- [ ] **Step 5: Inspect final diff**

Run: `git diff --stat HEAD~4..HEAD`

Expected: Changes are limited to `crates/web/src/account_admin_reads.rs`, `crates/web/src/lib.rs`, and `crates/web/src/routes/dashboard.rs`.

- [ ] **Step 6: Confirm issue acceptance criteria**

Check the implementation against issue #5:

```text
- The pending request queue is assembled through `pending_request_queue` using Account Admin terms.
- Approve, decline, detail, and revoke flows call shared ownership-sensitive lookup functions.
- Missing Invitation Link and GitHub User fallbacks are explicit in `pending_request_row` and tested.
- Tests cover queue rows, wrong-account access, missing related records, and successful Account Admin lookups.
```

- [ ] **Step 7: Commit final fixes if Step 1-6 required changes**

If verification required changes, commit only those fixes:

```bash
git add crates/web/src/account_admin_reads.rs crates/web/src/lib.rs crates/web/src/routes/dashboard.rs
git commit -m "test(web): verify account admin request reads"
```

If verification required no changes, do not create an empty commit.
