# Invitation Link Description Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add required admin-only invitation-link descriptions, persist them across storage adapters, and use them as the primary Console identifier.

**Architecture:** Description becomes a required `String` on the canonical `domain::InvitationLink` and every create boundary that constructs it. Storage persists `description` in `invitation_links`, with a migration backfill for legacy rows. Web routes validate description at create time, and Console views show description primary while requester-facing views continue hiding admin metadata.

**Tech Stack:** Rust workspace, Axum, Dioxus SSR, sqlx SQLite migrations, Cloudflare D1 adapter, Restate SDK JSON payloads, GitHub mock transport tests.

---

## File Structure

Create:

- `migrations/0002_invitation_link_description.sql`: schema change plus legacy data backfill.

Modify:

- `crates/domain/src/invitation_link.rs`: add required `description` field to `InvitationLink` and domain fixtures.
- `crates/storage/src/records.rs`: include `description` in SQL row types and domain conversion.
- `crates/storage/src/sqlx_impl.rs`: insert/select `description`, add migration backfill and round-trip tests.
- `crates/storage/src/tests.rs`: update cross-storage fixture and assert `description` round-trips through the trait suite.
- `crates/storage-d1/src/bind.rs`: include `description` in D1 join rows and domain conversion.
- `crates/storage-d1/src/wasm_impl.rs`: insert/select `description` in D1 SQL.
- `crates/restate-svc/src/invitation_link.rs`: add `description` to create payload and transition.
- `crates/web/src/commands.rs`: add `description` to route-facing create command and Restate payload assertions.
- `crates/web/src/routes/console.rs`: parse, validate, trim, and dispatch description on create.
- `crates/web/src/views/links.rs`: add create-form `Link details` section and detail-page description display.
- `crates/web/src/views/console.rs`: use description primary in recent invitation links.
- `crates/web/src/views/invitation.rs`: update fixtures and assert requester pages do not leak admin metadata.
- Test fixture files that construct `domain::InvitationLink`: update all struct literals with `description`.

---

### Task 1: Migration And Sqlx Storage

**Files:**

- Create: `migrations/0002_invitation_link_description.sql`
- Modify: `crates/domain/src/invitation_link.rs`
- Modify: `crates/storage/src/records.rs`
- Modify: `crates/storage/src/sqlx_impl.rs`
- Modify: `crates/storage/src/tests.rs`

- [ ] **Step 1: Write the failing migration backfill test**

Add this test inside the existing `#[cfg(test)] mod tests` in `crates/storage/src/sqlx_impl.rs`:

```rust
#[tokio::test]
async fn description_migration_backfills_legacy_rows() {
    let opts = SqliteConnectOptions::new()
        .in_memory(true)
        .foreign_keys(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(opts)
        .await
        .unwrap();

    sqlx::query(
        r#"
        CREATE TABLE invitation_links (
            id TEXT PRIMARY KEY,
            slug TEXT NOT NULL UNIQUE,
            internal_note TEXT
        )
        "#,
    )
    .execute(&pool)
    .await
    .unwrap();

    sqlx::query(
        r#"
        INSERT INTO invitation_links (id, slug, internal_note)
        VALUES
          ('legacy-note', 'codeFromNote1234', '  AI\n\n coding\tworkshop   '),
          ('legacy-code', 'codeFallback5678', '   ')
        "#,
    )
    .execute(&pool)
    .await
    .unwrap();

    for statement in include_str!("../../../migrations/0002_invitation_link_description.sql").split(';') {
        let statement = statement.trim();
        if !statement.is_empty() {
            sqlx::query(statement).execute(&pool).await.unwrap();
        }
    }

    let rows: Vec<(String, String)> = sqlx::query_as(
        r#"SELECT id, description FROM invitation_links ORDER BY id"#,
    )
    .fetch_all(&pool)
    .await
    .unwrap();

    assert_eq!(
        rows,
        vec![
            ("legacy-code".to_string(), "codeFallback5678".to_string()),
            ("legacy-note".to_string(), "AI coding workshop".to_string()),
        ]
    );
}
```

- [ ] **Step 2: Run the migration test to verify it fails**

Run: `cargo test -p storage description_migration_backfills_legacy_rows`

Expected: FAIL at compile time because `migrations/0002_invitation_link_description.sql` does not exist.

- [ ] **Step 3: Add the migration**

Create `migrations/0002_invitation_link_description.sql` with this SQLite/D1-portable SQL:

```sql
ALTER TABLE invitation_links ADD COLUMN description TEXT NOT NULL DEFAULT '';

WITH RECURSIVE normalized(id, value) AS (
  SELECT
    id,
    trim(
      replace(
        replace(
          replace(coalesce(nullif(trim(internal_note), ''), slug), char(13), ' '),
          char(10),
          ' '
        ),
        char(9),
        ' '
      )
    )
  FROM invitation_links
  UNION ALL
  SELECT id, replace(value, '  ', ' ')
  FROM normalized
  WHERE instr(value, '  ') > 0
), final AS (
  SELECT id, substr(value, 1, 120) AS description
  FROM normalized
  WHERE instr(value, '  ') = 0
)
UPDATE invitation_links
SET description = (
  SELECT final.description
  FROM final
  WHERE final.id = invitation_links.id
);
```

- [ ] **Step 4: Run the migration test to verify it passes**

Run: `cargo test -p storage description_migration_backfills_legacy_rows`

Expected: PASS.

- [ ] **Step 5: Add `description` to the domain model**

Update `crates/domain/src/invitation_link.rs`:

```rust
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InvitationLink {
    pub id: InvitationLinkId,
    pub slug: Slug,
    pub installation_id: u64,
    pub account_id: u64,
    pub created_by: u64, // user_id
    pub created_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>,
    pub max_uses: Option<u32>,
    pub uses_count: u32,
    pub permission: Permission,
    pub approval_required: bool,
    pub description: String,
    pub internal_note: Option<String>,
    pub revoked_at: Option<DateTime<Utc>>,
    pub revoked_by: Option<u64>,
    pub repos: Vec<InvitationLinkRepo>,
}
```

Update the domain test fixture in the same file:

```rust
description: "AI coding workshop".into(),
internal_note: None,
```

- [ ] **Step 6: Update Sqlx storage records**

Update `crates/storage/src/records.rs` so `InvitationLinkRow` and `InvitationLinkJoinRow` include `description` before `internal_note`:

```rust
pub description: String,
pub internal_note: Option<String>,
```

Update `InvitationLinkRow::try_into_domain`:

```rust
description: self.description,
internal_note: self.internal_note,
```

Update `InvitationLinkJoinRow::split`:

```rust
description: self.description,
internal_note: self.internal_note,
```

- [ ] **Step 7: Update Sqlx insert and select SQL**

In `crates/storage/src/sqlx_impl.rs`, change the `insert_invitation_link` statement to include `description`:

```rust
INSERT INTO invitation_links
  (id, slug, installation_id, account_id, created_by, created_at, expires_at,
   max_uses, uses_count, permission, approval_required, description, internal_note,
   revoked_at, revoked_by)
VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
```

Add the bind before `internal_note`:

```rust
.bind(&link.description)
.bind(link.internal_note.as_deref())
```

In `get_invitation_link_by_id`, `get_invitation_link_by_slug`, and `list_invitation_links_for_account`, add `l.description` between `l.approval_required` and `l.internal_note` in the `SELECT` list.

- [ ] **Step 8: Update storage fixtures and assertions**

In `crates/storage/src/tests.rs`, update `sample_link`:

```rust
description: "AI coding workshop".into(),
internal_note: None,
```

In `scenario_invitation_link_lifecycle`, after `assert_eq!(by_slug.id, link.id);`, add:

```rust
assert_eq!(by_slug.description, "AI coding workshop");
```

In `crates/storage/src/sqlx_impl.rs`, update the local `sample_link` fixture:

```rust
description: "Contractor onboarding".into(),
internal_note: Some("for the contractor".into()),
```

The existing `assert_eq!(got, link);` in `insert_and_get_invitation_link_round_trips_with_repos` then covers description round-trip.

- [ ] **Step 9: Run storage tests**

Run:

```bash
cargo test -p storage description_migration_backfills_legacy_rows
cargo test -p storage invitation_link
```

Expected: PASS for the filtered storage tests.

- [ ] **Step 10: Commit**

Run:

```bash
git add migrations/0002_invitation_link_description.sql crates/domain/src/invitation_link.rs crates/storage/src/records.rs crates/storage/src/sqlx_impl.rs crates/storage/src/tests.rs
git commit -m "feat(storage): add invitation link descriptions"
```

---

### Task 2: D1 Storage Adapter

**Files:**

- Modify: `crates/storage-d1/src/bind.rs`
- Modify: `crates/storage-d1/src/wasm_impl.rs`

- [ ] **Step 1: Run a compile check to expose D1 row drift**

Run: `cargo check -p storage-d1 --target wasm32-unknown-unknown`

Expected: FAIL because `domain::InvitationLink` now requires `description` and D1 row conversion has not been updated.

- [ ] **Step 2: Update D1 row binding**

In `crates/storage-d1/src/bind.rs`, add `description` to `InvitationLinkJoinRow`:

```rust
pub approval_required: i64,
pub description: String,
pub internal_note: Option<String>,
```

Update `row_to_invitation_link`:

```rust
approval_required: row.approval_required != 0,
description: row.description.clone(),
internal_note: row.internal_note.clone(),
```

Update the `first_rows.push(InvitationLinkJoinRow { ... })` copy in `collect_invitation_links`:

```rust
description: row.description.clone(),
internal_note: row.internal_note.clone(),
```

- [ ] **Step 3: Update D1 insert SQL**

In `crates/storage-d1/src/wasm_impl.rs`, create a `description` binding near the other link fields:

```rust
let description = link.description.clone();
```

Update the insert SQL:

```rust
"INSERT INTO invitation_links
   (id, slug, installation_id, account_id, created_by, created_at,
    expires_at, max_uses, uses_count, permission, approval_required,
    description, internal_note, revoked_at, revoked_by)
 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)"
```

Add the bind before `internal_note`:

```rust
JsValue::from_str(&description),
internal_note,
```

- [ ] **Step 4: Update D1 select SQL**

In `get_invitation_link_by_id`, `get_invitation_link_by_slug`, and `list_invitation_links_for_account`, add `l.description` between `l.approval_required` and `l.internal_note`:

```sql
l.approval_required, l.description, l.internal_note, l.revoked_at, l.revoked_by,
```

- [ ] **Step 5: Run D1 compile check**

Run: `cargo check -p storage-d1 --target wasm32-unknown-unknown`

Expected: PASS.

- [ ] **Step 6: Commit**

Run:

```bash
git add crates/storage-d1/src/bind.rs crates/storage-d1/src/wasm_impl.rs
git commit -m "feat(storage-d1): persist invitation link descriptions"
```

---

### Task 3: Restate And Web Command Boundaries

**Files:**

- Modify: `crates/restate-svc/src/invitation_link.rs`
- Modify: `crates/web/src/commands.rs`
- Modify: Restate fixture files that construct `domain::InvitationLink`

- [ ] **Step 1: Write failing Restate assertions**

In `crates/restate-svc/src/invitation_link.rs`, update `sample_input` with the new field:

```rust
description: "AI coding workshop".into(),
internal_note: Some("test".into()),
```

In `create_inserts_link_with_generated_slug`, add:

```rust
assert_eq!(link.description, "AI coding workshop");
```

In `create_transition_inserts_link_and_audits`, after the existing metadata assertions, add:

```rust
assert_eq!(link.description, "AI coding workshop");
assert!(event.metadata.get("description").is_none());
```

- [ ] **Step 2: Run Restate tests to verify they fail**

Run: `cargo test -p restate-svc invitation_link::tests::create`

Expected: FAIL because `CreateLinkInput` and `DomainInvitationLink` do not carry `description` yet.

- [ ] **Step 3: Add description to Restate create input and transition**

In `crates/restate-svc/src/invitation_link.rs`, add the field to `CreateLinkInput` before `internal_note`:

```rust
pub description: String,
pub internal_note: Option<String>,
```

In `create_invitation_link_transition`, populate the domain link:

```rust
description: input.description.clone(),
internal_note: input.internal_note.clone(),
```

Do not add `description` to the `serde_json::json!` audit metadata block.

- [ ] **Step 4: Write failing web command adapter assertion**

In `crates/web/src/commands.rs`, update the `create_invitation_link_calls_restate_create_and_returns_output` test command:

```rust
description: "AI coding workshop".into(),
internal_note: Some("team onboarding".into()),
```

Add the assertion:

```rust
assert_eq!(call.body["description"], "AI coding workshop");
```

- [ ] **Step 5: Add description to web create command**

In `crates/web/src/commands.rs`, add `description` to `CreateInvitationLink` before `internal_note`:

```rust
pub description: String,
pub internal_note: Option<String>,
```

The existing `RestateCommands::create_invitation_link` serializes `CreateInvitationLink` directly, so no separate payload type is needed.

- [ ] **Step 6: Update Restate fixture literals outside `invitation_link.rs`**

For each `domain::InvitationLink {` literal in these files, add a stable description near `approval_required`:

```rust
description: "AI coding workshop".into(),
```

Files:

- `crates/restate-svc/src/reconcile.rs`
- `crates/restate-svc/src/invitation_request.rs`
- `crates/restate-svc/src/github_invitation.rs`
- `crates/restate-svc/src/invitation_context.rs`

- [ ] **Step 7: Run Restate and command tests**

Run:

```bash
cargo test -p restate-svc invitation_link
cargo test -p web --lib create_invitation_link_calls_restate_create_and_returns_output
```

Expected: PASS.

- [ ] **Step 8: Commit**

Run:

```bash
git add crates/restate-svc/src/invitation_link.rs crates/restate-svc/src/reconcile.rs crates/restate-svc/src/invitation_request.rs crates/restate-svc/src/github_invitation.rs crates/restate-svc/src/invitation_context.rs crates/web/src/commands.rs
git commit -m "feat(commands): carry invitation link descriptions"
```

---

### Task 4: Create Route Validation

**Files:**

- Modify: `crates/web/src/routes/console.rs`

- [ ] **Step 1: Add failing validation tests**

At the bottom of `crates/web/src/routes/console.rs`, add this test module:

```rust
#[cfg(test)]
mod tests {
    use super::validate_description;

    #[test]
    fn description_validation_trims_valid_input() {
        assert_eq!(
            validate_description(Some("  AI coding workshop  ")).unwrap(),
            "AI coding workshop"
        );
    }

    #[test]
    fn description_validation_requires_value() {
        let err = validate_description(None).unwrap_err().to_string();
        assert!(err.contains("description is required"));

        let err = validate_description(Some("   ")).unwrap_err().to_string();
        assert!(err.contains("description is required"));
    }

    #[test]
    fn description_validation_rejects_multiline_text() {
        let err = validate_description(Some("AI\nworkshop")).unwrap_err().to_string();
        assert!(err.contains("description must be a single line"));
    }

    #[test]
    fn description_validation_rejects_over_120_characters() {
        let too_long = "x".repeat(121);
        let err = validate_description(Some(&too_long)).unwrap_err().to_string();
        assert!(err.contains("description must be 120 characters or fewer"));
    }
}
```

- [ ] **Step 2: Run validation tests to verify they fail**

Run: `cargo test -p web --lib description_validation`

Expected: FAIL because `validate_description` is not defined.

- [ ] **Step 3: Implement validation helper and form field**

In `CreateLinkForm`, add `description` as an optional form field before `permission`:

```rust
description: Option<String>,
permission: String,
```

Add this helper near `CreateLinkForm`:

```rust
const DESCRIPTION_MAX_CHARS: usize = 120;

fn validate_description(raw: Option<&str>) -> crate::error::Result<String> {
    let description = raw.unwrap_or("").trim();
    if description.is_empty() {
        return Err(crate::error::WebError::BadRequest(
            "description is required".into(),
        ));
    }
    if description.contains('\n') || description.contains('\r') {
        return Err(crate::error::WebError::BadRequest(
            "description must be a single line".into(),
        ));
    }
    if description.chars().count() > DESCRIPTION_MAX_CHARS {
        return Err(crate::error::WebError::BadRequest(
            "description must be 120 characters or fewer".into(),
        ));
    }
    Ok(description.to_string())
}
```

In `create_link`, call the helper before dispatch:

```rust
let description = match validate_description(form.description.as_deref()) {
    Ok(description) => description,
    Err(e) => return e.into_response(),
};
```

Pass the description to the command:

```rust
description,
internal_note,
```

- [ ] **Step 4: Run validation tests to verify they pass**

Run: `cargo test -p web --lib description_validation`

Expected: PASS.

- [ ] **Step 5: Run route compile check**

Run: `cargo check -p web`

Expected: FAIL only for remaining missing `description` fields in view/test fixtures, or PASS if later tasks have already updated fixtures.

- [ ] **Step 6: Commit if `cargo check -p web` has no unrelated errors**

If this task was executed after Task 5 and `cargo check -p web` passes, commit:

```bash
git add crates/web/src/routes/console.rs
git commit -m "feat(web): validate invitation link descriptions"
```

If `cargo check -p web` fails only because view/test fixtures still need `description`, defer the commit until Task 5 and include `crates/web/src/routes/console.rs` there.

---

### Task 5: Console And Requester Views

**Files:**

- Modify: `crates/web/src/views/links.rs`
- Modify: `crates/web/src/views/console.rs`
- Modify: `crates/web/src/views/invitation.rs`
- Modify: `crates/web/src/invitation_link_resolution.rs`
- Modify: `crates/web/src/account_admin_reads.rs`
- Modify: `crates/web/tests/invitation_resolution.rs`
- Modify: `crates/web/src/commands.rs` fixture at `seed_github_invitation`

- [ ] **Step 1: Update `LinkFormValues` and form render tests**

In `crates/web/src/views/links.rs`, add `description` to `LinkFormValues`:

```rust
pub description: String,
```

Set the default:

```rust
description: String::new(),
```

In `link_create_form_renders_sectioned_console_form`, add assertions:

```rust
assert!(html.contains("Link details"));
assert!(html.contains("name=\"description\""));
assert!(html.contains("required"));
assert!(html.contains("maxlength=\"120\""));
assert!(html.contains("AI coding workshop"));
assert!(html.find("Link details").unwrap() < html.find("Access configuration").unwrap());
```

- [ ] **Step 2: Implement `Link details` form section**

In `LinkCreateFormPage`, insert this section before the current `Access configuration` section:

```rust
section { class: "mac-panel",
    div { class: "space-y-4 p-4",
        div {
            h2 { class: "text-base font-semibold", "Link details" }
            p { class: "mt-1 text-sm text-base-content/65", "Name the admin purpose for this invitation link and keep optional notes separate." }
        }
        div { class: "form-control gap-2",
            label { class: "label", r#for: "description", span { class: "label-text font-medium", "Description" } }
            input { id: "description", r#type: "text", name: "description", value: "{props.form.description}", class: "input input-bordered w-full", required: true, maxlength: "120", placeholder: "AI coding workshop" }
            p { class: "text-sm text-base-content/65", "Visible only to admins. Use a short purpose or audience for this invitation link." }
        }
        div { class: "form-control gap-2",
            label { class: "label", r#for: "internal_note", span { class: "label-text font-medium", "Internal note" } }
            textarea { id: "internal_note", name: "internal_note", class: "textarea textarea-bordered w-full", placeholder: "Why this link exists", "{props.form.internal_note}" }
            p { class: "text-sm text-base-content/65", "Optional admin-only notes. Not visible in the invitation request flow." }
        }
    }
}
```

Remove the old internal-note control from the `Request handling` section.

- [ ] **Step 3: Update link detail fixture and assertions**

In `crates/web/src/views/links.rs`, update `sample_link`:

```rust
description: "AI coding workshop".into(),
internal_note: Some("Contractor onboarding".into()),
```

In `link_detail_renders_operational_context`, replace the title/header expectation:

```rust
assert!(html.contains("<title>AI coding workshop · acme</title>"));
assert!(html.contains("<h1 class=\"text-2xl font-bold\">AI coding workshop</h1>"));
assert!(html.contains("Invitation code"));
assert!(html.contains("abcdEFGH01234567"));
```

- [ ] **Step 4: Implement link detail display**

In `LinkDetailPage`, add:

```rust
let description = props.link.description.clone();
```

Update the layout title and header:

```rust
title: "{description} · {login}",
```

```rust
h1 { class: "text-2xl font-bold", "{description}" }
p { class: "text-sm text-base-content/70", "Invitation code: ", span { class: "font-mono", "{slug}" } }
```

Keep the share URL section label as `Invitation link`.

- [ ] **Step 5: Update overview recent links tests and view**

In `crates/web/src/views/console.rs`, update `sample_link`:

```rust
description: "AI coding workshop".into(),
internal_note: None,
```

Replace the old code-column test with assertions that description is primary and code is secondary:

```rust
assert!(html.contains("<th>Description</th>"));
assert!(!html.contains("<th>Code</th>"));
assert!(html.contains("AI coding workshop"));
assert!(html.contains("abcdEFGH01234567"));
assert!(html.find("AI coding workshop").unwrap() < html.find("abcdEFGH01234567").unwrap());
```

In `OverviewPage`, change the recent-links table header:

```rust
thead { tr { th { "Description" } th { "State" } th { class: "text-right", "Uses" } } }
```

Change the first cell:

```rust
td {
    a {
        class: "link link-hover font-medium",
        href: "/console/accounts/{login}/links/{id_str}",
        "{link.description}"
    }
    p { class: "mt-0.5 font-mono text-xs text-base-content/55", "{slug_str}" }
}
```

- [ ] **Step 6: Update requester-facing no-leak tests**

In `crates/web/src/views/invitation.rs`, update `sample_link`:

```rust
description: "AI coding workshop".into(),
internal_note: Some("Only admins should see this note".into()),
```

In `landing_page_uses_repository_access_language`, add:

```rust
assert!(!html.contains("AI coding workshop"));
assert!(!html.contains("Only admins should see this note"));
```

In `request_form_uses_repository_words`, add the same two negative assertions.

- [ ] **Step 7: Update remaining web fixtures**

Add `description: "AI coding workshop".into(),` to every remaining `InvitationLink {` literal in:

- `crates/web/src/invitation_link_resolution.rs`
- `crates/web/src/account_admin_reads.rs`
- `crates/web/tests/invitation_resolution.rs`
- `crates/web/src/commands.rs`

Use this command to verify no web literal is missed:

```bash
rg "InvitationLink \{" crates/web
```

Each literal should have a nearby `description:` field before `internal_note:`.

- [ ] **Step 8: Run view tests**

Run:

```bash
cargo test -p web --lib links
cargo test -p web --lib console
cargo test -p web --lib invitation
```

Expected: PASS.

- [ ] **Step 9: Run web compile check**

Run: `cargo check -p web`

Expected: PASS.

- [ ] **Step 10: Commit**

Run:

```bash
git add crates/web/src/views/links.rs crates/web/src/views/console.rs crates/web/src/views/invitation.rs crates/web/src/invitation_link_resolution.rs crates/web/src/account_admin_reads.rs crates/web/tests/invitation_resolution.rs crates/web/src/commands.rs crates/web/src/routes/console.rs
git commit -m "feat(web): show invitation link descriptions"
```

---

### Task 6: Full Fixture Sweep And Verification

**Files:**

- No planned edits. If verification exposes a missed fixture, return to the task that owns that file and apply the explicit edit pattern there.

- [ ] **Step 1: Search for remaining invitation-link literals**

Run: `rg "InvitationLink \{" crates`

Expected: Every domain `InvitationLink {` literal includes `description:` before `internal_note:`. Trait names such as `pub trait InvitationLink` are not struct literals and do not need changes.

- [ ] **Step 2: Run workspace compile check**

Run: `cargo check`

Expected: PASS. If it fails with `missing field description`, return to Task 1, Task 3, or Task 5 based on the file path, add `description: "AI coding workshop".into(),` to that fixture, and rerun `cargo check`.

- [ ] **Step 3: Run targeted tests from the spec**

Run:

```bash
cargo test -p storage
cargo test -p restate-svc invitation_link
cargo test -p web --lib links
cargo test -p web --lib console
cargo test -p web --lib invitation
```

Expected: PASS for all commands.

- [ ] **Step 4: Run full workspace tests**

Run: `cargo test`

Expected: PASS.

- [ ] **Step 5: Inspect final diff**

Run:

```bash
git status --short
git diff --check
git diff --stat
```

Expected: `git diff --check` has no output. The diff should be limited to migration, domain/storage adapters, Restate create payloads, web create route, web views, and fixture updates for invitation-link descriptions.

- [ ] **Step 6: Commit verification fixes if needed**

If Step 2 through Step 5 required additional fixture or formatting fixes, return to the task that owns those files and use that task's explicit commit command. Task 6 must not commit with a broad catch-all command.

If there are no additional changes, skip this commit.

---

## Self-Review Checklist

- Spec coverage: Task 1 covers domain, Sqlx storage, and migration backfill. Task 2 covers D1. Task 3 covers Restate and command boundaries. Task 4 covers validation. Task 5 covers Console/requester display. Task 6 covers fixture sweep and verification.
- Audit scope: Task 3 explicitly asserts `invitation_link.created` metadata omits description text.
- Requester privacy: Task 5 adds negative requester-view assertions for description and internal note.
- No edit flow: No task adds update routes, edit forms, or metadata update commands.
- No uniqueness constraint: No task adds a unique index or SQL uniqueness check for `description`.
