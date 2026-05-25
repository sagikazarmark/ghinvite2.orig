# Restate Json Payloads Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace `crates/restate-svc`'s custom Restate payload macro with idiomatic `restate_sdk::serde::Json<T>` and `schemars::JsonSchema` schemas.

**Architecture:** Restate-framed inputs, outputs, workflow journal values, promises, and generated client calls move to `Json<T>`. Handler implementations unwrap to plain Rust payloads at the boundary, except `InvitationRequest::decide`, which keeps `Json<Decision>` through `ctx.resolve_promise`. Domain schema support is added only for domain types embedded in Restate payloads, preserving existing serde wire formats.

**Tech Stack:** Rust 2024 workspace, `restate-sdk = 0.10`, `schemars = 1.2` with `chrono04`, `serde`, `serde_json`, `tokio`, local Restate integration tests.

---

## File Structure

| Path | Responsibility |
|---|---|
| `Cargo.toml` | Add workspace `schemars` dependency matching Restate SDK's `schemars 1.2` major version, with `chrono04`. |
| `crates/domain/Cargo.toml` | Consume workspace `schemars` for domain schema derives and manual schema impls. |
| `crates/domain/src/ids.rs` | Implement string JSON schema for ULID newtypes and add schema-shape tests. |
| `crates/domain/src/account.rs` | Add `JsonSchema` for `AccountType`; manually implement string-or-array schema for `SelectedRepos`; add schema-shape test. |
| `crates/domain/src/permission.rs` | Derive `JsonSchema` for `Permission`. |
| `crates/domain/src/invitation_link.rs` | Derive `JsonSchema` for `InvitationLinkRepo`. |
| `crates/domain/src/invitation_request.rs` | Derive `JsonSchema` for `RequestState`. |
| `crates/restate-svc/Cargo.toml` | Add workspace `schemars`; remove direct `bytes` after deleting the manual macro. |
| `crates/restate-svc/src/installation.rs` | Convert `Installation` handler payloads to `Json<T>`. |
| `crates/restate-svc/src/invitation_link.rs` | Convert `InvitationLink` handler payloads and output to `Json<T>`. |
| `crates/restate-svc/src/reconcile.rs` | Convert `Reconcile::daily_run` payload to `Json<T>`. |
| `crates/restate-svc/src/github_invitation.rs` | Convert `GithubInvitation` handler payloads to `Json<T>` and derive nested schema for `WebhookAction`. |
| `crates/restate-svc/src/invitation_request.rs` | Convert workflow payloads, promises, `ctx.run` outputs, and generated client call; remove obsolete wrapper newtypes where direct `Json<T>` works. |
| `crates/restate-svc/src/lib.rs` | Remove `restate_payload` module declaration. |
| `crates/restate-svc/src/restate_payload.rs` | Delete the custom macro module. |
| `crates/restate-svc/tests/integration_test.rs` | Expand ignored raw JSON ingress smoke coverage. |
| `docs/superpowers/plans/2026-05-04-ghinvite-dashboard.md` | Replace stale `impl_restate_json_payload!` reference. |

---

### Task 1: Add Domain Schema Support And Tests

**Files:**
- Modify: `Cargo.toml`
- Modify: `crates/domain/Cargo.toml`
- Modify: `crates/domain/src/ids.rs`
- Modify: `crates/domain/src/account.rs`
- Modify: `crates/domain/src/permission.rs`
- Modify: `crates/domain/src/invitation_link.rs`
- Modify: `crates/domain/src/invitation_request.rs`

- [ ] **Step 1: Add the workspace `schemars` dependency**

In the workspace root `Cargo.toml`, add `schemars` under `[workspace.dependencies]` near `serde`:

```toml
schemars = { version = "1.2", features = ["chrono04"] }
serde = { version = "1", features = ["derive"] }
```

In `crates/domain/Cargo.toml`, add `schemars.workspace = true` under `[dependencies]`:

```toml
[dependencies]
chrono.workspace = true
rand.workspace = true
schemars.workspace = true
serde.workspace = true
subtle.workspace = true
thiserror.workspace = true
ulid.workspace = true
```

- [ ] **Step 2: Add failing ID schema test**

In `crates/domain/src/ids.rs`, add these imports at the top:

```rust
use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::fmt;
use std::str::FromStr;
use ulid::Ulid;
```

In the `#[cfg(test)] mod tests` block, add this test:

```rust
    #[test]
    fn id_schema_matches_ulid_string_wire_format() {
        let schema = schemars::schema_for!(InvitationLinkId).to_value();
        assert_eq!(schema.get("type").and_then(serde_json::Value::as_str), Some("string"));
        assert_eq!(
            schema.get("pattern").and_then(serde_json::Value::as_str),
            Some("^[0-9A-HJKMNP-TV-Z]{26}$")
        );
    }
```

- [ ] **Step 3: Add failing `SelectedRepos` schema test**

In `crates/domain/src/account.rs`, add these imports at the top:

```rust
use chrono::{DateTime, Utc};
use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::fmt;
use std::str::FromStr;
```

In the `#[cfg(test)] mod tests` block, add this test:

```rust
    #[test]
    fn selected_repos_schema_matches_wire_format() {
        let schema = schemars::schema_for!(SelectedRepos).to_value();
        let variants = schema
            .get("oneOf")
            .and_then(serde_json::Value::as_array)
            .expect("SelectedRepos schema should use oneOf");

        assert!(variants.iter().any(|variant| {
            variant.get("type") == Some(&serde_json::json!("string"))
                && variant.get("const") == Some(&serde_json::json!("all"))
        }));
        assert!(variants.iter().any(|variant| {
            variant.get("type") == Some(&serde_json::json!("array"))
                && variant.pointer("/items/type") == Some(&serde_json::json!("integer"))
        }));
    }
```

- [ ] **Step 4: Run domain tests to verify schema tests fail**

Run: `cargo test -p domain schema_matches`

Expected: FAIL with errors that `InvitationLinkId` and `SelectedRepos` do not implement `schemars::JsonSchema`.

- [ ] **Step 5: Implement string schema for ULID newtypes**

In `crates/domain/src/ids.rs`, update the `ulid_newtype!` macro body to include this implementation after the existing `FromStr` impl:

```rust
        impl JsonSchema for $name {
            fn schema_name() -> Cow<'static, str> {
                stringify!($name).into()
            }

            fn schema_id() -> Cow<'static, str> {
                concat!(module_path!(), "::", stringify!($name)).into()
            }

            fn inline_schema() -> bool {
                true
            }

            fn json_schema(_generator: &mut SchemaGenerator) -> Schema {
                json_schema!({
                    "type": "string",
                    "pattern": "^[0-9A-HJKMNP-TV-Z]{26}$"
                })
            }
        }
```

- [ ] **Step 6: Implement domain `JsonSchema` derives and custom `SelectedRepos` schema**

In `crates/domain/src/account.rs`, update derives:

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "PascalCase")]
pub enum AccountType {
    User,
    Organization,
}
```

Add this manual implementation after the `Deserialize for SelectedRepos` impl:

```rust
impl JsonSchema for SelectedRepos {
    fn schema_name() -> Cow<'static, str> {
        "SelectedRepos".into()
    }

    fn schema_id() -> Cow<'static, str> {
        concat!(module_path!(), "::SelectedRepos").into()
    }

    fn json_schema(_generator: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "oneOf": [
                {
                    "type": "string",
                    "const": "all"
                },
                {
                    "type": "array",
                    "items": {
                        "type": "integer",
                        "minimum": 0
                    }
                }
            ]
        })
    }
}
```

In `crates/domain/src/permission.rs`, add `use schemars::JsonSchema;` and update the derive:

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Permission {
    Pull,
    Triage,
    Push,
    Maintain,
    Admin,
}
```

In `crates/domain/src/invitation_link.rs`, add `use schemars::JsonSchema;` and update only `InvitationLinkRepo`:

```rust
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct InvitationLinkRepo {
    pub repo_id: u64,
    pub repo_full_name: String,
}
```

In `crates/domain/src/invitation_request.rs`, add `use schemars::JsonSchema;` and update `RequestState`:

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum RequestState {
    Pending,
    Approved,
    Declined,
    Expired,
    /// Reserved for v2 (link revoke cascading); not produced by v1 code paths.
    Cancelled,
}
```

- [ ] **Step 7: Run domain tests to verify schema support**

Run: `cargo test -p domain`

Expected: PASS. The new schema tests and existing serde tests pass.

- [ ] **Step 8: Commit domain schema support**

```bash
git add Cargo.toml Cargo.lock crates/domain/Cargo.toml crates/domain/src/ids.rs crates/domain/src/account.rs crates/domain/src/permission.rs crates/domain/src/invitation_link.rs crates/domain/src/invitation_request.rs
git commit -m "refactor(domain): add Restate payload schema support"
```

---

### Task 2: Convert Simple Restate Services To `Json<T>`

**Files:**
- Modify: `crates/restate-svc/Cargo.toml`
- Modify: `crates/restate-svc/src/installation.rs`
- Modify: `crates/restate-svc/src/invitation_link.rs`
- Modify: `crates/restate-svc/src/reconcile.rs`

- [ ] **Step 1: Add `schemars` to `restate-svc`**

In `crates/restate-svc/Cargo.toml`, add `schemars.workspace = true` under `[dependencies]`:

```toml
[dependencies]
async-trait.workspace = true
restate-sdk = { workspace = true, features = ["http_server"] }
tokio.workspace = true
tracing-subscriber.workspace = true
audit.workspace = true
bytes = "1"
chrono.workspace = true
domain.workspace = true
github.workspace = true
rand.workspace = true
schemars.workspace = true
serde.workspace = true
serde_json.workspace = true
storage.workspace = true
thiserror.workspace = true
tracing.workspace = true
```

`bytes` stays for this task because `restate_payload.rs` still exists until all macro uses are removed.

- [ ] **Step 2: Convert `Installation`**

In `crates/restate-svc/src/installation.rs`, replace the imports and payload derives with:

```rust
use crate::audit::{Actor, Target};
use crate::error::HandlerError;
use crate::state::AppState;
use audit::EventType;
use chrono::{DateTime, Utc};
use domain::{Account, AccountType, SelectedRepos};
use restate_sdk::context::{ContextSideEffects, ObjectContext, RunFuture};
use restate_sdk::errors::TerminalError;
use restate_sdk::serde::Json;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
pub struct OnboardInput {
    pub installation_id: u64,
    pub actor_user_id: u64,
    pub account_id: u64,
    pub account_login: String,
    pub account_type: AccountType,
    pub selected_repos: SelectedRepos,
    pub installed_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
pub struct ReposChangedInput {
    pub installation_id: u64,
    pub selected_repos: SelectedRepos,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
pub struct UninstallInput {
    pub installation_id: u64,
    pub uninstalled_at: DateTime<Utc>,
}
```

Remove these lines:

```rust
use crate::impl_restate_json_payload;
impl_restate_json_payload!(OnboardInput);
impl_restate_json_payload!(ReposChangedInput);
impl_restate_json_payload!(UninstallInput);
```

Replace the `Installation` trait and impl method signatures with this exact shape:

```rust
#[restate_sdk::object]
pub trait Installation {
    async fn onboard(input: Json<OnboardInput>) -> std::result::Result<(), TerminalError>;
    async fn repos_changed(input: Json<ReposChangedInput>) -> std::result::Result<(), TerminalError>;
    async fn uninstall(input: Json<UninstallInput>) -> std::result::Result<(), TerminalError>;
}

impl Installation for InstallationImpl {
    async fn onboard(
        &self,
        ctx: ObjectContext<'_>,
        input: Json<OnboardInput>,
    ) -> std::result::Result<(), TerminalError> {
        let Json(input) = input;
        let request_id = Some(ctx.invocation_id().to_string());
        ctx.run(|| async {
            onboard_logic(&self.state, &input, request_id.clone())
                .await
                .map_err(crate::error::to_sdk_handler_error)
        })
        .name("onboard")
        .await
    }

    async fn repos_changed(
        &self,
        ctx: ObjectContext<'_>,
        input: Json<ReposChangedInput>,
    ) -> std::result::Result<(), TerminalError> {
        let Json(input) = input;
        let request_id = Some(ctx.invocation_id().to_string());
        ctx.run(|| async {
            repos_changed_logic(&self.state, &input, request_id.clone())
                .await
                .map_err(crate::error::to_sdk_handler_error)
        })
        .name("repos_changed")
        .await
    }

    async fn uninstall(
        &self,
        ctx: ObjectContext<'_>,
        input: Json<UninstallInput>,
    ) -> std::result::Result<(), TerminalError> {
        let Json(input) = input;
        let request_id = Some(ctx.invocation_id().to_string());
        ctx.run(|| async {
            uninstall_logic(&self.state, &input, request_id.clone())
                .await
                .map_err(crate::error::to_sdk_handler_error)
        })
        .name("uninstall")
        .await
    }
}
```

- [ ] **Step 3: Convert `InvitationLink`**

In `crates/restate-svc/src/invitation_link.rs`, remove `use crate::impl_restate_json_payload;`, add these imports, and add `JsonSchema` to each local payload derive:

```rust
use restate_sdk::serde::Json;
use schemars::JsonSchema;

#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
pub struct CreateLinkInput {
    pub installation_id: u64,
    pub account_id: u64,
    pub created_by: u64,
    pub created_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>,
    pub max_uses: Option<u32>,
    pub permission: Permission,
    pub approval_required: bool,
    pub internal_note: Option<String>,
    pub repos: Vec<InvitationLinkRepo>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
pub struct CreateLinkOutput {
    pub link_id: InvitationLinkId,
    pub slug: String,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
pub struct RevokeLinkInput {
    pub link_id: InvitationLinkId,
    pub by_user: u64,
    pub when: DateTime<Utc>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
pub struct TickExpirationInput {
    pub link_id: InvitationLinkId,
    pub at: DateTime<Utc>,
}
```

Remove these macro calls:

```rust
impl_restate_json_payload!(CreateLinkInput);
impl_restate_json_payload!(CreateLinkOutput);
impl_restate_json_payload!(RevokeLinkInput);
impl_restate_json_payload!(TickExpirationInput);
```

Replace the `InvitationLink` trait and impl methods with this shape, preserving the existing pure logic functions below it:

```rust
#[restate_sdk::object]
pub trait InvitationLink {
    async fn create(input: Json<CreateLinkInput>)
    -> std::result::Result<Json<CreateLinkOutput>, TerminalError>;
    async fn revoke(input: Json<RevokeLinkInput>) -> std::result::Result<(), TerminalError>;
    async fn tick_expiration(input: Json<TickExpirationInput>) -> std::result::Result<(), TerminalError>;
}

impl InvitationLink for InvitationLinkImpl {
    async fn create(
        &self,
        ctx: ObjectContext<'_>,
        input: Json<CreateLinkInput>,
    ) -> std::result::Result<Json<CreateLinkOutput>, TerminalError> {
        let Json(input) = input;
        let request_id = Some(ctx.invocation_id().to_string());
        let output = ctx
            .run(|| async {
                create_logic(&self.state, &input, request_id.clone())
                    .await
                    .map_err(crate::error::to_sdk_handler_error)
            })
            .name("create")
            .await?;
        Ok(Json(output))
    }

    async fn revoke(
        &self,
        ctx: ObjectContext<'_>,
        input: Json<RevokeLinkInput>,
    ) -> std::result::Result<(), TerminalError> {
        let Json(input) = input;
        let request_id = Some(ctx.invocation_id().to_string());
        ctx.run(|| async {
            revoke_logic(&self.state, &input, request_id.clone())
                .await
                .map_err(crate::error::to_sdk_handler_error)
        })
        .name("revoke")
        .await
    }

    async fn tick_expiration(
        &self,
        ctx: ObjectContext<'_>,
        input: Json<TickExpirationInput>,
    ) -> std::result::Result<(), TerminalError> {
        let Json(input) = input;
        let request_id = Some(ctx.invocation_id().to_string());
        ctx.run(|| async {
            tick_expiration_logic(&self.state, &input, request_id.clone())
                .await
                .map_err(crate::error::to_sdk_handler_error)
        })
        .name("tick_expiration")
        .await
    }
}
```

- [ ] **Step 4: Convert `Reconcile`**

In `crates/restate-svc/src/reconcile.rs`, remove `use crate::impl_restate_json_payload;`, add `Json` and `JsonSchema`, and update the payload:

```rust
use restate_sdk::serde::Json;
use schemars::JsonSchema;

#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
pub struct DailyRunInput {
    pub at: DateTime<Utc>,
}
```

Remove:

```rust
impl_restate_json_payload!(DailyRunInput);
```

Replace the trait and impl method with:

```rust
#[restate_sdk::service]
pub trait Reconcile {
    async fn daily_run(input: Json<DailyRunInput>) -> std::result::Result<(), TerminalError>;
}

impl Reconcile for ReconcileImpl {
    async fn daily_run(
        &self,
        ctx: Context<'_>,
        input: Json<DailyRunInput>,
    ) -> std::result::Result<(), TerminalError> {
        let Json(input) = input;
        let request_id = Some(ctx.invocation_id().to_string());
        ctx.run(|| async {
            daily_run_logic(&self.state, &input, request_id.clone())
                .await
                .map_err(crate::error::to_sdk_handler_error)
        })
        .name("daily_run")
        .await
    }
}
```

- [ ] **Step 5: Run checks for simple service conversion**

Run: `cargo check -p restate-svc`

Expected: PASS. If it fails in `github_invitation.rs` or `invitation_request.rs`, stop and confirm only files from this task were changed.

- [ ] **Step 6: Commit simple service conversion**

```bash
git add crates/restate-svc/Cargo.toml crates/restate-svc/src/installation.rs crates/restate-svc/src/invitation_link.rs crates/restate-svc/src/reconcile.rs Cargo.lock
git commit -m "refactor(restate-svc): use Json payloads for simple services"
```

---

### Task 3: Convert `GithubInvitation` And Its Generated Client Call

**Files:**
- Modify: `crates/restate-svc/src/github_invitation.rs`
- Modify: `crates/restate-svc/src/invitation_request.rs`

- [ ] **Step 1: Convert `GithubInvitation` payload derives and signatures**

In `crates/restate-svc/src/github_invitation.rs`, remove `use crate::impl_restate_json_payload;`, add `Json` and `JsonSchema`, and add `JsonSchema` to all local payload derives including `WebhookAction`:

```rust
use restate_sdk::serde::Json;
use schemars::JsonSchema;

#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
pub struct CreateInvitationInput {
    pub invitation_id: GithubInvitationId,
    pub invitation_request_id: RequestId,
    pub installation_id: u64,
    pub repo_id: u64,
    pub repo_full_name: String,
    pub recipient_login: String,
    pub permission: domain::Permission,
    pub now: DateTime<Utc>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
pub struct OnWebhookInput {
    pub invitation_id: GithubInvitationId,
    pub action: WebhookAction,
    pub at: DateTime<Utc>,
}

#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WebhookAction {
    Accepted,
    Declined,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
pub struct CancelInvitationInput {
    pub invitation_id: GithubInvitationId,
    pub installation_id: u64,
    pub by_user: Option<u64>,
    pub at: DateTime<Utc>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
pub struct TickExpireInput {
    pub invitation_id: GithubInvitationId,
    pub installation_id: u64,
    pub at: DateTime<Utc>,
}
```

Remove these macro calls:

```rust
impl_restate_json_payload!(CreateInvitationInput);
impl_restate_json_payload!(OnWebhookInput);
impl_restate_json_payload!(CancelInvitationInput);
impl_restate_json_payload!(TickExpireInput);
```

Replace the trait signatures:

```rust
#[restate_sdk::object]
pub trait GithubInvitation {
    async fn create(input: Json<CreateInvitationInput>) -> std::result::Result<(), TerminalError>;
    async fn on_webhook(input: Json<OnWebhookInput>) -> std::result::Result<(), TerminalError>;
    async fn cancel(input: Json<CancelInvitationInput>) -> std::result::Result<(), TerminalError>;
    async fn tick_expire(input: Json<TickExpireInput>) -> std::result::Result<(), TerminalError>;
}
```

- [ ] **Step 2: Unwrap `GithubInvitation` inputs at implementation edges**

For each method in `impl GithubInvitation for GithubInvitationImpl`, change the input type to `Json<...>` and add `let Json(input) = input;` before `request_id`.

The `create` method should look like this after the change:

```rust
    async fn create(
        &self,
        ctx: ObjectContext<'_>,
        input: Json<CreateInvitationInput>,
    ) -> std::result::Result<(), TerminalError> {
        let Json(input) = input;
        let request_id = Some(ctx.invocation_id().to_string());
        ctx.run(|| async {
            create_logic(&self.state, &input, request_id.clone())
                .await
                .map_err(crate::error::to_sdk_handler_error)
        })
        .name("create")
        .await
    }
```

Then update the remaining methods explicitly:

```rust
    async fn on_webhook(
        &self,
        ctx: ObjectContext<'_>,
        input: Json<OnWebhookInput>,
    ) -> std::result::Result<(), TerminalError> {
        let Json(input) = input;
        let request_id = Some(ctx.invocation_id().to_string());
        ctx.run(|| async {
            on_webhook_logic(&self.state, &input, request_id.clone())
                .await
                .map_err(crate::error::to_sdk_handler_error)
        })
        .name("on_webhook")
        .await
    }

    async fn cancel(
        &self,
        ctx: ObjectContext<'_>,
        input: Json<CancelInvitationInput>,
    ) -> std::result::Result<(), TerminalError> {
        let Json(input) = input;
        let request_id = Some(ctx.invocation_id().to_string());
        ctx.run(|| async {
            cancel_logic(&self.state, &input, request_id.clone())
                .await
                .map_err(crate::error::to_sdk_handler_error)
        })
        .name("cancel")
        .await
    }

    async fn tick_expire(
        &self,
        ctx: ObjectContext<'_>,
        input: Json<TickExpireInput>,
    ) -> std::result::Result<(), TerminalError> {
        let Json(input) = input;
        let request_id = Some(ctx.invocation_id().to_string());
        ctx.run(|| async {
            tick_expire_logic(&self.state, &input, request_id.clone())
                .await
                .map_err(crate::error::to_sdk_handler_error)
        })
        .name("tick_expire")
        .await
    }
```

- [ ] **Step 3: Update generated client fan-out call**

In `crates/restate-svc/src/invitation_request.rs`, add this import now because the generated client call needs it before the full workflow conversion:

```rust
use restate_sdk::serde::Json;
```

Change the generated client call inside the approval fan-out loop:

```rust
                ctx.object_client::<crate::github_invitation::GithubInvitationClient>(
                    inv_input.invitation_id.to_string(),
                )
                .create(Json(inv_input))
                .send();
```

- [ ] **Step 4: Run check for generated client compatibility**

Run: `cargo check -p restate-svc`

Expected: PASS. This proves the generated `GithubInvitationClient::create` call now matches the `Json<CreateInvitationInput>` signature.

- [ ] **Step 5: Commit `GithubInvitation` conversion**

```bash
git add crates/restate-svc/src/github_invitation.rs crates/restate-svc/src/invitation_request.rs
git commit -m "refactor(restate-svc): use Json payloads for github invitations"
```

---

### Task 4: Convert `InvitationRequest` Workflow Internals

**Files:**
- Modify: `crates/restate-svc/src/invitation_request.rs`

- [ ] **Step 1: Update imports and derives**

At the top of `crates/restate-svc/src/invitation_request.rs`, remove `use crate::impl_restate_json_payload;`. Keep the `Json` import added in Task 3 and add `JsonSchema`:

```rust
use restate_sdk::serde::Json;
use schemars::JsonSchema;
```

Update derives for local Restate-framed payloads and journaled values:

```rust
#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
pub struct SubmitRequestInput {
    pub request_id: RequestId,
    pub invitation_link_id: domain::InvitationLinkId,
    pub requester_id: u64,
    pub justification: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
pub enum Decision {
    Approve {
        decided_by: u64,
        decided_at: DateTime<Utc>,
    },
    Decline {
        decided_by: u64,
        decided_at: DateTime<Utc>,
        reason: Option<String>,
    },
}

#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
pub enum PreDecisionOutcome {
    AutoApprove { account_id: u64 },
    PendingDecision {
        account_id: u64,
        decision_deadline: DateTime<Utc>,
    },
}

#[derive(Clone, Debug, Deserialize, JsonSchema, Serialize)]
pub enum AppliedDecision {
    AutoApprove {
        at: DateTime<Utc>,
    },
    Approve {
        decided_by: u64,
        at: DateTime<Utc>,
    },
    Decline {
        decided_by: u64,
        at: DateTime<Utc>,
        reason: Option<String>,
    },
    Expire {
        at: DateTime<Utc>,
    },
}
```

Remove the `SubmitOutput` and `DispatchInputs` definitions entirely. Remove these macro calls entirely:

```rust
impl_restate_json_payload!(SubmitRequestInput);
impl_restate_json_payload!(Decision);
impl_restate_json_payload!(SubmitOutput);
impl_restate_json_payload!(PreDecisionOutcome);
impl_restate_json_payload!(AppliedDecision);
impl_restate_json_payload!(DispatchInputs);
```

- [ ] **Step 2: Change workflow trait signatures**

Replace the `InvitationRequest` trait with:

```rust
#[restate_sdk::workflow]
pub trait InvitationRequest {
    /// One-shot workflow per `request_id`. Returns the final state.
    async fn submit(input: Json<SubmitRequestInput>)
    -> std::result::Result<Json<RequestState>, TerminalError>;

    /// Resolve the "decision" durable promise in `submit` with an Approve/Decline decision.
    /// Called by the admin's approve/decline handler (Plan 5).
    #[shared]
    async fn decide(decision: Json<Decision>) -> std::result::Result<(), TerminalError>;
}
```

- [ ] **Step 3: Convert `submit` signature and pre-decision `ctx.run`**

Update the start of `submit` to unwrap the inbound `Json<SubmitRequestInput>` and wrap the pre-decision journal output:

```rust
    async fn submit(
        &self,
        ctx: WorkflowContext<'_>,
        input: Json<SubmitRequestInput>,
    ) -> std::result::Result<Json<RequestState>, TerminalError> {
        let Json(input) = input;
        let request_id = Some(ctx.invocation_id().to_string());

        let Json(outcome) = {
            let state = self.state.clone();
            let input = input.clone();
            let request_id = request_id.clone();
            ctx.run(move || {
                let state = state.clone();
                let input = input.clone();
                let request_id = request_id.clone();
                async move {
                    pre_decision_logic(&state, &input, input.created_at, request_id)
                        .await
                        .map(Json)
                        .map_err(crate::error::to_sdk_handler_error)
                }
            })
            .name("pre_decision")
            .await?
        };
```

- [ ] **Step 4: Convert decision promise to `Json<Decision>`**

Inside the `PendingDecision` branch, replace the promise declaration and promise match with:

```rust
                let promise = ctx.promise::<Json<Decision>>("decision");
```

Inside `restate_sdk::select!`, unwrap the resolved decision before matching:

```rust
                    res = promise => {
                        let Json(decision) = res?;
                        match decision {
                            Decision::Approve { decided_by, decided_at } => AppliedDecision::Approve {
                                decided_by,
                                at: decided_at,
                            },
                            Decision::Decline { decided_by, decided_at, reason } => AppliedDecision::Decline {
                                decided_by,
                                at: decided_at,
                                reason,
                            },
                        }
                    },
```

- [ ] **Step 5: Convert apply-decision `ctx.run` and remove `SubmitOutput`**

Replace the apply-decision block with:

```rust
        let Json(final_state) = {
            let state = self.state.clone();
            let request_id_inner = request_id.clone();
            let req_id = input.request_id;
            let decision = applied.clone();
            ctx.run(move || {
                let state = state.clone();
                let request_id_inner = request_id_inner.clone();
                let decision = decision.clone();
                async move {
                    apply_decision_logic(&state, req_id, account_id, decision, request_id_inner)
                        .await
                        .map(Json)
                        .map_err(crate::error::to_sdk_handler_error)
                }
            })
            .name("apply_decision")
            .await?
        };
```

Remove the obsolete comment that says returning `SubmitOutput` keeps `RequestState` Restate-serializable.

- [ ] **Step 6: Convert dispatch-inputs `ctx.run` and generated client call**

Replace the dispatch-inputs block with:

```rust
            let Json(inputs) = {
                let state = self.state.clone();
                let req_id = input.request_id;
                let now = input.created_at;
                ctx.run(move || {
                    let state = state.clone();
                    async move {
                        build_dispatch_inputs(&state, req_id, now)
                            .await
                            .map(Json)
                            .map_err(crate::error::to_sdk_handler_error)
                    }
                })
                .name("build_dispatch_inputs")
                .await?
            };

            for inv_input in inputs {
                ctx.object_client::<crate::github_invitation::GithubInvitationClient>(
                    inv_input.invitation_id.to_string(),
                )
                .create(Json(inv_input))
                .send();
            }
```

Remove comments that say `DispatchInputs` is needed for `Vec<T>` Restate framing.

- [ ] **Step 7: Return `Json<RequestState>` and keep `decide` wrapped**

At the end of `submit`, return:

```rust
        Ok(Json(final_state))
```

Replace `decide` with:

```rust
    async fn decide(
        &self,
        ctx: SharedWorkflowContext<'_>,
        decision: Json<Decision>,
    ) -> std::result::Result<(), TerminalError> {
        // Keep Json<Decision> intact: submit waits on the same wrapped promise type.
        ctx.resolve_promise("decision", decision);
        Ok(())
    }
```

- [ ] **Step 8: Update workflow comments**

Replace the comment above `PreDecisionOutcome` with:

```rust
/// Outcome of the pre-decision phase. Drives the workflow handler's branching.
/// Wrapped in `Json<PreDecisionOutcome>` at the `ctx.run("pre_decision")`
/// boundary so Restate journals it with SDK-managed JSON framing.
```

- [ ] **Step 9: Run check for workflow conversion**

Run: `cargo check -p restate-svc`

Expected: PASS. This proves direct `Json<RequestState>`, `Json<Vec<CreateInvitationInput>>`, and `Json<Decision>` promise framing compile.

If this fails only because `Json<Vec<CreateInvitationInput>>` is unsupported, restore a local wrapper for that exact dispatch list and derive `JsonSchema` on the wrapper. Keep `SubmitOutput` deleted if `Json<RequestState>` compiles.

- [ ] **Step 10: Commit workflow conversion**

```bash
git add crates/restate-svc/src/invitation_request.rs
git commit -m "refactor(restate-svc): use Json payloads in invitation workflow"
```

---

### Task 5: Delete The Custom Payload Macro And Clean Documentation

**Files:**
- Modify: `crates/restate-svc/Cargo.toml`
- Modify: `crates/restate-svc/src/lib.rs`
- Delete: `crates/restate-svc/src/restate_payload.rs`
- Modify: `docs/superpowers/plans/2026-05-04-ghinvite-dashboard.md`

- [ ] **Step 1: Remove macro module from `lib.rs`**

In `crates/restate-svc/src/lib.rs`, remove this line:

```rust
pub(crate) mod restate_payload;
```

- [ ] **Step 2: Delete `restate_payload.rs`**

Delete `crates/restate-svc/src/restate_payload.rs`.

- [ ] **Step 3: Remove dead `bytes` dependency**

In `crates/restate-svc/Cargo.toml`, remove this line from `[dependencies]`:

```toml
bytes = "1"
```

Leave `schemars.workspace = true` in place.

- [ ] **Step 4: Update stale dashboard plan reference**

In `docs/superpowers/plans/2026-05-04-ghinvite-dashboard.md`, replace the line mentioning the deleted macro with:

```markdown
- `Decision::Approve` / `Decision::Decline` JSON shape (Tasks 15, 16) matches `crates/restate-svc/src/invitation_request.rs::Decision` exactly via `#[derive(Serialize, Deserialize, JsonSchema)]` and `restate_sdk::serde::Json<Decision>` framing.
```

- [ ] **Step 5: Verify no macro references remain in Rust**

Run: `rg "impl_restate_json_payload|restate_payload" crates/restate-svc/src`

Expected: no matches.

- [ ] **Step 6: Run checks after macro deletion**

Run: `cargo check -p restate-svc`

Expected: PASS.

- [ ] **Step 7: Commit macro deletion and docs cleanup**

```bash
git add crates/restate-svc/Cargo.toml crates/restate-svc/src/lib.rs docs/superpowers/plans/2026-05-04-ghinvite-dashboard.md Cargo.lock
git rm crates/restate-svc/src/restate_payload.rs
git commit -m "refactor(restate-svc): remove custom Restate payload macro"
```

---

### Task 6: Expand Raw JSON Ingress Integration Smoke

**Files:**
- Modify: `crates/restate-svc/tests/integration_test.rs`

- [ ] **Step 1: Add domain import for generated IDs**

At the top of `crates/restate-svc/tests/integration_test.rs`, add:

```rust
use domain::RequestId;
```

- [ ] **Step 2: Replace the ignored smoke test with broader raw-ingress coverage**

Replace the existing `installation_install_and_query` test with this test:

```rust
#[tokio::test]
#[ignore]
async fn raw_json_ingress_accepts_object_and_workflow_payloads() {
    reset_github_stub().await;
    let _addr = setup_restate().await;

    let client = reqwest::Client::new();

    let onboard_resp = client
        .post("http://localhost:8080/Installation/1/onboard")
        .json(&serde_json::json!({
            "installation_id": 1,
            "actor_user_id": 7,
            "account_id": 42,
            "account_login": "test-org",
            "account_type": "Organization",
            "selected_repos": "all",
            "installed_at": "2026-05-04T12:00:00Z"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(onboard_resp.status(), 200, "Installation::onboard failed");

    let create_link_resp = client
        .post("http://localhost:8080/InvitationLink/raw-json-smoke/create")
        .json(&serde_json::json!({
            "installation_id": 1,
            "account_id": 42,
            "created_by": 7,
            "created_at": "2026-05-04T12:00:00Z",
            "expires_at": "2026-05-04T12:00:01Z",
            "max_uses": null,
            "permission": "pull",
            "approval_required": true,
            "internal_note": null,
            "repos": []
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(create_link_resp.status(), 200, "InvitationLink::create failed");
    let create_link_body: serde_json::Value = create_link_resp.json().await.unwrap();
    let link_id = create_link_body
        .get("link_id")
        .and_then(serde_json::Value::as_str)
        .expect("InvitationLink::create response should contain link_id")
        .to_string();
    assert!(
        create_link_body
            .get("slug")
            .and_then(serde_json::Value::as_str)
            .is_some(),
        "InvitationLink::create response should contain slug"
    );

    let request_id = RequestId::new().to_string();
    let submit_resp = client
        .post(format!(
            "http://localhost:8080/InvitationRequest/{request_id}/submit"
        ))
        .json(&serde_json::json!({
            "request_id": request_id,
            "invitation_link_id": link_id,
            "requester_id": 8,
            "justification": null,
            "created_at": "2026-05-04T12:00:00Z"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(submit_resp.status(), 200, "InvitationRequest::submit failed");
    let final_state: String = submit_resp.json().await.unwrap();
    assert_eq!(final_state, "expired");
}
```

This single test covers a no-output object method, a request/response object method, and a workflow path using raw JSON ingress.

- [ ] **Step 3: Run regular checks before the external-service smoke**

Run: `cargo test -p domain -p restate-svc`

Expected: PASS.

- [ ] **Step 4: Run ignored integration smoke**

Make sure local services are running:

```bash
docker compose up -d restate
cargo run -p github-stub -- --port 3001 &
```

Then run:

```bash
cargo test -p restate-svc --features integration -- --ignored
```

Expected: PASS. If Restate or github-stub is unavailable, record the exact service startup failure before changing code further.

- [ ] **Step 5: Commit integration smoke expansion**

```bash
git add crates/restate-svc/tests/integration_test.rs
git commit -m "test(restate-svc): cover raw Json ingress paths"
```

---

### Task 7: Final Verification

**Files:**
- No code changes expected.

- [ ] **Step 1: Run final compile check**

Run: `cargo check -p restate-svc`

Expected: PASS.

- [ ] **Step 2: Run required unit tests**

Run: `cargo test -p domain -p restate-svc`

Expected: PASS.

- [ ] **Step 3: Run required runtime ingress smoke**

With Restate and github-stub running, run:

```bash
cargo test -p restate-svc --features integration -- --ignored
```

Expected: PASS.

- [ ] **Step 4: Verify macro removal and docs cleanup**

Run: `rg "impl_restate_json_payload|restate_payload" crates docs/superpowers/plans/2026-05-04-ghinvite-dashboard.md`

Expected: no matches.

- [ ] **Step 5: Inspect git status**

Run: `git status --short`

Expected: no uncommitted changes.

---

## Plan Self-Review

| Spec requirement | Covered by |
|---|---|
| Add workspace `schemars 1.2` with `chrono04` | Task 1 Step 1 |
| Add schema support for domain payload field types | Task 1 Steps 2-7 |
| Preserve ULID string and `SelectedRepos` wire formats | Task 1 Steps 2-7 |
| Convert public Restate handler signatures to `Json<T>` | Tasks 2-4 |
| Keep pure handler logic on plain Rust payload types | Tasks 2-4 |
| Special-case `InvitationRequest::decide` promise resolution | Task 4 Step 7 |
| Remove obsolete `SubmitOutput` and `DispatchInputs` where direct `Json<T>` works | Task 4 Steps 1 and 5-6 |
| Update generated client calls to pass `Json(inv_input)` | Task 3 Step 3 and Task 4 Step 6 |
| Delete `restate_payload.rs` and all macro uses | Task 5 Steps 1-6 |
| Remove dead `bytes` dependency | Task 5 Step 3 |
| Update stale markdown references | Task 5 Step 4 |
| Add domain and service verification commands | Tasks 1, 2, 3, 4, 5, and 7 |
| Expand raw JSON ingress integration smoke | Task 6 |

Placeholder scan: no placeholder-marker strings or code placeholder snippets remain.

---

## Failure Modes Covered

| Failure mode | Covered by |
|---|---|
| `schemars` major-version mismatch | `cargo check -p restate-svc` with workspace `schemars 1.2` |
| Missing nested `JsonSchema` derive such as `WebhookAction` | `cargo check -p restate-svc` |
| ULID ID schema advertises object instead of string | `id_schema_matches_ulid_string_wire_format` |
| `SelectedRepos` schema lies about string-or-array wire format | `selected_repos_schema_matches_wire_format` |
| Promise type mismatch between `submit` and `decide` | `cargo check -p restate-svc` and runtime workflow smoke |
| Generated client call still passes bare `CreateInvitationInput` | `cargo check -p restate-svc` |
| Raw web-style JSON ingress breaks with `Json<T>` signatures | ignored integration smoke |

## Verification Commands

Required before completion:

```bash
cargo check -p restate-svc
cargo test -p domain -p restate-svc
cargo test -p restate-svc --features integration -- --ignored
rg "impl_restate_json_payload|restate_payload" crates docs/superpowers/plans/2026-05-04-ghinvite-dashboard.md
git status --short
```
