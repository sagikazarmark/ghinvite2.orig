# App Console Redesign Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Redesign the existing ghinvite frontend so every surface feels like a cohesive Stripe-style product console rather than a website.

**Architecture:** Keep the current Rust, Axum, Dioxus SSR, Tailwind CSS v4, and DaisyUI v5 architecture. Customize DaisyUI's theme with OKLCH tokens, then use DaisyUI primitives and Tailwind utilities to refine the layouts. Add only small prop-level view changes for active navigation state and SSR-friendly destructive confirmation modals.

**Tech Stack:** Rust, Axum, Dioxus 0.7 SSR, Tailwind CSS v4, DaisyUI v5, existing `web` crate route/view tests, existing npm CSS build.

---

## Spec Reference

Design spec: `docs/superpowers/specs/2026-05-21-app-console-redesign-design.md`

This plan intentionally avoids a new frontend framework, client-side hydration, clipboard JavaScript, design-system extraction, search, filtering, pagination, or audit-log implementation.

## File Structure

- Modify: `crates/web/assets/styles.css` - DaisyUI theme customization and base typography/background only.
- Modify: `crates/web/tests/route_smoke.rs` - static CSS and home route smoke assertions.
- Modify: `crates/web/src/views/components.rs` - global navbar polish using DaisyUI/Tailwind.
- Modify: `crates/web/src/views/layouts.rs` - app theme, dashboard active navigation, admin shell, invitation shell.
- Modify: `crates/web/src/views/home.rs` - compact app entry screen instead of marketing hero.
- Modify: `crates/web/src/views/dashboard.rs` - console overview panels and recent-link treatment.
- Modify: `crates/web/src/views/links.rs` - sectioned new-link form and stop-link confirmation modal.
- Modify: `crates/web/src/views/requests.rs` - denser admin decision list.
- Modify: `crates/web/src/views/settings.rs` - account-status panels with shared metadata treatment.
- Modify: `crates/web/src/views/invitation.rs` - recipient task panel polish with shared app theme.

## Task 1: DaisyUI Theme Tokens

**Files:**
- Modify: `crates/web/assets/styles.css`
- Modify: `crates/web/tests/route_smoke.rs:112-132`

- [ ] **Step 1: Extend the static CSS smoke test first**

In `crates/web/tests/route_smoke.rs`, replace `static_styles_returns_css` with:

```rust
#[tokio::test]
async fn static_styles_returns_css() {
    let app = build_test_app().await;
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/static/styles.css")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let ct = resp
        .headers()
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap();
    assert!(ct.contains("text/css"));
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8_lossy(&body);
    assert!(text.contains("ghinvite"));
    assert!(text.contains("--color-primary"));
    assert!(text.contains("oklch("));
}
```

- [ ] **Step 2: Run the focused smoke test and verify it fails**

Run: `cargo test -p web static_styles_returns_css --test route_smoke`

Expected: FAIL because the current generated CSS does not contain the custom `ghinvite` DaisyUI theme.

- [ ] **Step 3: Replace the Tailwind and DaisyUI source CSS**

Replace `crates/web/assets/styles.css` with:

```css
@import "tailwindcss";

@plugin "daisyui" {
  themes: ghinvite --default;
}

@plugin "daisyui/theme" {
  name: "ghinvite";
  default: true;
  prefersdark: false;
  color-scheme: light;

  --color-base-100: oklch(98.4% 0.006 255);
  --color-base-200: oklch(96.1% 0.008 255);
  --color-base-300: oklch(90.2% 0.012 255);
  --color-base-content: oklch(24% 0.025 255);

  --color-primary: oklch(52% 0.155 255);
  --color-primary-content: oklch(98% 0.004 255);
  --color-secondary: oklch(58% 0.08 260);
  --color-secondary-content: oklch(98% 0.004 255);
  --color-accent: oklch(63% 0.12 205);
  --color-accent-content: oklch(98% 0.004 255);
  --color-neutral: oklch(31% 0.025 255);
  --color-neutral-content: oklch(98% 0.004 255);

  --color-info: oklch(58% 0.13 245);
  --color-info-content: oklch(98% 0.004 255);
  --color-success: oklch(57% 0.12 155);
  --color-success-content: oklch(98% 0.004 155);
  --color-warning: oklch(74% 0.14 80);
  --color-warning-content: oklch(24% 0.03 80);
  --color-error: oklch(58% 0.18 25);
  --color-error-content: oklch(98% 0.004 25);

  --radius-selector: 0.5rem;
  --radius-field: 0.5rem;
  --radius-box: 0.75rem;
  --size-selector: 0.25rem;
  --size-field: 0.25rem;
  --border: 1px;
  --depth: 1;
  --noise: 0;
}

@layer base {
  html {
    font-family: ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
    text-rendering: optimizeLegibility;
  }

  body {
    min-height: 100vh;
    background: var(--color-base-200);
  }
}
```

- [ ] **Step 4: Rebuild CSS**

Run: `npm run build:css`

Working directory: `crates/web`

Expected: PASS and `crates/web/assets/styles.built.css` is regenerated locally.

- [ ] **Step 5: Run the focused smoke test and verify it passes**

Run: `cargo test -p web static_styles_returns_css --test route_smoke`

Expected: PASS.

- [ ] **Step 6: Check generated CSS tracking**

Run: `git ls-files crates/web/assets/styles.built.css`

Expected: no output. Do not commit `crates/web/assets/styles.built.css` unless this command unexpectedly shows the file is tracked.

- [ ] **Step 7: Commit**

```bash
git add crates/web/assets/styles.css crates/web/tests/route_smoke.rs
git commit -m "feat(web): add app console theme"
```

## Task 2: App Shell And Active Navigation

**Files:**
- Modify: `crates/web/src/views/components.rs`
- Modify: `crates/web/src/views/layouts.rs`
- Modify: `crates/web/src/views/home.rs`
- Modify: `crates/web/src/views/dashboard.rs`
- Modify: `crates/web/src/views/links.rs`
- Modify: `crates/web/src/views/requests.rs`
- Modify: `crates/web/src/views/settings.rs`
- Modify: `crates/web/src/views/invitation.rs`

- [ ] **Step 1: Add failing layout render tests**

Append this test module to `crates/web/src/views/layouts.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dashboard_layout_marks_active_navigation() {
        let html = crate::views::render::render(|| {
            rsx! {
                DashboardLayout {
                    signed_in_login: Some("admin".to_string()),
                    title: "Requests".to_string(),
                    account_login: Some("acme".to_string()),
                    active_nav: Some("requests".to_string()),
                    flash: None,
                    children: rsx! { p { "Queue" } },
                }
            }
        });

        assert!(html.contains("data-theme=\"ghinvite\""));
        assert!(html.contains("aria-current=\"page\""));
        assert!(html.contains("Pending requests"));
        assert!(html.contains("menu-active"));
    }

    #[test]
    fn invitation_layout_uses_product_theme() {
        let html = crate::views::render::render(|| {
            rsx! {
                InvitationLayout {
                    signed_in_login: None,
                    title: "Invite".to_string(),
                    account_login: None,
                    active_nav: None,
                    flash: None,
                    children: rsx! { p { "Invitation" } },
                }
            }
        });

        assert!(html.contains("data-theme=\"ghinvite\""));
        assert!(html.contains("Invitation"));
        assert!(html.contains("card"));
    }
}
```

- [ ] **Step 2: Run the focused layout tests and verify they fail**

Run: `cargo test -p web layout_ --lib`

Expected: FAIL because `LayoutProps` has no `active_nav` field and layouts still use `data-theme="light"`.

- [ ] **Step 3: Add active navigation to layout props**

In `crates/web/src/views/layouts.rs`, change `LayoutProps` to:

```rust
#[derive(Clone, PartialEq, Props)]
pub struct LayoutProps {
    pub signed_in_login: Option<String>,
    pub title: String,
    /// `Some(login)` when rendered under account dashboard routes. `None` for
    /// HomeLayout / InvitationLayout.
    pub account_login: Option<String>,
    /// The current dashboard section for active navigation styling.
    pub active_nav: Option<String>,
    /// One-shot status message rendered above `children`.
    pub flash: Option<crate::session::Flash>,
    /// The page content rendered inside the layout.
    pub children: Element,
}
```

- [ ] **Step 4: Polish the global navbar with DaisyUI and Tailwind**

Replace `Nav` in `crates/web/src/views/components.rs` with:

```rust
#[component]
pub fn Nav(props: NavProps) -> Element {
    rsx! {
        nav {
            class: "navbar min-h-14 border-b border-base-300 bg-base-100 px-4 text-base-content",
            div { class: "flex-1",
                a {
                    class: "btn btn-ghost px-2 text-base font-semibold tracking-tight",
                    href: "/",
                    "ghinvite"
                }
            }
            div { class: "flex-none gap-2",
                {match props.signed_in_login.as_deref() {
                    Some(login) => rsx! {
                        span { class: "hidden px-2 text-xs text-base-content/65 sm:inline-flex", "@{login}" }
                        a { class: "btn btn-primary btn-sm", href: "/install", "Install on another account" }
                        a { class: "btn btn-ghost btn-sm", href: "/logout", "Sign out" }
                    },
                    None => rsx! {
                        a { class: "btn btn-primary btn-sm", href: "/login", "Sign in with GitHub" }
                    }
                }}
            }
        }
    }
}
```

- [ ] **Step 5: Replace HomeLayout and DashboardLayout**

In `crates/web/src/views/layouts.rs`, replace `HomeLayout` and `DashboardLayout` with:

```rust
#[component]
pub fn HomeLayout(props: LayoutProps) -> Element {
    rsx! {
        head {
            title { "{props.title}" }
            link { rel: "stylesheet", href: "/static/styles.css" }
        }
        body {
            class: "min-h-screen bg-base-200 text-base-content antialiased",
            "data-theme": "ghinvite",
            Nav { signed_in_login: props.signed_in_login.clone() }
            main { class: "min-h-[calc(100vh-3.5rem)] px-4 py-8", {props.children} }
            Footer {}
        }
    }
}

#[component]
pub fn DashboardLayout(props: LayoutProps) -> Element {
    let login = props.account_login.clone().unwrap_or_default();
    let active_nav = props.active_nav.clone().unwrap_or_default();
    let overview_side = if active_nav == "overview" { "menu-active" } else { "" };
    let new_link_side = if active_nav == "new-link" { "menu-active" } else { "" };
    let requests_side = if active_nav == "requests" { "menu-active" } else { "" };
    let settings_side = if active_nav == "settings" { "menu-active" } else { "" };
    let overview_mobile = if active_nav == "overview" { "btn btn-primary btn-sm" } else { "btn btn-ghost btn-sm" };
    let new_link_mobile = if active_nav == "new-link" { "btn btn-primary btn-sm" } else { "btn btn-ghost btn-sm" };
    let requests_mobile = if active_nav == "requests" { "btn btn-primary btn-sm" } else { "btn btn-ghost btn-sm" };
    let settings_mobile = if active_nav == "settings" { "btn btn-primary btn-sm" } else { "btn btn-ghost btn-sm" };

    rsx! {
        head {
            title { "{props.title}" }
            link { rel: "stylesheet", href: "/static/styles.css" }
        }
        body {
            class: "min-h-screen bg-base-200 text-base-content antialiased",
            "data-theme": "ghinvite",
            Nav { signed_in_login: props.signed_in_login.clone() }
            nav { class: "border-b border-base-300 bg-base-100 md:hidden",
                div { class: "flex gap-2 overflow-x-auto px-4 py-2 whitespace-nowrap",
                    a { class: "{overview_mobile}", href: "/accounts/{login}", aria_current: if active_nav == "overview" { "page" } else { "false" }, "Overview" }
                    a { class: "{new_link_mobile}", href: "/accounts/{login}/links/new", aria_current: if active_nav == "new-link" { "page" } else { "false" }, "New link" }
                    a { class: "{requests_mobile}", href: "/accounts/{login}/requests", aria_current: if active_nav == "requests" { "page" } else { "false" }, "Requests" }
                    a { class: "btn btn-ghost btn-sm", href: "/accounts/{login}/audit", "Audit soon" }
                    a { class: "{settings_mobile}", href: "/accounts/{login}/settings", aria_current: if active_nav == "settings" { "page" } else { "false" }, "Settings" }
                }
            }
            div { class: "mx-auto flex w-full max-w-7xl flex-1 gap-6 px-4 py-6 lg:px-6",
                aside { class: "hidden w-56 shrink-0 md:block",
                    div { class: "sticky top-6 space-y-4",
                        div { class: "rounded-box border border-base-300 bg-base-100 p-3 shadow-sm",
                            p { class: "text-xs font-medium uppercase tracking-wide text-base-content/50", "Account" }
                            p { class: "mt-1 truncate text-sm font-semibold", "{login}" }
                        }
                        nav { class: "menu rounded-box border border-base-300 bg-base-100 p-2 shadow-sm",
                            li { a { class: "{overview_side}", href: "/accounts/{login}", aria_current: if active_nav == "overview" { "page" } else { "false" }, "Overview" } }
                            li { a { class: "{new_link_side}", href: "/accounts/{login}/links/new", aria_current: if active_nav == "new-link" { "page" } else { "false" }, "New link" } }
                            li { a { class: "{requests_side}", href: "/accounts/{login}/requests", aria_current: if active_nav == "requests" { "page" } else { "false" }, "Pending requests" } }
                            li { a { href: "/accounts/{login}/audit", "Audit log (coming soon)" } }
                            li { a { class: "{settings_side}", href: "/accounts/{login}/settings", aria_current: if active_nav == "settings" { "page" } else { "false" }, "Settings" } }
                        }
                    }
                }
                main { class: "min-w-0 flex-1",
                    {match &props.flash {
                        Some(f) => {
                            let alert_class = match f.level {
                                crate::session::FlashLevel::Success => "alert alert-success mb-4 shadow-sm",
                                crate::session::FlashLevel::Error => "alert alert-error mb-4 shadow-sm",
                                crate::session::FlashLevel::Info => "alert alert-info mb-4 shadow-sm",
                            };
                            rsx! { div { class: "{alert_class}", span { "{f.message}" } } }
                        }
                        None => rsx! {},
                    }}
                    {props.children}
                }
            }
        }
    }
}
```

- [ ] **Step 6: Replace InvitationLayout**

In `crates/web/src/views/layouts.rs`, replace `InvitationLayout` with:

```rust
#[component]
pub fn InvitationLayout(props: LayoutProps) -> Element {
    rsx! {
        head {
            title { "{props.title}" }
            link { rel: "stylesheet", href: "/static/styles.css" }
        }
        body {
            class: "min-h-screen bg-base-200 px-4 py-8 text-base-content antialiased",
            "data-theme": "ghinvite",
            main { class: "mx-auto flex min-h-[calc(100vh-4rem)] w-full max-w-lg items-center",
                div { class: "card w-full border border-base-300 bg-base-100 shadow-sm",
                    div { class: "card-body", {props.children} }
                }
            }
        }
    }
}
```

- [ ] **Step 7: Add active_nav to every layout invocation**

Add the new `active_nav` prop immediately after `account_login` in each layout invocation. Keep each invocation's existing `title`, `flash`, and `children` values unchanged in this task.

Use these exact insertions:

```rust
// crates/web/src/views/home.rs, HomeLayout
active_nav: None,

// crates/web/src/views/dashboard.rs, OverviewPage DashboardLayout
active_nav: Some("overview".to_string()),

// crates/web/src/views/links.rs, LinkCreateFormPage DashboardLayout
active_nav: Some("new-link".to_string()),

// crates/web/src/views/links.rs, LinkDetailPage DashboardLayout
active_nav: Some("overview".to_string()),

// crates/web/src/views/requests.rs, RequestsQueuePage DashboardLayout
active_nav: Some("requests".to_string()),

// crates/web/src/views/settings.rs, SettingsPage DashboardLayout
active_nav: Some("settings".to_string()),

// crates/web/src/views/invitation.rs, all three InvitationLayout invocations
active_nav: None,
```

- [ ] **Step 8: Run formatting and focused tests**

Run: `cargo fmt`

Run: `cargo test -p web layout_ --lib`

Expected: PASS.

- [ ] **Step 9: Commit**

```bash
git add crates/web/src/views/components.rs crates/web/src/views/layouts.rs crates/web/src/views/home.rs crates/web/src/views/dashboard.rs crates/web/src/views/links.rs crates/web/src/views/requests.rs crates/web/src/views/settings.rs crates/web/src/views/invitation.rs
git commit -m "feat(web): introduce app console shell"
```

## Task 3: Home And Dashboard Console Surfaces

**Files:**
- Modify: `crates/web/src/views/home.rs`
- Modify: `crates/web/src/views/dashboard.rs`
- Modify: `crates/web/tests/route_smoke.rs:134-155`

- [ ] **Step 1: Add failing home route assertions**

In `crates/web/tests/route_smoke.rs`, update `home_returns_html` after the existing assertions:

```rust
    assert!(text.contains("Access console for GitHub collaborators"));
    assert!(!text.contains("class=\"hero"));
```

- [ ] **Step 2: Add a failing dashboard render test**

Append this test module to `crates/web/src/views/dashboard.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overview_page_renders_console_copy() {
        let html = crate::views::render::render(|| {
            rsx! {
                OverviewPage {
                    signed_in_login: Some("admin".to_string()),
                    flash: None,
                    account_login: "acme".to_string(),
                    account_type: "Organization".to_string(),
                    pending_requests: 2,
                    active_links: 3,
                    recent_links: vec![],
                    now: Utc::now(),
                }
            }
        });

        assert!(html.contains("Console overview"));
        assert!(html.contains("Review queue"));
        assert!(html.contains("Create first share link"));
        assert!(html.contains("Recent share links"));
    }
}
```

- [ ] **Step 3: Run focused tests and verify they fail**

Run: `cargo test -p web home_returns_html --test route_smoke`

Run: `cargo test -p web overview_page_renders_console_copy --lib`

Expected: FAIL because the current home page still uses the hero structure and the dashboard copy has not been updated.

- [ ] **Step 4: Replace the home page body**

In `crates/web/src/views/home.rs`, replace the `children` value inside `HomeLayout` with:

```rust
children: rsx! {
    section { class: "mx-auto grid max-w-5xl gap-8 py-10 lg:grid-cols-[minmax(0,1fr)_22rem] lg:items-start",
        div { class: "max-w-2xl",
            p { class: "mb-3 text-sm font-medium text-primary", "GitHub access operations" }
            h1 { class: "text-3xl font-semibold tracking-tight text-base-content sm:text-4xl", "Access console for GitHub collaborators" }
            p { class: "mt-4 max-w-xl text-base leading-7 text-base-content/70",
                "Create share links, review access requests, and keep repository invitations moving without ad-hoc admin work."
            }
            div { class: "mt-6 flex flex-wrap gap-3",
                {match props.signed_in_login.as_deref() {
                    Some(_) => rsx! {
                        a { class: "btn btn-primary", href: "/install", "Install on another account" }
                    },
                    None => rsx! {
                        a { class: "btn btn-primary", href: "/login", "Sign in with GitHub" }
                    }
                }}
            }
        }
        aside { class: "card border border-base-300 bg-base-100 shadow-sm",
            div { class: "card-body gap-4",
                h2 { class: "card-title text-base", "What admins control" }
                ul { class: "space-y-3 text-sm text-base-content/75",
                    li { "Repository permissions, expiration, and usage limits" }
                    li { "Approval queues before invitations are sent" }
                    li { "Captured history for access workflows" }
                }
            }
        }
    }
}
```

- [ ] **Step 5: Replace the dashboard overview body**

In `crates/web/src/views/dashboard.rs`, replace the `children` body inside `OverviewPage` with:

```rust
children: rsx! {
    header { class: "mb-6 flex flex-col gap-3 sm:flex-row sm:items-end sm:justify-between",
        div {
            p { class: "text-sm font-medium text-primary", "{props.account_type}" }
            h1 { class: "text-2xl font-semibold tracking-tight", "Console overview" }
            p { class: "mt-1 text-sm text-base-content/65", "Manage collaborator access for {props.account_login}." }
        }
        a { class: "btn btn-primary btn-sm", href: "/accounts/{login}/links/new", "New share link" }
    }
    div { class: "grid grid-cols-1 gap-4 md:grid-cols-2",
        a { class: "card border border-base-300 bg-base-100 shadow-sm transition-colors hover:border-primary/40",
            href: "/accounts/{login}/requests",
            div { class: "card-body gap-2",
                p { class: "text-xs font-medium uppercase tracking-wide text-base-content/50", "Review queue" }
                div { class: "flex items-baseline gap-3",
                    p { class: "text-3xl font-semibold", "{props.pending_requests}" }
                    p { class: "text-sm text-base-content/65", "pending requests" }
                }
                p { class: "text-sm text-base-content/70", "Review people waiting for repository access before invitations are sent." }
            }
        }
        a { class: "card border border-base-300 bg-base-100 shadow-sm transition-colors hover:border-primary/40",
            href: "/accounts/{login}/links/new",
            div { class: "card-body gap-2",
                p { class: "text-xs font-medium uppercase tracking-wide text-base-content/50", "Active access links" }
                div { class: "flex items-baseline gap-3",
                    p { class: "text-3xl font-semibold", "{props.active_links}" }
                    p { class: "text-sm text-base-content/65", "active links" }
                }
                p { class: "text-sm text-base-content/70", "Create controlled URLs for GitHub collaborator requests." }
            }
        }
    }
    section { class: "mt-6 rounded-box border border-base-300 bg-base-100 shadow-sm",
        div { class: "flex items-center justify-between border-b border-base-300 px-5 py-4",
            h2 { class: "text-base font-semibold", "Recent share links" }
            a { class: "btn btn-ghost btn-sm", href: "/accounts/{login}/links/new", "Create link" }
        }
        {if props.recent_links.is_empty() {
            rsx! {
                div { class: "p-6 text-sm text-base-content/70",
                    h3 { class: "font-medium text-base-content", "No share links yet" }
                    p { class: "mt-1", "Create a link to let recipients request collaborator access without manual GitHub invites." }
                    a { class: "btn btn-primary btn-sm mt-4", href: "/accounts/{login}/links/new", "Create first share link" }
                }
            }
        } else {
            rsx! {
                div { class: "overflow-x-auto",
                    table { class: "table table-sm",
                        thead { tr { th { "Slug" } th { "State" } th { "Uses" } } }
                        tbody { {recent_links_view} }
                    }
                }
            }
        }}
    }
},
```

- [ ] **Step 6: Run formatting and focused tests**

Run: `cargo fmt`

Run: `cargo test -p web home_returns_html --test route_smoke`

Run: `cargo test -p web overview_page_renders_console_copy --lib`

Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/web/src/views/home.rs crates/web/src/views/dashboard.rs crates/web/tests/route_smoke.rs
git commit -m "feat(web): reshape home and overview surfaces"
```

## Task 4: Link Creation And Detail Console Treatment

**Files:**
- Modify: `crates/web/src/views/links.rs`

- [ ] **Step 1: Add failing render assertions for the new-link form and stop modal**

Inside the existing `#[cfg(test)] mod tests` in `crates/web/src/views/links.rs`, add this test:

```rust
#[test]
fn link_create_form_renders_sectioned_console_form() {
    let html = crate::views::render::render(|| {
        rsx! {
            LinkCreateFormPage {
                signed_in_login: Some("admin".to_string()),
                flash: None,
                account_login: "acme".to_string(),
                repos: vec![github::payloads::GhRepo {
                    id: 10,
                    full_name: "acme/api".to_string(),
                    private: true,
                }],
                form: LinkFormValues::default(),
            }
        }
    });

    assert!(html.contains("Access configuration"));
    assert!(html.contains("Request handling"));
    assert!(html.contains("Repository scope"));
    assert!(html.contains("acme/api"));
}
```

Then extend `link_detail_renders_operational_context` with:

```rust
        assert!(html.contains("id=\"stop-link-modal\""));
        assert!(html.contains("role=\"dialog\""));
        assert!(html.contains("Confirm stop"));
```

- [ ] **Step 2: Run focused link tests and verify they fail**

Run: `cargo test -p web link_create_form_renders_sectioned_console_form --lib`

Run: `cargo test -p web link_detail_renders_operational_context --lib`

Expected: FAIL because the form sections and modal markup are not rendered yet.

- [ ] **Step 3: Replace the new-link form with sectioned DaisyUI panels**

In `LinkCreateFormPage`, keep `let login` and `let perms`, then replace the `children` body with:

```rust
children: rsx! {
    header { class: "mb-6 flex flex-col gap-2",
        p { class: "text-sm font-medium text-primary", "Share links" }
        h1 { class: "text-2xl font-semibold tracking-tight", "New share link" }
        p { class: "max-w-2xl text-sm leading-6 text-base-content/70",
            "Create a controlled URL that lets GitHub users request collaborator access to selected repositories."
        }
    }
    form { method: "post", action: "/accounts/{login}/links", class: "max-w-3xl space-y-5",
        section { class: "card border border-base-300 bg-base-100 shadow-sm",
            div { class: "card-body gap-5",
                div {
                    h2 { class: "text-base font-semibold", "Access configuration" }
                    p { class: "mt-1 text-sm text-base-content/65", "Choose the GitHub permission level and the repositories this link can request." }
                }
                div { class: "form-control gap-2",
                    label { class: "label", r#for: "permission", span { class: "label-text font-medium", "Permission level" } }
                    select { id: "permission", name: "permission", class: "select select-bordered w-full",
                        {perms.iter().map(|p| {
                            let selected = props.form.permission == *p;
                            rsx! { option { value: "{p}", selected: selected, "{p}" } }
                        })}
                    }
                    p { class: "text-sm text-base-content/65", "Use pull for read-only access. Maintain and admin can change repository settings." }
                }
                div { class: "alert alert-warning shadow-sm",
                    span { "Review elevated permissions before sharing. Approved requests send GitHub collaborator invitations." }
                }
            }
        }
        section { class: "card border border-base-300 bg-base-100 shadow-sm",
            div { class: "card-body gap-5",
                div {
                    h2 { class: "text-base font-semibold", "Request handling" }
                    p { class: "mt-1 text-sm text-base-content/65", "Set approval, usage, and expiration guardrails." }
                }
                div { class: "form-control",
                    label { class: "label cursor-pointer justify-start gap-3",
                        input { r#type: "checkbox", name: "approval_required", value: "true", checked: props.form.approval_required, class: "checkbox" }
                        span { class: "label-text", "Require admin approval before invitations are sent" }
                    }
                    p { class: "text-sm text-base-content/65", "Leave unchecked to auto-approve requests that use this link." }
                }
                div { class: "grid grid-cols-1 gap-4 md:grid-cols-2",
                    div { class: "form-control gap-2",
                        label { class: "label", r#for: "max_uses", span { class: "label-text font-medium", "Max uses" } }
                        input { id: "max_uses", r#type: "number", name: "max_uses", value: "{props.form.max_uses}", class: "input input-bordered w-full", min: "1", placeholder: "Unlimited" }
                        p { class: "text-sm text-base-content/65", "Blank means unlimited requests." }
                    }
                    div { class: "form-control gap-2",
                        label { class: "label", r#for: "expires_in_days", span { class: "label-text font-medium", "Expires in days" } }
                        input { id: "expires_in_days", r#type: "number", name: "expires_in_days", value: "{props.form.expires_in_days}", class: "input input-bordered w-full", min: "1" }
                        p { class: "text-sm text-base-content/65", "Default is 30 days. Blank creates a link with no expiration." }
                    }
                }
                div { class: "form-control gap-2",
                    label { class: "label", r#for: "internal_note", span { class: "label-text font-medium", "Internal note" } }
                    textarea { id: "internal_note", name: "internal_note", class: "textarea textarea-bordered w-full", placeholder: "Why this link exists", "{props.form.internal_note}" }
                    p { class: "text-sm text-base-content/65", "Visible only to admins." }
                }
            }
        }
        section { class: "card border border-base-300 bg-base-100 shadow-sm",
            div { class: "card-body gap-4",
                div {
                    h2 { class: "text-base font-semibold", "Repository scope" }
                    p { class: "mt-1 text-sm text-base-content/65", "Select every repository this link may grant access to." }
                }
                {if props.repos.is_empty() {
                    rsx! { div { class: "alert shadow-sm", span { "No repositories are available for this installation." } } }
                } else {
                    rsx! {
                        div { class: "max-h-80 space-y-1 overflow-y-auto rounded-box border border-base-300 bg-base-200 p-3",
                            {props.repos.iter().map(|repo| {
                                let id = repo.id;
                                let checked = props.form.selected_repo_ids.contains(&id);
                                let full_name = repo.full_name.clone();
                                rsx! {
                                    label { class: "label cursor-pointer justify-start gap-3 rounded-field px-2 hover:bg-base-100",
                                        input { r#type: "checkbox", name: "repo_ids", value: "{id}", checked: checked, class: "checkbox checkbox-sm" }
                                        span { class: "text-sm", "{full_name}" }
                                    }
                                }
                            })}
                        }
                    }
                }}
            }
        }
        div { class: "flex justify-end",
            button { r#type: "submit", class: "btn btn-primary", "Create link" }
        }
    }
},
```

- [ ] **Step 4: Add an SSR-friendly DaisyUI modal to the stop-link action**

In `LinkDetailPage`, replace the active destructive-action section with:

```rust
section { class: "card border border-error/30 bg-base-100 shadow-sm",
    div { class: "card-body gap-4",
        div {
            h2 { class: "text-base font-semibold text-error", "Stop accepting new requests" }
            p { class: "mt-1 text-sm text-base-content/70",
                "This prevents new requests through this link. It does not cancel requests or GitHub invitations already in progress."
            }
        }
        a { class: "btn btn-error w-fit", href: "#stop-link-modal", "Stop accepting new requests" }
    }
}
div { class: "modal", role: "dialog", id: "stop-link-modal",
    div { class: "modal-box",
        h3 { class: "text-lg font-semibold", "Confirm stop" }
        p { class: "mt-2 text-sm text-base-content/70",
            "Recipients will no longer be able to create new requests from this share link. Existing requests and invitations continue."
        }
        div { class: "modal-action",
            a { class: "btn btn-ghost", href: "#", "Cancel" }
            form { method: "post", action: "/accounts/{login}/links/{id_str}/revoke",
                button { r#type: "submit", class: "btn btn-error", "Confirm stop" }
            }
        }
    }
    a { class: "modal-backdrop", href: "#", "Close" }
}
```

- [ ] **Step 5: Run formatting and focused tests**

Run: `cargo fmt`

Run: `cargo test -p web link_create_form_renders_sectioned_console_form --lib`

Run: `cargo test -p web link_detail_renders_operational_context --lib`

Run: `cargo test -p web link_form_defaults_are_safe --lib`

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/web/src/views/links.rs
git commit -m "feat(web): polish share link workflows"
```

## Task 5: Requests Queue Decision List

**Files:**
- Modify: `crates/web/src/views/requests.rs`

- [ ] **Step 1: Extend the queue render test first**

In `crates/web/src/views/requests.rs`, extend `requests_queue_renders_link_context_before_actions` with:

```rust
        assert!(html.contains("Decision queue"));
        assert!(html.contains("Approve request"));
        assert!(html.contains("Decline request"));
```

- [ ] **Step 2: Run the focused queue test and verify it fails**

Run: `cargo test -p web requests_queue_renders_link_context_before_actions --lib`

Expected: FAIL because the queue header and button copy are not updated yet.

- [ ] **Step 3: Replace the queue page body with a denser decision list**

In `RequestsQueuePage`, replace the `children` body with:

```rust
children: rsx! {
    header { class: "mb-6",
        p { class: "text-sm font-medium text-primary", "Requests" }
        h1 { class: "text-2xl font-semibold tracking-tight", "Decision queue" }
        p { class: "mt-1 text-sm text-base-content/65", "Review access requests before GitHub invitations are sent." }
    }
    {if props.rows.is_empty() {
        rsx! {
            div { class: "rounded-box border border-base-300 bg-base-100 p-6 text-base-content/70 shadow-sm",
                h2 { class: "font-medium text-base-content", "No pending requests" }
                p { class: "mt-1 text-sm", "Requests that need an admin decision will appear here." }
            }
        }
    } else {
        rsx! {
            div { class: "overflow-hidden rounded-box border border-base-300 bg-base-100 shadow-sm",
                {props.rows.iter().map(|r| {
                    let rid = r.request_id.clone();
                    let just = r.justification.clone();
                    let link_id = r.link_id.clone();
                    let link_slug = r.link_slug.clone();
                    let link_label = if link_id.is_empty() {
                        rsx! { span { "{link_slug}" } }
                    } else {
                        rsx! { a { class: "link", href: "/accounts/{login}/links/{link_id}", "{link_slug}" } }
                    };
                    let requester = r.requester_login.clone();
                    let created = r.created_at;
                    let permission = r.permission.clone().unwrap_or_else(|| "unknown permission".into());
                    let repos_available = !r.repos.is_empty();
                    let actions_available = !link_id.is_empty() && repos_available;
                    let expires = r.expires_at.map(|when| when.format("%Y-%m-%d").to_string()).unwrap_or_else(|| "No expiration".into());
                    let approval = match r.approval_required {
                        Some(true) => "Admin approval required",
                        Some(false) => "Auto-approved link",
                        None => "Approval mode unavailable",
                    };
                    let repos = if r.repos.is_empty() {
                        rsx! { p { class: "text-sm text-base-content/65", "Repository details unavailable" } }
                    } else {
                        rsx! {
                            ul { class: "mt-1 flex flex-wrap gap-2 text-sm",
                                {r.repos.iter().map(|repo| {
                                    let repo = repo.clone();
                                    rsx! { li { class: "badge badge-ghost", "{repo}" } }
                                })}
                            }
                        }
                    };
                    rsx! {
                        div { class: "grid gap-4 border-b border-base-300 p-4 last:border-b-0 md:grid-cols-[minmax(0,1fr)_auto] md:items-start",
                            div { class: "min-w-0 space-y-3",
                                div {
                                    p { class: "font-medium",
                                        strong { "@{requester}" }
                                        " requested access via "
                                        {link_label}
                                    }
                                    p { class: "text-xs text-base-content/55", "Requested at {created}" }
                                }
                                div { class: "flex flex-wrap gap-2",
                                    span { class: "badge badge-neutral", "Permission: {permission}" }
                                    span { class: "badge badge-ghost", "Expires: {expires}" }
                                    span { class: "badge badge-ghost", "{approval}" }
                                }
                                div {
                                    p { class: "text-xs font-medium uppercase tracking-wide text-base-content/50", "Repositories" }
                                    {repos}
                                }
                                {match just {
                                    Some(j) if !j.is_empty() => rsx! { blockquote { class: "text-sm italic text-base-content/80", "\"{j}\"" } },
                                    _ => rsx! {},
                                }}
                                {if repos_available {
                                    rsx! { p { class: "text-sm text-base-content/70", "Approving sends GitHub collaborator invitations for the repositories listed here." } }
                                } else {
                                    rsx! { p { class: "text-sm text-base-content/70", "This request cannot be completed until its share link details are available." } }
                                }}
                            }
                            {if actions_available {
                                rsx! {
                                    div { class: "flex gap-2 md:flex-col md:items-stretch",
                                        form { method: "post", action: "/accounts/{login}/requests/{rid}/approve",
                                            button { r#type: "submit", class: "btn btn-success btn-sm", "Approve request" }
                                        }
                                        form { method: "post", action: "/accounts/{login}/requests/{rid}/decline",
                                            button { r#type: "submit", class: "btn btn-error btn-sm", "Decline request" }
                                        }
                                    }
                                }
                            } else {
                                rsx! { p { class: "text-sm font-medium text-base-content/60", "Decision unavailable" } }
                            }}
                        }
                    }
                })}
            }
        }
    }}
},
```

- [ ] **Step 4: Run formatting and queue tests**

Run: `cargo fmt`

Run: `cargo test -p web requests_queue_renders_link_context_before_actions --lib`

Run: `cargo test -p web requests_queue_renders_missing_link_context_without_broken_link --lib`

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/web/src/views/requests.rs
git commit -m "feat(web): refine request decision queue"
```

## Task 6: Settings And Recipient Task Panels

**Files:**
- Modify: `crates/web/src/views/settings.rs`
- Modify: `crates/web/src/views/invitation.rs`
- Modify: `crates/web/tests/invitation_resolution.rs`

- [ ] **Step 1: Add recipient copy assertions**

In `crates/web/tests/invitation_resolution.rs`, update `landing_renders_active_link` after the existing repository assertion:

```rust
    assert!(text.contains("Access request"));
    assert!(text.contains("GitHub sign-in confirms your identity"));
```

The second assertion already exists in this repository; keep one copy of it after editing.

- [ ] **Step 2: Add settings and pending render assertions**

Append this test module to `crates/web/src/views/settings.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use domain::{Account, AccountType, SelectedRepos};

    #[test]
    fn settings_page_renders_account_status_panels() {
        let html = crate::views::render::render(|| {
            rsx! {
                SettingsPage {
                    signed_in_login: Some("admin".to_string()),
                    flash: None,
                    account: Account {
                        installation_id: 77,
                        account_id: 9001,
                        account_login: "acme".to_string(),
                        account_type: AccountType::Organization,
                        installed_at: Utc::now(),
                        uninstalled_at: None,
                        selected_repos: SelectedRepos::All,
                    },
                }
            }
        });

        assert!(html.contains("Account status"));
        assert!(html.contains("Installation scope"));
        assert!(html.contains("GitHub App settings"));
    }
}
```

In `crates/web/src/views/invitation.rs`, extend `pending_page_renders_refresh_affordance` with:

```rust
        assert!(html.contains("Request status"));
        assert!(html.contains("card"));
```

- [ ] **Step 3: Run focused tests and verify they fail where copy changed**

Run: `cargo test -p web landing_renders_active_link --test invitation_resolution`

Run: `cargo test -p web settings_page_renders_account_status_panels --lib`

Run: `cargo test -p web pending_page_renders_refresh_affordance --lib`

Expected: FAIL for the new landing/settings copy until the views are updated. The pending test may already pass after Task 2 because `InvitationLayout` renders a card.

- [ ] **Step 4: Replace settings content**

In `crates/web/src/views/settings.rs`, replace the `children` body with:

```rust
children: rsx! {
    header { class: "mb-6",
        p { class: "text-sm font-medium text-primary", "Settings" }
        h1 { class: "text-2xl font-semibold tracking-tight", "Account status" }
        p { class: "mt-1 text-sm text-base-content/65", "Current GitHub App installation state for {login}." }
    }
    div { class: "grid max-w-4xl gap-4 md:grid-cols-2",
        section { class: "card border border-base-300 bg-base-100 shadow-sm",
            div { class: "card-body gap-3",
                p { class: "text-xs font-medium uppercase tracking-wide text-base-content/50", "Account" }
                h2 { class: "text-lg font-semibold", "{login}" }
                p { class: "text-sm text-base-content/70", "{account_type}" }
            }
        }
        section { class: "card border border-base-300 bg-base-100 shadow-sm",
            div { class: "card-body gap-3",
                p { class: "text-xs font-medium uppercase tracking-wide text-base-content/50", "Installation scope" }
                h2 { class: "text-lg font-semibold", "{repos_label}" }
                p { class: "text-sm text-base-content/70", "Change repository selection in the GitHub App settings in this version." }
            }
        }
    }
    div { class: "alert mt-4 max-w-4xl shadow-sm",
        span { "Editable settings are planned for v1.1. This page reflects the active installation state." }
    }
},
```

- [ ] **Step 5: Polish recipient landing and form headers**

In `LandingPage`, replace the first page content block with:

```rust
h1 { class: "text-2xl font-semibold tracking-tight", "Access request" }
p { class: "mt-2 text-sm text-base-content/70",
    "This share link requests collaborator access to the following {repo_word}:"
}
ul { class: "mt-4 list-inside list-disc space-y-1 rounded-box border border-base-300 bg-base-200 p-4 text-sm", {repos_view} }
p { class: "mt-4",
    span { class: "badge badge-neutral", "Permission: {perm_label}" }
}
p { class: "mt-3 text-sm text-base-content/70",
    "GitHub sign-in confirms your identity before any request is sent."
}
{expiry_view}
div { class: "card-actions justify-end mt-6", {cta_view} }
```

In `RequestFormPage`, add a page title before the signed-in alert:

```rust
header { class: "mb-4",
    h1 { class: "text-2xl font-semibold tracking-tight", "Request access" }
    p { class: "mt-1 text-sm text-base-content/70", "Confirm the repositories and include context for the account admins." }
}
```

- [ ] **Step 6: Run formatting and focused tests**

Run: `cargo fmt`

Run: `cargo test -p web landing_renders_active_link --test invitation_resolution`

Run: `cargo test -p web settings_page_renders_account_status_panels --lib`

Run: `cargo test -p web pending_page_renders_refresh_affordance --lib`

Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/web/src/views/settings.rs crates/web/src/views/invitation.rs crates/web/tests/invitation_resolution.rs
git commit -m "feat(web): polish settings and recipient panels"
```

## Task 7: Full Verification

**Files:**
- Verify: `crates/web/assets/styles.css`
- Verify: `crates/web/src/views/*.rs`
- Verify: `crates/web/tests/*.rs`

- [ ] **Step 1: Run Rust formatting**

Run: `cargo fmt`

Expected: command exits 0.

- [ ] **Step 2: Rebuild CSS**

Run: `npm run build:css`

Working directory: `crates/web`

Expected: command exits 0.

- [ ] **Step 3: Run the web test suite**

Run: `cargo test -p web`

Expected: all `web` crate tests pass.

- [ ] **Step 4: Check banned visual/copy patterns in touched views**

Run: `rg "border-(left|right)-[2-9]|bg-clip-text|backdrop-blur|hero|audit trail|Revoke this link|Refresh to check status" crates/web/src/views crates/web/tests/route_smoke.rs crates/web/tests/invitation_resolution.rs`

Expected: no matches.

- [ ] **Step 5: Inspect generated CSS tracking and worktree**

Run: `git ls-files crates/web/assets/styles.built.css`

Expected: no output.

Run: `git status --short`

Expected: only intended source and test files are modified. `crates/web/assets/styles.built.css` may appear as untracked if the local build created it; do not add it unless `git ls-files` showed it is tracked.

- [ ] **Step 6: Final commit if verification changed tracked files**

If `cargo fmt` or final test fixes changed tracked files, commit them:

```bash
git add crates/web/assets/styles.css crates/web/src/views crates/web/tests
git commit -m "chore(web): finalize app console polish"
```

If `git status --short` shows no tracked modifications after verification, skip this commit.

## Self-Review Notes

- Spec coverage: Task 1 covers OKLCH DaisyUI theme customization. Task 2 covers app shell, active navigation, and invitation shell. Task 3 covers home and dashboard surfaces. Task 4 covers share-link form/detail and destructive modal confirmation. Task 5 covers the requests queue. Task 6 covers settings and recipient flows. Task 7 covers verification.
- Scope check: The plan stays inside CSS source, Dioxus views, and existing tests. It does not change storage, commands, workflows, route behavior, hydration, or frontend frameworks.
- Type consistency: `active_nav` is `Option<String>` on `LayoutProps`; all layout call sites pass a concrete `Some("overview")`, `Some("new-link")`, `Some("requests")`, `Some("settings")`, or `None`. Existing view props remain unchanged except layout invocation internals.
- Component constraint: DaisyUI primitives and Tailwind utilities are used throughout. Custom CSS is limited to DaisyUI theme tokens and base document styling.
