# Invitation Request Web Commands Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Deepen the web Invitation Request command interface so routes speak in stable ghinvite commands while Restate target details and payload serialization stay in `crates/web/src/commands.rs`.

**Architecture:** Reuse the existing `GhinviteCommands` trait and `RestateCommands` adapter. Add route-facing constructors for submit, approve, and decline commands, then convert those commands into private Restate payloads inside the command module. Preserve existing route behavior and move raw Restate mapping assertions into command tests.

**Tech Stack:** Rust 2024, axum route handlers, async-trait command facade, serde payload serialization, existing fake `RestateCommandAdapter` tests, cargo test.

---

## File Structure

- Modify: `crates/web/src/commands.rs`
  - Own Invitation Request Restate constants, key helper, private serializable payloads, public route-facing command constructors, and command mapping tests.
- Modify: `crates/web/src/routes/invitation.rs`
  - Use `SubmitInvitationRequest::new` and a domain-oriented warning log message.
- Modify: `crates/web/src/routes/dashboard.rs`
  - Use `DecideInvitationRequest::approve` and `DecideInvitationRequest::decline`, remove route access to the serialized decision enum, and use domain-oriented warning log messages.

No new production modules are needed. No storage, Restate service, view, or middleware files should change for this slice.

### Task 1: Command Mapping Tests

**Files:**
- Modify: `crates/web/src/commands.rs`

- [ ] **Step 1: Replace Invitation Request adapter tests with constructor-based fake-adapter tests**

In `crates/web/src/commands.rs`, replace the existing `submit_invitation_request_sends_restate_workflow` test and `decide_invitation_request_calls_restate_decide_with_decision_payload` test with these three tests:

```rust
    #[tokio::test]
    async fn submit_invitation_request_uses_invitation_request_command_adapter() {
        let (restate, calls) = RecordingRestateClient::new(Value::Null);
        let commands = RestateCommands::new(Arc::new(restate));
        let request_id = domain::RequestId::new();
        let share_link_id = domain::ShareLinkId::new();

        commands
            .submit_invitation_request(SubmitInvitationRequest::new(
                request_id,
                share_link_id,
                42,
                Some("need access".into()),
                at("2026-05-20T11:00:00Z"),
            ))
            .await
            .unwrap();

        let calls = calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        let call = &calls[0];
        assert_eq!(call.service, "InvitationRequest");
        assert_eq!(call.key, request_id.to_string());
        assert_eq!(call.method, "submit");
        assert!(call.send);
        assert_eq!(
            call.body,
            serde_json::json!({
                "request_id": request_id.to_string(),
                "share_link_id": share_link_id.to_string(),
                "requester_id": 42,
                "justification": "need access",
                "created_at": "2026-05-20T11:00:00Z"
            })
        );
    }

    #[tokio::test]
    async fn approve_invitation_request_uses_decision_payload() {
        let (restate, calls) = RecordingRestateClient::new(Value::Null);
        let commands = RestateCommands::new(Arc::new(restate));
        let request_id = domain::RequestId::new();

        commands
            .decide_invitation_request(DecideInvitationRequest::approve(
                request_id,
                7,
                at("2026-05-20T11:45:00Z"),
            ))
            .await
            .unwrap();

        let calls = calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        let call = &calls[0];
        assert_eq!(call.service, "InvitationRequest");
        assert_eq!(call.key, request_id.to_string());
        assert_eq!(call.method, "decide");
        assert!(!call.send);
        assert_eq!(
            call.body,
            serde_json::json!({
                "Approve": {
                    "decided_by": 7,
                    "decided_at": "2026-05-20T11:45:00Z"
                }
            })
        );
    }

    #[tokio::test]
    async fn decline_invitation_request_uses_decision_payload() {
        let (restate, calls) = RecordingRestateClient::new(Value::Null);
        let commands = RestateCommands::new(Arc::new(restate));
        let request_id = domain::RequestId::new();

        commands
            .decide_invitation_request(DecideInvitationRequest::decline(
                request_id,
                8,
                at("2026-05-20T12:15:00Z"),
                Some("not enough context".into()),
            ))
            .await
            .unwrap();

        let calls = calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        let call = &calls[0];
        assert_eq!(call.service, "InvitationRequest");
        assert_eq!(call.key, request_id.to_string());
        assert_eq!(call.method, "decide");
        assert!(!call.send);
        assert_eq!(
            call.body,
            serde_json::json!({
                "Decline": {
                    "decided_by": 8,
                    "decided_at": "2026-05-20T12:15:00Z",
                    "reason": "not enough context"
                }
            })
        );
    }
```

- [ ] **Step 2: Run command tests and verify RED**

Run: `cargo test -p web commands::tests`

Expected: FAIL to compile because `SubmitInvitationRequest::new`, `DecideInvitationRequest::approve`, and `DecideInvitationRequest::decline` do not exist yet.

### Task 2: Command API And Payload Encapsulation

**Files:**
- Modify: `crates/web/src/commands.rs`

- [ ] **Step 1: Add Invitation Request Restate constants**

In `crates/web/src/commands.rs`, add these constants after the existing Share Link constants:

```rust
const INVITATION_REQUEST_SERVICE: &str = "InvitationRequest";
const SUBMIT_INVITATION_REQUEST_METHOD: &str = "submit";
const DECIDE_INVITATION_REQUEST_METHOD: &str = "decide";
```

- [ ] **Step 2: Add an Invitation Request workflow key helper**

In `crates/web/src/commands.rs`, add this helper after `share_link_command_key`:

```rust
fn invitation_request_command_key(request_id: domain::RequestId) -> String {
    request_id.to_string()
}
```

- [ ] **Step 3: Replace Invitation Request adapter methods**

In the `impl<R> GhinviteCommands for RestateCommands<R>` block, replace `submit_invitation_request` and `decide_invitation_request` with this code:

```rust
    async fn submit_invitation_request(&self, command: SubmitInvitationRequest) -> Result<()> {
        let key = invitation_request_command_key(command.request_id);
        let payload = SubmitInvitationRequestPayload::from(command);
        self.restate
            .send(
                INVITATION_REQUEST_SERVICE,
                &key,
                SUBMIT_INVITATION_REQUEST_METHOD,
                &payload,
            )
            .await
    }

    async fn decide_invitation_request(&self, command: DecideInvitationRequest) -> Result<()> {
        let key = invitation_request_command_key(command.request_id);
        let payload = InvitationRequestDecisionPayload::from(command.decision);
        let _: serde_json::Value = self
            .restate
            .call(
                INVITATION_REQUEST_SERVICE,
                &key,
                DECIDE_INVITATION_REQUEST_METHOD,
                &payload,
            )
            .await?;
        Ok(())
    }
```

- [ ] **Step 4: Replace submit and decision command type definitions**

In `crates/web/src/commands.rs`, replace the existing `SubmitInvitationRequest`, `DecideInvitationRequest`, and `InvitationRequestDecision` definitions with this code:

```rust
#[derive(Clone, Debug)]
pub struct SubmitInvitationRequest {
    pub request_id: domain::RequestId,
    pub share_link_id: domain::ShareLinkId,
    pub requester_id: u64,
    pub justification: Option<String>,
    pub created_at: DateTime<Utc>,
}

impl SubmitInvitationRequest {
    pub fn new(
        request_id: domain::RequestId,
        share_link_id: domain::ShareLinkId,
        requester_id: u64,
        justification: Option<String>,
        created_at: DateTime<Utc>,
    ) -> Self {
        Self {
            request_id,
            share_link_id,
            requester_id,
            justification,
            created_at,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
struct SubmitInvitationRequestPayload {
    request_id: domain::RequestId,
    share_link_id: domain::ShareLinkId,
    requester_id: u64,
    justification: Option<String>,
    created_at: DateTime<Utc>,
}

impl From<SubmitInvitationRequest> for SubmitInvitationRequestPayload {
    fn from(command: SubmitInvitationRequest) -> Self {
        Self {
            request_id: command.request_id,
            share_link_id: command.share_link_id,
            requester_id: command.requester_id,
            justification: command.justification,
            created_at: command.created_at,
        }
    }
}

#[derive(Clone, Debug)]
pub struct DecideInvitationRequest {
    pub request_id: domain::RequestId,
    decision: InvitationRequestDecision,
}

impl DecideInvitationRequest {
    pub fn approve(
        request_id: domain::RequestId,
        decided_by: u64,
        decided_at: DateTime<Utc>,
    ) -> Self {
        Self {
            request_id,
            decision: InvitationRequestDecision::Approve {
                decided_by,
                decided_at,
            },
        }
    }

    pub fn decline(
        request_id: domain::RequestId,
        decided_by: u64,
        decided_at: DateTime<Utc>,
        reason: Option<String>,
    ) -> Self {
        Self {
            request_id,
            decision: InvitationRequestDecision::Decline {
                decided_by,
                decided_at,
                reason,
            },
        }
    }
}

#[derive(Clone, Debug)]
enum InvitationRequestDecision {
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

#[derive(Clone, Debug, Serialize)]
enum InvitationRequestDecisionPayload {
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

impl From<InvitationRequestDecision> for InvitationRequestDecisionPayload {
    fn from(decision: InvitationRequestDecision) -> Self {
        match decision {
            InvitationRequestDecision::Approve {
                decided_by,
                decided_at,
            } => Self::Approve {
                decided_by,
                decided_at,
            },
            InvitationRequestDecision::Decline {
                decided_by,
                decided_at,
                reason,
            } => Self::Decline {
                decided_by,
                decided_at,
                reason,
            },
        }
    }
}
```

- [ ] **Step 5: Run command tests and verify GREEN**

Run: `cargo test -p web commands::tests`

Expected: PASS. The new tests should prove that submit uses `send`, approve and decline use `call`, and the serialized payloads still match `crates/restate-svc/src/invitation_request.rs`.

- [ ] **Step 6: Commit command module changes**

Run:

```bash
git add crates/web/src/commands.rs
git commit -m "refactor(web): deepen invitation request commands"
```

Expected: commit succeeds and includes only `crates/web/src/commands.rs`.

### Task 3: Route Call Sites

**Files:**
- Modify: `crates/web/src/routes/invitation.rs`
- Modify: `crates/web/src/routes/dashboard.rs`

- [ ] **Step 1: Update recipient submission route**

In `crates/web/src/routes/invitation.rs`, replace the command construction in `submit_request` with this code:

```rust
        .submit_invitation_request(SubmitInvitationRequest::new(
            request_id,
            link.id,
            session.user_id,
            justification,
            now,
        ))
```

In the same error branch, replace the warning log message with this code:

```rust
        tracing::warn!(error = ?e, "submit invitation request command failed");
```

- [ ] **Step 2: Update dashboard imports**

In `crates/web/src/routes/dashboard.rs`, replace the command import block with this code:

```rust
use crate::commands::{CreateShareLink, DecideInvitationRequest, RevokeShareLink};
```

- [ ] **Step 3: Update approve route**

In `crates/web/src/routes/dashboard.rs`, replace the approve command construction with this code:

```rust
        .decide_invitation_request(DecideInvitationRequest::approve(
            request_id,
            admin.session.user_id,
            Utc::now(),
        ))
```

In the approve error branch, replace the warning log message with this code:

```rust
            tracing::warn!(error = ?e, "approve invitation request command failed");
```

- [ ] **Step 4: Update decline route**

In `crates/web/src/routes/dashboard.rs`, replace the decline command construction with this code:

```rust
        .decide_invitation_request(DecideInvitationRequest::decline(
            request_id,
            admin.session.user_id,
            Utc::now(),
            None,
        ))
```

In the decline error branch, replace the warning log message with this code:

```rust
            tracing::warn!(error = ?e, "decline invitation request command failed");
```

- [ ] **Step 5: Run web tests and verify route compilation**

Run: `cargo test -p web`

Expected: PASS. This catches any route or external test that still tries to import or construct `InvitationRequestDecision` directly.

- [ ] **Step 6: Commit route call-site changes**

Run:

```bash
git add crates/web/src/routes/invitation.rs crates/web/src/routes/dashboard.rs
git commit -m "refactor(web): use invitation request command constructors"
```

Expected: commit succeeds and includes only the two route files.

### Task 4: Final Verification

**Files:**
- Verify all touched files.

- [ ] **Step 1: Check formatting**

Run: `cargo fmt --all --check`

Expected: PASS.

- [ ] **Step 2: Run targeted command tests**

Run: `cargo test -p web commands::tests`

Expected: PASS.

- [ ] **Step 3: Run full web crate tests**

Run: `cargo test -p web`

Expected: PASS.

- [ ] **Step 4: Inspect final diff**

Run: `git diff --stat`

Expected: no uncommitted changes if Tasks 2 and 3 were committed. If formatting changed files during verification, inspect `git diff`, commit only formatting changes for files already touched by this plan, and rerun Steps 1 through 3.

- [ ] **Step 5: Update issue #10 after implementation is merged or pushed**

Post a GitHub issue comment only after the implementation commits are on the branch intended for maintainers. The comment must start with the triage disclaimer and include the verification commands that passed:

```markdown
> *This was generated by AI during triage.*

Implemented Invitation Request web command deepening.

Summary:
- Hid Invitation Request Restate target details and serialized payload shape inside `crates/web/src/commands.rs`.
- Updated recipient submission and Account Admin approve/decline routes to use command constructors.
- Preserved duplicate-pending, pending redirect, approve, and decline route behavior.

Verification:
- `cargo fmt --all --check`
- `cargo test -p web commands::tests`
- `cargo test -p web`
```

Save that exact Markdown body to `/tmp/opencode/issue-10-comment.md`, then run:

```bash
gh issue comment 10 --repo sagikazarmark/ghinvite2.orig --body-file /tmp/opencode/issue-10-comment.md
```

Expected: issue #10 has an implementation summary comment. Do not close the issue unless the maintainer explicitly asks for closure.
