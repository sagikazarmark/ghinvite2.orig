# Audit Log Page Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the Account Console audit route's plain `501` response with an authenticated coming-soon dashboard page.

**Architecture:** Add one focused Dioxus view for the Audit Log page, reuse `DashboardLayout`, and extend dashboard navigation so the Audit Log item can be active. Replace the route stub with a `RequireAdminOf` handler that renders the view without reading audit events.

**Tech Stack:** Rust, Axum, Dioxus SSR, tower-sessions, existing `web` crate route and view tests.

---

## Spec Reference

Design spec: `docs/superpowers/specs/2026-05-24-audit-log-page-design.md`

This plan intentionally avoids storage read APIs, audit event browsing, filters, pagination, search, export, disabled controls, sample rows, and table headers.

## File Structure

- Create `crates/web/src/views/audit.rs`: `AuditLogPage` component and its focused render test.
- Modify `crates/web/src/views/mod.rs`: export the `audit` view module.
- Modify `crates/web/src/views/layouts.rs`: add `audit` as a supported dashboard active navigation state.
- Modify `crates/web/src/routes/dashboard.rs`: replace the plain audit stub with an authenticated dashboard handler.
- Modify `crates/web/tests/route_smoke.rs`: assert unauthenticated audit-route access returns plain `404`.
- Modify `crates/web/tests/dashboard_flow.rs`: assert an authenticated account admin receives the Audit Log coming-soon page.

## Task 1: Add The Audit Log View

**Files:**
- Create: `crates/web/src/views/audit.rs`
- Modify: `crates/web/src/views/mod.rs`
- Test: `crates/web/src/views/audit.rs`

- [ ] **Step 1: Add the failing view test**

Create `crates/web/src/views/audit.rs` with this content:

```rust
use dioxus::prelude::*;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audit_log_page_renders_coming_soon_empty_state() {
        let html = crate::views::render::render(|| {
            rsx! {
                AuditLogPage {
                    signed_in_login: Some("admin".to_string()),
                    flash: None,
                    account_login: "acme".to_string(),
                }
            }
        });

        assert!(html.contains("Audit log"));
        assert!(html.contains("<title>Audit log · acme</title>"));
        assert!(html.contains("Review account activity for invitation links, requests, and GitHub invitations."));
        assert!(html.contains("Audit log coming soon"));
        assert!(html.contains("ghinvite records account activity for invitation links, requests, and GitHub invitations. Console browsing is not available yet."));
        assert!(html.contains("Back to overview"));
        assert!(html.contains("href=\"/accounts/acme\""));
        assert!(html.contains("mac-panel"));
        assert!(!html.contains("<table"));
        assert!(!html.contains("Filter"));
        assert!(!html.contains("No events"));
    }
}
```

In `crates/web/src/views/mod.rs`, add the module export at the top of the list:

```rust
pub mod audit;
pub mod components;
pub mod dashboard;
pub mod home;
pub mod invitation;
pub mod layouts;
pub mod links;
pub mod not_found;
pub mod render;
pub mod requests;
pub mod settings;
```

- [ ] **Step 2: Run the focused view test and verify it fails**

Run: `cargo test -p web audit_log_page_renders_coming_soon_empty_state --lib`

Expected: FAIL with a compile error naming missing `AuditLogPage`.

- [ ] **Step 3: Add the minimal Audit Log page component**

Replace `crates/web/src/views/audit.rs` with this content:

```rust
//! Audit Log coming-soon page.

use crate::session::Flash;
use crate::views::layouts::DashboardLayout;
use dioxus::prelude::*;

#[derive(Clone, PartialEq, Props)]
pub struct AuditLogPageProps {
    pub signed_in_login: Option<String>,
    pub flash: Option<Flash>,
    pub account_login: String,
}

#[component]
pub fn AuditLogPage(props: AuditLogPageProps) -> Element {
    let login = props.account_login.clone();

    rsx! {
        DashboardLayout {
            signed_in_login: props.signed_in_login.clone(),
            title: "Audit log · {login}",
            account_login: Some(props.account_login.clone()),
            active_nav: Some("audit".to_string()),
            flash: props.flash.clone(),
            children: rsx! {
                header { class: "mb-6",
                    p { class: "text-sm font-medium text-primary", "Audit" }
                    h1 { class: "text-2xl font-semibold tracking-tight", "Audit log" }
                    p { class: "mt-1 text-sm text-base-content/65", "Review account activity for invitation links, requests, and GitHub invitations." }
                }
                section { class: "mac-panel max-w-3xl p-6",
                    div { class: "max-w-2xl",
                        p { class: "text-xs font-semibold uppercase tracking-[0.18em] text-base-content/45", "Coming soon" }
                        h2 { class: "mt-3 text-lg font-semibold tracking-tight", "Audit log coming soon" }
                        p { class: "mt-2 text-sm leading-6 text-base-content/70",
                            "ghinvite records account activity for invitation links, requests, and GitHub invitations. Console browsing is not available yet."
                        }
                        a { class: "btn btn-primary btn-sm mt-5", href: "/accounts/{login}", "Back to overview" }
                    }
                }
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audit_log_page_renders_coming_soon_empty_state() {
        let html = crate::views::render::render(|| {
            rsx! {
                AuditLogPage {
                    signed_in_login: Some("admin".to_string()),
                    flash: None,
                    account_login: "acme".to_string(),
                }
            }
        });

        assert!(html.contains("Audit log"));
        assert!(html.contains("<title>Audit log · acme</title>"));
        assert!(html.contains("Review account activity for invitation links, requests, and GitHub invitations."));
        assert!(html.contains("Audit log coming soon"));
        assert!(html.contains("ghinvite records account activity for invitation links, requests, and GitHub invitations. Console browsing is not available yet."));
        assert!(html.contains("Back to overview"));
        assert!(html.contains("href=\"/accounts/acme\""));
        assert!(html.contains("mac-panel"));
        assert!(!html.contains("<table"));
        assert!(!html.contains("Filter"));
        assert!(!html.contains("No events"));
    }
}
```

- [ ] **Step 4: Run formatting and the focused view test**

Run: `cargo fmt`

Run: `cargo test -p web audit_log_page_renders_coming_soon_empty_state --lib`

Expected: PASS.

- [ ] **Step 5: Commit the view**

Run:

```bash
git add crates/web/src/views/audit.rs crates/web/src/views/mod.rs
git commit -m "feat(web): add audit log coming soon view"
```

## Task 2: Mark Audit Log Navigation Active

**Files:**
- Modify: `crates/web/src/views/layouts.rs`
- Test: `crates/web/src/views/layouts.rs`

- [ ] **Step 1: Add a failing layout test for the audit active state**

In `crates/web/src/views/layouts.rs`, inside the existing `#[cfg(test)] mod tests`, add this test after `dashboard_layout_renders_real_sidebar_and_account_control`:

```rust
#[test]
fn dashboard_layout_marks_audit_navigation_active() {
    let html = crate::views::render::render(|| {
        rsx! {
            DashboardLayout {
                signed_in_login: Some("admin".to_string()),
                title: "Audit log".to_string(),
                account_login: Some("acme".to_string()),
                active_nav: Some("audit".to_string()),
                flash: None,
                children: rsx! { p { "Audit" } },
            }
        }
    });

    assert!(html.contains("href=\"/accounts/acme/audit\""));
    assert!(html.contains("Audit log"));
    assert!(html.contains("aria-current=\"page\""));
    assert!(html.contains("app-nav-row app-nav-row-active flex w-full items-center"));
    assert!(!html.contains("Audit log (coming soon)"));
}
```

- [ ] **Step 2: Run the focused layout test and verify it fails**

Run: `cargo test -p web dashboard_layout_marks_audit_navigation_active --lib`

Expected: FAIL because `DashboardLayout` does not treat `active_nav: Some("audit")` as active yet.

- [ ] **Step 3: Add the audit active navigation class**

In `DashboardLayout`, after the `settings_side` variable, add `audit_side`:

```rust
let audit_side = if active_nav == "audit" {
    "app-nav-row app-nav-row-active flex w-full items-center"
} else {
    "app-nav-row flex w-full items-center"
};
```

Replace the desktop audit navigation link with:

```rust
a {
    class: "{audit_side}",
    href: "/accounts/{login}/audit",
    aria_current: if active_nav == "audit" { "page" } else { "false" },
    "Audit log"
}
```

Replace the mobile audit navigation link with:

```rust
a {
    class: "{audit_side}",
    href: "/accounts/{login}/audit",
    aria_current: if active_nav == "audit" { "page" } else { "false" },
    "Audit"
}
```

- [ ] **Step 4: Make the existing layout test resilient to the new audit `aria-current` attribute**

In `dashboard_layout_renders_real_sidebar_and_account_control`, replace the current audit-link assertion block:

```rust
assert!(
    html.contains(
        "class=\"app-nav-row flex w-full items-center\" href=\"/accounts/acme/audit\""
    ) || html.contains(
        "href=\"/accounts/acme/audit\" class=\"app-nav-row flex w-full items-center\""
    )
);
```

with this assertion:

```rust
assert!(html.contains("href=\"/accounts/acme/audit\""));
assert!(html.contains("Audit log"));
```

- [ ] **Step 5: Run formatting and layout tests**

Run: `cargo fmt`

Run: `cargo test -p web dashboard_layout_ --lib`

Expected: PASS.

- [ ] **Step 6: Commit the navigation update**

Run:

```bash
git add crates/web/src/views/layouts.rs
git commit -m "feat(web): mark audit navigation active"
```

## Task 3: Replace The Audit Route Stub

**Files:**
- Modify: `crates/web/src/routes/dashboard.rs`
- Modify: `crates/web/tests/route_smoke.rs`
- Modify: `crates/web/tests/dashboard_flow.rs`
- Test: `crates/web/tests/route_smoke.rs`
- Test: `crates/web/tests/dashboard_flow.rs`

- [ ] **Step 1: Replace the unauthenticated smoke expectation**

In `crates/web/tests/route_smoke.rs`, replace `dashboard_routes_return_501` with this test:

```rust
#[tokio::test]
async fn dashboard_audit_route_unauthenticated_returns_plain_404() {
    let app = build_test_app().await;
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/accounts/acme/audit")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(&body[..], b"Not Found");
}
```

- [ ] **Step 2: Add the authenticated dashboard flow test**

In `crates/web/tests/dashboard_flow.rs`, add this test after `dashboard_trailing_slash_for_admin_renders_dashboard_404`:

```rust
#[tokio::test]
async fn dashboard_audit_page_for_admin_renders_coming_soon_state() {
    let (app, cookie) = build_signed_in_admin_app().await;
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/accounts/acme/audit")
                .header("cookie", cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8_lossy(&body);
    assert!(text.contains("dashboard-frame"));
    assert!(text.contains("Audit log coming soon"));
    assert!(text.contains("ghinvite records account activity for invitation links, requests, and GitHub invitations. Console browsing is not available yet."));
    assert!(text.contains("Back to overview"));
    assert!(text.contains("href=\"/accounts/acme\""));
    assert!(text.contains("href=\"/accounts/acme/audit\""));
    assert!(text.contains("aria-current=\"page\""));
    assert!(text.contains("app-nav-row-active"));
    assert!(!text.contains("Audit log is not yet implemented."));
    assert!(!text.contains("<table"));
    assert!(!text.contains("Filter"));
    assert!(!text.contains("No events"));
}
```

- [ ] **Step 3: Run the route tests and verify they fail**

Run: `cargo test -p web dashboard_audit_route_unauthenticated_returns_plain_404 --test route_smoke`

Expected: FAIL because the current audit route returns HTTP `501` without account-admin auth.

Run: `cargo test -p web dashboard_audit_page_for_admin_renders_coming_soon_state --test dashboard_flow`

Expected: FAIL because the current audit route returns HTTP `501` instead of the dashboard page.

- [ ] **Step 4: Replace the audit route handler**

In `crates/web/src/routes/dashboard.rs`, remove this import because it is only used by the old stub:

```rust
use axum::http::StatusCode;
```

Change the audit route registration from:

```rust
.route("/accounts/{login}/audit", get(stub))
```

to:

```rust
.route("/accounts/{login}/audit", get(audit_page))
```

Add this handler after `requests_queue` and before `approve_request`:

```rust
async fn audit_page(admin: RequireAdminOf) -> impl IntoResponse {
    let flash = session::take_flash(&admin.tower).await.unwrap_or(None);
    let signed_in_login = Some(admin.session.login.clone());
    let account_login = admin.account.account_login.clone();

    let html = render(move || {
        rsx! {
            crate::views::audit::AuditLogPage {
                signed_in_login: signed_in_login.clone(),
                flash: flash.clone(),
                account_login: account_login.clone(),
            }
        }
    });
    Html(html).into_response()
}
```

Delete the old `stub` handler at the bottom of the file:

```rust
async fn stub() -> impl IntoResponse {
    (
        StatusCode::NOT_IMPLEMENTED,
        "Audit log is not yet implemented.",
    )
}
```

- [ ] **Step 5: Run formatting and focused route tests**

Run: `cargo fmt`

Run: `cargo test -p web dashboard_audit_route_unauthenticated_returns_plain_404 --test route_smoke`

Expected: PASS.

Run: `cargo test -p web dashboard_audit_page_for_admin_renders_coming_soon_state --test dashboard_flow`

Expected: PASS.

- [ ] **Step 6: Commit the route change**

Run:

```bash
git add crates/web/src/routes/dashboard.rs crates/web/tests/route_smoke.rs crates/web/tests/dashboard_flow.rs
git commit -m "feat(web): render audit log coming soon page"
```

## Task 4: Final Verification

**Files:**
- Verify: `crates/web/src/views/audit.rs`
- Verify: `crates/web/src/views/layouts.rs`
- Verify: `crates/web/src/routes/dashboard.rs`
- Verify: `crates/web/tests/route_smoke.rs`
- Verify: `crates/web/tests/dashboard_flow.rs`

- [ ] **Step 1: Run all focused audit and dashboard checks**

Run: `cargo test -p web audit --lib`

Expected: PASS.

Run: `cargo test -p web dashboard_audit_route_unauthenticated_returns_plain_404 --test route_smoke`

Expected: PASS.

Run: `cargo test -p web dashboard_audit_page_for_admin_renders_coming_soon_state --test dashboard_flow`

Expected: PASS.

- [ ] **Step 2: Run the full web crate test suite**

Run: `cargo test -p web`

Expected: PASS.

- [ ] **Step 3: Run formatting check**

Run: `cargo fmt --check`

Expected: PASS.

- [ ] **Step 4: Inspect final working tree state**

Run: `git status --short`

Expected: no output if each task was committed cleanly.

Run: `git log --oneline -5`

Expected: the recent history includes:

```text
feat(web): render audit log coming soon page
feat(web): mark audit navigation active
feat(web): add audit log coming soon view
```

- [ ] **Step 5: Commit formatting cleanup only if needed**

If `cargo fmt --check` failed and `cargo fmt` changed files, run:

```bash
cargo fmt
git add crates/web/src/views/audit.rs crates/web/src/views/layouts.rs crates/web/src/routes/dashboard.rs crates/web/tests/route_smoke.rs crates/web/tests/dashboard_flow.rs
git commit -m "style(web): format audit log page"
```

If `cargo fmt --check` passed, do not create an extra commit.
