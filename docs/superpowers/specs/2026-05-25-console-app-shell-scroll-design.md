# Console App Shell Scroll Design

## Context

GitHub issue #17 reports that when a console page is scrollable, the bottom of the desktop sidebar is not visible until the document is scrolled. The reporter also noted that the header has the same issue.

The current console layout renders a compact `app-header` followed by a `console-frame`. The sidebar is part of the normal document flow, and long page content causes the whole document to scroll. That means the header scrolls away and the sidebar bottom can sit below the viewport.

## Goal

Console pages should behave like a viewport-height app shell:

- The global header remains visible at the top while console content scrolls.
- The desktop sidebar fills the viewport below the header.
- The sidebar account switcher remains reachable at the bottom of the sidebar.
- Long console page content scrolls inside the main content pane.
- Mobile console navigation keeps the current horizontal row behavior.

## Non-Goals

- Do not redesign console navigation or page content.
- Do not add client-side routing or hydration.
- Do not change public Home pages or Invitation Request Flow pages.
- Do not introduce JavaScript for scroll management.

## Design

Use the existing markup and fix the shell behavior in CSS.

Make `.app-header` sticky at the top of the viewport with an explicit height of `3rem`. Make `.console-frame` use `height: calc(100vh - 3rem)` instead of only a minimum height, so the console shell occupies the remaining viewport below the header. Prevent document-level scrolling inside the console shell by giving `.console-frame` `overflow: hidden`.

Keep `.console-sidebar` at the full height of the console frame. Its current flex column structure already lets the nav take available space and keeps `.sidebar-account-switcher` at the bottom via the nav's `flex-1`. Add `min-height: 0` where needed so flex children can shrink correctly.

Make `.console-main` the scrolling region with `overflow: auto` and `min-height: 0`. This keeps long pages usable without moving the header or hiding the bottom of the sidebar.

On mobile, the desktop sidebar remains hidden. The mobile nav stays inside the right-hand content column and scrolls with the main content as it does today.

## Testing

- Add or update render/CSS assertions for the shell classes that encode the sticky header, fixed console-frame height, sidebar height, and main-pane scrolling behavior.
- Rebuild Tailwind CSS so `assets/styles.built.css` includes the updated component CSS.
- Run `cargo test -p web`.
- If feasible in the local environment, run a browser-level layout check against a long console page to confirm the header remains visible and the sidebar bottom is within the viewport while main content scrolls.

## Issue Triage

Category: bug.

Recommended pre-implementation state: ready-for-agent. The issue is sufficiently specified for an agent because the affected shell, expected behavior, and verification path are clear.
