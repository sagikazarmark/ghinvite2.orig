# Inline Description Validation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Re-render the Console new invitation link form with inline description validation errors while preserving submitted form values, and keep valid submissions on the existing create-link command path.

**Architecture:** Keep the server authoritative and preserve the existing SSR Dioxus architecture. Add a small validation result shape to the existing link form view model, convert submitted form values back into that model on validation failure, and keep route orchestration shallow.

**Tech Stack:** Rust 2024, axum, serde_qs, Dioxus SSR, tower-sessions, github test mocks, in-memory sqlx storage.

---

## File Structure

- Modify `crates/web/src/views/links.rs`: add form validation state, render validation summary, render field-level description error, and add accessible description input associations.
- Modify `crates/web/src/routes/console.rs`: change description validation to return a structured error, preserve submitted values on validation failure, and re-render the create-link form.
- Modify `crates/web/tests/console_flow.rs`: add a command recorder and route tests for invalid and valid create-link submissions.
- Keep `docs/superpowers/specs/2026-05-26-inline-description-validation-design.md`: design record for this issue.

---

### Task 1: View Model And Markup

**Files:**
- Modify: `crates/web/src/views/links.rs`

- [ ] **Step 1: Write the failing view test**

Add this test inside `#[cfg(test)] mod tests` in `crates/web/src/views/links.rs`:

```rust
#[test]
fn link_create_form_renders_description_validation_errors_and_preserved_values() {
    let mut form = LinkFormValues {
        description: "   ".to_string(),
        permission: "push".to_string(),
        approval_required: true,
        max_uses: "7".to_string(),
        expires_in_days: "45".to_string(),
        internal_note: "Keep this note".to_string(),
        selected_repo_ids: vec![10],
        errors: LinkFormErrors {
            summary: vec!["Fix the highlighted fields before creating this invitation link.".to_string()],
            description: Some("Description is required. Use short, single-line admin-only context for this invitation link.".to_string()),
        },
    };

    let html = crate::views::render::render(move || {
        rsx! {
            LinkCreateFormPage {
                signed_in_login: Some("admin".to_string()),
                flash: None,
                account_login: "acme".to_string(),
                repos: vec![
                    github::payloads::GhRepo { id: 10, full_name: "acme/api".to_string(), private: true },
                    github::payloads::GhRepo { id: 11, full_name: "acme/web".to_string(), private: true },
                ],
                form: form.clone(),
            }
        }
    });

    assert!(html.contains("Fix the highlighted fields before creating this invitation link."));
    assert!(html.contains("Description is required. Use short, single-line admin-only context for this invitation link."));
    assert!(html.contains("id=\"link-form-errors\""));
    assert!(html.contains("role=\"alert\""));
    assert!(html.contains("aria-live=\"polite\""));
    assert!(html.contains("id=\"description-error\""));
    assert!(html.contains("aria-invalid=\"true\""));
    assert!(html.contains("aria-describedby=\"description-help description-error\""));
    assert!(html.contains("value=\"push\" selected"));
    assert!(html.contains("name=\"approval_required\" value=\"true\" checked"));
    assert!(html.contains("name=\"max_uses\" value=\"7\""));
    assert!(html.contains("name=\"expires_in_days\" value=\"45\""));
    assert!(html.contains("Keep this note"));
    assert!(html.contains("value=\"10\" checked"));
    assert!(html.contains("value=\"11\""));
}
```

- [ ] **Step 2: Run the failing view test**

Run: `cargo test -p web --lib link_create_form_renders_description_validation_errors_and_preserved_values`

Expected: FAIL because `LinkFormValues` has no `errors` field and `LinkFormErrors` is not defined.

- [ ] **Step 3: Add form error state and markup**

In `crates/web/src/views/links.rs`, add the error model after `LinkFormValues`:

```rust
#[derive(Clone, Default, PartialEq)]
pub struct LinkFormErrors {
    pub summary: Vec<String>,
    pub description: Option<String>,
}
```

Add `errors: LinkFormErrors` to `LinkFormValues`, and set it to `LinkFormErrors::default()` in `Default::default()`.

At the start of `LinkCreateFormPage`, add:

```rust
let has_description_error = props.form.errors.description.is_some();
let description_class = if has_description_error {
    "input input-bordered input-error w-full"
} else {
    "input input-bordered w-full"
};
let description_described_by = if has_description_error {
    "description-help description-error"
} else {
    "description-help"
};
```

Inside the form, before the first section, render the summary when `props.form.errors.summary` is not empty:

```rust
{if props.form.errors.summary.is_empty() {
    rsx! {}
} else {
    rsx! {
        div { id: "link-form-errors", class: "alert alert-error items-start", role: "alert", aria_live: "polite",
            div {
                h2 { class: "font-semibold", "We couldn't create this invitation link" }
                ul { class: "mt-1 list-disc space-y-1 pl-5 text-sm",
                    {props.form.errors.summary.iter().map(|message| rsx! { li { "{message}" } })}
                }
            }
        }
    }
}}
```

Update the description input and helper/error area to:

```rust
input {
    id: "description",
    r#type: "text",
    name: "description",
    value: "{props.form.description}",
    class: "{description_class}",
    required: true,
    maxlength: "120",
    placeholder: "AI coding workshop",
    aria_invalid: has_description_error.then_some("true"),
    aria_describedby: "{description_described_by}",
}
p { id: "description-help", class: "text-sm text-base-content/65", "Visible only to admins. Use a short purpose or audience for this invitation link." }
{props.form.errors.description.as_ref().map(|message| rsx! {
    p { id: "description-error", class: "text-sm font-medium text-error", "{message}" }
})}
```

- [ ] **Step 4: Run the view tests**

Run: `cargo test -p web --lib link_create_form`

Expected: PASS for the existing form test and the new validation markup test.

---

### Task 2: Route Validation Re-Render

**Files:**
- Modify: `crates/web/src/routes/console.rs`

- [ ] **Step 1: Write or update route-local description validation tests**

Replace the existing `description_validation_*` tests in `crates/web/src/routes/console.rs` with tests that assert structured errors:

```rust
#[test]
fn description_validation_trims_valid_input() {
    assert_eq!(
        validate_description(Some("  AI coding workshop  ")).unwrap(),
        "AI coding workshop"
    );
}

#[test]
fn description_validation_requires_value() {
    let err = validate_description(None).unwrap_err();
    assert_eq!(err.message, "Description is required. Use short, single-line admin-only context for this invitation link.");

    let err = validate_description(Some("   ")).unwrap_err();
    assert_eq!(err.message, "Description is required. Use short, single-line admin-only context for this invitation link.");
}

#[test]
fn description_validation_rejects_multiline_text() {
    let err = validate_description(Some("AI\nworkshop")).unwrap_err();
    assert_eq!(err.message, "Description must be a single line.");
}

#[test]
fn description_validation_rejects_over_120_characters() {
    let too_long = "x".repeat(121);
    let err = validate_description(Some(&too_long)).unwrap_err();
    assert_eq!(err.message, "Description must be 120 characters or fewer.");
}
```

- [ ] **Step 2: Run the failing route-local validation tests**

Run: `cargo test -p web --lib description_validation`

Expected: FAIL because `validate_description` still returns `crate::error::Result<String>`.

- [ ] **Step 3: Implement structured validation and form preservation**

In `crates/web/src/routes/console.rs`, add this type near `DESCRIPTION_MAX_CHARS`:

```rust
#[derive(Clone, Debug, PartialEq, Eq)]
struct DescriptionValidationError {
    message: String,
}
```

Change `validate_description` to:

```rust
fn validate_description(raw: Option<&str>) -> Result<String, DescriptionValidationError> {
    let description = raw.unwrap_or("").trim();
    if description.is_empty() {
        return Err(DescriptionValidationError {
            message: "Description is required. Use short, single-line admin-only context for this invitation link.".into(),
        });
    }
    if description.contains('\n') || description.contains('\r') {
        return Err(DescriptionValidationError {
            message: "Description must be a single line.".into(),
        });
    }
    if description.chars().count() > DESCRIPTION_MAX_CHARS {
        return Err(DescriptionValidationError {
            message: "Description must be 120 characters or fewer.".into(),
        });
    }
    Ok(description.to_string())
}
```

Add helper methods near `validate_description`:

```rust
impl CreateLinkForm {
    fn into_view_values(self) -> crate::views::links::LinkFormValues {
        crate::views::links::LinkFormValues {
            description: self.description.unwrap_or_default(),
            permission: self.permission,
            approval_required: self.approval_required.is_some(),
            max_uses: self.max_uses.unwrap_or_default(),
            expires_in_days: self.expires_in_days.unwrap_or_default(),
            internal_note: self.internal_note.unwrap_or_default(),
            selected_repo_ids: self.repo_ids,
            errors: crate::views::links::LinkFormErrors::default(),
        }
    }
}

async fn load_installation_repos_for_form(
    state: &AppState,
    admin: &RequireConsoleAdminOf,
) -> Vec<github::payloads::GhRepo> {
    let user_api = github::oauth::UserApiClient::new(
        state.github_transport.clone(),
        admin.session.access_token.clone(),
    );
    match user_api
        .list_user_installation_repos(admin.account.installation_id)
        .await
    {
        Ok(r) => r.repositories,
        Err(e) => {
            tracing::warn!(error = ?e, "failed to list installation repos; rendering form with empty list");
            vec![]
        }
    }
}
```

Use `load_installation_repos_for_form` in `new_link_form` instead of duplicating the GitHub client call.

In `create_link`, replace the current description error path with:

```rust
let description = match validate_description(form.description.as_deref()) {
    Ok(description) => description,
    Err(error) => {
        let repos = load_installation_repos_for_form(&state, &admin).await;
        let mut form_values = form.into_view_values();
        form_values.errors.summary = vec!["Fix the highlighted fields before creating this invitation link.".into()];
        form_values.errors.description = Some(error.message);

        let signed_in_login = Some(admin.session.login.clone());
        let account_login = admin.account.account_login.clone();
        let html = render(move || {
            rsx! {
                crate::views::links::LinkCreateFormPage {
                    signed_in_login: signed_in_login.clone(),
                    flash: None,
                    account_login: account_login.clone(),
                    repos: repos.clone(),
                    form: form_values.clone(),
                }
            }
        });
        return Html(html).into_response();
    }
};
```

- [ ] **Step 4: Run route-local validation tests**

Run: `cargo test -p web --lib description_validation`

Expected: PASS.

---

### Task 3: Console Route Integration

**Files:**
- Modify: `crates/web/tests/console_flow.rs`

- [ ] **Step 1: Add command recording support**

In `crates/web/tests/console_flow.rs`, import command trait/types and `Mutex`:

```rust
use std::sync::{Arc, Mutex};
use web::commands::{
    CreateInvitationLink, CreateInvitationLinkOutput, DecideInvitationRequest, GhinviteCommands,
    OnboardInstallation, RecordInstallationUninstalled, RecordRepositorySelectionChange,
    RevokeInvitationLink, RouteGithubInvitationWebhook, SubmitInvitationRequest,
};
```

Add these test helpers after `oauth_sign_in_expectations`:

```rust
#[derive(Clone, Debug, PartialEq)]
enum RecordedCommand {
    CreateInvitationLink {
        description: String,
        internal_note: Option<String>,
        permission: domain::Permission,
        approval_required: bool,
        max_uses: Option<u32>,
        repo_ids: Vec<u64>,
    },
}

#[derive(Default)]
struct RecordingCommands {
    calls: Arc<Mutex<Vec<RecordedCommand>>>,
}

#[async_trait::async_trait]
impl GhinviteCommands for RecordingCommands {
    async fn create_invitation_link(
        &self,
        command: CreateInvitationLink,
    ) -> web::Result<CreateInvitationLinkOutput> {
        self.calls
            .lock()
            .unwrap()
            .push(RecordedCommand::CreateInvitationLink {
                description: command.description,
                internal_note: command.internal_note,
                permission: command.permission,
                approval_required: command.approval_required,
                max_uses: command.max_uses,
                repo_ids: command.repos.into_iter().map(|repo| repo.repo_id).collect(),
            });
        Ok(CreateInvitationLinkOutput {
            link_id: domain::InvitationLinkId::new(),
            slug: "abcdEFGH01234567".to_string(),
        })
    }

    async fn revoke_invitation_link(&self, _command: RevokeInvitationLink) -> web::Result<()> {
        panic!("unexpected revoke_invitation_link command")
    }

    async fn submit_invitation_request(
        &self,
        _command: SubmitInvitationRequest,
    ) -> web::Result<()> {
        panic!("unexpected submit_invitation_request command")
    }

    async fn decide_invitation_request(
        &self,
        _command: DecideInvitationRequest,
    ) -> web::Result<()> {
        panic!("unexpected decide_invitation_request command")
    }

    async fn onboard_installation(&self, _command: OnboardInstallation) -> web::Result<()> {
        panic!("unexpected onboard_installation command")
    }

    async fn record_repository_selection_change(
        &self,
        _command: RecordRepositorySelectionChange,
    ) -> web::Result<()> {
        panic!("unexpected record_repository_selection_change command")
    }

    async fn record_installation_uninstalled(
        &self,
        _command: RecordInstallationUninstalled,
    ) -> web::Result<()> {
        panic!("unexpected record_installation_uninstalled command")
    }

    async fn route_github_invitation_webhook(
        &self,
        _command: RouteGithubInvitationWebhook,
    ) -> web::Result<()> {
        panic!("unexpected route_github_invitation_webhook command")
    }
}
```

- [ ] **Step 2: Add route helper with recordable commands**

Add this helper near the existing signed-in admin app helpers:

```rust
async fn build_signed_in_admin_app_with_recording_commands(
    expectations: Vec<Expectation>,
) -> (axum::Router, String, Arc<Mutex<Vec<RecordedCommand>>>) {
    let storage = Arc::new(storage::SqlxStorage::in_memory().await.unwrap());
    storage
        .insert_installation(&Account {
            installation_id: 77,
            account_id: 9001,
            account_login: "acme".into(),
            account_type: AccountType::Organization,
            installed_at: Utc::now(),
            uninstalled_at: None,
            selected_repos: SelectedRepos::All,
        })
        .await
        .unwrap();

    let storage: Arc<dyn storage::Storage> = storage;
    let transport: Arc<dyn github::HttpTransport> = Arc::new(MockTransport::scripted(expectations));
    let commands = Arc::new(RecordingCommands::default());
    let calls = commands.calls.clone();
    let state = AppState::new(storage, transport, commands, WebConfig::for_local_dev());
    let session_store = tower_sessions::MemoryStore::default();
    let app = build_app(state, session_store);

    let resp1 = app
        .clone()
        .oneshot(Request::builder().uri("/login").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp1.status(), StatusCode::SEE_OTHER);
    let cookie1 = session_cookie(&resp1, None);
    let location = resp1.headers().get("location").unwrap().to_str().unwrap();
    let state = state_from_location(location);

    let resp2 = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/oauth/callback?code=test-code&state={state}"))
                .header("cookie", &cookie1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp2.status(), StatusCode::SEE_OTHER);
    let cookie2 = session_cookie(&resp2, Some(cookie1));

    (app, cookie2, calls)
}

fn installation_repos_expectation() -> Expectation {
    Expectation::ok_json(
        Method::Get,
        "https://api.github.com/user/installations/77/repositories?per_page=100",
        serde_json::json!({
            "total_count": 2,
            "repositories": [
                {"id": 10, "full_name": "acme/api", "private": true},
                {"id": 11, "full_name": "acme/web", "private": true}
            ]
        }),
    )
}
```

- [ ] **Step 3: Write invalid route behavior test**

Add this test to `crates/web/tests/console_flow.rs`:

```rust
#[tokio::test]
async fn create_link_invalid_description_rerenders_form_with_errors_and_preserved_values() {
    let mut expectations = oauth_expectations();
    expectations.push(installation_repos_expectation());
    let (app, cookie, calls) = build_signed_in_admin_app_with_recording_commands(expectations).await;

    let body = "description=%20%20%20&permission=push&approval_required=true&max_uses=7&expires_in_days=45&internal_note=Keep+this+note&repo_ids=10";
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/console/accounts/acme/links")
                .header("cookie", cookie)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    assert!(calls.lock().unwrap().is_empty());
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8_lossy(&body);
    assert!(text.contains("New invitation link"));
    assert!(text.contains("Fix the highlighted fields before creating this invitation link."));
    assert!(text.contains("Description is required. Use short, single-line admin-only context for this invitation link."));
    assert!(text.contains("aria-invalid=\"true\""));
    assert!(text.contains("aria-describedby=\"description-help description-error\""));
    assert!(text.contains("value=\"push\" selected"));
    assert!(text.contains("name=\"approval_required\" value=\"true\" checked"));
    assert!(text.contains("name=\"max_uses\" value=\"7\""));
    assert!(text.contains("name=\"expires_in_days\" value=\"45\""));
    assert!(text.contains("Keep this note"));
    assert!(text.contains("value=\"10\" checked"));
    assert!(text.contains("acme/api"));
}
```

- [ ] **Step 4: Write valid route behavior test**

Add this test to `crates/web/tests/console_flow.rs`:

```rust
#[tokio::test]
async fn create_link_valid_submission_invokes_command_and_redirects() {
    let mut expectations = oauth_expectations();
    expectations.push(installation_repos_expectation());
    let (app, cookie, calls) = build_signed_in_admin_app_with_recording_commands(expectations).await;

    let body = "description=%20AI+coding+workshop%20&permission=push&approval_required=true&max_uses=7&expires_in_days=45&internal_note=Keep+this+note&repo_ids=10";
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/console/accounts/acme/links")
                .header("cookie", cookie)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    let location = resp.headers().get("location").unwrap().to_str().unwrap();
    assert!(location.starts_with("/console/accounts/acme/links/"));
    assert_eq!(calls.lock().unwrap().len(), 1);
    assert_eq!(
        calls.lock().unwrap()[0],
        RecordedCommand::CreateInvitationLink {
            description: "AI coding workshop".to_string(),
            internal_note: Some("Keep this note".to_string()),
            permission: domain::Permission::Push,
            approval_required: true,
            max_uses: Some(7),
            repo_ids: vec![10],
        }
    );
}
```

- [ ] **Step 5: Run the route tests**

Run: `cargo test -p web --test console_flow create_link_`

Expected: PASS for the invalid and valid create-link route tests.

---

### Task 4: Verification And Issue Closure

**Files:**
- No code files expected beyond prior tasks.

- [ ] **Step 1: Run focused tests**

Run: `cargo test -p web --lib description_validation link_create_form`

Expected: PASS.

- [ ] **Step 2: Run route tests**

Run: `cargo test -p web --test console_flow create_link_`

Expected: PASS.

- [ ] **Step 3: Run package tests**

Run: `cargo test -p web`

Expected: PASS.

- [ ] **Step 4: Inspect worktree**

Run: `git status --short`

Expected: only intended files for this issue are changed.

- [ ] **Step 5: Close issue #24**

Run: `gh issue close 24 --repo sagikazarmark/ghinvite2.orig --comment "Implemented inline description validation for the new invitation link form."`

Expected: GitHub reports issue #24 closed.

---

## Self-Review

- Spec coverage: The plan covers invalid description re-rendering, preserved submitted values, summary and field errors, accessible description associations, valid command flow, tests, and issue closure.
- Placeholder scan: No `TBD`, `TODO`, or unspecified implementation steps remain.
- Type consistency: `LinkFormErrors`, `LinkFormValues.errors`, `DescriptionValidationError`, and route helper names are used consistently across tasks.
