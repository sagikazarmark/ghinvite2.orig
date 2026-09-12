# crates/ghinvite-island

The browser island for the new invitation link form
([ADR 0001](../../docs/adr/0001-ssr-first-with-dioxus-islands.md)): a
`dioxus-web` root that mounts on the server-rendered form container and
re-renders **the same `LinkCreateForm` component** the server rendered, with a
[dioform](https://github.com/sagikazarmark/dioform) 0.7 form behind it. Field
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
| `tests/parity.rs` | Renders `LinkCreateForm` (server) and `LinkFormIsland` (island) with `dioxus_ssr` for the covered props and asserts the HTML is **identical**, byte for byte. |
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
4. **Restoring a rejected browser POST**: when `values.errors` is non-empty,
   the config uses `FormConfig::browser_rejection((), …)` before
   `use_form_config`. It supplies a `BrowserRejection` with
   `SubmitError::field(path, msg)` for each field slot and
   `SubmitError::form(msg)` for extra summary lines. Restoration runs once per
   form instance: the supplied typed values become the draft and baseline,
   and errors are immediately visible as a prior rejected attempt, without
   marking fields touched or starting a fake submission. Rerenders do not
   replay it. Client-reproducible errors and server-only field/form messages
   now coexist; client validation no longer causes server diagnostics to be lost.
5. **Raw numeric text that does not parse** (`max_uses: "abc"`,
   `expires_in_days: "0"`) is attached to that rejection with `raw_field`.
   The parsed bindings consume the restored input when they mount, preserving
   the raw text and parse error without simulating `on_input` or marking the
   field touched. This restoration is configured only for failed responses
   with non-empty errors. Chromium sanitises non-numeric text out of a
   `type="number"` input, so there the field shows empty with the error;
   the binding still holds the parse blocker.
6. Errors are folded back into `LinkFormErrors` with the same `attach` the
   server uses (parse errors first, then visible validation errors; summary
   line added on the first attach; form-level messages appended), so the
   first frame matches the server's HTML for the covered response values.
7. **Correction and retry**: editing a related field clears its restored
   field error; unrelated field errors and form-level messages remain. A fresh
   core preflight retires the prior rejection and validates the current values,
   allowing an unchanged retry when only server-side errors were present and
   client/native validation passes. A mounted numeric parse blocker returns
   `ParseBlocked` before core preflight, retaining the rejection. After the
   parse blocker is corrected, the next submit can reach fresh core preflight.

Valid noncanonical numeric text (for example, `"007"` or `" 7 "`) still goes
through the typed model and `format_count`, yielding `"7"` on mount. This is
existing behavior: the migration restores invalid raw input from rejected
responses, not arbitrary numeric spelling. First-frame byte parity is not
promised for valid noncanonical numeric text or invalid numeric props with no
response errors. The server remains the validation authority.

## Registry Input Integration

The island enables dioform's `dioxus-field` feature and converts description,
max use, and expiration to `dioxus_field::Binding<String>` and permission to
`dioxus_field::Binding<Option<String>>`. All adapters use the shared
`commit_on_focus_exit` helper, created inside the one-time handlers hook.
It delegates writes, including their origin, to the original binding. Its
native-change commit callback is a no-op; blur calls `binding.commit()` followed
by `binding.focus_exit()`.
This preserves validation when leaving even an unchanged empty field, avoids a
duplicate commit from native change before blur, and does not add per-keystroke
validation. Dioform still owns revalidation and stale-error clearing after errors.

`LinkFormHandlers::description` carries that binding to the original UI `Field`
wrapper's `RegistryText` variant; `max_uses` and `expires_in_days` carry their
raw-text bindings to `RegistryNumber`. `RegistryInputField` (renamed from
`RegistryTextField`) handles both variants and puts the binding in registry Field
context and applies `with_meta_values` for explicit ID, name, required state, and
visible errors. Bound Input reads that binding directly, with no `value` override;
only unbound SSR uses a memo for preserved values. Metadata drives error color
and `aria-invalid`, including `"false"` when valid. FieldLabel and FieldError
consume the same context; the polite error region stays mounted and is emptied
on correction.

Both numeric bindings still come from
`ParsedTextBinding<CreateLinkForm, Option<u32>>`, created by `use_number_with`
using the existing `parse_max_uses` / `parse_expires_in_days` custom parsers and
`format_count`. The registry conversion exposes the parsed binding's raw text,
not a separate numeric model. Repeated invalid edits remain visible even when
the error message is unchanged. Parse errors are folded into `LinkFormErrors`
before visible validator errors; the view passes those errors through props to
`with_meta_values`, rather than using a separate registry error source.

`RegistryNumber` forwards `type="number"`, `inputmode="numeric"`, and optional
`min` / `max`; both new-link fields retain `min="1"` with no `max`. Blank still
parses to `None` (unlimited max use or no expiration), and valid values remain
`Option<u32>` in the domain model. The shared expiration validator still checks
timestamp overflow using the supplied `now`, just as the server does. The
focus-exit adapter does not change immediate parse errors or add per-keystroke
validator runs.

Unbound SSR preserves raw numeric text through the value memo; bound browser
Inputs read the parsed binding directly, with no value override. Chromium can
sanitize restored `abc` to an empty number display, but the binding retains the
invalid text and blocks progressive submission. Unchanged blur and unrelated
edits do not repair it or convert it to `None`; an actual numeric-field edit is
needed to correct or explicitly clear it.

`LinkFormHandlers::permission` carries the adapted select binding to
`PermissionSelect`, which supplies registry Field context and `with_meta_values`
for ID, name, and visible errors. NativeSelect writes through this binding on
native input, not native change. Its five options have explicit `form_value`
strings `pull`, `triage`, `push`, `maintain`, and `admin`, so native POSTs never
submit positional indices. No placeholder or empty option is rendered.

Unlike the Inputs' direct bound display, permission uses a controlled,
prop-derived display memo in both SSR and the browser. Raw unsupported values
such as `owner` or an empty string remain in dioform for validation while the
control displays `pull`. Passing the raw invalid value to NativeSelect would
blank the DOM selection because it writes the value property. The old native
select omitted `selected` on every option for an unsupported value; the shared
component now explicitly selects `pull`, preserving first-option display and
native POST semantics. Mounting or leaving an unchanged field never writes this
fallback into the model; progressive submit remains blocked until a supported
input corrects it. Empty or unknown DOM input strings are ignored by NativeSelect.

Permission metadata drives `select-error` and `aria-invalid` (`"false"` when
valid). Its FieldError at `permission-error` is also an always-mounted polite
live-region `div` with nested error `div`s, emptied rather than removed on
correction. First-frame parity tests cover every supported permission and
unsupported/empty display fallbacks, including explicit selected-option markup.

Stable IDs, native help styling, and explicit `aria-describedby` and
`aria-errormessage` remain application-owned with dioform 0.7. Later sibling
registration is too late for Input's or NativeSelect's first SSR attributes;
native help does not register, and registry FieldDescription's forced `label` class is unsuitable for
that help text. Only the new-link numeric fields migrated; legacy native number
controls elsewhere, textarea, and checkboxes retain their rendering and listeners.
Registry source and installation are unchanged, with no new dependencies. Pins and
replacement boundaries are in the
[component documentation](../ghinvite-ui/src/components/README.md).

After rebuilding CSS and the island as described in the
[browser setup](../../tests/browser/README.md), run the timing regression from
the repository root:

```bash
npm test --prefix tests/browser -- --grep 'description validates on focus exit|permission|numeric'
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

The dioform 0.7 rejection-restoration migration measured approximately 342 KiB gzipped Wasm + JS,
compared with 337 KiB for NativeSelect, 328 KiB for the description Field-context
migration, and the 243 KiB pre-registry baseline, below the
600 KiB budget. Sizes vary with toolchain and source; the build script reports
and enforces the current total.

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

The fixture also accepts `--preserved-values` for a rejected description with
otherwise valid values, or `--permission-only-invalid` for raw `owner` with only
the permission error and summary. Combine the latter with `--empty-permission`
for an empty raw value, or `--unvalidated` to omit initial errors and exercise
unchanged focus-exit validation. These permission fixtures have a valid
description and selected repository; the browser server exposes them at
`/permission-invalid`, `/permission-empty`, and `/permission-unvalidated`.

`--numeric-only-invalid` seeds raw `abc` with only the max-use error and summary;
add `--expiration` to target expiration instead. The remaining fields are valid.
The browser server exposes these fixtures at `/max_uses-invalid` and
`/expires_in_days-invalid` to check sanitized-empty displays without losing the
underlying parse blocker.

The form posts to `/console/accounts/acme/links`, which a static server 404s;
that is fine for checking mount, inline validation and blocked submits. **This
`index.html` is for local smoke-testing only and must never be deployed**:
`wrangler/web.toml` serves `dist/public` as Static Assets, where an
`index.html` would shadow `/` — `scripts/build-island.sh` recreates
`dist/public` from scratch, so re-run it before `wrangler deploy`.
