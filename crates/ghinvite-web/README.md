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
adding new utility classes in the views (`crates/ghinvite-ui/src/*.rs`), or
upgrading DaisyUI, and commit the result — the built file is tracked so
`cargo build` never depends on npm.

Tailwind v4 auto-detects class names only under the directory the CLI runs
in (this crate). The views live in `crates/ghinvite-ui`, so `assets/styles.css`
registers them explicitly with `@source "../../ghinvite-ui/src";` (paths are
relative to the stylesheet). If views ever move again, update that directive
or their utility classes silently disappear from the built CSS.

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

Run it from the repo root so `/assets/*` resolves to `dist/public/assets`
(see [Island assets](#island-assets)); without the island bundle the new
invitation link form is simply the server-rendered form.

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

### Views and the `ghinvite-ui` crate

The Dioxus components (layouts, pages, the `Field` form primitive, `Flash`)
and the shared new-invitation-link form model (`link_form`) live in
`crates/ghinvite-ui`, which depends only on `dioxus`, `dioform-core` +
`dioform-derive`, `ghinvite-core`, `chrono`, and `serde` — never on this
crate, the GitHub client, or storage
([ADR 0001](../../docs/adr/0001-ssr-first-with-dioxus-islands.md)). This crate
re-exports them as `ghinvite_web::views::*` and adds `views::render`, the
`dioxus_ssr` renderer the routes call. `session::Flash` is a re-export of
`ghinvite_ui::flash::Flash`. Data crosses into the views as plain props: for
example the console route maps the GitHub `GhRepo` payload into
`views::link_form::RepositoryChoice` once, where repositories are loaded, and
both validation and the form page work on that type.

### Form validation

`forms::create_link` is a thin adapter over the shared model: it parses the
raw POST body (`CreateLinkSubmission`) into `link_form::CreateLinkForm` with
the shared parsers, runs `link_form::register_validators` through a
`dioform_core::FormCore`, and maps each error to `LinkFormErrors` by field
identity. The `FormCore` holds `Rc` and is not `Send`, so the route loads the
available repositories first and validates synchronously afterwards; the
core never crosses an `.await`. No validation rule lives in this crate.

**Build client-side crates with `-p`, never
`cargo build --workspace --target wasm32-unknown-unknown`.** Throughout this
crate, `ghinvite-github`, and `ghinvite-storage-d1`, `cfg(target_arch =
"wasm32")` means "Cloudflare Workers"; in `ghinvite-ui` it means "browser". A
workspace-wide wasm32 build would unify features across both and drag
Worker-only dependencies into the browser build (and vice versa). The
canonical checks are:

```bash
cargo check -p ghinvite-ui --target wasm32-unknown-unknown          # browser
cargo build -p ghinvite-web-worker --target wasm32-unknown-unknown  # Worker
```

## Style stack

- **Tailwind CSS v4** with **DaisyUI v5** (npm-driven build).
- The Tailwind v4 CSS-first config (`@import "tailwindcss"; @plugin "daisyui";`)
  in `assets/styles.css` replaces the old `tailwind.config.js`. It also
  carries the `@source` directive that points Tailwind at `crates/ghinvite-ui`.
- Built CSS (`assets/styles.built.css`) is a build artifact but is **tracked**,
  so `cargo build` and the Workers deploy never depend on npm. Rebuild and
  commit it whenever the views or `styles.css` change.

## Client-side JavaScript and the Content-Security-Policy

Every HTML response carries an enforced `Content-Security-Policy`
(`src/middleware/csp.rs`, mounted once in `build_app`). The policy is
`script-src 'self' 'wasm-unsafe-eval'` and `style-src 'self'`, so:

- **No inline `<script>` blocks.** Not in layouts, not in pages, not in
  components. The browser will refuse to run them. Rendered HTML may contain
  only these script elements: `<script src="/static/app.js"></script>` in the
  layout `<head>` (`views::components::AppScript`); on a page that hosts a
  Dioxus island, one `<script type="application/json" id="…">` data block
  (inert — the browser never executes it, and the CSP does not apply to it)
  and one `<script type="module" src="/assets/…">` tag. Data blocks are
  written with `link_form::json_for_script_block`, which escapes `<` so
  user-typed text cannot close the element.
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
  the home page, a Console page, and a public invitation page; view and route
  tests assert that every `<script` is one of the allowed forms above (no
  inline executable script).

The header is only set on `text/html` responses — static assets, the JSON
webhook receiver, plain-text errors, and redirects are left alone.

## Island assets

The new invitation link page (`/console/accounts/{login}/links/new`) hosts
the first Dioxus island ([ADR 0001](../../docs/adr/0001-ssr-first-with-dioxus-islands.md)).
The server-rendered page references exactly one client file for it:

```html
<script type="module" src="/assets/ghinvite-island.js"></script>
```

That name is **stable** (`ghinvite_ui::link_form::LINK_FORM_ISLAND_MODULE_SRC`).
`dx bundle` emits content-hashed files (`island-dxh<hash>.js`,
`island_bg-dxh<hash>.wasm`), so the island build script
(`scripts/build-island.sh`) also writes a tiny `ghinvite-island.js` loader
that `import`s the hashed bundle. The SSR page never learns the hash, and the
Rust build has no dependency on the `dx` build.

- **Where the files come from:** `scripts/build-island.sh` runs `dx bundle`
  for the island crate and copies the output to `dist/public/assets/`
  (plus `dist/public/_headers`, which marks the hashed files
  `immutable`). `dist/` is git-ignored.
- **Native dev** (`cargo run -p ghinvite-web`): `build_app` nests a
  `tower_http::services::ServeDir` at `/assets` over
  `WebConfig::island_assets_dir` — `dist/public/assets` relative to the
  working directory by default, overridable with `GHINVITE_ISLAND_ASSETS_DIR`,
  `None` to register no route. `tower-http/fs` is a non-wasm32 dependency
  only.
- **Workers:** Cloudflare Static Assets serve `/assets/*` before the Worker is
  invoked (`[assets] directory = "../dist/public"` in `wrangler/web.toml`);
  the Worker has no `/assets` route and `island_assets_dir` is `None`. The
  directory must contain only `assets/*` (no `index.html`), and
  `not_found_handling` must stay unset — see the comments in that file.
- **Missing assets degrade to the plain form.** If the bundle has not been
  built (tests, a fresh checkout, CI without `dx`), the browser gets a 404 for
  the module and the server-rendered form works exactly as before; nothing
  else on the page depends on it.

## What this crate does NOT do (yet)

- **Dashboard routes** (`/accounts/{login}/...`) — Plan 5. Currently 501 stubs.
- **Recipient flow** (`/i/{slug}...`) and `/webhooks/github` — Plan 6.
  Currently 501 stubs.
- **Workers entry / D1Storage** — Plan 7. The library is target-agnostic;
  Plan 7 wraps `build_app()` in `#[event(fetch)]`.
- **E2E tests** — Plan 8.
