# ghinvite Web Command Facade Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace route-level raw Restate calls with typed ghinvite web commands while preserving product behavior.

**Architecture:** Add a `crates/web/src/commands.rs` facade with a route-facing `GhinviteCommands` async trait and production `RestateCommands` adapter. Keep `RestateClient` as the low-level HTTP adapter, but remove it from `AppState` so routes can only invoke named ghinvite commands.

**Tech Stack:** Rust 2024, axum, async-trait, serde, existing `RestateClient`, in-process axum Restate recorders for command tests.

---

### Task 1: Command Facade Tests

**Files:**
- Create: `crates/web/src/commands.rs`
- Modify: `crates/web/src/lib.rs`

- [ ] **Step 1: Write failing command mapping tests**

Add tests in `crates/web/src/commands.rs` that construct `RestateCommands` with a local axum recorder and assert these mappings:

- `create_share_link` calls `ShareLink/{account_id}/create` and returns typed `{ link_id, slug }`.
- `submit_invitation_request` sends `InvitationRequest/{request_id}/submit`.
- `route_github_invitation_webhook` sends `GithubInvitation/{invitation_id}/on_webhook` with action values `accepted` and `declined`.

- [ ] **Step 2: Run tests and verify RED**

Run: `cargo test -p web commands::tests`

Expected: FAIL because `crate::commands` does not exist yet.

### Task 2: Command Facade Implementation

**Files:**
- Create: `crates/web/src/commands.rs`
- Modify: `crates/web/src/lib.rs`
- Modify: `crates/web/src/state.rs`
- Modify: `crates/web/src/main.rs`

- [ ] **Step 1: Add route-facing types**

Define `GhinviteCommands: Send + Sync + 'static` with async methods:

- `create_share_link(CreateShareLink) -> Result<CreateShareLinkOutput>`
- `revoke_share_link(RevokeShareLink) -> Result<()>`
- `submit_invitation_request(SubmitInvitationRequest) -> Result<()>`
- `decide_invitation_request(DecideInvitationRequest) -> Result<()>`
- `onboard_installation(OnboardInstallation) -> Result<()>`
- `record_repository_selection_change(RecordRepositorySelectionChange) -> Result<()>`
- `record_installation_uninstalled(RecordInstallationUninstalled) -> Result<()>`
- `route_github_invitation_webhook(RouteGithubInvitationWebhook) -> Result<()>`

- [ ] **Step 2: Add production adapter**

Implement `RestateCommands` over `Arc<RestateClient>`, preserving current Restate service names, method names, keys, and send/call behavior.

- [ ] **Step 3: Move AppState to command trait**

Change `AppState` to hold `Arc<dyn GhinviteCommands>` instead of `Arc<RestateClient>`. Update `main.rs` to wrap `RestateClient` in `RestateCommands`.

- [ ] **Step 4: Run tests and verify GREEN for command module**

Run: `cargo test -p web commands::tests`

Expected: PASS.

### Task 3: Route Migration

**Files:**
- Modify: `crates/web/src/routes/dashboard.rs`
- Modify: `crates/web/src/routes/invitation.rs`
- Modify: `crates/web/src/routes/setup.rs`
- Modify: `crates/web/src/routes/webhook.rs`

- [ ] **Step 1: Replace raw Restate calls**

Replace every `state.restate.send` and `state.restate.call` in web routes with named command methods from `state.commands`.

- [ ] **Step 2: Keep route responsibilities unchanged**

Routes still own form parsing, authorization checks, GitHub user-token reads, webhook HMAC verification, webhook payload parsing, and timestamp selection.

- [ ] **Step 3: Preserve behavior**

Keep current redirects, flash messages, storage reads, and webhook status behavior. The only intentional behavior fix is repository-invitation webhook action serialization through the typed command enum.

### Task 4: Test Updates

**Files:**
- Modify: `crates/web/tests/setup_flow.rs`
- Modify: `crates/web/tests/dashboard_flow.rs`
- Modify: `crates/web/tests/route_smoke.rs`
- Modify: `crates/web/tests/oauth_flow.rs`

- [ ] **Step 1: Update app builders**

Construct `RestateCommands` for route tests that can still use production command behavior, or fake `GhinviteCommands` where the route should assert command-level intent.

- [ ] **Step 2: Move raw Restate mapping assertions**

Keep raw Restate HTTP payload assertions in `commands::tests`, not broad route tests.

- [ ] **Step 3: Run web tests**

Run: `cargo test -p web`

Expected: PASS.

### Task 5: Verification

**Files:**
- Verify all touched Rust files.

- [ ] **Step 1: Format**

Run: `cargo fmt --all --check`

Expected: PASS.

- [ ] **Step 2: Targeted tests**

Run: `cargo test -p web`

Expected: PASS.

- [ ] **Step 3: Broader affected tests**

Run: `cargo test -p domain -p storage -p github -p web`

Expected: PASS.
