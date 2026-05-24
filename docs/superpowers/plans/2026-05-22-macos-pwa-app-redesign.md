# macOS PWA App Redesign Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Redesign the authenticated ghinvite frontend into a compact macOS PWA-style utility with a real sidebar, icon theme switcher, denser tables, and native-feeling app rhythm.

**Architecture:** Keep the app server-rendered with Dioxus SSR. Use DaisyUI as the primitive layer and Tailwind plus small custom CSS classes for the app shell, native panel surfaces, table density, and theme toggle state. Avoid data model or backend workflow changes; the sidebar account control shows the active account and installation action until route data exposes multiple accounts.

**Tech Stack:** Rust, Dioxus SSR, Axum, Tailwind CSS v4, DaisyUI v5, localStorage-backed theme sync.

---

## File Structure

- Modify `crates/web/assets/styles.css`: retune DaisyUI OKLCH theme values and add small app-level classes for header, theme toggle, sidebar, panels, property rows, and compact tables.
- Modify generated `crates/web/assets/styles.built.css`: rebuild through `npm run build:css` after CSS changes.
- Modify `crates/web/src/views/components.rs`: replace the select theme picker with accessible icon buttons and tighten the global header.
- Modify `crates/web/src/views/layouts.rs`: turn `DashboardLayout` into a real app frame with persistent desktop sidebar, mobile nav fallback, and sidebar-bottom account control.
- Modify `crates/web/src/views/dashboard.rs`: make overview content denser and table-first.
- Modify `crates/web/src/views/requests.rs`: turn the request queue into a compact decision surface.
- Modify `crates/web/src/views/links.rs`: make new-link form grouped and dense; make link detail a property inspector.
- Modify `crates/web/src/views/settings.rs`: render account status as compact property groups.
- Modify `crates/web/src/views/invitation.rs`: align recipient pages with the new panel and control vocabulary without adding admin sidebar chrome.
- Modify tests in the same Rust view files plus `crates/web/tests/route_smoke.rs`: update assertions away from the old select/header/sidebar details and add coverage for the new shell controls.

## Task 1: CSS Foundation And Generated Styles

**Files:**
- Modify: `crates/web/assets/styles.css`
- Modify: `crates/web/assets/styles.built.css`
- Modify: `crates/web/tests/route_smoke.rs`

- [ ] **Step 1: Write the failing generated CSS smoke test**

In `crates/web/tests/route_smoke.rs`, extend `static_styles_returns_css` with assertions for the app-level classes the redesign depends on:

```rust
    assert!(text.contains(".app-header"));
    assert!(text.contains(".theme-toggle"));
    assert!(text.contains(".dashboard-sidebar"));
    assert!(text.contains(".mac-panel"));
    assert!(text.contains(".compact-table"));
```

- [ ] **Step 2: Run the focused test to verify it fails**

Run: `cargo test -p web static_styles_returns_css`

Expected: FAIL because `styles.built.css` does not yet contain `.app-header`, `.theme-toggle`, `.dashboard-sidebar`, `.mac-panel`, or `.compact-table`.

- [ ] **Step 3: Retune theme tokens and add app-level CSS**

In `crates/web/assets/styles.css`, leave the imports and DaisyUI plugin setup at the top of the file. Replace the light and dark theme values with the following restrained OKLCH values, preserving the existing theme names `ghinvite` and `ghinvite-dark`:

```css
@plugin "daisyui/theme" {
  name: "ghinvite";
  default: true;
  prefersdark: false;
  color-scheme: light;

  --color-base-100: oklch(98.2% 0.006 250);
  --color-base-200: oklch(94.8% 0.008 250);
  --color-base-300: oklch(88.4% 0.012 250);
  --color-base-content: oklch(23.5% 0.02 250);

  --color-primary: oklch(54% 0.15 255);
  --color-primary-content: oklch(98% 0.006 255);
  --color-secondary: oklch(58% 0.065 260);
  --color-secondary-content: oklch(98% 0.006 260);
  --color-accent: oklch(62% 0.095 210);
  --color-accent-content: oklch(98% 0.006 210);
  --color-neutral: oklch(32% 0.02 250);
  --color-neutral-content: oklch(98% 0.006 250);

  --color-info: oklch(58% 0.12 245);
  --color-info-content: oklch(98% 0.006 245);
  --color-success: oklch(56% 0.11 155);
  --color-success-content: oklch(98% 0.006 155);
  --color-warning: oklch(72% 0.13 82);
  --color-warning-content: oklch(24% 0.03 82);
  --color-error: oklch(57% 0.16 25);
  --color-error-content: oklch(98% 0.006 25);

  --radius-selector: 0.45rem;
  --radius-field: 0.45rem;
  --radius-box: 0.75rem;
  --size-selector: 0.25rem;
  --size-field: 0.25rem;
  --border: 1px;
  --depth: 1;
  --noise: 0;
}

@plugin "daisyui/theme" {
  name: "ghinvite-dark";
  default: false;
  prefersdark: false;
  color-scheme: dark;

  --color-base-100: oklch(24% 0.018 250);
  --color-base-200: oklch(18.5% 0.016 250);
  --color-base-300: oklch(31% 0.02 250);
  --color-base-content: oklch(91.5% 0.011 250);

  --color-primary: oklch(70% 0.12 255);
  --color-primary-content: oklch(18.5% 0.016 250);
  --color-secondary: oklch(72% 0.065 260);
  --color-secondary-content: oklch(18.5% 0.016 250);
  --color-accent: oklch(73% 0.08 210);
  --color-accent-content: oklch(18.5% 0.016 250);
  --color-neutral: oklch(84% 0.011 250);
  --color-neutral-content: oklch(18.5% 0.016 250);

  --color-info: oklch(72% 0.1 245);
  --color-info-content: oklch(18.5% 0.016 250);
  --color-success: oklch(70% 0.1 155);
  --color-success-content: oklch(18.5% 0.016 250);
  --color-warning: oklch(78% 0.12 82);
  --color-warning-content: oklch(20% 0.024 82);
  --color-error: oklch(68% 0.14 25);
  --color-error-content: oklch(18.5% 0.016 250);

  --radius-selector: 0.45rem;
  --radius-field: 0.45rem;
  --radius-box: 0.75rem;
  --size-selector: 0.25rem;
  --size-field: 0.25rem;
  --border: 1px;
  --depth: 1;
  --noise: 0;
}
```

Below the existing `@layer base` block, add this `@layer components` block:

```css
@layer components {
  .app-header {
    min-height: 3rem;
    height: 3rem;
    border-bottom: 1px solid color-mix(in oklch, var(--color-base-content) 12%, transparent);
    background: color-mix(in oklch, var(--color-base-100) 92%, var(--color-base-200));
  }

  .app-header .btn {
    min-height: 2rem;
    height: 2rem;
  }

  .theme-toggle {
    display: inline-flex;
    align-items: center;
    gap: 0.125rem;
    padding: 0.125rem;
    border: 1px solid color-mix(in oklch, var(--color-base-content) 12%, transparent);
    border-radius: 0.65rem;
    background: color-mix(in oklch, var(--color-base-200) 82%, transparent);
  }

  .theme-toggle .btn {
    width: 1.75rem;
    min-height: 1.75rem;
    height: 1.75rem;
    padding: 0;
    border-radius: 0.5rem;
    color: color-mix(in oklch, var(--color-base-content) 72%, transparent);
  }

  .theme-toggle .btn[aria-pressed="true"] {
    background: var(--color-base-100);
    color: var(--color-base-content);
    box-shadow: 0 1px 2px color-mix(in oklch, var(--color-base-content) 12%, transparent);
  }

  .dashboard-frame {
    display: flex;
    min-height: calc(100vh - 3rem);
    background: var(--color-base-200);
  }

  .dashboard-sidebar {
    width: 15rem;
    border-right: 1px solid color-mix(in oklch, var(--color-base-content) 12%, transparent);
    background: color-mix(in oklch, var(--color-base-100) 56%, var(--color-base-200));
  }

  .dashboard-main {
    min-width: 0;
    flex: 1 1 auto;
    padding: 1.25rem;
  }

  .app-nav-row {
    min-height: 2rem;
    border-radius: 0.55rem;
    padding: 0.375rem 0.625rem;
    color: color-mix(in oklch, var(--color-base-content) 78%, transparent);
  }

  .app-nav-row:hover {
    background: color-mix(in oklch, var(--color-base-content) 7%, transparent);
    color: var(--color-base-content);
  }

  .app-nav-row-active {
    background: color-mix(in oklch, var(--color-primary) 13%, var(--color-base-100));
    color: var(--color-primary);
    font-weight: 600;
  }

  .mac-panel {
    border: 1px solid color-mix(in oklch, var(--color-base-content) 12%, transparent);
    border-radius: var(--radius-box);
    background: var(--color-base-100);
    box-shadow: 0 1px 2px color-mix(in oklch, var(--color-base-content) 8%, transparent);
  }

  .property-list {
    display: grid;
  }

  .property-row {
    display: grid;
    grid-template-columns: minmax(8rem, 12rem) minmax(0, 1fr);
    gap: 1rem;
    border-top: 1px solid color-mix(in oklch, var(--color-base-content) 10%, transparent);
    padding: 0.75rem 1rem;
  }

  .property-row:first-child {
    border-top: 0;
  }

  .property-label {
    color: color-mix(in oklch, var(--color-base-content) 58%, transparent);
    font-size: 0.8125rem;
    font-weight: 500;
  }

  .compact-table :where(th, td) {
    padding: 0.5rem 0.75rem;
    vertical-align: middle;
  }

  .compact-table :where(th) {
    color: color-mix(in oklch, var(--color-base-content) 55%, transparent);
    font-size: 0.6875rem;
    font-weight: 600;
    letter-spacing: 0.04em;
    text-transform: uppercase;
  }
}
```

- [ ] **Step 4: Rebuild CSS**

Run: `npm run build:css` from `crates/web`.

Expected: PASS and `crates/web/assets/styles.built.css` changes.

- [ ] **Step 5: Run the focused test to verify it passes**

Run: `cargo test -p web static_styles_returns_css`

Expected: PASS.

- [ ] **Step 6: Commit CSS foundation**

```bash
git add crates/web/assets/styles.css crates/web/assets/styles.built.css crates/web/tests/route_smoke.rs
git commit -m "feat(web): add macos app shell styling"
```

## Task 2: Compact Header And Icon Theme Switcher

**Files:**
- Modify: `crates/web/src/views/components.rs`

- [ ] **Step 1: Replace the nav test with theme icon coverage**

In `crates/web/src/views/components.rs`, replace `nav_renders_theme_selector` with:

```rust
    #[test]
    fn nav_renders_compact_header_theme_icons_and_signed_in_controls() {
        let html = crate::views::render::render(|| {
            rsx! { Nav { signed_in_login: Some("admin".to_string()) } }
        });

        assert!(html.contains("app-header"));
        assert!(html.contains("theme-toggle"));
        assert!(html.contains("aria-label=\"Use light theme\""));
        assert!(html.contains("aria-label=\"Use dark theme\""));
        assert!(html.contains("data-theme-value=\"ghinvite\""));
        assert!(html.contains("data-theme-value=\"ghinvite-dark\""));
        assert!(html.contains("aria-pressed=\"true\""));
        assert!(html.contains("Sign out"));
        assert!(html.contains("@admin"));
        assert!(html.contains("window.localStorage.getItem(key)"));
        assert!(html.contains("window.localStorage.setItem(key, next)"));
        assert!(!html.contains("<select"));
        assert!(!html.contains("Install on another account"));
    }
```

- [ ] **Step 2: Run the focused test to verify it fails**

Run: `cargo test -p web nav_renders_compact_header_theme_icons_and_signed_in_controls`

Expected: FAIL because `Nav` still renders a select and the older navbar classes.

- [ ] **Step 3: Replace theme sync script**

In `crates/web/src/views/components.rs`, replace `THEME_SYNC_SCRIPT` with:

```rust
const THEME_SYNC_SCRIPT: &str = r#"
(function () {
  var key = 'ghinvite-theme';
  var light = 'ghinvite';
  var dark = 'ghinvite-dark';

  function valid(value) {
    return value === light || value === dark;
  }

  function apply(value) {
    var theme = valid(value) ? value : light;
    document.documentElement.setAttribute('data-theme', theme);
    if (document.body) {
      document.body.setAttribute('data-theme', theme);
    }
    var buttons = document.querySelectorAll('[data-theme-value]');
    for (var i = 0; i < buttons.length; i += 1) {
      var active = buttons[i].getAttribute('data-theme-value') === theme;
      buttons[i].setAttribute('aria-pressed', active ? 'true' : 'false');
    }
  }

  var stored = light;
  try {
    stored = window.localStorage.getItem(key) || light;
  } catch (_) {
    stored = light;
  }
  apply(stored);

  document.addEventListener('DOMContentLoaded', function () {
    apply(stored);
  });

  document.addEventListener('click', function (event) {
    var button = event.target.closest('[data-theme-value]');
    if (!button) {
      return;
    }
    var next = valid(button.getAttribute('data-theme-value')) ? button.getAttribute('data-theme-value') : light;
    apply(next);
    try {
      window.localStorage.setItem(key, next);
    } catch (_) {}
  });
})();
"#;
```

- [ ] **Step 4: Replace `Nav` markup**

In the same file, replace `pub fn Nav` with this implementation. The SVG paths are inline to avoid a new icon dependency.

```rust
#[component]
pub fn Nav(props: NavProps) -> Element {
    rsx! {
        nav {
            class: "app-header navbar min-h-0 gap-2 px-3 py-0 text-base-content sm:px-4",
            div { class: "min-w-0 flex-1",
                a {
                    class: "btn btn-ghost btn-sm h-8 min-h-0 px-2 text-sm font-semibold tracking-tight",
                    href: "/",
                    span { class: "grid size-5 place-items-center rounded-md bg-primary text-[0.7rem] font-bold text-primary-content", "g" }
                    span { class: "ml-1", "ghinvite" }
                }
            }
            div { class: "flex flex-none items-center justify-end gap-2",
                div { class: "theme-toggle", role: "group", aria_label: "Theme",
                    button {
                        r#type: "button",
                        class: "btn btn-ghost btn-xs",
                        aria_label: "Use light theme",
                        aria_pressed: "true",
                        "data-theme-value": "ghinvite",
                        svg { xmlns: "http://www.w3.org/2000/svg", view_box: "0 0 24 24", fill: "none", stroke: "currentColor", stroke_width: "1.8", class: "size-4", aria_hidden: "true",
                            path { stroke_linecap: "round", stroke_linejoin: "round", d: "M12 4.5V3m0 18v-1.5M4.5 12H3m18 0h-1.5M6.34 6.34 5.28 5.28m13.44 13.44-1.06-1.06m0-11.32 1.06-1.06M5.28 18.72l1.06-1.06M16.5 12a4.5 4.5 0 1 1-9 0 4.5 4.5 0 0 1 9 0Z" }
                        }
                    }
                    button {
                        r#type: "button",
                        class: "btn btn-ghost btn-xs",
                        aria_label: "Use dark theme",
                        aria_pressed: "false",
                        "data-theme-value": "ghinvite-dark",
                        svg { xmlns: "http://www.w3.org/2000/svg", view_box: "0 0 24 24", fill: "none", stroke: "currentColor", stroke_width: "1.8", class: "size-4", aria_hidden: "true",
                            path { stroke_linecap: "round", stroke_linejoin: "round", d: "M20.25 14.15A7.5 7.5 0 0 1 9.85 3.75a8.25 8.25 0 1 0 10.4 10.4Z" }
                        }
                    }
                }
                {match props.signed_in_login.as_deref() {
                    Some(login) => rsx! {
                        span { class: "hidden max-w-32 truncate px-1 text-xs text-base-content/60 sm:inline-flex", "@{login}" }
                        a { class: "btn btn-ghost btn-sm h-8 min-h-0 px-2", href: "/logout", "Sign out" }
                    },
                    None => rsx! {
                        a { class: "btn btn-primary btn-sm h-8 min-h-0 px-3", href: "/login", "Sign in" }
                    }
                }}
            }
            script { "{THEME_SYNC_SCRIPT}" }
        }
    }
}
```

- [ ] **Step 5: Run focused component tests**

Run: `cargo test -p web nav_renders_compact_header_theme_icons_and_signed_in_controls`

Expected: PASS.

- [ ] **Step 6: Commit header changes**

```bash
git add crates/web/src/views/components.rs
git commit -m "feat(web): add compact icon theme header"
```

## Task 3: Real Dashboard Sidebar And Account Control

**Files:**
- Modify: `crates/web/src/views/layouts.rs`

- [ ] **Step 1: Replace the dashboard layout test**

In `crates/web/src/views/layouts.rs`, replace `dashboard_layout_marks_active_navigation` with:

```rust
    #[test]
    fn dashboard_layout_renders_real_sidebar_and_account_control() {
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
        assert!(html.contains("app-header"));
        assert!(html.contains("dashboard-frame"));
        assert!(html.contains("dashboard-sidebar"));
        assert!(html.contains("mobile-dashboard-nav"));
        assert!(html.contains("sidebar-account-switcher"));
        assert!(html.contains("Install another account"));
        assert!(html.contains("aria-current=\"page\""));
        assert!(html.contains("app-nav-row-active"));
        assert!(html.contains("Pending requests"));
        assert!(!html.contains("rounded-box border border-base-300 bg-base-100 p-3 shadow-sm"));
    }
```

- [ ] **Step 2: Run the focused test to verify it fails**

Run: `cargo test -p web dashboard_layout_renders_real_sidebar_and_account_control`

Expected: FAIL because the layout still uses the floating sidebar card and old mobile button row.

- [ ] **Step 3: Replace the desktop and mobile dashboard shell**

In `DashboardLayout`, update the active-nav class variables to the new app nav rows:

```rust
    let overview_side = if active_nav == "overview" {
        "app-nav-row app-nav-row-active"
    } else {
        "app-nav-row"
    };
    let new_link_side = if active_nav == "new-link" {
        "app-nav-row app-nav-row-active"
    } else {
        "app-nav-row"
    };
    let requests_side = if active_nav == "requests" {
        "app-nav-row app-nav-row-active"
    } else {
        "app-nav-row"
    };
    let settings_side = if active_nav == "settings" {
        "app-nav-row app-nav-row-active"
    } else {
        "app-nav-row"
    };
```

Replace the body of `DashboardLayout` with this shell:

```rust
    rsx! {
        head {
            title { "{props.title}" }
            link { rel: "stylesheet", href: "/static/styles.css" }
        }
        body {
            class: "min-h-screen bg-base-200 text-base-content antialiased",
            "data-theme": "ghinvite",
            Nav { signed_in_login: props.signed_in_login.clone() }
            div { class: "dashboard-frame",
                aside { class: "dashboard-sidebar hidden shrink-0 flex-col md:flex",
                    nav { class: "flex-1 space-y-1 p-3 text-sm",
                        a { class: "{overview_side}", href: "/accounts/{login}", aria_current: if active_nav == "overview" { "page" } else { "false" }, "Overview" }
                        a { class: "{new_link_side}", href: "/accounts/{login}/links/new", aria_current: if active_nav == "new-link" { "page" } else { "false" }, "New link" }
                        a { class: "{requests_side}", href: "/accounts/{login}/requests", aria_current: if active_nav == "requests" { "page" } else { "false" }, "Pending requests" }
                        a { class: "app-nav-row", href: "/accounts/{login}/audit", "Audit log" }
                        a { class: "{settings_side}", href: "/accounts/{login}/settings", aria_current: if active_nav == "settings" { "page" } else { "false" }, "Settings" }
                    }
                    div { class: "sidebar-account-switcher border-t border-base-300 p-3",
                        p { class: "text-[0.68rem] font-semibold uppercase tracking-wide text-base-content/45", "Active account" }
                        a { class: "mt-2 flex min-w-0 items-center gap-2 rounded-box px-2 py-2 text-sm hover:bg-base-100", href: "/accounts/{login}/settings",
                            span { class: "grid size-7 shrink-0 place-items-center rounded-lg bg-base-300 text-xs font-semibold", "@" }
                            span { class: "min-w-0 flex-1 truncate font-medium", "{login}" }
                        }
                        a { class: "btn btn-ghost btn-xs mt-2 h-7 min-h-0 w-full justify-start px-2", href: "/install", "Install another account" }
                    }
                }
                div { class: "min-w-0 flex-1",
                    nav { class: "mobile-dashboard-nav border-b border-base-300 bg-base-100 px-3 py-2 md:hidden",
                        div { class: "flex gap-1 overflow-x-auto whitespace-nowrap text-sm",
                            a { class: "{overview_side}", href: "/accounts/{login}", aria_current: if active_nav == "overview" { "page" } else { "false" }, "Overview" }
                            a { class: "{new_link_side}", href: "/accounts/{login}/links/new", aria_current: if active_nav == "new-link" { "page" } else { "false" }, "New link" }
                            a { class: "{requests_side}", href: "/accounts/{login}/requests", aria_current: if active_nav == "requests" { "page" } else { "false" }, "Requests" }
                            a { class: "app-nav-row", href: "/accounts/{login}/audit", "Audit" }
                            a { class: "{settings_side}", href: "/accounts/{login}/settings", aria_current: if active_nav == "settings" { "page" } else { "false" }, "Settings" }
                        }
                    }
                    main { class: "dashboard-main",
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

- [ ] **Step 4: Tune `HomeLayout` and `InvitationLayout` for the smaller header**

In `HomeLayout`, change the `main` class to:

```rust
main { class: "min-h-[calc(100vh-3rem)] px-4 py-6", {props.children} }
```

In `InvitationLayout`, change the `main` and inner panel classes to:

```rust
main { class: "min-h-[calc(100vh-3rem)] px-4 py-6",
    div { class: "mx-auto flex min-h-[calc(100vh-6rem)] w-full max-w-lg items-center",
        div { class: "mac-panel w-full",
            div { class: "p-6", {props.children} }
        }
    }
}
```

Update `invitation_layout_uses_product_theme` to assert `mac-panel` instead of `card`.

- [ ] **Step 5: Run focused layout tests**

Run: `cargo test -p web dashboard_layout_renders_real_sidebar_and_account_control invitation_layout_uses_product_theme`

Expected: PASS.

- [ ] **Step 6: Commit layout changes**

```bash
git add crates/web/src/views/layouts.rs
git commit -m "feat(web): add native dashboard sidebar"
```

## Task 4: Dense Overview And Request Queue

**Files:**
- Modify: `crates/web/src/views/dashboard.rs`
- Modify: `crates/web/src/views/requests.rs`

- [ ] **Step 1: Tighten overview test expectations**

In `crates/web/src/views/dashboard.rs`, update `overview_page_renders_console_copy` so it verifies the dense overview vocabulary:

```rust
        assert!(html.contains("Console overview"));
        assert!(html.contains("Review queue"));
        assert!(html.contains("Create first share link"));
        assert!(html.contains("Recent share links"));
        assert!(html.contains("mac-panel"));
        assert!(html.contains("compact-table"));
        assert!(!html.contains("text-3xl"));
```

- [ ] **Step 2: Tighten request queue test expectations**

In `crates/web/src/views/requests.rs`, extend `requests_queue_renders_link_context_before_actions` with:

```rust
        assert!(html.contains("request-decision-list"));
        assert!(html.contains("mac-panel"));
        assert!(html.contains("compact-table"));
```

- [ ] **Step 3: Run focused tests to verify they fail**

Run: `cargo test -p web overview_page_renders_console_copy requests_queue_renders_link_context_before_actions`

Expected: FAIL because neither page has the new dense classes yet.

- [ ] **Step 4: Replace overview content structure**

In `OverviewPage`, change the `recent_links_view` row cells to use compact link and badge classes:

```rust
            tr { class: "hover:bg-base-200/70",
                td { class: "font-mono text-xs",
                    a {
                        class: "link link-hover",
                        href: "/accounts/{login}/links/{id_str}",
                        "{slug_str}"
                    }
                }
                td { span { class: "{badge} badge-sm", "{label}" } }
                td { class: "text-right tabular-nums", "{link.uses_count}" }
            }
```

Replace the `children` block inside `DashboardLayout` with a compact header, summary panel, and dense table:

```rust
            children: rsx! {
                header { class: "mb-4 flex flex-col gap-2 sm:flex-row sm:items-center sm:justify-between",
                    div {
                        h1 { class: "text-xl font-semibold tracking-tight", "Console overview" }
                        p { class: "mt-0.5 text-sm text-base-content/60", "{props.account_type} account: {props.account_login}" }
                    }
                    a { class: "btn btn-primary btn-sm", href: "/accounts/{login}/links/new", "New share link" }
                }
                section { class: "mac-panel mb-4 overflow-hidden",
                    div { class: "grid divide-y divide-base-300 md:grid-cols-2 md:divide-x md:divide-y-0",
                        a { class: "block p-4 transition-colors hover:bg-base-200/60", href: "/accounts/{login}/requests",
                            p { class: "text-sm font-medium", "Review queue" }
                            p { class: "mt-1 text-sm text-base-content/65", "{props.pending_requests} pending requests need an admin decision." }
                        }
                        a { class: "block p-4 transition-colors hover:bg-base-200/60", href: "/accounts/{login}/links/new",
                            p { class: "text-sm font-medium", "Active access links" }
                            p { class: "mt-1 text-sm text-base-content/65", "{props.active_links} active links can accept collaborator requests." }
                        }
                    }
                }
                section { class: "mac-panel overflow-hidden",
                    div { class: "flex items-center justify-between border-b border-base-300 px-4 py-3",
                        h2 { class: "text-sm font-semibold", "Recent share links" }
                        a { class: "btn btn-ghost btn-xs h-7 min-h-0", href: "/accounts/{login}/links/new", "Create link" }
                    }
                    {if props.recent_links.is_empty() {
                        rsx! {
                            div { class: "p-5 text-sm text-base-content/70",
                                h3 { class: "font-medium text-base-content", "No share links yet" }
                                p { class: "mt-1", "Create a link to let recipients request collaborator access without manual GitHub invites." }
                                a { class: "btn btn-primary btn-sm mt-4", href: "/accounts/{login}/links/new", "Create first share link" }
                            }
                        }
                    } else {
                        rsx! {
                            div { class: "overflow-x-auto",
                                table { class: "table compact-table table-sm",
                                    thead { tr { th { "Slug" } th { "State" } th { class: "text-right", "Uses" } } }
                                    tbody { {recent_links_view} }
                                }
                            }
                        }
                    }}
                }
            },
```

- [ ] **Step 5: Replace request queue surface**

In `RequestsQueuePage`, replace the non-empty wrapper with this compact decision list:

```rust
                        div { class: "request-decision-list mac-panel compact-table overflow-hidden",
                            {props.rows.iter().map(|r| {
                                let rid = r.request_id.clone();
                                let just = r.justification.clone();
                                let link_id = r.link_id.clone();
                                let link_slug = r.link_slug.clone();
                                let link_label = if link_id.is_empty() {
                                    rsx! { span { "{link_slug}" } }
                                } else {
                                    rsx! { a { class: "link link-hover", href: "/accounts/{login}/links/{link_id}", "{link_slug}" } }
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
                                        ul { class: "mt-1 flex flex-wrap gap-1.5 text-sm",
                                            {r.repos.iter().map(|repo| {
                                                let repo = repo.clone();
                                                rsx! { li { class: "badge badge-ghost badge-sm", "{repo}" } }
                                            })}
                                        }
                                    }
                                };
                                rsx! {
                                    div { class: "grid gap-3 border-b border-base-300 px-4 py-3 last:border-b-0 lg:grid-cols-[minmax(10rem,14rem)_minmax(0,1fr)_auto] lg:items-start",
                                        div { class: "min-w-0",
                                            p { class: "truncate text-sm font-medium", "@{requester}" }
                                            p { class: "mt-0.5 text-xs text-base-content/55", "Requested at {created}" }
                                        }
                                        div { class: "min-w-0 space-y-2",
                                            p { class: "text-sm",
                                                "Via "
                                                {link_label}
                                            }
                                            div { class: "flex flex-wrap gap-1.5",
                                                span { class: "badge badge-neutral badge-sm", "Permission: {permission}" }
                                                span { class: "badge badge-ghost badge-sm", "Expires: {expires}" }
                                                span { class: "badge badge-ghost badge-sm", "{approval}" }
                                            }
                                            div {
                                                p { class: "text-[0.68rem] font-semibold uppercase tracking-wide text-base-content/45", "Repositories" }
                                                {repos}
                                            }
                                            {match just {
                                                Some(j) if !j.is_empty() => rsx! { blockquote { class: "rounded-box bg-base-200 px-3 py-2 text-sm text-base-content/80", "\"{j}\"" } },
                                                _ => rsx! {},
                                            }}
                                            {if repos_available {
                                                rsx! { p { class: "text-xs text-base-content/65", "Approving sends GitHub collaborator invitations for the repositories listed here." } }
                                            } else {
                                                rsx! { p { class: "text-xs text-base-content/65", "This request cannot be completed until its share link details are available." } }
                                            }}
                                        }
                                        {if actions_available {
                                            rsx! {
                                                div { class: "flex gap-2 lg:flex-col lg:items-stretch",
                                                    form { method: "post", action: "/accounts/{login}/requests/{rid}/approve",
                                                        button { r#type: "submit", class: "btn btn-success btn-sm h-8 min-h-0", "Approve request" }
                                                    }
                                                    form { method: "post", action: "/accounts/{login}/requests/{rid}/decline",
                                                        button { r#type: "submit", class: "btn btn-error btn-sm h-8 min-h-0", "Decline request" }
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
```

Update the empty state class to:

```rust
div { class: "mac-panel p-5 text-base-content/70",
```

- [ ] **Step 6: Run focused tests**

Run: `cargo test -p web overview_page_renders_console_copy requests_queue_renders_link_context_before_actions requests_queue_renders_missing_link_context_without_broken_link`

Expected: PASS.

- [ ] **Step 7: Commit overview and request queue changes**

```bash
git add crates/web/src/views/dashboard.rs crates/web/src/views/requests.rs
git commit -m "feat(web): densify overview and requests"
```

## Task 5: Forms, Property Inspector, Settings, And Recipient Polish

**Files:**
- Modify: `crates/web/src/views/links.rs`
- Modify: `crates/web/src/views/settings.rs`
- Modify: `crates/web/src/views/invitation.rs`

- [ ] **Step 1: Update link and settings tests**

In `crates/web/src/views/links.rs`, extend `link_create_form_renders_sectioned_console_form`:

```rust
        assert!(html.contains("mac-panel"));
        assert!(html.contains("repo-choice-row"));
```

In `link_detail_renders_operational_context`, add:

```rust
        assert!(html.contains("property-list"));
        assert!(html.contains("property-row"));
        assert!(html.contains("mac-panel"));
        assert!(!html.contains("md:grid-cols-2 gap-4"));
```

In `crates/web/src/views/settings.rs`, extend `settings_page_renders_account_status_panels`:

```rust
        assert!(html.contains("property-list"));
        assert!(html.contains("mac-panel"));
```

In `crates/web/src/views/invitation.rs`, extend `pending_page_renders_refresh_affordance`:

```rust
        assert!(html.contains("Request status"));
        assert!(!html.contains("card-title"));
```

- [ ] **Step 2: Run focused tests to verify they fail**

Run: `cargo test -p web link_create_form_renders_sectioned_console_form link_detail_renders_operational_context settings_page_renders_account_status_panels pending_page_renders_refresh_affordance`

Expected: FAIL because the pages still use card-heavy markup.

- [ ] **Step 3: Update new-link form panels and repository rows**

In `LinkCreateFormPage`, replace each `section { class: "card border border-base-300 bg-base-100 shadow-sm"` with:

```rust
section { class: "mac-panel",
```

Replace each direct `div { class: "card-body ..."` inside those sections with a plain padded div, using `p-4` and the existing gap:

```rust
div { class: "space-y-4 p-4",
```

For repository rows, replace the label class with:

```rust
label { class: "repo-choice-row flex cursor-pointer items-center gap-3 rounded-lg px-2 py-1.5 text-sm hover:bg-base-100",
```

Keep the existing checkbox input and repository name span.

- [ ] **Step 4: Replace link detail card grid with property rows**

In `LinkDetailPage`, replace the share URL section with:

```rust
                section { class: "mac-panel mb-4 overflow-hidden",
                    div { class: "border-b border-base-300 px-4 py-3",
                        h2 { class: "text-sm font-semibold", "Share URL" }
                        p { class: "mt-0.5 text-xs text-base-content/60", "Send this URL to recipients who should request access." }
                    }
                    div { class: "p-4",
                        input {
                            class: "input input-bordered input-sm w-full font-mono text-xs",
                            readonly: true,
                            value: "{props.share_url}",
                            aria_label: "Share URL",
                        }
                        div { class: "mt-3",
                            a { class: "btn btn-outline btn-sm h-8 min-h-0", href: "{preview_href}", "Open recipient preview" }
                        }
                    }
                }
```

Replace the permission, uses, expiration, and approval card grid with one property list:

```rust
                section { class: "mac-panel mb-4 overflow-hidden",
                    dl { class: "property-list",
                        div { class: "property-row",
                            dt { class: "property-label", "Permission" }
                            dd { class: "text-sm", "{perm}" }
                        }
                        div { class: "property-row",
                            dt { class: "property-label", "Uses" }
                            dd { class: "text-sm tabular-nums", "{uses}" }
                        }
                        div { class: "property-row",
                            dt { class: "property-label", "Expiration" }
                            dd { class: "text-sm", "{expires}" }
                        }
                        div { class: "property-row",
                            dt { class: "property-label", "Approval" }
                            dd { class: "text-sm", "{approval}" }
                        }
                    }
                }
```

Replace the repository card with:

```rust
                section { class: "mac-panel mb-4 overflow-hidden",
                    div { class: "border-b border-base-300 px-4 py-3",
                        h2 { class: "text-sm font-semibold", "Repositories" }
                    }
                    div { class: "p-4",
                        ul { class: "space-y-1 text-sm", {repos} }
                    }
                }
```

Replace the internal note card branch with:

```rust
                {match props.link.internal_note.as_deref() {
                    Some(note) => rsx! {
                        section { class: "mac-panel mb-4 overflow-hidden",
                            div { class: "border-b border-base-300 px-4 py-3",
                                h2 { class: "text-sm font-semibold", "Internal note" }
                            }
                            div { class: "p-4",
                                p { class: "text-sm text-base-content/80", "{note}" }
                            }
                        }
                    },
                    None => rsx! {},
                }}
```

For the danger section, keep the modal but change the section class to:

```rust
section { class: "mac-panel border-error/30 bg-base-100",
```

- [ ] **Step 5: Update settings page to property rows**

In `SettingsPage`, replace the two-card grid with:

```rust
                section { class: "mac-panel max-w-3xl overflow-hidden",
                    dl { class: "property-list",
                        div { class: "property-row",
                            dt { class: "property-label", "Account" }
                            dd {
                                p { class: "text-sm font-medium", "{login}" }
                                p { class: "mt-0.5 text-xs text-base-content/60", "{account_type}" }
                            }
                        }
                        div { class: "property-row",
                            dt { class: "property-label", "Installation scope" }
                            dd {
                                p { class: "text-sm font-medium", "{repos_label}" }
                                p { class: "mt-0.5 text-xs text-base-content/60", "Change repository selection in the GitHub App settings in this version." }
                            }
                        }
                    }
                }
                div { class: "alert mt-4 max-w-3xl shadow-sm",
                    span { "Editable settings are planned for v1.1. This page reflects the active installation state." }
                }
```

- [ ] **Step 6: Align recipient status heading with app vocabulary**

In `PendingPage`, replace:

```rust
h1 { class: "card-title text-2xl mb-4", "Request status" }
```

with:

```rust
h1 { class: "mb-4 text-xl font-semibold tracking-tight", "Request status" }
```

In `LandingPage`, replace the heading with:

```rust
h1 { class: "text-xl font-semibold tracking-tight", "Access request" }
```

In `RequestFormPage`, replace the heading with:

```rust
h1 { class: "text-xl font-semibold tracking-tight", "Request access" }
```

- [ ] **Step 7: Run focused tests**

Run: `cargo test -p web link_create_form_renders_sectioned_console_form link_detail_renders_operational_context settings_page_renders_account_status_panels pending_page_renders_refresh_affordance`

Expected: PASS.

- [ ] **Step 8: Commit form and property inspector changes**

```bash
git add crates/web/src/views/links.rs crates/web/src/views/settings.rs crates/web/src/views/invitation.rs
git commit -m "feat(web): refine forms and detail panels"
```

## Task 6: Full Verification And Final Polish

**Files:**
- Modify only files that fail formatting, tests, or CSS build.

- [ ] **Step 1: Format Rust code**

Run: `cargo fmt`

Expected: PASS with no output or only rustfmt-normalized file changes.

- [ ] **Step 2: Run web package tests**

Run: `cargo test -p web`

Expected: PASS.

- [ ] **Step 3: Rebuild CSS after all markup changes**

Run: `npm run build:css` from `crates/web`.

Expected: PASS and `styles.built.css` includes any classes introduced by later tasks.

- [ ] **Step 4: Re-run the web package tests after CSS rebuild**

Run: `cargo test -p web`

Expected: PASS.

- [ ] **Step 5: Inspect the final diff**

Run: `git status --short`

Expected: only files touched by this plan are modified.

Run: `git diff -- crates/web/assets/styles.css crates/web/assets/styles.built.css crates/web/src/views/components.rs crates/web/src/views/layouts.rs crates/web/src/views/dashboard.rs crates/web/src/views/requests.rs crates/web/src/views/links.rs crates/web/src/views/settings.rs crates/web/src/views/invitation.rs crates/web/tests/route_smoke.rs`

Expected: diff shows the compact header, icon theme buttons, real sidebar, sidebar-bottom account control, denser overview and requests, property rows, and CSS foundation.

- [ ] **Step 6: Commit final verification fixes if needed**

If formatting or CSS rebuild changed files after the previous task commits, commit only those files:

```bash
git add crates/web/assets/styles.css crates/web/assets/styles.built.css crates/web/src/views/components.rs crates/web/src/views/layouts.rs crates/web/src/views/dashboard.rs crates/web/src/views/requests.rs crates/web/src/views/links.rs crates/web/src/views/settings.rs crates/web/src/views/invitation.rs crates/web/tests/route_smoke.rs
git commit -m "chore(web): verify macos pwa redesign"
```

If there are no remaining changes, do not create an empty commit.
