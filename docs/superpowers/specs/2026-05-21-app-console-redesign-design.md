# ghinvite App Console Redesign: Design

**Status:** approved for implementation planning  
**Date:** 2026-05-21  
**Scope:** Product UI redesign across the existing Dioxus SSR frontend so ghinvite feels like a cohesive app console rather than a website.

## 1. Product Goal

ghinvite should feel like a polished Stripe-style console for GitHub collaborator access operations. The app should remain calm, precise, and familiar, but the current presentation should stop reading as default DaisyUI scaffolding or a marketing page wrapped around admin workflows.

This pass is a visual and structural polish pass. It should keep the existing Rust, Dioxus SSR, Tailwind, and DaisyUI stack, and it should reuse DaisyUI and Tailwind customization wherever possible.

## 2. Design Direction

The interface should use a restrained product theme. A GitHub admin is usually reviewing access from a work laptop during normal team operations, between other tasks, and needs quick confidence without decorative distraction. That scene supports a light, console-like theme rather than a dark control-room theme.

Visual direction:

- Use warm tinted neutral surfaces, crisp text, and one controlled blue accent for primary actions, active navigation, and focus states.
- Use OKLCH color values through DaisyUI theme customization and CSS variables.
- Keep system UI typography, but make the type scale, line height, and metadata treatment more deliberate.
- Use spacing rhythm to make pages feel structured, not centered and brochure-like.
- Make navigation, page headers, and panel hierarchy carry the app feel.

Color strategy: Restrained. Accent color should appear in primary actions, active navigation, focus rings, and key status affordances, not as decoration.

## 3. Component Strategy

DaisyUI and Tailwind are the base layer for this redesign. Custom CSS is allowed, but it is a last resort when DaisyUI theme customization or Tailwind utility composition cannot express the required result cleanly.

Preferred primitives:

- `navbar` for global chrome.
- `menu` for dashboard navigation.
- `card`, `alert`, `badge`, `btn`, `input`, `select`, `textarea`, `table`, and `modal` for standard product controls.
- Tailwind utilities for layout, responsive behavior, alignment, density, and state refinement.

Custom CSS should be limited to:

- DaisyUI theme values and OKLCH tokens.
- Base body and app-shell background treatment.
- Small reusable app-level utility classes only when repeated Tailwind strings become hard to maintain.

Do not create a separate component system in this pass.

## 4. App Shell

The app shell should make admin pages feel like one console.

Required changes:

- Refine `Nav` so product identity, sign-in state, and account actions feel like toolbar controls rather than marketing navigation.
- Refine `DashboardLayout` with a stronger shell: persistent top nav, desktop side navigation, mobile tab row, and content width that supports admin work.
- Keep sidebar navigation familiar and predictable. Current sections remain Overview, New link, Requests, Audit coming soon, and Settings.
- Give active or current navigation states a clear visual treatment where route context is available or easy to derive.
- Replace generic page spacing with compact headers and consistent content lanes.
- Keep `InvitationLayout` centered and focused, but align its theme, controls, and status vocabulary with the admin app.

Out of scope: route-aware nav architecture if it would require broad route plumbing. A simple current-section prop is acceptable if needed.

## 5. Screen Treatments

### Home

The home page should not feel like the primary product surface. It can remain simple, but it should become a concise entry screen rather than a large marketing hero.

- Reduce hero scale and centered website treatment.
- Emphasize sign-in and installation entry points.
- Keep copy direct and operational.

### Dashboard Overview

The overview should feel like a console landing page.

- Use compact status summaries with direct actions.
- Avoid hero metrics and decorative stat cards.
- Present recent links as operational rows or panels, not marketing cards.
- Improve empty states so they teach the first admin action.

### New Share Link

The new-link form should read as a structured settings form.

- Group permission, approval, limits, notes, and repositories into clear sections.
- Use DaisyUI form controls with consistent labels, helper text, and vertical rhythm.
- Keep elevated-permission warnings visible without over-coloring the page.
- Make repository selection feel like an app control, using DaisyUI and Tailwind customization rather than a custom widget.

### Link Detail

The link detail page should be an operational hub.

- Keep the share URL prominent but compact.
- Present permission, usage, expiration, approval mode, repositories, and internal note with consistent metadata treatment.
- Separate destructive actions from informational panels.
- Add a confirmation modal for stopping a link if it can be done with DaisyUI's CSS-friendly modal pattern and SSR-friendly markup.
- Keep inline consequence copy near the action even when a modal is used.

### Requests Queue

The queue should feel like an admin decision list.

- Use a denser list or table-like layout rather than a stack of generic cards.
- Show requester, share link, permission, repositories, justification, and created time before actions.
- Keep approve and decline actions visually consistent.
- Consider a confirmation modal for decline only if the final implementation shows the consequence is easy to miss. Do not add modals to every action by default.
- Preserve responsive stacking on small screens.

### Settings

Settings should feel like account status, not a placeholder.

- Use subdued panels for account identity and repository access scope.
- Keep future-editing copy secondary.
- Use the same metadata and panel treatment as link detail.

### Recipient Flow

Recipient pages should feel like a focused app task.

- Keep the centered task panel, but use the same theme tokens, controls, badges, alerts, and button language as admin pages.
- Make repository and permission information easy to scan.
- Keep reassurance copy direct: GitHub sign-in confirms identity and the request is visible to admins.
- Status pages should use consistent alerts and clear next actions.

## 6. Data Flow And Architecture

The redesign should stay server-rendered.

- Dioxus SSR remains the rendering model.
- Existing view props should be reused wherever possible.
- Tiny prop additions are acceptable for app-shell state, such as active navigation, if they keep markup clear.
- No client-side hydration is required.
- No new frontend framework, bundler, or component extraction is required.
- No storage or domain model changes are expected.

CSS flow:

- Define DaisyUI theme customization in `crates/web/assets/styles.css`.
- Rebuild `crates/web/assets/styles.built.css` with the existing `npm run build:css` workflow.
- Do not commit generated CSS unless it is already tracked and modified by the build.

## 7. Destructive Actions And Error States

Destructive confirmations are allowed when they improve safety.

Rules:

- Use DaisyUI modal patterns before writing custom modal CSS.
- Keep modals limited to consequential destructive actions, especially stopping a share link.
- Keep consequence copy visible inline near the triggering action.
- Avoid JavaScript-only confirmations unless the existing stack already supports them without hydration.
- Preserve native form submission for POST actions.

Error, warning, success, and info states should use a consistent DaisyUI alert vocabulary with theme-tuned colors. Important state text must not rely on color alone.

## 8. Accessibility And Responsive Requirements

- Keep all primary actions as native links or buttons.
- Preserve visible labels for form controls.
- Use focus-visible treatment through DaisyUI theme or Tailwind classes.
- Keep color contrast readable on all tuned surfaces.
- Ensure dashboard navigation remains reachable on mobile.
- Ensure queue rows and forms stack cleanly on small screens.
- Avoid decorative motion. Any transitions should be 150 to 250 ms and tied to state.

## 9. Out Of Scope

- New frontend framework or client-side app architecture.
- Full design-system extraction.
- Clipboard JavaScript.
- New product workflows, search, filtering, pagination, or audit-log implementation.
- Broad route or storage refactors.
- Custom components when DaisyUI customization is sufficient.

## 10. Testing Strategy

- Run `cargo fmt` after Rust view changes.
- Run `cargo test -p web` for view and route coverage.
- Run `npm run build:css` in `crates/web` after CSS/theme changes.
- Add or adjust render and smoke tests for critical copy, destructive confirmation affordances, and important layout cues where practical.
- Inspect generated CSS status before committing any build output.

## 11. Acceptance Criteria

- Admin pages feel like one app console rather than separate website-like pages.
- DaisyUI and Tailwind remain the primary implementation tools.
- Theme customization uses OKLCH values and avoids default DaisyUI visual identity.
- Navigation, page headers, panels, forms, tables or lists, alerts, badges, and buttons share a consistent visual vocabulary.
- Destructive actions have clear inline consequence copy, with modal confirmation where justified and SSR-friendly.
- Recipient pages share the same product vocabulary without becoming marketing pages.
- Mobile navigation and key workflows remain usable.
- `cargo fmt`, `cargo test -p web`, and `npm run build:css` pass.
