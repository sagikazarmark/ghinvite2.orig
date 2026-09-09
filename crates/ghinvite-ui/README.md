# crates/ghinvite-ui

Dioxus view components for ghinvite: the three layouts (Home / Console /
Invitation), every page, the `Field` form primitive, and the `Flash` message
type the layouts render. Props in, markup out; no hooks, no I/O.

This crate exists so the views can be built for the browser
([ADR 0001](../../docs/adr/0001-ssr-first-with-dioxus-islands.md)). It depends
on `dioxus`, `ghinvite-core`, `chrono`, and `serde` only — never on
`ghinvite-web`, `ghinvite-github`, or either storage crate, all of which treat
`cfg(target_arch = "wasm32")` as "Cloudflare Workers". Data that originates on
the server (a GitHub payload, a session) crosses into the views as plain props,
e.g. `links::RepositoryChoice` rather than the GitHub client's `GhRepo`.

## Consumers

- `ghinvite-web` re-exports this crate as `ghinvite_web::views::*`, adds
  `views::render` (the `dioxus_ssr` renderer; `dioxus-ssr` is deliberately not
  a dependency here), and re-exports `flash::{Flash, FlashLevel}` from
  `ghinvite_web::session`.
- A future browser island (`dioxus-web`) depends on this crate directly.

## Checks

```bash
cargo test -p ghinvite-ui                                    # rendered-markup tests
cargo check -p ghinvite-ui --target wasm32-unknown-unknown   # browser build
```

Always `-p`, never `cargo build --workspace --target wasm32-unknown-unknown`:
feature unification would drag Worker-only dependencies into the browser
build and vice versa.

## Styling

Utility classes used here are compiled into `crates/ghinvite-web/assets/styles.built.css`
by Tailwind, which finds this crate through the `@source "../../ghinvite-ui/src";`
directive in `crates/ghinvite-web/assets/styles.css`. After adding or changing
classes, run `npm run build:css` in `crates/ghinvite-web` and commit the result
(see that crate's README).
