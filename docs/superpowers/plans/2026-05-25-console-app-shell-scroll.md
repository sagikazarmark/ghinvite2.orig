# Console App Shell Scroll Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Keep the console header and desktop sidebar visible while long console page content scrolls inside the main pane.

**Architecture:** Preserve the existing Dioxus console layout and implement the app-shell behavior with CSS. The header becomes sticky, the console frame is constrained to the viewport below it, the content column becomes a flex column, and `.console-main` becomes the scroll container.

**Tech Stack:** Rust, Dioxus SSR, Axum route tests, Tailwind CSS v4, DaisyUI v5, npm CSS build.

---

## File Structure

- Modify `crates/web/src/views/layouts.rs`: add the content-column flex classes needed for `.console-main` to scroll within the console frame, and add an SSR assertion for the shell structure.
- Modify `crates/web/tests/route_smoke.rs`: extend the existing `/static/styles.css` route test so the generated CSS locks down sticky header, fixed console-frame height, sidebar height, and main-pane scrolling.
- Modify `crates/web/assets/styles.css`: update the authored component CSS for the console app shell.
- Modify `crates/web/assets/styles.built.css`: regenerate the served CSS asset with `npm run build:css` from `crates/web`.

### Task 1: Add Failing Console Shell Contract Tests

**Files:**
- Modify: `crates/web/src/views/layouts.rs:173-188`
- Modify: `crates/web/tests/route_smoke.rs:139-143`

- [ ] **Step 1: Add the SSR shell structure assertion**

In `crates/web/src/views/layouts.rs`, inside `console_layout_renders_real_sidebar_and_account_control`, add this assertion after the existing `assert!(html.contains("console-sidebar"));` line:

```rust
        assert!(html.contains("console-content min-w-0 flex flex-1 flex-col"));
```

- [ ] **Step 2: Add generated CSS assertions**

In `crates/web/tests/route_smoke.rs`, replace this block in `static_styles_returns_css`:

```rust
    assert!(text.contains(".app-header"));
    assert!(text.contains(".theme-toggle"));
    assert!(text.contains(".console-sidebar"));
    assert!(text.contains(".mac-panel"));
    assert!(text.contains(".compact-table"));
```

with this block:

```rust
    assert!(text.contains(".app-header"));
    assert!(text.contains("position:sticky"));
    assert!(text.contains("top:0"));
    assert!(text.contains("z-index:20"));
    assert!(text.contains(".theme-toggle"));
    assert!(text.contains(".console-frame"));
    assert!(text.contains("height:calc(100vh - 3rem)"));
    assert!(text.contains("overflow:hidden"));
    assert!(text.contains(".console-sidebar"));
    assert!(text.contains("height:100%"));
    assert!(text.contains(".console-main"));
    assert!(text.contains("overflow:auto"));
    assert!(text.contains(".mac-panel"));
    assert!(text.contains(".compact-table"));
```

- [ ] **Step 3: Run targeted tests and verify they fail for the expected reason**

Run from the repository root:

```bash
cargo test -p web console_layout_renders_real_sidebar_and_account_control
cargo test -p web static_styles_returns_css
```

Expected: FAIL. The layout test should report the missing `console-content min-w-0 flex flex-1 flex-col` substring. The CSS route test should report at least one missing new CSS declaration such as `position:sticky`.

### Task 2: Implement Console App Shell CSS

**Files:**
- Modify: `crates/web/src/views/layouts.rs:96-106`
- Modify: `crates/web/assets/styles.css:98-160`
- Modify: `crates/web/assets/styles.built.css`

- [ ] **Step 1: Make the content column a flex column**

In `crates/web/src/views/layouts.rs`, replace this wrapper around the mobile nav and main content:

```rust
                div { class: "min-w-0 flex-1",
```

with this wrapper:

```rust
                div { class: "console-content min-w-0 flex flex-1 flex-col",
```

- [ ] **Step 2: Update the authored app-shell CSS**

In `crates/web/assets/styles.css`, replace the current `.app-header`, `.console-frame`, `.console-sidebar`, and `.console-main` blocks with this code:

```css
  .app-header {
    position: sticky;
    top: 0;
    z-index: 20;
    min-height: 3rem;
    height: 3rem;
    border-bottom: 1px solid color-mix(in oklch, var(--color-base-content) 12%, transparent);
    background: color-mix(in oklch, var(--color-base-100) 92%, var(--color-base-200));
  }

  .console-frame {
    display: flex;
    height: calc(100vh - 3rem);
    min-height: 0;
    overflow: hidden;
    background: var(--color-base-200);
  }

  .console-sidebar {
    width: 15rem;
    height: 100%;
    min-height: 0;
    border-right: 1px solid color-mix(in oklch, var(--color-base-content) 12%, transparent);
    background: color-mix(in oklch, var(--color-base-100) 56%, var(--color-base-200));
  }

  .console-main {
    min-width: 0;
    min-height: 0;
    flex: 1 1 auto;
    overflow: auto;
    padding: 1.25rem;
  }
```

- [ ] **Step 3: Rebuild the generated CSS asset**

Run from `crates/web`:

```bash
npm run build:css
```

Expected: PASS. `crates/web/assets/styles.built.css` is updated.

- [ ] **Step 4: Run targeted tests and verify they pass**

Run from the repository root:

```bash
cargo test -p web console_layout_renders_real_sidebar_and_account_control
cargo test -p web static_styles_returns_css
```

Expected: PASS. Both targeted tests pass.

### Task 3: Verify, Inspect, and Commit

**Files:**
- Verify: `crates/web/src/views/layouts.rs`
- Verify: `crates/web/assets/styles.css`
- Verify: `crates/web/assets/styles.built.css`
- Verify: `crates/web/tests/route_smoke.rs`

- [ ] **Step 1: Run the full web crate test suite**

Run from the repository root:

```bash
cargo test -p web
```

Expected: PASS. All `web` crate tests pass.

- [ ] **Step 2: Check browser automation availability**

Run from the repository root:

```bash
bash -lc 'for b in chromium chromium-browser google-chrome; do if command -v "$b" >/dev/null 2>&1; then printf "%s\n" "$b"; exit 0; fi; done; printf "no browser automation available\n"; exit 0'
```

Expected: The command prints either an available browser binary name or `no browser automation available`. If no browser binary is available, note that browser-level layout verification was not available and rely on the CSS/SSR regression checks.

- [ ] **Step 3: Inspect the final diff**

Run from the repository root:

```bash
git diff -- crates/web/src/views/layouts.rs crates/web/tests/route_smoke.rs crates/web/assets/styles.css crates/web/assets/styles.built.css
```

Expected: The diff only contains the layout class addition, the authored CSS app-shell updates, regenerated CSS, and the new test assertions.

- [ ] **Step 4: Commit the implementation**

Run from the repository root:

```bash
git add crates/web/src/views/layouts.rs crates/web/tests/route_smoke.rs crates/web/assets/styles.css crates/web/assets/styles.built.css
git commit -m "fix(web): keep console shell in viewport"
```

Expected: A commit is created with the console shell fix and regression tests.

## Self-Review

- Spec coverage: Task 2 makes the header sticky, constrains the console frame to the viewport below the header, keeps the sidebar full-height, and makes the main pane the scroll container. Task 1 locks those requirements down with SSR and generated CSS assertions. Task 3 runs the full crate test suite and checks browser automation availability.
- Placeholder scan: The plan contains concrete file paths, code snippets, commands, expected outcomes, and a commit message.
- Type and name consistency: The plan uses the existing `.app-header`, `.console-frame`, `.console-sidebar`, and `.console-main` names, and introduces one markup class, `console-content`, only where the content column needs flex-column behavior.
