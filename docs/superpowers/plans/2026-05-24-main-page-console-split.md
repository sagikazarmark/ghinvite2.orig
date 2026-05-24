# Main Page And Account Console Split Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Split ghinvite into a public main page and an authenticated Account Console under `/console`, while preserving recipient-flow focus and account authorization concealment.

**Architecture:** Keep the existing axum + Dioxus server-rendered structure, but rename the account-admin route/view shell from dashboard to console. Add `/console` as an authenticated account-selection entry point, move account-scoped routes to `/console/accounts/{login}`, and keep recipient flow routes under `/i/{slug}` with focused chrome.

**Tech Stack:** Rust 2024, axum 0.8, Dioxus SSR 0.7, tower-sessions, storage trait, GitHub mock transport tests, Tailwind/DaisyUI CSS.

---

## File Structure

Create or rename:

- Rename `crates/web/src/routes/dashboard.rs` to `crates/web/src/routes/console.rs`. It owns `/console`, `/console/accounts/{login}`, and all account-scoped console subroutes.
- Rename `crates/web/src/views/dashboard.rs` to `crates/web/src/views/console.rs`. It owns the console overview and the new `/console` entry views.
- Rename `crates/web/tests/dashboard_flow.rs` to `crates/web/tests/console_flow.rs`.

Modify:

- `crates/web/src/routes/mod.rs`: export `console` instead of `dashboard`.
- `crates/web/src/lib.rs`: merge `routes::console::router()` instead of `routes::dashboard::router()`.
- `crates/web/src/middleware/auth.rs`: replace account-route 404-first behavior with console-aware auth that redirects unauthenticated console requests before concealment checks.
- `crates/web/src/session.rs`: allow `/console` return targets.
- `crates/web/src/routes/oauth.rs`: logout always redirects to `/`.
- `crates/web/src/routes/setup.rs`: redirect setup completion to `/console/accounts/{login}`.
- `crates/web/src/routes/not_found.rs`: load signed-in session state for public real 404 pages.
- `crates/web/src/views/layouts.rs`: rename `DashboardLayout` to `ConsoleLayout`; make `InvitationLayout` logo-only.
- `crates/web/src/views/components.rs`: add signed-in `Console` header action outside recipient pages.
- `crates/web/src/views/home.rs`: rewrite public home and signed-in shortcut UI.
- `crates/web/src/views/{audit,links,requests,settings,not_found}.rs`: import/use `ConsoleLayout` and update all generated console URLs.
- `crates/web/src/views/invitation.rs`: add status-page exit actions and adjust logout links after logout behavior changes.
- `crates/web/assets/styles.css`: rename dashboard classes to console classes and remove unused home feature-card styles.
- `crates/web/assets/styles.built.css`: regenerate from `styles.css`.
- Tests under `crates/web/tests/` and unit tests under `crates/web/src/views/`: update expected routes, class names, header behavior, home behavior, and return target behavior.

Do not add compatibility routes or tests for old `/accounts/...` URLs.

---

### Task 1: Return Targets And Logout

**Files:**
- Modify: `crates/web/src/session.rs:46-56`
- Modify: `crates/web/src/routes/oauth.rs:22-72`
- Modify: `crates/web/tests/route_smoke.rs:95-110`

- [ ] **Step 1: Write the failing return target test**

In `crates/web/src/session.rs`, extend `return_to_validation`:

```rust
        assert_eq!(
            validate_return_to("/console"),
            Some("/console".to_string())
        );
        assert_eq!(
            validate_return_to("/console/accounts/acme/links/new"),
            Some("/console/accounts/acme/links/new".to_string())
        );
        assert_eq!(validate_return_to("/console/../admin"), None);
        assert_eq!(validate_return_to("//evil.com/console"), None);
```

- [ ] **Step 2: Write the failing logout test**

In `crates/web/tests/route_smoke.rs`, update `logout_clears_session_and_redirects_home` to call logout with a return target and still expect `/`:

```rust
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/logout?return_to=/console/accounts/acme")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    let location = resp.headers().get("location").unwrap().to_str().unwrap();
    assert_eq!(location, "/");
```

- [ ] **Step 3: Run the focused tests and verify they fail**

Run: `cargo test -p web session::tests::return_to_validation`

Run: `cargo test -p web --test route_smoke logout_clears_session_and_redirects_home`

Expected: `return_to_validation` fails because `/console` is rejected, or the logout test fails if logout still honors a return target.

- [ ] **Step 4: Implement return target and logout behavior**

In `crates/web/src/session.rs`, change `validate_return_to`:

```rust
pub fn validate_return_to(raw: &str) -> Option<String> {
    if raw.starts_with("//") || raw.contains("..") {
        None
    } else if raw.starts_with("/i/") {
        Some(raw.to_string())
    } else if raw == "/console" || raw.starts_with("/console/") {
        Some(raw.to_string())
    } else if raw == "/setup/github" || raw.starts_with("/setup/github?") {
        Some(raw.to_string())
    } else {
        None
    }
}
```

In `crates/web/src/routes/oauth.rs`, remove `LogoutQuery`, remove the logout query extractor, and make logout always redirect home:

```rust
async fn logout(tower: TowerSession) -> impl IntoResponse {
    session::clear(&tower).await;
    Redirect::to("/")
}
```

- [ ] **Step 5: Run the focused tests and verify they pass**

Run: `cargo test -p web session::tests::return_to_validation`

Run: `cargo test -p web --test route_smoke logout_clears_session_and_redirects_home`

Expected: both tests pass.

- [ ] **Step 6: Commit**

```bash
git add crates/web/src/session.rs crates/web/src/routes/oauth.rs crates/web/tests/route_smoke.rs
git commit -m "fix(web): allow console return targets"
```

---

### Task 2: Rename Dashboard Shell To Console Shell

**Files:**
- Rename: `crates/web/src/routes/dashboard.rs` to `crates/web/src/routes/console.rs`
- Rename: `crates/web/src/views/dashboard.rs` to `crates/web/src/views/console.rs`
- Rename: `crates/web/tests/dashboard_flow.rs` to `crates/web/tests/console_flow.rs`
- Modify: `crates/web/src/routes/mod.rs`
- Modify: `crates/web/src/lib.rs`
- Modify: `crates/web/src/views/mod.rs`
- Modify: `crates/web/src/views/layouts.rs`
- Modify: `crates/web/assets/styles.css`

- [ ] **Step 1: Rename files**

Run:

```bash
git mv crates/web/src/routes/dashboard.rs crates/web/src/routes/console.rs
git mv crates/web/src/views/dashboard.rs crates/web/src/views/console.rs
git mv crates/web/tests/dashboard_flow.rs crates/web/tests/console_flow.rs
```

- [ ] **Step 2: Update module exports**

In `crates/web/src/routes/mod.rs`, replace `pub mod dashboard;` with:

```rust
pub mod console;
```

In `crates/web/src/lib.rs`, replace the dashboard router merge with:

```rust
        .merge(routes::console::router())
```

In `crates/web/src/views/mod.rs`, replace `pub mod dashboard;` with:

```rust
pub mod console;
```

- [ ] **Step 3: Rename layout component and classes**

In `crates/web/src/views/layouts.rs`, rename `DashboardLayout` to `ConsoleLayout` and replace these class names:

```rust
"dashboard-frame" -> "console-frame"
"dashboard-sidebar" -> "console-sidebar"
"mobile-dashboard-nav" -> "mobile-console-nav"
"dashboard-main" -> "console-main"
```

Also update the test names to `console_layout_renders_real_sidebar_and_account_control` and `console_layout_marks_audit_navigation_active`.

- [ ] **Step 4: Rename CSS selectors**

In `crates/web/assets/styles.css`, replace:

```css
.dashboard-frame
.dashboard-sidebar
.dashboard-main
```

with:

```css
.console-frame
.console-sidebar
.console-main
```

- [ ] **Step 5: Update imports to `ConsoleLayout`**

Replace `DashboardLayout` imports and calls in these files:

```rust
crates/web/src/views/audit.rs
crates/web/src/views/console.rs
crates/web/src/views/links.rs
crates/web/src/views/not_found.rs
crates/web/src/views/requests.rs
crates/web/src/views/settings.rs
```

Use:

```rust
use crate::views::layouts::ConsoleLayout;
```

and:

```rust
ConsoleLayout {
```

- [ ] **Step 6: Update route references to renamed modules**

In `crates/web/src/routes/console.rs`, update references from `crate::views::dashboard::OverviewPage` to:

```rust
crate::views::console::OverviewPage
```

- [ ] **Step 7: Run formatting and the focused view tests**

Run: `cargo fmt --all`

Run: `cargo test -p web layouts::tests`

Run: `cargo test -p web console::tests`

Expected: tests pass. There should be no unresolved `DashboardLayout` or missing module errors after this task.

- [ ] **Step 8: Commit**

```bash
git add crates/web/src/routes/mod.rs crates/web/src/lib.rs crates/web/src/views/mod.rs crates/web/src/views/layouts.rs crates/web/src/views/audit.rs crates/web/src/views/console.rs crates/web/src/views/links.rs crates/web/src/views/not_found.rs crates/web/src/views/requests.rs crates/web/src/views/settings.rs crates/web/src/routes/console.rs crates/web/tests/console_flow.rs crates/web/assets/styles.css
git commit -m "refactor(web): rename dashboard shell to console"
```

---

### Task 3: Move Account Routes Under `/console/accounts`

**Files:**
- Modify: `crates/web/src/routes/console.rs`
- Modify: `crates/web/src/views/layouts.rs`
- Modify: `crates/web/src/views/{audit,console,links,not_found,requests}.rs`
- Modify: `crates/web/src/routes/setup.rs`
- Modify: `crates/web/tests/console_flow.rs`
- Modify: `crates/web/tests/route_smoke.rs`

- [ ] **Step 1: Update failing route tests to new paths**

In `crates/web/tests/console_flow.rs`, replace route requests and assertions:

```rust
"/accounts/acme" -> "/console/accounts/acme"
"/accounts/acme/" -> "/console/accounts/acme/"
"/accounts/acme/missing" -> "/console/accounts/acme/missing"
"/accounts/acme/audit" -> "/console/accounts/acme/audit"
"/accounts/acme/links/not-a-link-id/revoke" -> "/console/accounts/acme/links/not-a-link-id/revoke"
"href=\"/accounts/acme\"" -> "href=\"/console/accounts/acme\""
"href=\"/accounts/acme/audit\"" -> "href=\"/console/accounts/acme/audit\""
```

In `crates/web/tests/route_smoke.rs`, delete `dashboard_audit_route_unauthenticated_returns_plain_404`. Do not keep a test whose purpose is old `/accounts/...` behavior. Task 4 adds the replacement console auth test.

- [ ] **Step 2: Run the focused tests and verify they fail**

Run: `cargo test -p web --test console_flow`

Expected: tests fail because routes still register old account paths.

- [ ] **Step 3: Update route registrations**

In `crates/web/src/routes/console.rs`, replace the router with `/console` and `/console/accounts/{login}` routes:

```rust
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/console", get(console_index))
        .route("/console/accounts/{login}", get(overview))
        .route(
            "/console/accounts/{login}/",
            get(not_found).fallback(plain_not_found),
        )
        .route("/console/accounts/{login}/links/new", get(new_link_form))
        .route("/console/accounts/{login}/links", axum::routing::post(create_link))
        .route("/console/accounts/{login}/links/{link_id}", get(link_detail))
        .route(
            "/console/accounts/{login}/links/{link_id}/revoke",
            axum::routing::post(revoke_link),
        )
        .route("/console/accounts/{login}/requests", get(requests_queue))
        .route(
            "/console/accounts/{login}/requests/{request_id}/approve",
            axum::routing::post(approve_request),
        )
        .route(
            "/console/accounts/{login}/requests/{request_id}/decline",
            axum::routing::post(decline_request),
        )
        .route("/console/accounts/{login}/settings", get(settings_page))
        .route("/console/accounts/{login}/audit", get(audit_page))
        .route(
            "/console/accounts/{login}/{*rest}",
            get(not_found).fallback(plain_not_found),
        )
}
```

Add a temporary authenticated placeholder for `console_index`; Task 5 will replace it:

```rust
async fn console_index(tower: tower_sessions::Session) -> impl IntoResponse {
    let session = session::load(&tower).await.unwrap_or_default();
    if !session.is_authenticated() {
        return login_redirect("/console").into_response();
    }
    crate::error::WebError::NotFound.into_response()
}
```

Add helper:

```rust
fn login_redirect(return_to: &str) -> axum::response::Redirect {
    let encoded: String = url::form_urlencoded::byte_serialize(return_to.as_bytes()).collect();
    axum::response::Redirect::to(&format!("/login?return_to={encoded}"))
}
```

- [ ] **Step 4: Update generated console links**

Use these replacements in view files and route redirects:

```rust
format!("/accounts/{}", login) -> format!("/console/accounts/{}", login)
"/accounts/{login}" -> "/console/accounts/{login}"
"/accounts/{login}/links/new" -> "/console/accounts/{login}/links/new"
"/accounts/{login}/links" -> "/console/accounts/{login}/links"
"/accounts/{login}/links/{id_str}" -> "/console/accounts/{login}/links/{id_str}"
"/accounts/{login}/links/{id_str}/revoke" -> "/console/accounts/{login}/links/{id_str}/revoke"
"/accounts/{login}/requests" -> "/console/accounts/{login}/requests"
"/accounts/{login}/requests/{rid}/approve" -> "/console/accounts/{login}/requests/{rid}/approve"
"/accounts/{login}/requests/{rid}/decline" -> "/console/accounts/{login}/requests/{rid}/decline"
"/accounts/{login}/settings" -> "/console/accounts/{login}/settings"
"/accounts/{login}/audit" -> "/console/accounts/{login}/audit"
```

In `crates/web/src/routes/setup.rs`, change setup success redirect to:

```rust
Ok(Redirect::to(&format!("/console/accounts/{account_login}")).into_response())
```

- [ ] **Step 5: Run focused route tests**

Run: `cargo test -p web --test console_flow`

Run: `cargo test -p web --test route_smoke`

Expected: route namespace tests pass with the existing unauthenticated concealment behavior. Task 4 changes unauthenticated console routes from 404 to login redirects.

- [ ] **Step 6: Commit**

```bash
git add crates/web/src/routes/console.rs crates/web/src/views/layouts.rs crates/web/src/views/audit.rs crates/web/src/views/console.rs crates/web/src/views/links.rs crates/web/src/views/not_found.rs crates/web/src/views/requests.rs crates/web/src/routes/setup.rs crates/web/tests/console_flow.rs crates/web/tests/route_smoke.rs
git commit -m "feat(web): move account console routes under console"
```

---

### Task 4: Console Auth Redirects And Generic Real 404s

**Files:**
- Modify: `crates/web/src/middleware/auth.rs`
- Modify: `crates/web/src/routes/console.rs`
- Modify: `crates/web/src/routes/not_found.rs`
- Modify: `crates/web/src/views/not_found.rs`
- Modify: `crates/web/tests/console_flow.rs`
- Modify: `crates/web/tests/route_smoke.rs`

- [ ] **Step 1: Write failing unauthenticated console redirect tests**

In `crates/web/tests/console_flow.rs`, update unauthenticated account tests to expect redirects:

```rust
#[tokio::test]
async fn console_account_route_unauthenticated_redirects_to_login() {
    let app = build_test_app().await;
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/console/accounts/acme/audit")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    let location = resp.headers().get("location").unwrap().to_str().unwrap();
    assert_eq!(location, "/login?return_to=%2Fconsole%2Faccounts%2Facme%2Faudit");
}
```

In `crates/web/tests/route_smoke.rs`, add `console_audit_route_unauthenticated_redirects_to_login`, request `/console/accounts/acme/audit`, and assert the same encoded login redirect.

- [ ] **Step 2: Write failing authenticated unknown-account 404 test**

Add a test that signs in but does not insert an `acme` installation:

```rust
#[tokio::test]
async fn console_unknown_account_for_signed_in_user_renders_generic_404() {
    let (app, cookie) = build_signed_in_app_without_installation().await;
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/console/accounts/acme")
                .header("cookie", cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8_lossy(&body);
    assert!(text.contains("Page not found"));
    assert!(text.contains("app-header"));
    assert!(text.contains("@octocat"));
    assert!(!text.contains("console-frame"));
    assert!(!text.contains("acme"));
}
```

Implement `build_signed_in_app_without_installation()` by copying `build_signed_in_admin_app()` but without `insert_installation` and without the org membership expectation.

- [ ] **Step 3: Run focused tests and verify they fail**

Run: `cargo test -p web --test console_flow console_account_route_unauthenticated_redirects_to_login`

Run: `cargo test -p web --test console_flow console_unknown_account_for_signed_in_user_renders_generic_404`

Expected: failures show current extractor behavior returns plain 404 or no generic HTML 404.

- [ ] **Step 4: Add generic not-found renderer helper and signed-in public fallback**

In `crates/web/src/middleware/auth.rs`, add:

```rust
fn generic_not_found_response(signed_in_login: Option<String>) -> axum::response::Response {
    let html = render(move || {
        rsx! {
            crate::views::not_found::PublicNotFoundPage {
                signed_in_login: signed_in_login.clone(),
            }
        }
    });
    (axum::http::StatusCode::NOT_FOUND, Html(html)).into_response()
}
```

In `crates/web/src/routes/not_found.rs`, update the public fallback signature to accept the tower session and pass signed-in state into `PublicNotFoundPage`:

```rust
pub async fn public(method: Method, uri: Uri, tower: tower_sessions::Session) -> Response {
    if method != Method::GET && method != Method::HEAD {
        return plain_not_found();
    }

    if is_asset_like_miss(uri.path()) {
        return plain_not_found();
    }

    let session = crate::session::load(&tower).await.unwrap_or_default();
    let signed_in_login = if session.is_authenticated() {
        Some(session.login.clone())
    } else {
        None
    };

    if method == Method::HEAD {
        return (
            StatusCode::NOT_FOUND,
            [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
            Body::empty(),
        )
            .into_response();
    }

    let html = render(move || {
        rsx! {
            crate::views::not_found::PublicNotFoundPage {
                signed_in_login: signed_in_login.clone(),
            }
        }
    });
    (StatusCode::NOT_FOUND, Html(html)).into_response()
}
```

Update console-shell 404 copy in `crates/web/src/views/not_found.rs`:

```rust
const CONSOLE_NOT_FOUND_MESSAGE: &str = "This console page is not available.";
```

Use `CONSOLE_NOT_FOUND_MESSAGE` in `DashboardNotFoundPage`, then rename that component to `ConsoleNotFoundPage`.

- [ ] **Step 5: Replace `RequireAdminOf` with console-aware extractor behavior**

In `crates/web/src/middleware/auth.rs`, rename `RequireAdminOf` to `RequireConsoleAdminOf` and set `type Rejection = axum::response::Response`.

In the unauthenticated branch, return a login redirect with encoded current URI:

```rust
        if !session.is_authenticated() {
            let return_to = parts
                .uri
                .path_and_query()
                .map(|value| value.as_str())
                .unwrap_or("/console");
            let encoded: String = url::form_urlencoded::byte_serialize(return_to.as_bytes()).collect();
            return Err(axum::response::Redirect::to(&format!(
                "/login?return_to={encoded}"
            ))
            .into_response());
        }
```

For missing account or non-admin, return generic real 404:

```rust
        let account = match state.storage.get_active_installation_by_login(&login).await {
            Ok(Some(account)) => account,
            Ok(None) => return Err(generic_not_found_response(Some(session.login.clone()))),
            Err(error) => return Err(crate::error::WebError::Storage(error).into_response()),
        };
```

For failed admin check, use the same generic 404 response. For GitHub/storage/internal errors, preserve existing error response behavior.

- [ ] **Step 6: Update console route handlers to use the renamed extractor**

In `crates/web/src/routes/console.rs`, replace `RequireAdminOf` with `RequireConsoleAdminOf` in imports and handler signatures.

Update `console_not_found_response` to render `crate::views::not_found::ConsoleNotFoundPage`.

- [ ] **Step 7: Run focused tests and verify they pass**

Run: `cargo test -p web --test console_flow`

Run: `cargo test -p web --test route_smoke console_audit_route_unauthenticated_redirects_to_login`

Expected: console auth redirect, generic 404, and route smoke tests pass.

- [ ] **Step 8: Commit**

```bash
git add crates/web/src/middleware/auth.rs crates/web/src/routes/console.rs crates/web/src/routes/not_found.rs crates/web/src/views/not_found.rs crates/web/tests/console_flow.rs crates/web/tests/route_smoke.rs
git commit -m "fix(web): authenticate console urls before concealment"
```

---

### Task 5: `/console` Account Picker And Empty/Error States

**Files:**
- Modify: `crates/web/src/routes/console.rs`
- Modify: `crates/web/src/views/console.rs`
- Modify: `crates/web/tests/console_flow.rs`

- [ ] **Step 1: Add view tests for console index states**

In `crates/web/src/views/console.rs`, add structs:

```rust
#[derive(Clone, PartialEq)]
pub struct ConsoleAccountChoice {
    pub login: String,
    pub account_type: String,
}

#[derive(Clone, PartialEq)]
pub enum ConsoleIndexState {
    AccountPicker { accounts: Vec<ConsoleAccountChoice> },
    Empty,
    LoadError,
}
```

Add tests:

```rust
#[test]
fn console_index_renders_account_picker() {
    let html = crate::views::render::render(|| {
        rsx! {
            ConsoleIndexPage {
                signed_in_login: Some("admin".to_string()),
                state: ConsoleIndexState::AccountPicker { accounts: vec![
                    ConsoleAccountChoice { login: "acme".to_string(), account_type: "Organization".to_string() },
                    ConsoleAccountChoice { login: "octocat".to_string(), account_type: "Personal account".to_string() },
                ] },
            }
        }
    });

    assert!(html.contains("Choose an account"));
    assert!(html.contains("href=\"/console/accounts/acme\""));
    assert!(html.contains("Organization"));
    assert!(html.contains("Personal account"));
}

#[test]
fn console_index_renders_empty_state() {
    let html = crate::views::render::render(|| {
        rsx! {
            ConsoleIndexPage {
                signed_in_login: Some("admin".to_string()),
                state: ConsoleIndexState::Empty,
            }
        }
    });

    assert!(html.contains("No accounts connected"));
    assert!(html.contains("Install GitHub App"));
    assert!(html.contains("href=\"/install\""));
    assert!(html.contains("Go home"));
}

#[test]
fn console_index_renders_load_error() {
    let html = crate::views::render::render(|| {
        rsx! {
            ConsoleIndexPage {
                signed_in_login: Some("admin".to_string()),
                state: ConsoleIndexState::LoadError,
            }
        }
    });

    assert!(html.contains("We couldn’t load your accounts") || html.contains("We couldn't load your accounts"));
    assert!(html.contains("Try again"));
    assert!(html.contains("href=\"/console\""));
}
```

Use ASCII apostrophe in implementation copy: `We couldn't load your accounts`.

- [ ] **Step 2: Implement `ConsoleIndexPage`**

In `crates/web/src/views/console.rs`, add:

```rust
#[derive(Clone, PartialEq, Props)]
pub struct ConsoleIndexPageProps {
    pub signed_in_login: Option<String>,
    pub state: ConsoleIndexState,
}

#[component]
pub fn ConsoleIndexPage(props: ConsoleIndexPageProps) -> Element {
    rsx! {
        crate::views::layouts::HomeLayout {
            signed_in_login: props.signed_in_login.clone(),
            title: "Console · ghinvite".to_string(),
            account_login: None,
            active_nav: None,
            flash: None,
            children: rsx! {
                section { class: "mx-auto w-full max-w-3xl py-8 sm:py-12",
                    {match props.state.clone() {
                        ConsoleIndexState::AccountPicker { accounts } => rsx! {
                            h1 { class: "text-2xl font-semibold tracking-tight", "Choose an account" }
                            p { class: "mt-2 text-sm text-base-content/65", "Select the account console you want to manage." }
                            div { class: "mac-panel mt-5 divide-y divide-base-300 overflow-hidden",
                                {accounts.into_iter().map(|account| {
                                    let href = format!("/console/accounts/{}", account.login);
                                    rsx! {
                                        a { class: "flex items-center justify-between gap-3 px-4 py-3 hover:bg-base-200/60", href: "{href}",
                                            span { class: "min-w-0",
                                                span { class: "block truncate text-sm font-medium", "{account.login}" }
                                                span { class: "mt-0.5 block text-xs text-base-content/60", "{account.account_type}" }
                                            }
                                            span { class: "text-sm text-primary", "Open" }
                                        }
                                    }
                                })}
                            }
                            a { class: "btn btn-ghost btn-sm mt-4", href: "/install", "Install another account" }
                        },
                        ConsoleIndexState::Empty => rsx! {
                            div { class: "mac-panel p-6",
                                p { class: "text-xs font-semibold uppercase tracking-[0.18em] text-base-content/45", "Console" }
                                h1 { class: "mt-3 text-2xl font-semibold tracking-tight", "No accounts connected" }
                                p { class: "mt-2 text-sm leading-6 text-base-content/70", "Install the GitHub App on a personal account or organization before creating share links." }
                                div { class: "mt-5 flex flex-col gap-2 sm:flex-row",
                                    a { class: "btn btn-primary", href: "/install", "Install GitHub App" }
                                    a { class: "btn btn-ghost", href: "/", "Go home" }
                                }
                            }
                        },
                        ConsoleIndexState::LoadError => rsx! {
                            div { class: "mac-panel p-6",
                                p { class: "text-xs font-semibold uppercase tracking-[0.18em] text-error", "Account loading failed" }
                                h1 { class: "mt-3 text-2xl font-semibold tracking-tight", "We couldn't load your accounts" }
                                p { class: "mt-2 text-sm leading-6 text-base-content/70", "GitHub account discovery did not complete. Try again before changing installation settings." }
                                div { class: "mt-5 flex flex-col gap-2 sm:flex-row",
                                    a { class: "btn btn-primary", href: "/console", "Try again" }
                                    a { class: "btn btn-ghost", href: "/install", "Install GitHub App" }
                                    a { class: "btn btn-ghost", href: "/", "Go home" }
                                }
                            }
                        },
                    }}
                }
            },
        }
    }
}
```

- [ ] **Step 3: Add route integration tests**

In `crates/web/tests/console_flow.rs`, add these tests with concrete assertions:

```rust
#[tokio::test]
async fn console_index_unauthenticated_redirects_to_login() {
    let app = build_test_app().await;
    let resp = app
        .oneshot(Request::builder().uri("/console").body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    let location = resp.headers().get("location").unwrap().to_str().unwrap();
    assert_eq!(location, "/login?return_to=%2Fconsole");
}

#[tokio::test]
async fn console_index_redirects_when_one_admin_account() {
    let (app, cookie) = build_signed_in_admin_app_with_console_installations(vec!["acme"]).await;
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/console")
                .header("cookie", cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    let location = resp.headers().get("location").unwrap().to_str().unwrap();
    assert_eq!(location, "/console/accounts/acme");
}

#[tokio::test]
async fn console_index_renders_picker_for_multiple_admin_accounts() {
    let (app, cookie) = build_signed_in_admin_app_with_console_installations(vec!["octocat", "acme"]).await;
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/console")
                .header("cookie", cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8_lossy(&body);
    assert!(text.contains("Choose an account"));
    assert!(text.find("/console/accounts/acme").unwrap() < text.find("/console/accounts/octocat").unwrap());
}

#[tokio::test]
async fn console_index_renders_empty_state_for_no_admin_accounts() {
    let (app, cookie) = build_signed_in_app_without_installation().await;
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/console")
                .header("cookie", cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8_lossy(&body);
    assert!(text.contains("No accounts connected"));
    assert!(text.contains("Install GitHub App"));
}

#[tokio::test]
async fn console_index_renders_error_when_github_installations_fail() {
    let (app, cookie) = build_signed_in_app_with_failed_installation_discovery().await;
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/console")
                .header("cookie", cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8_lossy(&body);
    assert!(text.contains("We couldn't load your accounts"));
    assert!(text.contains("Try again"));
}
```

Use `MockTransport::scripted` expectations for GitHub:

```rust
fn oauth_sign_in_expectations() -> Vec<Expectation> {
    vec![
        Expectation {
            method: Method::Post,
            url: "https://github.com/login/oauth/access_token".into(),
            required_headers: BTreeMap::new(),
            expected_body: None,
            response: Response {
                status: 200,
                headers: BTreeMap::new(),
                body: br#"{"access_token":"u_xxx","token_type":"bearer","scope":"read:user read:org"}"#.to_vec(),
            },
        },
        Expectation::ok_json(
            Method::Get,
            "https://api.github.com/user",
            serde_json::json!({"id": 42, "login": "octocat"}),
        ),
    ]
}
```

For one or more visible installations, append an installation-list expectation:

```rust
Expectation::ok_json(
    Method::Get,
    "https://api.github.com/user/installations?per_page=100",
    serde_json::json!({
        "total_count": 1,
        "installations": [{
            "id": 77,
            "account": {"id": 9001, "login": "acme", "type": "Organization"},
            "repository_selection": "all",
            "target_type": "Organization"
        }]
    }),
)
```

For organization admin verification, include:

```rust
Expectation::ok_json(
    Method::Get,
    "https://api.github.com/user/memberships/orgs/acme",
    serde_json::json!({"role": "admin", "state": "active"}),
)
```

Define `build_signed_in_admin_app_with_console_installations(logins: Vec<&str>)` in the test module. It should insert active local installations for every login, build a mock transport from `oauth_sign_in_expectations()` plus the installation-list and membership expectations, sign in through `/login` and `/oauth/callback`, then return `(app, cookie)`. Use account ids `9001 + index as u64` and installation ids `77 + index as u64` so the local rows and GitHub payloads intersect by account id.

Define `build_signed_in_app_with_failed_installation_discovery()` with an in-memory storage instance, a mock transport, a local-dev app state, the `/login` request, the `/oauth/callback` request, and the returned session cookie. The mock transport should use `oauth_sign_in_expectations()` followed by this failing installation-discovery expectation:

```rust
Expectation {
    method: Method::Get,
    url: "https://api.github.com/user/installations?per_page=100".into(),
    required_headers: BTreeMap::new(),
    expected_body: None,
    response: Response {
        status: 500,
        headers: BTreeMap::new(),
        body: b"server error".to_vec(),
    },
}
```

- [ ] **Step 4: Run route tests and verify they fail**

Run: `cargo test -p web --test console_flow console_index`

Expected: tests fail until `/console` discovery is implemented.

- [ ] **Step 5: Implement console account discovery**

In `crates/web/src/routes/console.rs`, replace the placeholder `console_index` with:

```rust
async fn console_index(
    axum::extract::State(state): axum::extract::State<AppState>,
    tower: tower_sessions::Session,
) -> impl IntoResponse {
    let mut session = session::load(&tower).await.unwrap_or_default();
    if !session.is_authenticated() {
        return login_redirect("/console").into_response();
    }

    let accounts = match load_console_accounts(&state, &mut session).await {
        Ok(accounts) => accounts,
        Err(error) => {
            tracing::warn!(error = ?error, "failed to load console accounts");
            let signed_in_login = Some(session.login.clone());
            let html = render(move || rsx! {
                crate::views::console::ConsoleIndexPage {
                    signed_in_login: signed_in_login.clone(),
                    state: crate::views::console::ConsoleIndexState::LoadError,
                }
            });
            return Html(html).into_response();
        }
    };

    let _ = session::save(&tower, &session).await;

    if accounts.len() == 1 {
        return axum::response::Redirect::to(&format!(
            "/console/accounts/{}",
            accounts[0].login
        ))
        .into_response();
    }

    let state_view = if accounts.is_empty() {
        crate::views::console::ConsoleIndexState::Empty
    } else {
        crate::views::console::ConsoleIndexState::AccountPicker { accounts }
    };
    let signed_in_login = Some(session.login.clone());
    let html = render(move || rsx! {
        crate::views::console::ConsoleIndexPage {
            signed_in_login: signed_in_login.clone(),
            state: state_view.clone(),
        }
    });
    Html(html).into_response()
}
```

Add helper:

```rust
async fn load_console_accounts(
    state: &AppState,
    session: &mut session::Session,
) -> Result<Vec<crate::views::console::ConsoleAccountChoice>, crate::error::WebError> {
    let user_api = github::oauth::UserApiClient::new(
        state.github_transport.clone(),
        session.access_token.clone(),
    );
    let visible = user_api.list_user_installations().await?;
    let mut accounts = Vec::new();

    for installation in visible.installations {
        let Some(account) = state
            .storage
            .get_active_installation_by_account_id(installation.account.id)
            .await?
        else {
            continue;
        };

        let is_admin = if account.account_type == domain::AccountType::User {
            session.login == account.account_login
        } else {
            crate::middleware::auth::check_admin(state, session, &account.account_login).await?
        };

        if is_admin {
            accounts.push(crate::views::console::ConsoleAccountChoice {
                login: account.account_login,
                account_type: match account.account_type {
                    domain::AccountType::User => "Personal account".to_string(),
                    domain::AccountType::Organization => "Organization".to_string(),
                },
            });
        }
    }

    accounts.sort_by(|a, b| a.login.cmp(&b.login));
    Ok(accounts)
}
```

- [ ] **Step 6: Run focused tests and verify they pass**

Run: `cargo test -p web --test console_flow console_index`

Expected: all `/console` entry tests pass.

- [ ] **Step 7: Commit**

```bash
git add crates/web/src/routes/console.rs crates/web/src/views/console.rs crates/web/tests/console_flow.rs
git commit -m "feat(web): add console account picker"
```

---

### Task 6: Rewrite Home Page And Normal Header

**Files:**
- Modify: `crates/web/src/routes/home.rs`
- Modify: `crates/web/src/views/home.rs`
- Modify: `crates/web/src/views/components.rs`
- Modify: `crates/web/assets/styles.css`
- Modify: `crates/web/tests/home_flow.rs`
- Modify: `crates/web/tests/route_smoke.rs`

- [ ] **Step 1: Write failing home route tests**

In `crates/web/tests/home_flow.rs`, replace the redirect-following test with direct assertions that signed-in `/` stays home:

```rust
#[tokio::test]
async fn signed_in_admin_with_existing_installation_stays_on_home() {
    let (app, cookie) = build_signed_in_app_with_installation().await;
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8_lossy(&body);
    assert!(text.contains("Share Link Code"));
    assert!(text.contains("Create invitation link"));
    assert!(text.contains("href=\"/console\""));
    assert!(text.contains("Console"));
    assert!(text.contains("@octocat"));
    assert!(!text.contains("home-features"));
}
```

Update `route_smoke::home_returns_html` to assert signed-out concise copy and no feature cards:

```rust
    assert!(text.contains("Sign in with GitHub"));
    assert!(text.contains("GitHub repository access"));
    assert!(!text.contains("home-features"));
    assert!(!text.contains("home-feature-item"));
```

- [ ] **Step 2: Write failing header component test**

In `crates/web/src/views/components.rs`, update `nav_renders_single_theme_toggle_and_signed_in_controls`:

```rust
        assert!(html.contains("href=\"/console\""));
        assert!(html.contains("Console"));
        assert!(html.contains("Sign out"));
        assert!(html.contains("@admin"));
```

- [ ] **Step 3: Run focused tests and verify they fail**

Run: `cargo test -p web --test home_flow`

Run: `cargo test -p web --test route_smoke home_returns_html`

Run: `cargo test -p web components::tests::nav_renders_single_theme_toggle_and_signed_in_controls`

Expected: tests fail because `/` still redirects and home still has feature cards.

- [ ] **Step 4: Stop redirecting signed-in home**

In `crates/web/src/routes/home.rs`, remove the storage lookup and redirect block. Keep only session-derived signed-in login and render `HomePage`:

```rust
async fn home(tower: TowerSession) -> impl IntoResponse {
    let session = session::load(&tower).await.unwrap_or_default();
    let signed_in_login = if session.is_authenticated() {
        Some(session.login.clone())
    } else {
        None
    };

    let html = render(move || rsx! { HomePage { signed_in_login: signed_in_login.clone() } });
    Html(html).into_response()
}
```

Remove unused `State`, `Redirect`, and `AppState` imports from `home.rs`.

- [ ] **Step 5: Add Console action to normal header**

In `crates/web/src/views/components.rs`, inside the signed-in branch, render:

```rust
                    Some(login) => rsx! {
                        a { class: "btn btn-primary btn-sm h-8 min-h-0 px-3", href: "/console", "Console" }
                        span { class: "hidden max-w-32 truncate px-1 text-xs text-base-content/60 sm:inline-flex", "@{login}" }
                        a { class: "btn btn-ghost btn-sm h-8 min-h-0 px-2", href: "/logout", "Sign out" }
                    },
```

- [ ] **Step 6: Rewrite home view**

In `crates/web/src/views/home.rs`, replace `HomePage` children with a two-state layout:

```rust
const SHARE_CODE_SCRIPT: &str = r#"
(function () {
  document.addEventListener('click', function (event) {
    var button = event.target.closest('[data-open-share-code]');
    if (!button) return;
    openShareCode();
  });
  document.addEventListener('keydown', function (event) {
    if (event.key !== 'Enter' || !event.target.matches('[data-share-code-input]')) return;
    event.preventDefault();
    openShareCode();
  });
  function openShareCode() {
    var input = document.querySelector('[data-share-code-input]');
    var message = document.querySelector('[data-share-code-message]');
    var code = input ? input.value.trim() : '';
    if (!code) {
      if (message) message.textContent = 'Enter a share link code first.';
      if (input) input.focus();
      return;
    }
    if (message) message.textContent = '';
    window.location.href = '/i/' + encodeURIComponent(code);
  }
})();
"#;
```

For signed-out users, render concise copy and sign-in action:

```rust
None => rsx! {
    section { class: "mx-auto flex min-h-[calc(100vh-10rem)] w-full max-w-xl items-center py-10",
        div { class: "mac-panel w-full p-6 text-center",
            p { class: "text-xs font-semibold uppercase tracking-[0.18em] text-primary", "GitHub access" }
            h1 { class: "mt-3 text-2xl font-semibold tracking-tight", "Request and manage repository access" }
            p { class: "mt-3 text-sm leading-6 text-base-content/70", "ghinvite helps GitHub users request repository access and helps account admins create controlled invitation links." }
            a { class: "btn btn-primary mt-5", href: "/login", "Sign in with GitHub" }
        }
    }
}
```

For signed-in users, render the shortcut and separated console action:

```rust
Some(_) => rsx! {
    section { class: "mx-auto flex min-h-[calc(100vh-10rem)] w-full max-w-xl items-center py-10",
        div { class: "mac-panel w-full p-6",
            div {
                h1 { class: "text-2xl font-semibold tracking-tight", "Open an invitation" }
                p { class: "mt-2 text-sm leading-6 text-base-content/70", "Enter the code from an invitation link to request repository access." }
                div { class: "form-control mt-5 gap-2",
                    label { class: "label", r#for: "share-link-code", span { class: "label-text font-medium", "Share Link Code" } }
                    div { class: "flex flex-col gap-2 sm:flex-row",
                        input { id: "share-link-code", class: "input input-bordered flex-1", placeholder: "Paste code", "data-share-code-input": "true" }
                        button { r#type: "button", class: "btn btn-primary", "data-open-share-code": "true", "Open invitation" }
                    }
                    p { class: "min-h-5 text-sm text-error", "aria-live": "polite", "data-share-code-message": "true" }
                }
            }
            div { class: "my-6 border-t border-base-300" }
            div { class: "flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between",
                div {
                    h2 { class: "text-base font-semibold", "Create an invitation link" }
                    p { class: "mt-1 text-sm text-base-content/65", "Open the console to create a share link for your account." }
                }
                a { class: "btn btn-outline", href: "/console", "Create invitation link" }
            }
            script { "{SHARE_CODE_SCRIPT}" }
        }
    }
}
```

- [ ] **Step 7: Remove unused home feature styles**

In `crates/web/assets/styles.css`, remove `.home-features`, `.home-feature-item`, `.home-feature-kicker`, and the media block that offsets feature items. Keep `.home-hero` only if still used; remove it if the new home view no longer uses it.

- [ ] **Step 8: Run focused tests and verify they pass**

Run: `cargo test -p web --test home_flow`

Run: `cargo test -p web --test route_smoke home_returns_html`

Run: `cargo test -p web components::tests::nav_renders_single_theme_toggle_and_signed_in_controls`

Expected: home and header tests pass.

- [ ] **Step 9: Commit**

```bash
git add crates/web/src/routes/home.rs crates/web/src/views/home.rs crates/web/src/views/components.rs crates/web/assets/styles.css crates/web/tests/home_flow.rs crates/web/tests/route_smoke.rs
git commit -m "feat(web): rewrite public home page"
```

---

### Task 7: Focus Recipient Layout And Add Status Exits

**Files:**
- Modify: `crates/web/src/views/layouts.rs`
- Modify: `crates/web/src/views/invitation.rs`
- Modify: `crates/web/tests/route_smoke.rs`
- Modify: `crates/web/tests/invitation_resolution.rs`

- [ ] **Step 1: Write failing layout test**

In `crates/web/src/views/layouts.rs`, update `invitation_layout_uses_product_theme`:

```rust
        assert!(html.contains("ghinvite"));
        assert!(html.contains("href=\"/\""));
        assert!(!html.contains("app-header"));
        assert!(!html.contains("theme-toggle"));
        assert!(!html.contains("Sign in"));
        assert!(!html.contains("Sign out"));
        assert!(!html.contains("Console"));
```

- [ ] **Step 2: Write failing pending-page exits test**

In `crates/web/src/views/invitation.rs`, update `pending_page_renders_refresh_affordance`:

```rust
        assert!(html.contains("Create your own link"));
        assert!(html.contains("href=\"/console\""));
        assert!(html.contains("Go home"));
        assert!(html.contains("href=\"/\""));
```

- [ ] **Step 3: Run focused tests and verify they fail**

Run: `cargo test -p web layouts::tests::invitation_layout_uses_product_theme`

Run: `cargo test -p web invitation::tests::pending_page_renders_refresh_affordance`

Expected: tests fail because invitation layout still renders `Nav` and pending page has no exit actions.

- [ ] **Step 4: Make invitation layout logo-only**

In `crates/web/src/views/layouts.rs`, replace `Nav` in `InvitationLayout` with:

```rust
            a {
                class: "fixed left-4 top-3 z-10 inline-flex items-center gap-2 rounded-lg px-2 py-1.5 text-sm font-semibold tracking-tight text-base-content hover:bg-base-100",
                href: "/",
                span { class: "grid size-5 place-items-center rounded-md bg-primary text-[0.7rem] font-bold text-primary-content", "g" }
                span { "ghinvite" }
            }
```

Keep the existing centered `main` and `mac-panel` structure.

- [ ] **Step 5: Add status exit actions**

In `crates/web/src/views/invitation.rs`, add this below `{status_view}` in `PendingPage`:

```rust
                div { class: "mt-6 flex flex-col gap-2 sm:flex-row",
                    a { class: "btn btn-primary", href: "/console", "Create your own link" }
                    a { class: "btn btn-ghost", href: "/", "Go home" }
                }
```

Update request-form logout link to `/logout` because logout no longer preserves return targets:

```rust
let logout_href = "/logout".to_string();
```

- [ ] **Step 6: Update route smoke invitation 404 assertions**

In `crates/web/tests/route_smoke.rs`, invitation 404 tests should assert focused layout behavior:

```rust
    assert!(text.contains("mac-panel"));
    assert!(!text.contains("app-header"));
    assert!(!text.contains("theme-toggle"));
```

- [ ] **Step 7: Run focused tests and verify they pass**

Run: `cargo test -p web layouts::tests::invitation_layout_uses_product_theme`

Run: `cargo test -p web invitation::tests::pending_page_renders_refresh_affordance`

Run: `cargo test -p web --test route_smoke invitation_landing_unknown_slug_returns_404`

Run: `cargo test -p web --test route_smoke invitation_unknown_nested_route_returns_recipient_404`

Expected: recipient layout and status tests pass.

- [ ] **Step 8: Commit**

```bash
git add crates/web/src/views/layouts.rs crates/web/src/views/invitation.rs crates/web/tests/route_smoke.rs crates/web/tests/invitation_resolution.rs
git commit -m "feat(web): focus recipient invitation layout"
```

---

### Task 8: Final Rename Cleanup, CSS Build, And Full Verification

**Files:**
- Modify: any remaining files containing dashboard route/view/CSS naming
- Modify: `crates/web/assets/styles.built.css`
- Modify: tests with stale `/accounts` or dashboard assertions

- [ ] **Step 1: Search for stale dashboard naming and old account URLs**

Run: `rg "dashboard|Dashboard|/accounts/|dashboard-" crates/web/src crates/web/tests crates/web/assets/styles.css`

Expected: remaining matches are either historical comments that should be updated or intentional domain-independent text. There should be no generated `/accounts/...` links, no `DashboardLayout`, and no dashboard CSS classes.

- [ ] **Step 2: Update remaining stale assertions and comments**

Apply these rules to every remaining match:

```text
dashboard route/module/test wording -> console route/module/test wording
DashboardLayout -> ConsoleLayout
dashboard-frame -> console-frame
dashboard-sidebar -> console-sidebar
dashboard-main -> console-main
mobile-dashboard-nav -> mobile-console-nav
/accounts/{login} generated links -> /console/accounts/{login}
```

Do not keep tests whose purpose is old `/accounts/...` behavior.

- [ ] **Step 3: Regenerate CSS**

Run: `npm run build:css`

Workdir: `crates/web`

Expected: `assets/styles.built.css` updates and contains `.console-frame` but not `.dashboard-frame`.

- [ ] **Step 4: Format Rust**

Run: `cargo fmt --all`

Expected: command exits 0.

- [ ] **Step 5: Run full web test suite**

Run: `cargo test -p web`

Expected: all web crate tests pass.

- [ ] **Step 6: Run final stale-string checks**

Run: `rg "DashboardLayout|dashboard-frame|dashboard-sidebar|dashboard-main|mobile-dashboard-nav|href=\"/accounts|action=\"/accounts" crates/web/src crates/web/tests crates/web/assets/styles.css`

Expected: no matches.

Run: `rg "/accounts/" crates/web/src crates/web/tests`

Expected: no generated link/action/test-route matches. If comments mention removed old routes, rewrite the comments to console wording.

- [ ] **Step 7: Commit**

```bash
git add crates/web/src crates/web/tests crates/web/assets/styles.css crates/web/assets/styles.built.css
git commit -m "chore(web): finish console route rename"
```

---

## Final Verification Checklist

- [ ] `cargo fmt --all` exits 0.
- [ ] `cargo test -p web` exits 0.
- [ ] `npm run build:css` exits 0 from `crates/web`.
- [ ] `rg "DashboardLayout|dashboard-frame|dashboard-sidebar|dashboard-main|mobile-dashboard-nav|href=\"/accounts|action=\"/accounts" crates/web/src crates/web/tests crates/web/assets/styles.css` returns no matches.
- [ ] Manual diff review confirms no `/accounts/...` compatibility routes were added.
- [ ] Manual diff review confirms recipient pages do not render normal header controls.
- [ ] Manual diff review confirms `/console...` unauthenticated paths redirect to login before account authorization checks.

## Spec Coverage Review

- Public home rewrite: Task 6.
- `/console` authenticated entry and account picker: Task 5.
- Account routes under `/console/accounts/{login}`: Task 3.
- Auth redirect before concealment and generic real 404: Task 4.
- Recipient logo-only layout and status exits: Task 7.
- Repo-wide dashboard-to-console rename: Tasks 2 and 8.
- Logout and return target behavior: Task 1.
- Setup redirect: Task 3.
- Tests only for new console behavior: Tasks 3 through 8.
