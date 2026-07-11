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

## What this crate does NOT do (yet)

- **Dashboard routes** (`/accounts/{login}/...`) — Plan 5. Currently 501 stubs.
- **Recipient flow** (`/i/{slug}...`) and `/webhooks/github` — Plan 6.
  Currently 501 stubs.
- **Workers entry / D1Storage** — Plan 7. The library is target-agnostic;
  Plan 7 wraps `build_app()` in `#[event(fetch)]`.
- **E2E tests** — Plan 8.
