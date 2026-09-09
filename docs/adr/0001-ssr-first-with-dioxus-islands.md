---
status: accepted
date: 2026-09-09
---

# SSR-first on Cloudflare Workers, with Dioxus admitted only as fresh-render islands

The v1 design spec locked "Dioxus fullstack SSR + hydration, server functions for state changes" (Q9), but that combination cannot run on the chosen host: `dioxus-server` hard-depends on `axum/tokio` and `tokio-tungstenite`, which pull `tokio/net` and `mio` and fail to compile for `wasm32-unknown-unknown`. Hydration was deferred in every plan since with no reason recorded, and later specs fenced it out as a scope rule rather than a decision. This ADR records the actual decision.

**ghinvite stays SSR-first.** Every page is rendered on the Worker by axum + `dioxus_ssr`, every mutation is a native browser `<form method="post">` handled by an axum route, and the server is the sole validation authority. Client-side Dioxus is admitted only as **fresh-render islands**: a `dioxus-web` root mounted via `Config::rootelement` onto a single server-rendered container (first: the new invitation link form), re-rendering that subtree from a JSON props blob after the wasm loads. Islands are progressive enhancement — the page must be fully usable before the island mounts and with JavaScript disabled.

**dioform is the shared form model.** Form structs use `#[derive(Form)]`; validators are registered once and run on the server through `dioform-core` (Dioxus-free) inside the axum handler, and in the browser through the `dioform` bindings. The island uses `progressive_submit()`: it blocks the native POST only when client preflight finds a known blocker, otherwise the browser POST proceeds unchanged to the existing route.

## Considered options

- **Dioxus fullstack proper on Workers** — rejected: does not compile (verified by scratch `cargo check --target wasm32-unknown-unknown`; the dioform demo README records the same finding; no maintained bridge crate exists).
- **Hydrated islands** (`dioxus-web` `hydrate` feature over `dioxus_ssr` `pre_render` output) — feasible but requires `dioxus-fullstack-core` on both sides and a hand-rolled `initial_dioxus_hydration_data` payload, an undocumented internal protocol. Deferred; a fresh-render island can be upgraded to this later if input loss before wasm load is actually reported.
- **Console as a `dioxus-web` SPA on Workers Static Assets, public pages stay SSR** — feasible (auth cookie and `RequireConsoleAdminOf` reuse unchanged; `worker::Env::assets()` exists) but costs a JSON API for every console read model, a second rendering model, and 13–20 engineer-days for a console with one real form. **This is the named escape hatch**, triggered when a console feature needs cross-page client state (e.g. a repository picker with filtering over >100 repositories, a multi-step link wizard, optimistic approve/decline) or when a third island makes SSR-form/island duplication visibly costly.
- **Move web hosting off Workers** so fullstack is first-class — rejected: trades the product's stated "Rust on Workers" identity and D1 for framework convenience; the spec named leaving Workers only as a Restate contingency.
- **Stay SSR-only, no client runtime, no dioform** — rejected as the *decision* but is the fallback if the island spike fails. It has already caused feature cuts (copy-to-clipboard, dynamic field warnings) and forecloses dogfooding dioform, which the maintainer owns and values.
- **Replace Dioxus SSR with a plain template engine** — rejected: `rsx!` components are the one artefact that carries into any client path.

## Consequences

- A `ghinvite-ui` crate (views only, no server dependencies) must exist before the first island, because `cfg(target_arch = "wasm32")` currently means "Cloudflare Workers" throughout `ghinvite-web`, `ghinvite-github`, and `ghinvite-storage-d1`, and a browser build would collide with those gates. Build client crates with `-p`, never `--workspace --target wasm32`.
- Workers Static Assets serve the island bundle from `dist/public`, which must contain **only** `assets/*` (and `_headers`). Static Assets answer first for any path that matches a file and fall through to the Worker otherwise; because no SSR route lives under `/assets/`, this is safe without `run_worker_first`. `not_found_handling = "single-page-application"` must **not** be used: on `compatibility_date >= 2025-04-01` it serves `index.html` for every unmatched navigation and would hijack `/`, `/i/{slug}`, and `/login`. Never deploy a `dist/public` containing an `index.html` for the same reason. (Amended 2026-09-09 after #36: the original text asked for `run_worker_first` covering all SSR routes; that is unnecessary under the assets-only-under-`/assets/` rule.)
- A real Content-Security-Policy must be applied for the first time (`script-src 'self' 'wasm-unsafe-eval'`). The existing inline theme script moves to a static file. **No new ad-hoc inline `<script>` blocks**: client behaviour goes through the island or a shared static script. This supersedes the inline-script approach in the repository-scope progressive-enhancement ticket.
- Adopting dioform requires bumping the Rust toolchain from 1.85 to >= 1.92 (`dioform-core` `rust-version`) and Dioxus from 0.7.7 to 0.7.10 (the `dioform` facade pins `dioxus-signals`/`dioxus-hooks` exactly).
- `dioform_core::FormCore` holds `Rc` and is not `Send`; in axum handlers it must be constructed, run, and dropped between `.await` points (load repositories first, then validate synchronously).
- The recipient pending page's auto-refresh is a `<meta http-equiv="refresh">`, not a client runtime. The Critical UX item in the foundations review was notifications, which no rendering choice addresses.
- Dogfooding is bounded by product need: one reversible island exercises dioform's typed model, validators, and `progressive_submit`/`browser_submit`, but not `managed_submit` or fullstack adapters. Expanding beyond that is a library-strategy decision and should be recorded as such, not justified as UX.
