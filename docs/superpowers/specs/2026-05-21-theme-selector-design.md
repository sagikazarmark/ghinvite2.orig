# ghinvite Theme Selector: Design

**Status:** approved for implementation planning  
**Date:** 2026-05-21  
**Scope:** Add a light/dark theme selector to the existing Dioxus SSR frontend, defaulting to the current light `ghinvite` theme.

## 1. Product Goal

Users should be able to switch the app console between light and dark themes without changing account, workflow, or server state. The selector should feel like part of the existing app shell and should keep light as the default experience.

## 2. Theme Strategy

The product remains a precise, calm app console. Light is the SSR default because admins often use ghinvite during normal work hours on a laptop. Dark is an optional preference for users working in dimmer environments or who prefer lower-luminance tools.

Theme names:

- `ghinvite`: light theme, current default.
- `ghinvite-dark`: dark theme, restrained console variant.

Both themes should use DaisyUI theme customization and OKLCH tokens. The dark theme should keep the same controlled blue accent family, with dark neutral surfaces and readable tinted text. It should not use neon, pure black, or decorative contrast.

## 3. Component And Interaction

Add the selector to the shared `Nav` component so it appears on home, dashboard, and recipient layouts.

Control requirements:

- Use DaisyUI/Tailwind primitives, preferably a compact native `select` styled with `select select-bordered select-sm`.
- Provide an accessible label. The visual label may be screen-reader-only if space is tight.
- Include two options: `Light` and `Dark`.
- Keep the signed-in controls and sign-in CTA behavior unchanged.

## 4. Persistence And Fallback

Persistence should be browser-local, not server-side.

Behavior:

- SSR renders `data-theme="ghinvite"` by default on layout bodies.
- A small inline script reads `localStorage.getItem("ghinvite-theme")`.
- Accepted stored values are `ghinvite` and `ghinvite-dark` only.
- Invalid, missing, or inaccessible storage falls back to `ghinvite`.
- The script applies the selected theme to both `document.documentElement` and `document.body` using `data-theme`.
- On selector changes, the script updates the `data-theme` values and writes the preference to `localStorage`.
- If JavaScript is unavailable, the app remains usable in the light theme.

Flash-of-light risk is acceptable for this pass because avoiding it with cookies would add server preference plumbing that the product does not need yet.

## 5. Architecture

Keep the change local to presentation files:

- `crates/web/assets/styles.css`: add `ghinvite-dark` DaisyUI theme next to `ghinvite`.
- `crates/web/src/views/components.rs`: add the selector and inline synchronization script in or near `Nav`.
- `crates/web/src/views/layouts.rs`: keep SSR defaults as `data-theme="ghinvite"`; update tests for selector presence if useful.
- `crates/web/tests/route_smoke.rs`: extend CSS smoke coverage for the dark theme tokens after rebuilding CSS.

No route, session, storage, command, or domain changes are needed.

## 6. Testing Strategy

- Add a CSS smoke assertion that served CSS contains `ghinvite-dark` and still contains `ghinvite`.
- Add a render test that `Nav` includes the theme selector, `Light`, `Dark`, and the `ghinvite-theme` storage key.
- Keep or add a layout assertion that SSR defaults remain `data-theme="ghinvite"`.
- Run `cargo fmt`.
- Run focused tests for the selector and CSS smoke coverage.
- Run `npm run build:css` in `crates/web` after editing `styles.css`.
- Run `cargo test -p web` before completion.

## 7. Acceptance Criteria

- The app renders light by default without JavaScript.
- Users can choose Light or Dark from the shared navbar.
- The selected theme persists in browser storage under `ghinvite-theme`.
- Invalid or inaccessible storage falls back to light.
- Both themes are DaisyUI themes using OKLCH tokens.
- No generated CSS is committed unless it is tracked.
- `cargo test -p web` and `npm run build:css` pass.
