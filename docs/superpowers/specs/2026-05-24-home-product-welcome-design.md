# Home Product Welcome Design

## Context

The root page remains a signed-out product welcome page. Signed-in users continue into the dashboard flow instead of seeing a marketing page first.

`PRODUCT.md` defines ghinvite as a calm, precise product UI for GitHub account admins and request recipients. The welcome page should look like the beginning of the app, not a brochure site.

## Goals

- Add a hero section before the dashboard experience.
- Place a single primary sign-in action at the bottom center of the hero.
- Add a features section below the hero.
- Replace the header theme switcher with one icon button instead of two icon buttons.
- Polish the surface so it feels more like a modern webapp/native app while keeping the product register restrained.

## Non-Goals

- No new authenticated dashboard behavior.
- No new OAuth or installation flow behavior.
- No marketing-style oversized hero, decorative gradient text, glass panels, or generic SaaS card grid.

## Visual Direction

Physical scene: an account admin opens ghinvite during a normal workday on a laptop, expecting a trustworthy access-control tool before making repository permission decisions.

Color strategy: restrained product palette using existing OKLCH DaisyUI theme tokens. Accent color is reserved for the primary action, focus states, and selected state treatment.

The page should use native-feeling system typography, precise borders, subtle tinted surfaces, and compact spacing. The hero can feel elevated, but not promotional.

## Page Structure

### Header

The existing app header remains compact and consistent across public and dashboard pages.

The theme switcher becomes a single icon-only button:

- It reads the stored theme from `localStorage` using the existing `ghinvite-theme` key.
- It toggles between `ghinvite` and `ghinvite-dark`.
- It updates `data-theme` on `document.documentElement` and `document.body`.
- It exposes an accessible label for the next action, such as `Switch to dark theme` or `Switch to light theme`.
- It uses one visible icon at a time, not two side-by-side buttons.

### Hero

The hero replaces the current two-column intro.

Content:

- Eyebrow: `GitHub access operations`.
- Heading: concise, product-focused access control message.
- Body: one short paragraph explaining share links, review queues, and controlled repository invitations.
- Primary action: `Sign in with GitHub`, centered at the bottom of the hero for signed-out users.
- Signed-in fallback, if this component renders with a signed-in login: use `Install on another account` in the same centered primary action position.

Layout:

- Single-column hero with text centered enough to guide the page, but constrained to readable width.
- CTA sits visually below the copy with enough separation to read as the end of the hero.
- Use a subtle product panel or shell-like surface treatment rather than a decorative marketing background.

### Features

Below the hero, add a features section with three differentiated product capabilities:

- Controlled share links: admins define repository access, permission level, expiration, and usage limits.
- Request review: admins review GitHub identities and access consequences before invitations are sent.
- Operational history: access workflows leave an understandable record for follow-up and auditing.

The section should avoid identical icon-card repetition. It can use a compact stacked list, segmented rows, or a restrained three-column layout with clear text hierarchy and consistent affordances.

## Implementation Notes

- Update `crates/web/src/views/home.rs` for the signed-out welcome page markup.
- Update `crates/web/src/views/components.rs` for the single-button theme switcher and script behavior.
- Update `crates/web/assets/styles.css` for any reusable landing or theme-toggle styling.
- Rebuild `crates/web/assets/styles.built.css` with the existing Tailwind script if styles change.
- Keep body and layout theme names unchanged: `ghinvite` and `ghinvite-dark`.

## Testing

- Update component tests that currently expect two theme buttons.
- Add or update home rendering coverage for the hero, centered sign-in action, and features section text.
- Run relevant Rust tests for the web crate.
- Run the web CSS build if stylesheet classes or custom CSS change.

## Acceptance Criteria

- `/` presents a hero followed by a features section for signed-out users.
- The hero has one primary sign-in button at the bottom center.
- The header theme switcher renders one icon button, not a two-button group.
- Theme toggling still persists via `localStorage` and applies to the page immediately.
- The design remains restrained, app-like, responsive, and accessible.
