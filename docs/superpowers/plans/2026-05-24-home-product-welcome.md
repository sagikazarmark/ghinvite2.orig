# Home Product Welcome Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a restrained signed-out product welcome page with a hero, features section, centered sign-in action, and single-icon theme toggle.

**Architecture:** Keep the behavior inside the existing Dioxus SSR views and app shell. The home page markup changes in `views/home.rs`, the theme toggle stays in the shared `Nav` component, and CSS polish remains in the existing Tailwind/DaisyUI stylesheet pipeline.

**Tech Stack:** Rust 2024, Axum, Dioxus 0.7 SSR, Tailwind CSS v4, DaisyUI v5, npm CSS build, `cargo test -p web`.

---

## File Structure

- Modify `crates/web/src/views/home.rs`: replace the current two-column home intro with a product welcome hero and features section.
- Modify `crates/web/src/views/components.rs`: replace the two-button theme group with a single icon button and update the theme script.
- Modify `crates/web/assets/styles.css`: update theme-toggle styling and add reusable home hero/features styling.
- Modify `crates/web/assets/styles.built.css`: rebuild generated CSS after stylesheet and utility-class changes.
- Modify `crates/web/tests/route_smoke.rs`: assert the home page and built CSS include the new public surface.
- Modify `crates/web/src/views/components.rs` tests: assert the nav renders a single theme toggle button.

## Task 1: Home Page Hero And Features

**Files:**
- Modify: `crates/web/tests/route_smoke.rs:146-169`
- Modify: `crates/web/src/views/home.rs:11-53`

- [ ] **Step 1: Write the failing home route test**

Replace `home_returns_html` in `crates/web/tests/route_smoke.rs` with this function:

```rust
#[tokio::test]
async fn home_returns_html() {
    let app = build_test_app().await;
    let resp = app
        .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let ct = resp
        .headers()
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap();
    assert!(ct.contains("text/html"));
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8_lossy(&body);
    assert!(text.contains("ghinvite"));
    assert!(text.contains("home-hero"));
    assert!(text.contains("home-hero-action"));
    assert!(text.contains("Sign in with GitHub"));
    assert!(text.contains("Controlled GitHub invitations without access guesswork"));
    assert!(text.contains("home-features"));
    assert!(text.contains("Controlled invitation links"));
    assert!(text.contains("Review requests before invitations"));
    assert!(text.contains("Operational history"));
    assert!(!text.contains(concat!("class=\"he", "ro")));
}
```

- [ ] **Step 2: Run the home route test and verify it fails**

Run:

```bash
cargo test -p web home_returns_html
```

Expected: FAIL because the current home page does not render `home-hero`, `home-features`, or the new heading.

- [ ] **Step 3: Implement the home hero and features markup**

Replace the `HomePage` function in `crates/web/src/views/home.rs` with this function:

```rust
#[component]
pub fn HomePage(props: HomePageProps) -> Element {
    rsx! {
        HomeLayout {
            signed_in_login: props.signed_in_login.clone(),
            title: "ghinvite - controlled GitHub access requests".to_string(),
            account_login: None,
            active_nav: None,
            flash: None,
            children: rsx! {
                div { class: "mx-auto flex w-full max-w-6xl flex-col gap-10 py-8 sm:py-12",
                    section { class: "home-hero px-5 py-12 sm:px-8 sm:py-16",
                        div { class: "mx-auto flex max-w-3xl flex-col items-center text-center",
                            p { class: "text-xs font-semibold uppercase tracking-[0.18em] text-primary", "GitHub access operations" }
                            h1 { class: "mt-4 text-3xl font-semibold tracking-tight text-base-content sm:text-5xl", "Controlled GitHub invitations without access guesswork" }
                            p { class: "mt-5 max-w-2xl text-base leading-7 text-base-content/68 sm:text-lg",
                                "Create invitation links, review incoming requests, and send repository invitations only after the access consequences are clear."
                            }
                        }
                        div { class: "home-hero-action mt-9 flex justify-center",
                            {match props.signed_in_login.as_deref() {
                                Some(_) => rsx! {
                                    a { class: "btn btn-primary h-10 min-h-0 px-5 shadow-sm", href: "/install", "Install on another account" }
                                },
                                None => rsx! {
                                    a { class: "btn btn-primary h-10 min-h-0 px-5 shadow-sm", href: "/login", "Sign in with GitHub" }
                                }
                            }}
                        }
                    }

                    section { class: "home-features",
                        div { class: "max-w-2xl",
                            p { class: "text-xs font-semibold uppercase tracking-[0.18em] text-base-content/45", "What the app keeps under control" }
                            h2 { class: "mt-2 text-xl font-semibold tracking-tight text-base-content sm:text-2xl", "A focused workflow for collaborator access" }
                        }
                        div { class: "mt-5 grid gap-3 lg:grid-cols-3",
                            article { class: "home-feature-item",
                                p { class: "home-feature-kicker", "01" }
                                h3 { class: "mt-3 text-base font-semibold text-base-content", "Controlled invitation links" }
                                p { class: "mt-2 text-sm leading-6 text-base-content/66",
                                    "Define repositories, permission level, expiration, and usage limits before a request reaches the queue."
                                }
                            }
                            article { class: "home-feature-item",
                                p { class: "home-feature-kicker", "02" }
                                h3 { class: "mt-3 text-base font-semibold text-base-content", "Review requests before invitations" }
                                p { class: "mt-2 text-sm leading-6 text-base-content/66",
                                    "Confirm GitHub identity and requested access before ghinvite sends repository collaborator invitations."
                                }
                            }
                            article { class: "home-feature-item",
                                p { class: "home-feature-kicker", "03" }
                                h3 { class: "mt-3 text-base font-semibold text-base-content", "Operational history" }
                                p { class: "mt-2 text-sm leading-6 text-base-content/66",
                                    "Keep request outcomes understandable for follow-up, support, and access reviews."
                                }
                            }
                        }
                    }
                }
            },
        }
    }
}
```

- [ ] **Step 4: Run the home route test and verify it passes**

Run:

```bash
cargo test -p web home_returns_html
```

Expected: PASS.

- [ ] **Step 5: Commit the home markup and route test**

Run:

```bash
git add crates/web/src/views/home.rs crates/web/tests/route_smoke.rs
git commit -m "feat(web): add product welcome home page"
```

Expected: commit succeeds with only the home view and route smoke test staged.

## Task 2: Single Icon Theme Toggle

**Files:**
- Modify: `crates/web/src/views/components.rs:11-109`

- [ ] **Step 1: Write the failing nav component test**

Replace `nav_renders_compact_header_theme_icons_and_signed_in_controls` in `crates/web/src/views/components.rs` with this test:

```rust
#[test]
fn nav_renders_single_theme_toggle_and_signed_in_controls() {
    let html = crate::views::render::render(|| {
        rsx! { Nav { signed_in_login: Some("admin".to_string()) } }
    });

    assert!(html.contains("app-header"));
    assert!(html.contains("theme-toggle"));
    assert!(html.contains("data-theme-toggle=\"true\""));
    assert_eq!(html.matches("data-theme-value=").count(), 1);
    assert!(html.contains("data-theme-value=\"ghinvite-dark\""));
    assert!(html.contains("aria-label=\"Switch to dark theme\""));
    assert!(html.contains("data-current-theme=\"ghinvite\""));
    assert_eq!(html.matches("data-theme-icon=").count(), 1);
    assert!(html.contains("data-theme-icon=\"true\""));
    assert!(html.contains("data-theme-icon-path=\"true\""));
    assert!(html.contains("data-current-icon=\"moon\""));
    assert!(!html.contains("aria-label=\"Use light theme\""));
    assert!(!html.contains("aria-label=\"Use dark theme\""));
    assert!(!html.contains("role=\"group\""));
    assert!(html.contains("Sign out"));
    assert!(html.contains("@admin"));
    assert!(html.contains("window.localStorage.getItem(key)"));
    assert!(html.contains("window.localStorage.setItem(key, next)"));
    assert!(!html.contains("<select"));
    assert!(!html.contains("Install on another account"));
}
```

- [ ] **Step 2: Run the nav test and verify it fails**

Run:

```bash
cargo test -p web nav_renders_single_theme_toggle_and_signed_in_controls
```

Expected: FAIL because the current nav renders two `data-theme-value` buttons and the old `Use light theme` / `Use dark theme` labels.

- [ ] **Step 3: Replace the theme script and nav markup**

In `crates/web/src/views/components.rs`, replace `THEME_SYNC_SCRIPT` and `Nav` with this code:

```rust
const THEME_SYNC_SCRIPT: &str = r#"
(function () {
  var key = 'ghinvite-theme';
  var light = 'ghinvite';
  var dark = 'ghinvite-dark';

  function valid(value) {
    return value === light || value === dark;
  }

  function toggleLabel(theme) {
    return theme === dark ? 'Switch to light theme' : 'Switch to dark theme';
  }

  function nextTheme(theme) {
    return theme === dark ? light : dark;
  }

  var moonPath = 'M20.25 14.15A7.5 7.5 0 0 1 9.85 3.75a8.25 8.25 0 1 0 10.4 10.4Z';
  var sunPath = 'M12 4.5V3m0 18v-1.5M4.5 12H3m18 0h-1.5M6.34 6.34 5.28 5.28m13.44 13.44-1.06-1.06m0-11.32 1.06-1.06M5.28 18.72l1.06-1.06M16.5 12a4.5 4.5 0 1 1-9 0 4.5 4.5 0 0 1 9 0Z';

  function syncToggle(button, theme) {
    var label = toggleLabel(theme);
    var darkActive = theme === dark;
    button.setAttribute('aria-label', label);
    button.setAttribute('title', label);
    button.setAttribute('aria-pressed', darkActive ? 'true' : 'false');
    button.setAttribute('data-current-theme', theme);
    button.setAttribute('data-theme-value', nextTheme(theme));

    var icon = button.querySelector('[data-theme-icon]');
    if (icon) {
      icon.setAttribute('data-current-icon', darkActive ? 'sun' : 'moon');
    }
    var path = button.querySelector('[data-theme-icon-path]');
    if (path) {
      path.setAttribute('d', darkActive ? sunPath : moonPath);
    }
  }

  function apply(value) {
    var theme = valid(value) ? value : light;
    document.documentElement.setAttribute('data-theme', theme);
    if (document.body) {
      document.body.setAttribute('data-theme', theme);
    }
    var toggles = document.querySelectorAll('[data-theme-toggle]');
    for (var i = 0; i < toggles.length; i += 1) {
      syncToggle(toggles[i], theme);
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
    var button = event.target.closest('[data-theme-toggle]');
    if (!button) {
      return;
    }
    var next = valid(button.getAttribute('data-theme-value')) ? button.getAttribute('data-theme-value') : nextTheme(stored);
    stored = next;
    apply(next);
    try {
      window.localStorage.setItem(key, next);
    } catch (_) {}
  });
})();
"#;

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
                button {
                    r#type: "button",
                    class: "theme-toggle btn btn-ghost btn-sm h-8 min-h-0",
                    aria_label: "Switch to dark theme",
                    aria_pressed: "false",
                    title: "Switch to dark theme",
                    "data-theme-toggle": "true",
                    "data-theme-value": "ghinvite-dark",
                    "data-current-theme": "ghinvite",
                    span { class: "theme-icon", "data-theme-icon": "true", "data-current-icon": "moon", "aria-hidden": "true",
                        svg { xmlns: "http://www.w3.org/2000/svg", view_box: "0 0 24 24", fill: "none", stroke: "currentColor", stroke_width: "1.8", class: "size-4",
                            path { "data-theme-icon-path": "true", stroke_linecap: "round", stroke_linejoin: "round", d: "M20.25 14.15A7.5 7.5 0 0 1 9.85 3.75a8.25 8.25 0 1 0 10.4 10.4Z" }
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

- [ ] **Step 4: Run the nav test and verify it passes**

Run:

```bash
cargo test -p web nav_renders_single_theme_toggle_and_signed_in_controls
```

Expected: PASS.

- [ ] **Step 5: Commit the single-toggle behavior**

Run:

```bash
git add crates/web/src/views/components.rs
git commit -m "feat(web): use single icon theme toggle"
```

Expected: commit succeeds with only `components.rs` staged.

## Task 3: CSS Polish And Built Asset

**Files:**
- Modify: `crates/web/tests/route_smoke.rs:112-144`
- Modify: `crates/web/assets/styles.css:110-212`
- Modify: `crates/web/assets/styles.built.css`

- [ ] **Step 1: Write the failing static CSS test**

Replace `static_styles_returns_css` in `crates/web/tests/route_smoke.rs` with this function:

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
    assert!(text.contains("ghinvite-dark"));
    assert!(text.contains("color-scheme:dark"));
    assert!(text.contains(".app-header"));
    assert!(text.contains(".theme-toggle"));
    assert!(text.contains(".home-hero"));
    assert!(text.contains(".home-feature-item"));
    assert!(text.contains(".home-feature-kicker"));
    assert!(text.contains(".dashboard-sidebar"));
    assert!(text.contains(".mac-panel"));
    assert!(text.contains(".compact-table"));
}
```

- [ ] **Step 2: Run the static CSS test and verify it fails**

Run:

```bash
cargo test -p web static_styles_returns_css
```

Expected: FAIL because the built CSS does not include `.home-hero`, `.home-feature-item`, or `.home-feature-kicker` yet.

- [ ] **Step 3: Update source CSS for the single toggle and home surface**

In `crates/web/assets/styles.css`, replace the existing `.theme-toggle` block and `.theme-toggle .btn` rules with this CSS inside `@layer components`:

```css
  .theme-toggle {
    width: 2rem;
    min-height: 2rem;
    height: 2rem;
    padding: 0;
    border: 1px solid color-mix(in oklch, var(--color-base-content) 12%, transparent);
    border-radius: 0.65rem;
    background: color-mix(in oklch, var(--color-base-200) 72%, var(--color-base-100));
    color: color-mix(in oklch, var(--color-base-content) 76%, transparent);
    box-shadow: 0 1px 2px color-mix(in oklch, var(--color-base-content) 7%, transparent);
    transition:
      background-color 180ms cubic-bezier(0.22, 1, 0.36, 1),
      border-color 180ms cubic-bezier(0.22, 1, 0.36, 1),
      color 180ms cubic-bezier(0.22, 1, 0.36, 1),
      box-shadow 180ms cubic-bezier(0.22, 1, 0.36, 1);
  }

  .theme-toggle:hover,
  .theme-toggle:focus-visible {
    border-color: color-mix(in oklch, var(--color-primary) 35%, var(--color-base-content) 10%);
    background: color-mix(in oklch, var(--color-primary) 10%, var(--color-base-100));
    color: var(--color-base-content);
    box-shadow: 0 2px 6px color-mix(in oklch, var(--color-base-content) 10%, transparent);
  }

  .theme-toggle[data-current-theme="ghinvite-dark"] {
    background: color-mix(in oklch, var(--color-base-100) 86%, var(--color-primary) 8%);
  }

  .theme-icon {
    display: grid;
    place-items: center;
  }
```

Then append this CSS before the closing brace of `@layer components`:

```css
  .home-hero {
    position: relative;
    overflow: hidden;
    border: 1px solid color-mix(in oklch, var(--color-base-content) 12%, transparent);
    border-radius: 1.25rem;
    background:
      radial-gradient(circle at 50% 0%, color-mix(in oklch, var(--color-primary) 13%, transparent), transparent 38%),
      linear-gradient(180deg, color-mix(in oklch, var(--color-base-100) 98%, var(--color-primary) 2%), var(--color-base-100));
    box-shadow:
      0 18px 45px color-mix(in oklch, var(--color-base-content) 8%, transparent),
      inset 0 1px 0 color-mix(in oklch, var(--color-base-content) 4%, var(--color-base-100));
  }

  .home-hero > * {
    position: relative;
  }

  .home-hero::after {
    content: "";
    position: absolute;
    inset: auto 12% 0;
    height: 1px;
    background: linear-gradient(90deg, transparent, color-mix(in oklch, var(--color-primary) 40%, transparent), transparent);
  }

  .home-features {
    border-radius: 1rem;
  }

  .home-feature-item {
    min-height: 12rem;
    border: 1px solid color-mix(in oklch, var(--color-base-content) 10%, transparent);
    border-radius: var(--radius-box);
    background: color-mix(in oklch, var(--color-base-100) 82%, var(--color-base-200));
    padding: 1rem;
    box-shadow: 0 1px 2px color-mix(in oklch, var(--color-base-content) 5%, transparent);
  }

  .home-feature-kicker {
    display: inline-grid;
    place-items: center;
    width: 2rem;
    height: 1.5rem;
    border-radius: 999px;
    background: color-mix(in oklch, var(--color-primary) 12%, var(--color-base-100));
    color: var(--color-primary);
    font-size: 0.72rem;
    font-weight: 700;
    letter-spacing: 0.04em;
  }

  @media (min-width: 1024px) {
    .home-feature-item:nth-child(2) {
      margin-top: 1rem;
    }

    .home-feature-item:nth-child(3) {
      margin-top: 2rem;
    }
  }
```

- [ ] **Step 4: Rebuild the generated CSS**

Run from `crates/web`:

```bash
npm run build:css
```

Expected: command exits 0 and updates `crates/web/assets/styles.built.css`.

- [ ] **Step 5: Run the static CSS test and verify it passes**

Run:

```bash
cargo test -p web static_styles_returns_css
```

Expected: PASS.

- [ ] **Step 6: Commit CSS polish and built asset**

Run:

```bash
git add crates/web/assets/styles.css crates/web/assets/styles.built.css crates/web/tests/route_smoke.rs
git commit -m "style(web): polish home welcome surface"
```

Expected: commit succeeds with source CSS, generated CSS, and the static CSS test staged.

## Task 4: Final Verification

**Files:**
- Verify: `crates/web/src/views/home.rs`
- Verify: `crates/web/src/views/components.rs`
- Verify: `crates/web/assets/styles.css`
- Verify: `crates/web/assets/styles.built.css`
- Verify: `crates/web/tests/route_smoke.rs`

- [ ] **Step 1: Format Rust code**

Run:

```bash
cargo fmt --all
```

Expected: command exits 0.

- [ ] **Step 2: Run the web test suite**

Run:

```bash
cargo test -p web
```

Expected: PASS for all `web` crate tests.

- [ ] **Step 3: Re-run the CSS build after formatting and test edits**

Run from `crates/web`:

```bash
npm run build:css
```

Expected: command exits 0. If `crates/web/assets/styles.built.css` changes, include it in the final commit.

- [ ] **Step 4: Inspect the final diff**

Run:

```bash
git status --short
git diff
```

Expected: only files from this plan are modified, and the diff shows the approved home welcome, single toggle, CSS polish, and tests.

- [ ] **Step 5: Commit final formatting or generated updates if needed**

If Step 1 or Step 3 changed files after the task commits, run:

```bash
git add crates/web/src/views/home.rs crates/web/src/views/components.rs crates/web/assets/styles.css crates/web/assets/styles.built.css crates/web/tests/route_smoke.rs
git commit -m "chore(web): verify home welcome polish"
```

Expected: commit succeeds if there are final formatting or generated CSS changes. If `git status --short` is empty, skip this commit.

- [ ] **Step 6: Confirm clean plan scope**

Run:

```bash
git status --short
```

Expected: no unstaged or untracked files from this plan remain.

## Self-Review

- Spec coverage: Task 1 covers the hero, bottom-centered sign-in action, and features section. Task 2 covers the single-icon theme switcher and persistence behavior. Task 3 covers restrained webapp/native polish and generated CSS. Task 4 covers verification.
- Placeholder scan: The plan uses exact paths, commands, expected results, and code blocks for each code-changing step.
- Type consistency: The plan keeps existing `HomePageProps`, `NavProps`, theme names `ghinvite` and `ghinvite-dark`, storage key `ghinvite-theme`, and route paths `/login` and `/install` unchanged.
