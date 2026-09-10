# crates/ghinvite-island

The browser island for the new invitation link form
([ADR 0001](../../docs/adr/0001-ssr-first-with-dioxus-islands.md)): a
`dioxus-web` root that mounts on the server-rendered form container and
re-renders **the same `LinkCreateForm` component** the server rendered, with a
[dioform](https://github.com/sagikazarmark/dioform) form behind it. Field
errors appear on commit (leaving a field), the repository-scope error clears
as soon as a repository is checked, and `progressive_submit()` cancels the
native POST only while a known blocker exists — otherwise the browser POSTs to
the existing route exactly as it does with JavaScript disabled.

This is the only crate that depends on the `dioform` facade and (on wasm32)
on `dioxus-web`. `ghinvite-ui` owns the markup and the shared form model
(`link_form`); `ghinvite-web` runs the same validators on the server through
`dioform-core`.

## Layout

| Path | What |
| --- | --- |
| `src/lib.rs` | `LinkFormIsland(props: LinkFormIslandProps)` — the reactive form. Compiles natively (for tests) and for the browser. |
| `src/main.rs` | The wasm32 entrypoint: reads the props blob, empties `#link-form-island`, sets `data-island="mounted"` on it, launches Dioxus there. An empty `main` on native. |
| `tests/parity.rs` | Renders `LinkCreateForm` (server) and `LinkFormIsland` (island) with `dioxus_ssr` for the same props and asserts the HTML is **identical**, byte for byte. |
| `examples/ssr_fixture.rs` | Prints the server-rendered page as a full HTML document, for local smoke tests (see below). |

## How the island works

1. The page (`ghinvite_ui::links::LinkCreateFormPage`) emits
   `<div id="link-form-island">{form}</div>`, a
   `<script type="application/json" id="link-form-props">` block holding a
   serialised `LinkFormIslandProps` (`action`, the preserved `values` with
   the server's errors, the available `repos`, the server's `now`), and
   `<script type="module" src="/assets/ghinvite-island.js">`.
2. `main` deserialises the blob. `dioxus-web`'s non-hydrating mount *appends*
   to its root, so the entrypoint empties the container first (otherwise two
   forms end up in the DOM — spike finding, #33), then launches.
3. `LinkFormIsland` builds the `CreateLinkForm` model from the values (the
   same mapping as the server's `to_model`: text verbatim, a numeric guardrail
   that does not parse left blank), configures the form with
   `register_validators(core, &repos, now)` and `ValidationMode::on_commit()`,
   binds every control (`text`, `textarea`, `select`, `checkbox`,
   `use_number_with(parse_max_uses, …)`, `use_multi_select`), and renders
   `LinkCreateForm` with a `LinkFormValues` read back from the bindings and a
   `LinkFormHandlers` bundle of the bindings' listeners. Field ids stay the
   shared components' (`description`, `permission`, `repo_ids`, …).
4. **Seeding server errors** (once, on mount): if `values.errors` is
   non-empty, `form.begin_submission()`. The shared validators run first; if
   they reject the preserved values (`Blocked`) the submit attempt has made
   the same errors the server produced visible. If they pass (`Started`), the
   server knew something the browser cannot — the messages are attached as
   `SubmitError::field(path, msg)` on their slots and summary-only lines as
   `SubmitError::form(msg)`, then `finish_submission_with_errors`. Either way
   the error clears when the admin edits the field.
5. **Raw numeric text that does not parse** (`max_uses: "abc"`,
   `expires_in_days: "0"`) is re-applied to the parsed binding with
   `on_input(raw)` after seeding, so the input shows the text and the same
   parse error the server reported, and submission stays blocked until it is
   fixed. Chromium sanitises non-numeric text out of a `type="number"` input,
   so there the field shows empty with the error; the server stays the
   authority.
6. Errors are folded back into `LinkFormErrors` with the same `attach` the
   server uses (parse errors first, then visible validation errors; summary
   line added on the first attach; form-level messages appended), so the
   first frame matches the server's HTML exactly.

## Registry Input Integration

The island enables dioform's `dioxus-field` feature and converts the description
binding to `dioxus_field::Binding<String>`. The adapter delegates writes, including
their origin, to the original binding. Its native-change commit callback is a
no-op; focus exit calls `binding.commit()` followed by `binding.focus_exit()`.
This preserves validation when leaving even an unchanged empty field, avoids a
duplicate commit from native change before blur, and does not add per-keystroke
validation. Dioform still owns revalidation and stale-error clearing after errors.

`LinkFormHandlers::description` carries that binding to the original UI `Field`
wrapper's `RegistryText` variant. Its isolated component derives a controlled
value memo from props; application labels, IDs, help/error markup, and ARIA remain
authoritative. Other controls keep their existing native listeners. Registry source
pins and replacement boundaries are in the
[component documentation](../ghinvite-ui/src/components/README.md).

After rebuilding CSS and the island as described in the
[browser setup](../../tests/browser/README.md), run the timing regression from
the repository root:

```bash
npm test --prefix tests/browser -- --grep 'description validates on focus exit'
```

## Building

Always with `-p`, never `--workspace --target wasm32-unknown-unknown`: in the
Worker crates `cfg(target_arch = "wasm32")` means "Cloudflare Workers", here
it means "browser", and a workspace-wide wasm32 build would unify the two.

```bash
scripts/build-island.sh                                              # dx bundle → dist/public (what the server serves)
cargo check  -p ghinvite-island --target wasm32-unknown-unknown       # quick browser check
cargo clippy -p ghinvite-island --target wasm32-unknown-unknown -- -D warnings
cargo test   -p ghinvite-island                                       # parity test + unit tests (native)
```

`scripts/build-island.sh` runs `dx bundle -p ghinvite-island --platform web
--profile island --debug-symbols false`, reads
`target/dx/ghinvite-island/release/web/.manifest.json` for the hashed file
names, copies only those files to `dist/public/assets/`, writes the stable
loader `dist/public/assets/ghinvite-island.js` (`import "/assets/<hashed>.js";`)
and `dist/public/_headers`, prints raw and gzipped sizes, and fails if the
gzipped `.wasm` + `.js` exceed 600 KB. It needs the Dioxus CLI (`dx` 0.7.x)
and the `wasm32-unknown-unknown` target; `dx` invokes the `cargo` on your
`PATH`, so keep the rustup-managed one first so `rust-toolchain.toml` is
honoured.

The registry pilot measured approximately 317 KiB gzipped Wasm + JS against a
243 KiB baseline, below the 600 KiB budget. Sizes vary with toolchain and source;
the build script reports and enforces the current total.

`[profile.island]` in the root `Cargo.toml` (inherits `release`; `opt-level =
"z"`, `lto`, `codegen-units = 1`, `panic = "abort"`, `strip`) exists for this
bundle alone, so the Workers build (`worker-build --release`) keeps cargo's
defaults. dx's own `wasm-release` profile only sets `opt-level = "s"` on top
of `release` and produced a ~30% larger wasm here (838 KB / 330 KB gzipped vs
574 KB / 238 KB). dx runs `wasm-bindgen` and `wasm-opt -Oz` either way.

Native dependencies stay clean: `dioxus/web` is a `[target.'cfg(target_arch
= "wasm32")']` dependency, so `cargo tree -p ghinvite-web -e features | grep
dioxus-web` on the host prints nothing.

## Smoke-testing locally

For repeatable browser regressions without the full stack, see
[`tests/browser`](../../tests/browser/README.md). Its Playwright suite serves the
real SSR fixture, built CSS and island assets under the production CSP, with a
native POST echo endpoint; CI runs desktop/mobile-layout and light/dark projects.

Run the native web server from the repo root after `scripts/build-island.sh`;
it serves `dist/public/assets` under `/assets/*` (override with
`GHINVITE_ISLAND_ASSETS_DIR`). Open the new invitation link page and check
that `#link-form-island` gains `data-island="mounted"`, that leaving the
description empty and clicking Create shows the errors without navigating,
and that a valid submit is a normal POST.

Without the full stack, the example renders the page as a static document:

```bash
cargo run -p ghinvite-island --example ssr_fixture > dist/public/index.html                # fresh form, 3 repos
cargo run -p ghinvite-island --example ssr_fixture -- --with-errors > dist/public/index.html
python3 -m http.server -d dist/public 8000    # then open http://127.0.0.1:8000/
```

The form posts to `/console/accounts/acme/links`, which a static server 404s;
that is fine for checking mount, inline validation and blocked submits. **This
`index.html` is for local smoke-testing only and must never be deployed**:
`wrangler/web.toml` serves `dist/public` as Static Assets, where an
`index.html` would shadow `/` — `scripts/build-island.sh` recreates
`dist/public` from scratch, so re-run it before `wrangler deploy`.
