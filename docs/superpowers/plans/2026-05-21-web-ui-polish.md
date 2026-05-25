# Web UI Polish Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Polish the existing ghinvite Dioxus SSR web UI so GitHub access decisions are safer, clearer, accessible, and usable on mobile.

**Architecture:** Keep the existing Axum route handlers, Dioxus SSR components, Tailwind, and DaisyUI. Extend the small account-admin read model only where the approval queue needs more context, and render all new UI server-side without hydration.

**Tech Stack:** Rust, Axum, Dioxus 0.7 SSR, Tailwind CSS v4, DaisyUI v5, `storage::SqlxStorage` integration tests, existing `web` crate route/view tests.

---

## Spec Reference

Design spec: `docs/superpowers/specs/2026-05-21-web-ui-polish-design.md`

This plan intentionally avoids a custom design system, JavaScript clipboard behavior, audit-log implementation, repo search, pagination, and confirmation modals.

## File Structure

- Modify: `crates/web/src/views/links.rs` - safer new-link form copy, defaults, link detail facts, recipient preview action, and stop-link wording.
- Modify: `crates/web/src/routes/dashboard.rs` - route flash copy and mapping from account-admin read rows into richer request rows.
- Modify: `crates/web/src/account_admin_reads.rs` - approval queue read model with permission, repositories, expiration, and approval mode.
- Modify: `crates/web/src/views/requests.rs` - approval queue layout, responsive stacking, request context, and decision consequence copy.
- Modify: `crates/web/src/views/layouts.rs` - mobile dashboard navigation and audit coming-soon labeling.
- Modify: `crates/web/src/views/dashboard.rs` - responsive overview grid and more useful empty state copy.
- Modify: `crates/web/src/views/settings.rs` - account-status copy instead of placeholder copy.
- Modify: `crates/web/src/views/home.rs` - avoid implying a finished audit-log UI.
- Modify: `crates/web/src/views/invitation.rs` - recipient reassurance copy, visible textarea label, and refresh affordance on pending status.
- Modify: `crates/web/tests/route_smoke.rs` - home copy assertion.
- Modify: `crates/web/tests/invitation_resolution.rs` - recipient rendering assertions for new labels and refresh affordance.

## Task 1: Approval Queue Read Model

**Files:**
- Modify: `crates/web/src/account_admin_reads.rs:6-97`
- Test: `crates/web/src/account_admin_reads.rs:469-534`

- [ ] **Step 1: Extend the hydrated row test first**

In `crates/web/src/account_admin_reads.rs`, update `account_admin_pending_request_queue_returns_hydrated_rows_oldest_first` after the existing `rows[0]` assertions:

```rust
        assert_eq!(rows[0].permission, Some(Permission::Pull));
        assert_eq!(rows[0].repos, vec!["acme/api".to_string()]);
        assert_eq!(rows[0].expires_at, None);
        assert_eq!(rows[0].approval_required, Some(true));
```

Update `account_admin_pending_request_queue_uses_missing_related_record_fallbacks` after the existing fallback assertions:

```rust
        assert_eq!(rows[0].permission, None);
        assert!(rows[0].repos.is_empty());
        assert_eq!(rows[0].expires_at, None);
        assert_eq!(rows[0].approval_required, None);
```

- [ ] **Step 2: Run the focused read-model test and verify it fails**

Run: `cargo test -p web account_admin_pending_request_queue --lib`

Expected: FAIL with compile errors that `AccountAdminPendingRequestRow` has no fields named `permission`, `repos`, `expires_at`, and `approval_required`.

- [ ] **Step 3: Add the new row fields**

In `crates/web/src/account_admin_reads.rs`, change the row struct to:

```rust
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AccountAdminPendingRequestRow {
    pub(crate) request_id: RequestId,
    pub(crate) link_slug: String,
    pub(crate) link_id: Option<InvitationLinkId>,
    pub(crate) requester_login: String,
    pub(crate) justification: Option<String>,
    pub(crate) created_at: DateTime<Utc>,
    pub(crate) permission: Option<domain::Permission>,
    pub(crate) repos: Vec<String>,
    pub(crate) expires_at: Option<DateTime<Utc>>,
    pub(crate) approval_required: Option<bool>,
}
```

- [ ] **Step 4: Hydrate the new fields from `InvitationLink`**

Replace `pending_request_row` with:

```rust
fn pending_request_row(
    request: InvitationRequest,
    invitation_link: Option<InvitationLink>,
    requester: Option<domain::User>,
) -> AccountAdminPendingRequestRow {
    let (link_slug, link_id, permission, repos, expires_at, approval_required) = match invitation_link {
        Some(link) => {
            let repos = link
                .repos
                .into_iter()
                .map(|repo| repo.repo_full_name)
                .collect();
            (
                link.slug.as_str().to_string(),
                Some(link.id),
                Some(link.permission),
                repos,
                link.expires_at,
                Some(link.approval_required),
            )
        }
        None => ("(deleted link)".to_string(), None, None, Vec::new(), None, None),
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
        permission,
        repos,
        expires_at,
        approval_required,
    }
}
```

- [ ] **Step 5: Run the focused read-model tests**

Run: `cargo test -p web account_admin_pending_request_queue --lib`

Expected: PASS for the three `account_admin_pending_request_queue_*` tests.

- [ ] **Step 6: Commit**

```bash
git add crates/web/src/account_admin_reads.rs
git commit -m "feat(web): hydrate approval queue link context"
```

## Task 2: Safer Link Creation Defaults And Copy

**Files:**
- Modify: `crates/web/src/views/links.rs:19-135`
- Test: `crates/web/src/views/links.rs`

- [ ] **Step 1: Add a failing unit test for safer form defaults**

Append this test module to `crates/web/src/views/links.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn link_form_defaults_are_safe() {
        let form = LinkFormValues::default();

        assert_eq!(form.permission, "pull");
        assert_eq!(form.expires_in_days, "30");
        assert_eq!(form.max_uses, "");
        assert!(!form.approval_required);
    }
}
```

- [ ] **Step 2: Run the default test and verify it fails**

Run: `cargo test -p web link_form_defaults_are_safe --lib`

Expected: FAIL because the derived `Default` sets `permission` and `expires_in_days` to empty strings.

- [ ] **Step 3: Replace the derived default with an explicit safe default**

In `crates/web/src/views/links.rs`, change the `LinkFormValues` derive and add an explicit `Default` impl:

```rust
#[derive(Clone, PartialEq)]
pub struct LinkFormValues {
    pub permission: String,
    pub approval_required: bool,
    pub max_uses: String,
    pub expires_in_days: String,
    pub internal_note: String,
    pub selected_repo_ids: Vec<u64>,
}

impl Default for LinkFormValues {
    fn default() -> Self {
        Self {
            permission: "pull".into(),
            approval_required: false,
            max_uses: String::new(),
            expires_in_days: "30".into(),
            internal_note: String::new(),
            selected_repo_ids: Vec::new(),
        }
    }
}
```

- [ ] **Step 4: Run the default test and verify it passes**

Run: `cargo test -p web link_form_defaults_are_safe --lib`

Expected: PASS.

- [ ] **Step 5: Add explicit labels and helper copy to the form**

Replace the `children` body of `LinkCreateFormPage` with this structure. Keep the surrounding `DashboardLayout` props unchanged.

```rust
children: rsx! {
    header { class: "mb-6",
        h1 { class: "text-2xl font-bold", "New invitation link" }
        p { class: "mt-2 text-sm text-base-content/70 max-w-2xl",
            "Create a URL that lets a GitHub user request collaborator access to the repositories you choose."
        }
    }
    form {
        method: "post",
        action: "/accounts/{login}/links",
        class: "space-y-6 max-w-2xl",
        div { class: "form-control gap-2",
            label { class: "label", r#for: "permission", span { class: "label-text font-medium", "Permission level" } }
            select {
                id: "permission",
                name: "permission",
                class: "select select-bordered w-full",
                {perms.iter().map(|p| {
                    let selected = props.form.permission == *p;
                    rsx! { option { value: "{p}", selected: selected, "{p}" } }
                })}
            }
            p { class: "text-sm text-base-content/70", "Use pull for read-only access. Maintain and admin can change repository settings." }
        }
        div { class: "alert alert-warning",
            span { "Review elevated permissions before sharing. A link can send GitHub collaborator invitations when a request is approved." }
        }
        div { class: "form-control",
            label { class: "label cursor-pointer justify-start gap-3",
                input {
                    r#type: "checkbox",
                    name: "approval_required",
                    value: "true",
                    checked: props.form.approval_required,
                    class: "checkbox",
                }
                span { class: "label-text", "Require admin approval before invitations are sent" }
            }
            p { class: "text-sm text-base-content/70", "Leave unchecked to auto-approve requests that use this link." }
        }
        div { class: "grid grid-cols-1 md:grid-cols-2 gap-4",
            div { class: "form-control gap-2",
                label { class: "label", r#for: "max_uses", span { class: "label-text font-medium", "Max uses" } }
                input {
                    id: "max_uses",
                    r#type: "number",
                    name: "max_uses",
                    value: "{props.form.max_uses}",
                    class: "input input-bordered w-full",
                    min: "1",
                    placeholder: "Unlimited",
                }
                p { class: "text-sm text-base-content/70", "Blank means unlimited requests." }
            }
            div { class: "form-control gap-2",
                label { class: "label", r#for: "expires_in_days", span { class: "label-text font-medium", "Expires in days" } }
                input {
                    id: "expires_in_days",
                    r#type: "number",
                    name: "expires_in_days",
                    value: "{props.form.expires_in_days}",
                    class: "input input-bordered w-full",
                    min: "1",
                }
                p { class: "text-sm text-base-content/70", "Default is 30 days. Blank creates a link with no expiration." }
            }
        }
        div { class: "form-control gap-2",
            label { class: "label", r#for: "internal_note", span { class: "label-text font-medium", "Internal note" } }
            textarea {
                id: "internal_note",
                name: "internal_note",
                class: "textarea textarea-bordered w-full",
                placeholder: "Why this link exists, visible only to admins",
                "{props.form.internal_note}"
            }
            p { class: "text-sm text-base-content/70", "Use notes to explain the audience, project, or expiry reason." }
        }
        fieldset { class: "form-control gap-2",
            legend { class: "label", span { class: "label-text font-medium", "Repositories" } }
            p { class: "text-sm text-base-content/70", "Select every repository this link may grant access to." }
            {if props.repos.is_empty() {
                rsx! { div { class: "alert", span { "No repositories are available for this installation." } } }
            } else {
                rsx! {
                    div { class: "space-y-1 max-h-80 overflow-y-auto rounded-box border border-base-300 bg-base-100 p-3",
                        {props.repos.iter().map(|repo| {
                            let id = repo.id;
                            let checked = props.form.selected_repo_ids.contains(&id);
                            let full_name = repo.full_name.clone();
                            rsx! {
                                label { class: "label cursor-pointer justify-start gap-2",
                                    input {
                                        r#type: "checkbox",
                                        name: "repo_ids",
                                        value: "{id}",
                                        checked: checked,
                                        class: "checkbox checkbox-sm",
                                    }
                                    span { "{full_name}" }
                                }
                            }
                        })}
                    }
                }
            }}
        }
        div { class: "form-control pt-2",
            button {
                r#type: "submit",
                class: "btn btn-primary",
                "Create link"
            }
        }
    }
}
```

- [ ] **Step 6: Run formatting and focused tests**

Run: `cargo fmt`

Run: `cargo test -p web link_form_defaults_are_safe --lib`

Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/web/src/views/links.rs
git commit -m "feat(web): make invitation link creation safer"
```

## Task 3: Link Detail Operational Hub

**Files:**
- Modify: `crates/web/src/views/links.rs:137-214`
- Modify: `crates/web/src/routes/dashboard.rs:290-340`
- Test: `crates/web/src/views/links.rs`

- [ ] **Step 1: Add a failing link-detail render test**

Inside the existing `#[cfg(test)] mod tests` in `crates/web/src/views/links.rs`, add these imports and helpers:

```rust
    use chrono::{DateTime, Utc};
    use dioxus::prelude::*;
    use domain::{Permission, InvitationLink, InvitationLinkId, InvitationLinkRepo, Slug};

    fn dt(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn sample_link() -> InvitationLink {
        InvitationLink {
            id: InvitationLinkId::new(),
            slug: Slug::from_string("abcdEFGH01234567".to_string()).unwrap(),
            installation_id: 1,
            account_id: 9001,
            created_by: 701,
            created_at: dt("2026-05-04T12:00:00Z"),
            expires_at: Some(dt("2026-06-03T12:00:00Z")),
            max_uses: Some(5),
            uses_count: 2,
            permission: Permission::Push,
            approval_required: true,
            internal_note: Some("Contractor onboarding".into()),
            revoked_at: None,
            revoked_by: None,
            repos: vec![
                InvitationLinkRepo { repo_id: 10, repo_full_name: "acme/api".into() },
                InvitationLinkRepo { repo_id: 11, repo_full_name: "acme/web".into() },
            ],
        }
    }
```

Add this test:

```rust
    #[test]
    fn link_detail_renders_operational_context() {
        let link = sample_link();
        let html = crate::views::render::render(move || {
            rsx! {
                LinkDetailPage {
                    signed_in_login: Some("admin".into()),
                    flash: None,
                    account_login: "acme".into(),
                    link: link.clone(),
                    now: dt("2026-05-05T12:00:00Z"),
                    share_url: "http://127.0.0.1:8787/i/abcdEFGH01234567".into(),
                }
            }
        });

        assert!(html.contains("Open recipient preview"));
        assert!(html.contains("Stop accepting new requests"));
        assert!(html.contains("acme/api"));
        assert!(html.contains("acme/web"));
        assert!(html.contains("push"));
        assert!(html.contains("2 / 5"));
        assert!(html.contains("Contractor onboarding"));
        assert!(!html.contains("Revoke this link"));
    }
```

- [ ] **Step 2: Run the detail render test and verify it fails**

Run: `cargo test -p web link_detail_renders_operational_context --lib`

Expected: FAIL because current link detail does not render preview action, repos, note, or the stop-link wording.

- [ ] **Step 3: Add small formatting helpers in `LinkDetailPage`**

Inside `LinkDetailPage`, after `perm`, add:

```rust
    let uses = props
        .link
        .max_uses
        .map(|n| format!("{} / {}", props.link.uses_count, n))
        .unwrap_or_else(|| format!("{} / unlimited", props.link.uses_count));
    let expires = props
        .link
        .expires_at
        .map(|when| when.format("%Y-%m-%d").to_string())
        .unwrap_or_else(|| "No expiration".into());
    let approval = if props.link.approval_required {
        "Admin approval required"
    } else {
        "Requests are auto-approved"
    };
    let preview_href = props.share_url.clone();
    let repos = props.link.repos.iter().map(|repo| {
        let full_name = repo.repo_full_name.clone();
        rsx! { li { "{full_name}" } }
    });
```

- [ ] **Step 4: Replace the link detail body**

Replace the `children` content of `LinkDetailPage` with:

```rust
children: rsx! {
    header { class: "mb-6 flex flex-col gap-3 md:flex-row md:items-center md:justify-between",
        div {
            h1 { class: "text-2xl font-bold", "{slug}" }
            p { class: "text-sm text-base-content/70", "Invitation link details and controls" }
        }
        span { class: "{badge_class}", "{badge_label}" }
    }
    section { class: "card bg-base-100 shadow mb-6",
        div { class: "card-body gap-4",
            div {
                h2 { class: "card-title", "Share URL" }
                p { class: "text-sm text-base-content/70", "Send this URL to recipients who should request access." }
            }
            input {
                class: "input input-bordered font-mono text-sm w-full",
                readonly: true,
                value: "{props.share_url}",
                aria_label: "Share URL",
            }
            div { class: "card-actions justify-start",
                a { class: "btn btn-outline", href: "{preview_href}", "Open recipient preview" }
            }
        }
    }
    section { class: "grid grid-cols-1 md:grid-cols-2 gap-4 mb-6",
        div { class: "card bg-base-100 shadow", div { class: "card-body",
            h3 { class: "font-semibold", "Permission" }
            p { "{perm}" }
        }}
        div { class: "card bg-base-100 shadow", div { class: "card-body",
            h3 { class: "font-semibold", "Uses" }
            p { "{uses}" }
        }}
        div { class: "card bg-base-100 shadow", div { class: "card-body",
            h3 { class: "font-semibold", "Expiration" }
            p { "{expires}" }
        }}
        div { class: "card bg-base-100 shadow", div { class: "card-body",
            h3 { class: "font-semibold", "Approval" }
            p { "{approval}" }
        }}
    }
    section { class: "card bg-base-100 shadow mb-6",
        div { class: "card-body",
            h2 { class: "card-title", "Repositories" }
            ul { class: "list-disc list-inside space-y-1", {repos} }
        }
    }
    {match props.link.internal_note.as_deref() {
        Some(note) => rsx! {
            section { class: "card bg-base-100 shadow mb-6",
                div { class: "card-body",
                    h2 { class: "card-title", "Internal note" }
                    p { "{note}" }
                }
            }
        },
        None => rsx! {},
    }}
    {if active {
        rsx! {
            section { class: "card bg-base-100 border border-error/30",
                div { class: "card-body",
                    h2 { class: "card-title text-error", "Stop accepting new requests" }
                    p { class: "text-sm text-base-content/70",
                        "This prevents new requests through this link. It does not cancel requests or GitHub invitations already in progress."
                    }
                    form {
                        method: "post",
                        action: "/accounts/{login}/links/{id_str}/revoke",
                        class: "mt-2",
                        button {
                            r#type: "submit",
                            class: "btn btn-error",
                            "Stop accepting new requests"
                        }
                    }
                }
            }
        }
    } else {
        rsx! { p { class: "text-base-content/70", "This link is no longer accepting requests." } }
    }}
}
```

- [ ] **Step 5: Update route flash messages**

In `crates/web/src/routes/dashboard.rs`, inside `revoke_link`, change the error flash message to:

```rust
message: "Could not stop this link. Please try again.".into(),
```

Change the success flash message to:

```rust
message: "Link stopped accepting new requests.".into(),
```

- [ ] **Step 6: Run formatting and focused tests**

Run: `cargo fmt`

Run: `cargo test -p web link_detail_renders_operational_context --lib`

Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/web/src/views/links.rs crates/web/src/routes/dashboard.rs
git commit -m "feat(web): expand invitation link detail context"
```

## Task 4: Approval Queue Context And Responsive Layout

**Files:**
- Modify: `crates/web/src/views/requests.rs:8-90`
- Modify: `crates/web/src/routes/dashboard.rs:346-379`
- Test: `crates/web/src/views/requests.rs`

- [ ] **Step 1: Add a failing render test for request context**

Append this test module to `crates/web/src/views/requests.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{DateTime, Utc};
    use dioxus::prelude::*;

    fn dt(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    #[test]
    fn requests_queue_renders_link_context_before_actions() {
        let html = crate::views::render::render(|| {
            rsx! {
                RequestsQueuePage {
                    signed_in_login: Some("admin".into()),
                    flash: None,
                    account_login: "acme".into(),
                    rows: vec![PendingRequestRow {
                        request_id: "01ARZ3NDEKTSV4RRFFQ69G5FAV".into(),
                        link_slug: "QueueSlug0000006".into(),
                        link_id: "01ARZ3NDEKTSV4RRFFQ69G5FAA".into(),
                        requester_login: "octocat".into(),
                        justification: Some("Need access for launch".into()),
                        created_at: dt("2026-05-04T12:30:00Z"),
                        permission: Some("push".into()),
                        repos: vec!["acme/api".into(), "acme/web".into()],
                        expires_at: Some(dt("2026-06-03T12:00:00Z")),
                        approval_required: Some(true),
                    }],
                }
            }
        });

        assert!(html.contains("push"));
        assert!(html.contains("acme/api"));
        assert!(html.contains("acme/web"));
        assert!(html.contains("Approving sends GitHub collaborator invitations"));
        assert!(html.contains("Need access for launch"));
    }
}
```

- [ ] **Step 2: Run the queue render test and verify it fails**

Run: `cargo test -p web requests_queue_renders_link_context_before_actions --lib`

Expected: FAIL with missing `PendingRequestRow` fields, then missing rendered context once fields exist.

- [ ] **Step 3: Extend the view row props**

In `crates/web/src/views/requests.rs`, change `PendingRequestRow` to:

```rust
#[derive(Clone, PartialEq)]
pub struct PendingRequestRow {
    pub request_id: String,
    pub link_slug: String,
    pub link_id: String,
    pub requester_login: String,
    pub justification: Option<String>,
    pub created_at: DateTime<Utc>,
    pub permission: Option<String>,
    pub repos: Vec<String>,
    pub expires_at: Option<DateTime<Utc>>,
    pub approval_required: Option<bool>,
}
```

- [ ] **Step 4: Map route read-model fields into view rows**

In `crates/web/src/routes/dashboard.rs`, update the `PendingRequestRow` mapping in `requests_queue`:

```rust
            .map(|row| crate::views::requests::PendingRequestRow {
                request_id: row.request_id.to_string(),
                link_slug: row.link_slug,
                link_id: row.link_id.map(|id| id.to_string()).unwrap_or_default(),
                requester_login: row.requester_login,
                justification: row.justification,
                created_at: row.created_at,
                permission: row.permission.map(|permission| permission.to_string()),
                repos: row.repos,
                expires_at: row.expires_at,
                approval_required: row.approval_required,
            })
```

- [ ] **Step 5: Replace the request-row layout**

In `RequestsQueuePage`, inside the `props.rows.iter().map` closure, add:

```rust
                                let permission = r.permission.clone().unwrap_or_else(|| "unknown permission".into());
                                let expires = r
                                    .expires_at
                                    .map(|when| when.format("%Y-%m-%d").to_string())
                                    .unwrap_or_else(|| "No expiration".into());
                                let approval = match r.approval_required {
                                    Some(true) => "Admin approval required",
                                    Some(false) => "Auto-approved link",
                                    None => "Approval mode unavailable",
                                };
                                let repos = if r.repos.is_empty() {
                                    rsx! { p { class: "text-sm text-base-content/70", "Repository details unavailable" } }
                                } else {
                                    rsx! {
                                        ul { class: "list-disc list-inside text-sm",
                                            {r.repos.iter().map(|repo| {
                                                let repo = repo.clone();
                                                rsx! { li { "{repo}" } }
                                            })}
                                        }
                                    }
                                };
```

Then replace the card markup with:

```rust
                                    div { class: "card bg-base-100 shadow",
                                        div { class: "card-body gap-4",
                                            div { class: "flex flex-col gap-4 md:flex-row md:justify-between md:items-start",
                                                div { class: "space-y-3",
                                                    div {
                                                        p {
                                                            strong { "@{requester}" }
                                                            " requested access via "
                                                            a { class: "link", href: "/accounts/{login}/links/{link_id}", "{link_slug}" }
                                                        }
                                                        p { class: "text-xs text-base-content/60", "Requested at {created}" }
                                                    }
                                                    div { class: "flex flex-wrap gap-2",
                                                        span { class: "badge badge-neutral", "Permission: {permission}" }
                                                        span { class: "badge badge-ghost", "Expires: {expires}" }
                                                        span { class: "badge badge-ghost", "{approval}" }
                                                    }
                                                    div {
                                                        h3 { class: "font-semibold text-sm", "Repositories" }
                                                        {repos}
                                                    }
                                                    {match just {
                                                        Some(j) if !j.is_empty() => rsx! {
                                                            blockquote { class: "text-sm italic text-base-content/80", "\"{j}\"" }
                                                        },
                                                        _ => rsx! {},
                                                    }}
                                                    p { class: "text-sm text-base-content/70",
                                                        "Approving sends GitHub collaborator invitations for the repositories listed here."
                                                    }
                                                }
                                                div { class: "flex gap-2 md:flex-col md:items-stretch",
                                                    form {
                                                        method: "post",
                                                        action: "/accounts/{login}/requests/{rid}/approve",
                                                        button { r#type: "submit", class: "btn btn-success btn-sm", "Approve" }
                                                    }
                                                    form {
                                                        method: "post",
                                                        action: "/accounts/{login}/requests/{rid}/decline",
                                                        button { r#type: "submit", class: "btn btn-error btn-sm", "Decline" }
                                                    }
                                                }
                                            }
                                        }
                                    }
```

- [ ] **Step 6: Improve the empty state**

Replace the empty state paragraph in `RequestsQueuePage` with:

```rust
rsx! {
    div { class: "rounded-box bg-base-100 p-6 text-base-content/70",
        h2 { class: "font-semibold text-base-content", "No pending requests" }
        p { class: "mt-1 text-sm", "Requests that need an admin decision will appear here before GitHub invitations are sent." }
    }
}
```

- [ ] **Step 7: Run formatting and focused tests**

Run: `cargo fmt`

Run: `cargo test -p web requests_queue_renders_link_context_before_actions --lib`

Run: `cargo test -p web account_admin_pending_request_queue --lib`

Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add crates/web/src/views/requests.rs crates/web/src/routes/dashboard.rs crates/web/src/account_admin_reads.rs
git commit -m "feat(web): show approval request context"
```

## Task 5: Dashboard Navigation, Overview, Settings, And Home Copy

**Files:**
- Modify: `crates/web/src/views/layouts.rs:37-82`
- Modify: `crates/web/src/views/dashboard.rs:56-96`
- Modify: `crates/web/src/views/settings.rs:31-46`
- Modify: `crates/web/src/views/home.rs:23-28`
- Test: `crates/web/tests/route_smoke.rs:134-153`

- [ ] **Step 1: Add route-smoke assertions for home copy**

In `crates/web/tests/route_smoke.rs`, update `home_returns_html` after the existing assertions:

```rust
    assert!(text.contains("captured history"));
    assert!(!text.contains("audit trail"));
```

- [ ] **Step 2: Run the home smoke test and verify it fails**

Run: `cargo test -p web home_returns_html --test route_smoke`

Expected: FAIL because the current home copy contains `audit trail` and not `captured history`.

- [ ] **Step 3: Add mobile dashboard navigation**

In `DashboardLayout`, after the `Nav` component and before the main flex container, add:

```rust
            nav { class: "md:hidden bg-base-100 border-y border-base-300 overflow-x-auto",
                div { class: "flex gap-2 px-4 py-2 whitespace-nowrap",
                    a { class: "btn btn-ghost btn-sm", href: "/accounts/{login}", "Overview" }
                    a { class: "btn btn-ghost btn-sm", href: "/accounts/{login}/links/new", "New link" }
                    a { class: "btn btn-ghost btn-sm", href: "/accounts/{login}/requests", "Requests" }
                    a { class: "btn btn-ghost btn-sm", href: "/accounts/{login}/audit", "Audit soon" }
                    a { class: "btn btn-ghost btn-sm", href: "/accounts/{login}/settings", "Settings" }
                }
            }
```

In the desktop sidebar, change the audit nav label to:

```rust
li { a { href: "/accounts/{login}/audit", "Audit log (coming soon)" } }
```

- [ ] **Step 4: Make dashboard summary responsive and clearer**

In `OverviewPage`, change the summary grid class from:

```rust
class: "grid grid-cols-2 gap-4 mb-8",
```

to:

```rust
class: "grid grid-cols-1 md:grid-cols-2 gap-4 mb-8",
```

Inside the first summary card body, after the count, add:

```rust
p { class: "text-sm text-base-content/70", "Review people waiting for repository access." }
```

Inside the second summary card body, after the count, add:

```rust
p { class: "text-sm text-base-content/70", "Create another controlled access URL." }
```

Replace the recent-links empty state with:

```rust
rsx! {
    div { class: "rounded-box bg-base-100 p-6 text-base-content/70",
        h3 { class: "font-semibold text-base-content", "No invitation links yet" }
        p { class: "mt-1 text-sm", "Create a link to let recipients request GitHub collaborator access without sending invites by hand." }
        a { class: "btn btn-primary btn-sm mt-4", href: "/accounts/{login}/links/new", "Create first link" }
    }
}
```

- [ ] **Step 5: Rewrite settings as account status**

In `SettingsPage`, replace the `children` body with:

```rust
children: rsx! {
    header { class: "mb-6",
        h1 { class: "text-2xl font-bold", "Settings" }
        p { class: "mt-2 text-sm text-base-content/70", "Account status for this GitHub App installation." }
    }
    div { class: "space-y-4 max-w-2xl",
        div { class: "card bg-base-100 shadow",
            div { class: "card-body",
                h2 { class: "card-title text-base", "Account" }
                p { strong { "{login}" } " ({account_type})" }
            }
        }
        div { class: "card bg-base-100 shadow",
            div { class: "card-body",
                h2 { class: "card-title text-base", "Repository access" }
                p { "{repos_label}" }
                p { class: "text-sm text-base-content/70", "Change repository selection in the GitHub App installation settings for now." }
            }
        }
        div { class: "alert",
            span { "Editable settings are planned for v1.1. This page currently reflects the active installation state." }
        }
    }
}
```

- [ ] **Step 6: Adjust home copy**

In `HomePage`, replace the paragraph text with:

```rust
"ghinvite turns ad-hoc \"can you add this person to our repo?\" \
 messages into shareable links with optional approval, expiration, \
 and captured history."
```

- [ ] **Step 7: Run focused tests**

Run: `cargo fmt`

Run: `cargo test -p web home_returns_html --test route_smoke`

Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add crates/web/src/views/layouts.rs crates/web/src/views/dashboard.rs crates/web/src/views/settings.rs crates/web/src/views/home.rs crates/web/tests/route_smoke.rs
git commit -m "feat(web): polish dashboard chrome and copy"
```

## Task 6: Recipient Flow Reassurance And Labels

**Files:**
- Modify: `crates/web/src/views/invitation.rs:90-283`
- Modify: `crates/web/src/routes/invitation.rs:222-229`
- Test: `crates/web/tests/invitation_resolution.rs:319-552`

- [ ] **Step 1: Add recipient rendering assertions**

In `crates/web/tests/invitation_resolution.rs`, update `landing_renders_active_link` after `assert!(text.contains("acme/api"));`:

```rust
    assert!(text.contains("GitHub sign-in confirms your identity"));
```

Update `request_form_with_other_recipient_pending_request_renders_form` after the existing submit assertion:

```rust
    assert!(text.contains("Justification"));
    assert!(text.contains("visible to account admins"));
```

Update `request_form_with_same_recipient_declined_request_renders_form` after the existing submit assertion:

```rust
    assert!(text.contains("Justification"));
```

- [ ] **Step 2: Add a pending-page render unit test**

Append this test module to `crates/web/src/views/invitation.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use dioxus::prelude::*;
    use domain::RequestState;

    #[test]
    fn pending_page_renders_refresh_affordance() {
        let html = crate::views::render::render(|| {
            rsx! {
                PendingPage {
                    slug: "abcdEFGH01234567".into(),
                    request_id: "01ARZ3NDEKTSV4RRFFQ69G5FAV".into(),
                    request_state: Some(RequestState::Pending),
                    signed_in_login: Some("octocat".into()),
                }
            }
        });

        assert!(html.contains("Check again"));
        assert!(html.contains("awaiting admin review"));
    }
}
```

- [ ] **Step 3: Run recipient tests and verify they fail**

Run: `cargo test -p web --test invitation_resolution`

Run: `cargo test -p web pending_page_renders_refresh_affordance --lib`

Expected: FAIL because the new copy and refresh affordance are not rendered yet.

- [ ] **Step 4: Improve landing reassurance copy**

In `LandingPage`, after the permission badge block and before `{expiry_view}`, add:

```rust
p { class: "text-sm text-base-content/70",
    "GitHub sign-in confirms your identity before any request is sent."
}
```

Change the repo list class from:

```rust
class: "list-disc list-inside mb-4",
```

to:

```rust
class: "list-disc list-inside mb-4 space-y-1",
```

- [ ] **Step 5: Add a visible request-form textarea label**

In `RequestFormPage`, replace the textarea form-control with:

```rust
div { class: "form-control gap-2",
    label { class: "label", r#for: "justification", span { class: "label-text font-medium", "Justification" } }
    textarea {
        id: "justification",
        name: "justification",
        class: "textarea textarea-bordered w-full",
        placeholder: "Why do you need access?",
        rows: "3",
    }
    p { class: "text-sm text-base-content/70", "Optional, visible to account admins." }
}
```

- [ ] **Step 6: Add refresh affordances to pending statuses**

In `PendingProps`, add the request id so the refresh link points back to the current pending URL:

```rust
#[derive(Clone, PartialEq, Props)]
pub struct PendingProps {
    pub slug: String,
    pub request_id: String,
    pub request_state: Option<RequestState>,
    pub signed_in_login: Option<String>,
}
```

In `crates/web/src/routes/invitation.rs`, pass the request id into `PendingPage`:

```rust
            crate::views::invitation::PendingPage {
                slug: slug.clone(),
                request_id: request_id_str.clone(),
                request_state,
                signed_in_login: signed_in_login.clone(),
            }
```

In `PendingPage`, define this before `status_view`:

```rust
    let request_id = props.request_id.clone();
    let refresh_href = format!("/i/{slug}/pending/{request_id}");
```

For the `None` state, replace the alert with:

```rust
div { class: "alert alert-warning",
    div {
        p { "Your request is being processed." }
        a { class: "link", href: "{refresh_href}", "Check again" }
    }
}
```

For `Some(RequestState::Pending)`, replace the alert with:

```rust
div { class: "alert alert-warning",
    div {
        p { "Your request is awaiting admin review." }
        a { class: "link", href: "{refresh_href}", "Check again" }
    }
}
```

For `Some(RequestState::Approved)`, replace the message with:

```rust
"Approved! Check your GitHub notifications and email for the repository invitation."
```

- [ ] **Step 7: Run recipient tests**

Run: `cargo fmt`

Run: `cargo test -p web --test invitation_resolution`

Run: `cargo test -p web pending_page_renders_refresh_affordance --lib`

Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add crates/web/src/views/invitation.rs crates/web/src/routes/invitation.rs crates/web/tests/invitation_resolution.rs
git commit -m "feat(web): clarify recipient invitation flow"
```

## Task 7: Full Verification

**Files:**
- Verify: `crates/web/src/views/*.rs`
- Verify: `crates/web/src/routes/dashboard.rs`
- Verify: `crates/web/src/account_admin_reads.rs`
- Verify: `crates/web/tests/*.rs`

- [ ] **Step 1: Run Rust formatting**

Run: `cargo fmt`

Expected: command exits 0 with no output or formatting-only changes.

- [ ] **Step 2: Run the web crate test suite**

Run: `cargo test -p web`

Expected: all `web` crate tests pass.

- [ ] **Step 3: Build Tailwind CSS**

Run: `npm run build:css`

Working directory: `crates/web`

Expected: Tailwind completes successfully. `crates/web/assets/styles.built.css` is not tracked, so do not commit it unless `git status --short` shows it as an intended tracked modification.

- [ ] **Step 4: Check for banned user-facing copy**

Run: `rg "Revoke this link|audit trail|Refresh to check status" crates/web/src/views crates/web/src/routes/dashboard.rs`

Expected: no matches.

- [ ] **Step 5: Inspect working tree**

Run: `git status --short`

Expected: only intended source and test files are modified.

- [ ] **Step 6: Final commit**

If previous tasks were committed individually and this task only produced no source changes, skip this commit. If formatting or final test fixes changed files, commit them:

```bash
git add crates/web/src crates/web/tests
git commit -m "chore(web): finalize UI polish verification"
```

## Self-Review Notes

- Spec coverage: Tasks 2 and 3 cover link creation and detail, Task 4 covers approval queue context, Task 5 covers dashboard navigation, overview, settings, and home copy, Task 6 covers recipient flow, and Task 7 covers verification.
- Scope check: The work stays inside existing web views, dashboard routes, and account-admin read models. Audit implementation, custom theming, JavaScript clipboard behavior, repo search, and pagination remain out of scope.
- Type consistency: `AccountAdminPendingRequestRow.permission` uses `Option<domain::Permission>` in the read model and is converted to `Option<String>` for `views::requests::PendingRequestRow`. Repository names are `Vec<String>` in both layers.
