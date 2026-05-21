# Theme Selector Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a light/dark theme selector to the shared app navbar, defaulting to the existing light `ghinvite` theme and persisting the user's choice in browser storage.

**Architecture:** Keep SSR default markup on the light `ghinvite` theme. Add a second DaisyUI theme named `ghinvite-dark`, then add a compact native selector and inline synchronization script to the shared `Nav` component. The preference is browser-local via `localStorage`, with no route, session, storage, or domain changes.

**Tech Stack:** Rust, Dioxus 0.7 SSR, Tailwind CSS v4, DaisyUI v5, browser `localStorage`, existing `web` crate route/view tests, existing npm CSS build.

---

## Spec Reference

Design spec: `docs/superpowers/specs/2026-05-21-theme-selector-design.md`

## File Structure

- Modify: `crates/web/assets/styles.css` - add the `ghinvite-dark` DaisyUI theme next to the existing light theme.
- Modify: `crates/web/tests/route_smoke.rs` - extend the static CSS smoke test to verify dark theme tokens are served.
- Modify: `crates/web/src/views/components.rs` - add the shared theme selector and inline localStorage synchronization script.
- Modify: `crates/web/src/views/layouts.rs` - keep SSR default `data-theme="ghinvite"` and extend layout/nav render coverage if needed.

## Task 1: Dark DaisyUI Theme

**Files:**
- Modify: `crates/web/assets/styles.css`
- Modify: `crates/web/tests/route_smoke.rs:112-137`

- [ ] **Step 1: Extend the CSS smoke test first**

In `crates/web/tests/route_smoke.rs`, update `static_styles_returns_css` after the existing theme assertions:

```rust
    assert!(text.contains("ghinvite-dark"));
    assert!(text.contains("color-scheme:dark"));
```

- [ ] **Step 2: Run the focused smoke test and verify it fails**

Run: `cargo test -p web static_styles_returns_css --test route_smoke`

Expected: FAIL because served CSS does not contain `ghinvite-dark` yet.

- [ ] **Step 3: Add the dark theme to Tailwind and DaisyUI source CSS**

In `crates/web/assets/styles.css`, change the DaisyUI theme list from:

```css
@plugin "daisyui" {
  themes: ghinvite --default;
}
```

to:

```css
@plugin "daisyui" {
  themes: ghinvite --default, ghinvite-dark;
}
```

Then append this theme after the existing `@plugin "daisyui/theme"` block for `ghinvite` and before `@layer base`:

```css
@plugin "daisyui/theme" {
  name: "ghinvite-dark";
  default: false;
  prefersdark: false;
  color-scheme: dark;

  --color-base-100: oklch(22% 0.018 255);
  --color-base-200: oklch(18% 0.018 255);
  --color-base-300: oklch(30% 0.02 255);
  --color-base-content: oklch(91% 0.012 255);

  --color-primary: oklch(68% 0.13 255);
  --color-primary-content: oklch(18% 0.018 255);
  --color-secondary: oklch(70% 0.075 260);
  --color-secondary-content: oklch(18% 0.018 255);
  --color-accent: oklch(72% 0.1 205);
  --color-accent-content: oklch(18% 0.018 255);
  --color-neutral: oklch(84% 0.012 255);
  --color-neutral-content: oklch(18% 0.018 255);

  --color-info: oklch(72% 0.11 245);
  --color-info-content: oklch(18% 0.018 255);
  --color-success: oklch(70% 0.11 155);
  --color-success-content: oklch(18% 0.018 155);
  --color-warning: oklch(78% 0.13 80);
  --color-warning-content: oklch(20% 0.025 80);
  --color-error: oklch(68% 0.15 25);
  --color-error-content: oklch(18% 0.018 25);

  --radius-selector: 0.5rem;
  --radius-field: 0.5rem;
  --radius-box: 0.75rem;
  --size-selector: 0.25rem;
  --size-field: 0.25rem;
  --border: 1px;
  --depth: 1;
  --noise: 0;
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

Expected: no output. Do not commit `crates/web/assets/styles.built.css` unless this command unexpectedly shows it is tracked.

- [ ] **Step 7: Commit**

```bash
git add crates/web/assets/styles.css crates/web/tests/route_smoke.rs
git commit -m "feat(web): add dark app theme"
```

## Task 2: Shared Theme Selector

**Files:**
- Modify: `crates/web/src/views/components.rs`
- Modify: `crates/web/src/views/layouts.rs`

- [ ] **Step 1: Add a failing nav render test**

Append this test module to `crates/web/src/views/components.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nav_renders_theme_selector() {
        let html = crate::views::render::render(|| {
            rsx! { Nav { signed_in_login: None } }
        });

        assert!(html.contains("Theme"));
        assert!(html.contains("Light"));
        assert!(html.contains("Dark"));
        assert!(html.contains("ghinvite-theme"));
        assert!(html.contains("ghinvite-dark"));
    }
}
```

- [ ] **Step 2: Run the focused nav test and verify it fails**

Run: `cargo test -p web nav_renders_theme_selector --lib`

Expected: FAIL because `Nav` has no selector or storage synchronization script yet.

- [ ] **Step 3: Add selector and inline script to `Nav`**

In `crates/web/src/views/components.rs`, add this constant after `NavProps`:

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
    var selector = document.getElementById('theme-selector');
    if (selector) {
      selector.value = theme;
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
    var selector = document.getElementById('theme-selector');
    if (!selector) {
      return;
    }
    selector.addEventListener('change', function (event) {
      var next = valid(event.target.value) ? event.target.value : light;
      apply(next);
      try {
        window.localStorage.setItem(key, next);
      } catch (_) {}
    });
  });
})();
"#;
```

Then replace the current `div` block in `Nav` that has `class: "flex-none gap-2"` and contains the signed-in match expression with:

```rust
            div { class: "flex-none items-center gap-2",
                label { class: "sr-only", r#for: "theme-selector", "Theme" }
                select {
                    id: "theme-selector",
                    class: "select select-bordered select-sm w-24",
                    aria_label: "Theme",
                    option { value: "ghinvite", selected: true, "Light" }
                    option { value: "ghinvite-dark", "Dark" }
                }
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
            script { "{THEME_SYNC_SCRIPT}" }
```

- [ ] **Step 4: Keep layout default tests explicit**

In `crates/web/src/views/layouts.rs`, extend `dashboard_layout_marks_active_navigation` with:

```rust
        assert!(html.contains("data-theme=\"ghinvite\""));
        assert!(!html.contains("data-theme=\"ghinvite-dark\""));
```

The first assertion may already exist. Keep one copy and add the dark-theme negative assertion.

- [ ] **Step 5: Run formatting and focused tests**

Run: `cargo fmt`

Run: `cargo test -p web nav_renders_theme_selector --lib`

Run: `cargo test -p web dashboard_layout_marks_active_navigation --lib`

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/web/src/views/components.rs crates/web/src/views/layouts.rs
git commit -m "feat(web): add theme selector"
```

## Task 3: Full Verification

**Files:**
- Verify: `crates/web/assets/styles.css`
- Verify: `crates/web/src/views/components.rs`
- Verify: `crates/web/src/views/layouts.rs`
- Verify: `crates/web/tests/route_smoke.rs`

- [ ] **Step 1: Run Rust formatting check**

Run: `cargo fmt --check`

Expected: PASS.

- [ ] **Step 2: Rebuild CSS**

Run: `npm run build:css`

Working directory: `crates/web`

Expected: PASS.

- [ ] **Step 3: Run the web test suite**

Run: `cargo test -p web`

Expected: PASS.

- [ ] **Step 4: Check generated CSS tracking**

Run: `git ls-files crates/web/assets/styles.built.css`

Expected: no output.

- [ ] **Step 5: Inspect worktree**

Run: `git status --short`

Expected: no tracked modifications. `crates/web/assets/styles.built.css` must not be staged if untracked.

- [ ] **Step 6: Commit only if verification changed tracked files**

If formatting or small fixes changed tracked files, commit them:

```bash
git add crates/web/assets/styles.css crates/web/src/views/components.rs crates/web/src/views/layouts.rs crates/web/tests/route_smoke.rs
git commit -m "chore(web): finalize theme selector"
```

If there are no tracked modifications, skip this commit.

## Self-Review Notes

- Spec coverage: Task 1 adds the dark DaisyUI theme and CSS smoke coverage. Task 2 adds the shared selector, localStorage persistence script, and explicit SSR light default coverage. Task 3 verifies formatting, CSS build, tests, generated CSS tracking, and clean status.
- Scope check: The plan stays inside CSS source, shared views, layout tests, and route smoke tests. It does not change routes, sessions, storage, commands, or domain models.
- Type consistency: Theme values are literal strings `ghinvite` and `ghinvite-dark`; the storage key is `ghinvite-theme` in both test and script.
