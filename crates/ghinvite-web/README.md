# crates/ghinvite-web

axum-based web server for ghinvite. Three Dioxus 0.7 SSR layouts (Home /
Dashboard / Invitation), OAuth login + install flow consuming `crates/ghinvite-github`,
Restate ingress client for state-changing actions.

This crate is **library + native binary in Plan 4**. Plan 5 fills in dashboard
routes, Plan 6 the recipient flow + webhook receiver, Plan 7 the Workers
`#[event(fetch)]` entry + D1 storage + secrets management.

## Local dev

### One-time setup

```bash
cd crates/ghinvite-web
npm install
```

`devenv.nix` provisions Node 20 — if you use `direnv`/`devenv shell`, npm is
already on `$PATH`. Otherwise install Node 20+ yourself.

### Build the CSS

```bash
cd crates/ghinvite-web
npm run build:css
```

This produces `crates/ghinvite-web/assets/styles.built.css`, which is `include_str!`'d
into the binary at compile time. Re-run after editing `assets/styles.css`,
adding new utility classes in `src/views/*.rs`, or upgrading DaisyUI.

For continuous build during dev:

```bash
cd crates/ghinvite-web
npm run watch:css
```

### Run the server

```bash
# Start Restate (in another terminal).
docker compose up -d restate

# Run the web binary.
cargo run -p ghinvite-web
```

The server binds `127.0.0.1:8787`. Hit `http://127.0.0.1:8787` to see the home
page. `/login` redirects to GitHub OAuth — set `GHINVITE_GITHUB_CLIENT_ID` and
`GHINVITE_GITHUB_CLIENT_SECRET` in your environment to point at a real GitHub
App, otherwise the redirect lands on GitHub's "App not found" page.

### Run tests

```bash
cargo test -p ghinvite-web
```

## Architecture

`web::build_app(state, session_store) -> axum::Router` is the single entry
point. `state` carries `Arc<dyn Storage>`, `Arc<dyn HttpTransport>`,
`Arc<RestateClient>`, and `WebConfig`. `session_store` is any
`tower_sessions::SessionStore` impl — `MemoryStore` for tests, `SqliteStore`
for native dev, `D1Store` for production (Plan 7).

Every route handler that touches Restate calls
`state.restate.send::<Input>("Service", "key", "method", &input)`. The web
binary never writes domain state directly — it always goes through Plan 3's
handlers via `RestateClient`. The only exception is the OAuth callback, which
upserts the `users` row directly via `state.storage.upsert_user`.

For OAuth, every authenticated route constructs a `github::oauth::UserApiClient`
per-request from `state.github_transport` plus the session's access token.

## Style stack

- **Tailwind CSS v4** with **DaisyUI v5** (npm-driven build).
- The Tailwind v4 CSS-first config (`@import "tailwindcss"; @plugin "daisyui";`)
  in `assets/styles.css` replaces the old `tailwind.config.js`.
- Built CSS is `.gitignore`d — it's a build artifact. CI / Plan 7 deploy runs
  `npm run build:css` before `cargo build`.

## Client-side JavaScript and the Content-Security-Policy

Every HTML response carries an enforced `Content-Security-Policy`
(`src/middleware/csp.rs`, mounted once in `build_app`). The policy is
`script-src 'self' 'wasm-unsafe-eval'` and `style-src 'self'`, so:

- **No inline `<script>` blocks.** Not in layouts, not in pages, not in
  components. The browser will refuse to run them. Rendered HTML must contain
  exactly one script element: `<script src="/static/app.js"></script>` in the
  layout `<head>` (`views::components::AppScript`).
- **Client behaviour goes in `assets/app.js`**, served at `/static/app.js`
  (`include_str!`, like the CSS). It runs on every page, so each section must
  guard for the absence of the elements it wires — prefer `data-*` hooks in
  the markup plus delegated `document` listeners, as the theme sync and the
  invitation-code shortcut do. Richer interactivity is a Dioxus island per
  [ADR 0001](../../docs/adr/0001-ssr-first-with-dioxus-islands.md), not a new
  script tag.
- **No inline `style=""` attributes** and no external stylesheets or fonts;
  use Tailwind/DaisyUI classes in the built stylesheet.
- The policy is a single constant, `middleware::csp::CONTENT_SECURITY_POLICY`.
  If a page genuinely needs a new origin (for example GitHub avatars under
  `img-src`), extend that constant and its per-directive rationale rather
  than weakening it with `'unsafe-inline'`. Route tests assert the header on
  the home page, a Console page, and a public invitation page; view tests
  assert that no inline script remains.

The header is only set on `text/html` responses — static assets, the JSON
webhook receiver, plain-text errors, and redirects are left alone.

## What this crate does NOT do (yet)

- **Dashboard routes** (`/accounts/{login}/...`) — Plan 5. Currently 501 stubs.
- **Recipient flow** (`/i/{slug}...`) and `/webhooks/github` — Plan 6.
  Currently 501 stubs.
- **Workers entry / D1Storage** — Plan 7. The library is target-agnostic;
  Plan 7 wraps `build_app()` in `#[event(fetch)]`.
- **E2E tests** — Plan 8.
