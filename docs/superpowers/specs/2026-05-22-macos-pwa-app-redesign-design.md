# ghinvite macOS PWA App Redesign: Design

**Status:** approved for implementation planning  
**Date:** 2026-05-22  
**Scope:** Frontend redesign of the authenticated ghinvite app shell and core admin screens so the product feels like an installed macOS PWA utility, not a website or generic DaisyUI console.

## 1. Product Goal

ghinvite should feel like a precise native app for GitHub access operations. Account admins should be able to switch account context, review requests, create links, and inspect link state without fighting oversized headers, loose table spacing, or website-style page structure.

The redesign keeps the existing Rust, Dioxus SSR, Tailwind, and DaisyUI stack. It changes the shell, visual density, theme controls, table treatment, and screen hierarchy. It does not add new product workflows beyond an account switcher affordance that can initially reflect the active account and link to installation when only one account is available in the current view data.

## 2. Scene And Direction

An account admin opens ghinvite as an installed PWA on a MacBook during normal work, switches between GitHub account contexts, checks pending repository access requests, and creates an invitation link while expecting the interface to behave like a compact utility.

This scene supports a restrained product theme with light mode as the default and dark mode available through a compact icon toggle. The interface should borrow from macOS utility apps through density, structure, muted surfaces, and native-feeling controls, not through fake window chrome or decorative nostalgia.

Color strategy: Restrained. Use OKLCH tinted neutrals, one controlled blue accent for primary actions and current selection, and semantic colors for request outcomes and warnings. Avoid pure black, pure white, decorative gradients, glass effects, and saturated inactive states.

## 3. App Shell

The shell is the center of the redesign.

Required structure:

- Use a compact global header around 44 to 52 px tall.
- Put app identity on the header left: a small glyph or mark plus `ghinvite`.
- Put signed-in controls on the header right: icon-only sun and moon theme switcher, signed-in login as quiet text where space allows, and `Sign out` as a quiet button or link.
- Replace the current pseudo-sidebar with a real desktop sidebar, around 220 to 248 px wide, occupying the full app height below the header.
- Put navigation in the sidebar: Overview, New link, Requests, Audit log, Settings.
- Use compact nav rows with native-style selected state, subtle hover state, and optional inline icons if they can be done without a new icon dependency.
- Put the account switcher at the sidebar bottom, not the header. It should show the active account and provide an `Install another account` action.
- If the route layer exposes multiple accounts later, the same bottom control can become a real switcher list. This redesign should not require a backend or data model change.
- On mobile, keep the header compact and replace the desktop sidebar with a responsive navigation treatment that works without client-side routing or hydration. A compact horizontal nav row is acceptable for this pass.

The header should no longer feel like marketing navigation. The sidebar should no longer be a floating card. Together they should read as one installed app frame.

## 4. Theme Switcher

Replace the current select-based theme switcher with icon controls.

Requirements:

- Use sun and moon icons or icon-like inline markup that does not require a new dependency.
- Keep the control keyboard accessible and labeled for screen readers.
- Preserve the existing localStorage key and light or dark theme persistence behavior unless there is a concrete reason to change it.
- Keep the theme sync SSR-safe and small.
- Tune both `ghinvite` and `ghinvite-dark` DaisyUI themes in OKLCH.

The dark theme should feel like a native app dark mode, not a neon developer dashboard. It should use tinted charcoal surfaces, readable contrast, and the same restrained accent behavior as light mode.

## 5. Main Content Rhythm

The current content is too spacious and card-heavy. Replace hero-like page treatment with compact app rhythm.

Rules:

- Page headers should be compact title rows with optional subtitle and local actions.
- Body content should align to the app frame rather than sit inside large marketing containers.
- Use panels only where they clarify grouping. Avoid repeated equal card grids.
- Tighten vertical spacing and table padding.
- Keep body copy direct and short.
- Keep status, permission, repository, expiration, and approval details visible before consequential actions.

## 6. Screen Treatments

### Dashboard Overview

The overview should be a compact operational start page.

- Replace metric-like cards with concise summary rows or compact panels.
- Show pending requests and active links as navigable work areas, not hero metrics.
- Present recent invitation links in a dense table or list with status, slug, uses, and direct navigation.
- Empty state should teach the first admin action with one clear primary action.

### New Invitation Link

The create form should feel like a macOS settings sheet inside the app frame.

- Use grouped form sections for access configuration, request handling, and repository scope.
- Keep labels concise and helper text close to controls.
- Keep elevated-permission warning visible, but restrained.
- Repository selection should use dense rows with checkboxes, hover state, and enough spacing for scanning.
- Primary action should sit at the end of the form, aligned with the form content.

### Link Detail

The link detail page should feel like a property inspector.

- Show the share URL in a compact readonly field with recipient preview action.
- Present permission, usage, expiration, approval mode, repositories, and internal note in structured property rows.
- Avoid a grid of identical cards.
- Keep destructive link stopping visually separate and clear.
- Use a confirmation modal only for the destructive stop action, preserving inline consequence copy near the trigger.

### Requests Queue

The requests page should be a decision table.

- Use a denser table-like surface instead of card rows.
- Align requester, invitation link, permission, repositories, requested time, justification, and actions.
- Tighten cell padding.
- Preserve responsive behavior by stacking each request into readable blocks on small screens.
- Keep approve and decline actions visually consistent and close to the row they affect.
- Do not rely on color alone for outcome or status meaning.

### Settings

Settings should read as account and installation status.

- Use compact grouped panels or property rows.
- Keep active account identity and installation scope clear.
- Keep future-editing copy secondary.

### Recipient Flow

Recipient pages are not the main focus of this redesign, but they should remain visually compatible.

- Keep recipient pages focused and centered.
- Tune theme, controls, alerts, and typography to match the new app vocabulary.
- Do not introduce the full admin sidebar into public recipient flows.

## 7. Tailwind And DaisyUI Strategy

DaisyUI remains the primitive layer. Tailwind handles precision.

Use DaisyUI for:

- Buttons, badges, alerts, inputs, selects, textareas, checkboxes, tables, and modals.
- Theme customization through OKLCH tokens.
- Accessible default control behavior where it fits.

Use Tailwind and small custom CSS for:

- App shell dimensions.
- Sidebar, toolbar, and native panel surfaces.
- Compact table density.
- Theme switcher icon states.
- Reusable utility classes only when repeated class strings become hard to maintain.

Do not create a new design system or add a frontend framework in this pass.

## 8. Data Flow And Architecture

The redesign stays server-rendered.

- Dioxus SSR remains the rendering model.
- Existing view props should be reused wherever possible.
- Small prop additions are acceptable if needed for active navigation or account-switcher display.
- No client-side routing or hydration is required.
- No database, domain model, or command workflow changes are required.
- The account switcher can initially represent the active account and installation action if only one account is available.

## 9. Accessibility And Responsive Requirements

- Keep all actions as native links or buttons.
- Keep visible labels for form controls.
- Give icon-only theme controls accessible labels.
- Preserve visible focus treatment.
- Maintain readable contrast in both themes.
- Ensure the sidebar navigation has a usable mobile alternative.
- Ensure dense tables remain scannable and usable on narrow screens.
- Keep motion minimal, 150 to 250 ms, tied only to state changes.

## 10. Out Of Scope

- New frontend framework.
- Client-side routing or hydration.
- Backend account-list work for a full multi-account switcher.
- New search, filtering, pagination, or audit-log implementation.
- Full design-system extraction.
- Decorative fake macOS traffic lights or window chrome.
- Broad copy rewrite outside affected app surfaces.

## 11. Testing Strategy

- Run `cargo fmt` after Rust view changes.
- Run `cargo test -p web` for view and route coverage.
- Run `npm run build:css` in `crates/web` after CSS or theme changes.
- Update tests that assert the old select-based theme selector, oversized header, or sidebar markup.
- Add focused render assertions for icon theme controls, sidebar account control, and key requests-table content where practical.

## 12. Acceptance Criteria

- The app reads as a compact installed macOS PWA utility rather than a website.
- Header height is reduced and contains sign-out plus icon theme switching on the right.
- The desktop sidebar is a real persistent navigation region.
- The account switcher or active account control sits at the sidebar bottom.
- Tables and request rows are denser, aligned, and easier to scan.
- Cards are reduced where property rows, grouped panels, or tables are more appropriate.
- DaisyUI and Tailwind remain the primary implementation tools.
- Themes use OKLCH and avoid default DaisyUI visual identity.
- Mobile navigation and core workflows remain usable.
- `cargo fmt`, `cargo test -p web`, and `npm run build:css` pass.
