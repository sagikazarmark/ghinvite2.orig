# crates/ghinvite-ui

Dioxus view components for ghinvite: the three layouts (Home / Console /
Invitation), every page, the `Field` form primitive (`field`), the `Flash`
message type the layouts render, and the shared form model for the new
invitation link form (`link_form`). Views consume props without I/O; the registry
Input reads its binding from Field context when bound, or a prop-derived memo
for preserved values in unbound SSR. Permission's registry NativeSelect uses
Field context for writes and metadata, with a controlled display memo in both
SSR and the browser to handle unsupported values without changing the model.

This crate exists so the views can be built for the browser
([ADR 0001](../../docs/adr/0001-ssr-first-with-dioxus-islands.md)). It depends
on `dioxus`, `dioxus-field`, `dioxus-primitives`, `dioform-core` + `dioform-derive`,
`ghinvite-core`, `chrono`, `serde` and `serde_json`, never on `ghinvite-web`,
`ghinvite-github`, or either storage crate, all of which treat `cfg(target_arch = "wasm32")` as
"Cloudflare Workers". Data that originates on the server (a GitHub payload, a
session) crosses into the views as plain props, e.g. `link_form::RepositoryChoice`
rather than the GitHub client's `GhRepo`.

## The shared form model (`link_form`)

`link_form::CreateLinkForm` is the new invitation link form as a typed
`#[derive(Form)]` model (dioform, via `#[form(crate = "::dioform_core")]` so
no Dioxus facade is pulled in). The module also owns:

- the pure parsers for the numeric guardrails (`parse_max_uses`,
  `parse_expires_in_days`): typed text → `Option<u32>` or the message to show;
- `register_validators(core, available_repos, now)`: the five rules of the
  form (PRD #23) registered once as sync field validators on typed field paths.
  `now` and the available repositories are injected, so the rules are pure;
- `LinkFormErrors`, which maps a dioform `FieldIdentity` to the slot the view
  renders it in (`attach`), plus every user-facing message as a constant;
- `LinkFormValues`, the form as the view renders it (submitted values verbatim
  plus `LinkFormErrors`), and `LinkFormIslandProps`, the JSON the page hands
  the island (`action`, `values`, `repos`, the server's `now`). All three are
  `Serialize`/`Deserialize`.

The server (`ghinvite-web::forms::create_link`) runs it through
`dioform_core::FormCore`; the browser island hands the same
`register_validators` to the `dioform` facade's `FormConfig::register_core`
and the same parsers to `use_number_with`. `LinkCreateForm` renders its
`name` attributes from `CreateLinkForm::fields()`, so the HTML and the model
cannot drift. The `dioform` facade and `dioxus-web` are deliberately **not**
dependencies of this crate.

The workspace uses dioform 0.7. On a failed POST with non-empty
`LinkFormValues.errors`, the island configures `FormConfig::browser_rejection`
to restore the supplied typed values as draft and baseline and show errors from
the prior rejected attempt without marking fields touched. Server-only field
and form messages survive alongside client-reproducible errors. Parsed numeric
bindings consume restored invalid raw input when they mount; no simulated input
or submission is needed. A fresh core preflight retires the rejection and can
allow an unchanged retry of server-only errors. `ParseBlocked` stops before
that preflight and retains the rejection until a later submit can reach it.
See the [island lifecycle](../ghinvite-island/README.md#how-the-island-works).

## The new invitation link form and its island (`links`)

`links::LinkCreateFormPage` is the Console page. Inside it, the form is one
component the island renders too, so browser and server produce the same
markup from the same inputs:

- `LinkCreateForm { action, form: LinkFormValues, repos, handlers }` — just
  the `<form>`: summary alert, four sections, submit button. `handlers` is an
  optional `links::LinkFormHandlers` (the form's `onsubmit`, a
  `dioxus_field::Binding<String>` for description, max use, and expiration,
  `dioxus_field::Binding<Option<String>>` for permission,
  `field::ControlHandlers { oninput, onchange, onblur }` for other native controls, and
  `RepositoryScopeHandlers` for the checkbox group); the server uses the default.
- `field::Field` - text / textarea / number controls; `RegistryText` and
  `RegistryNumber` delegate to `RegistryInputField` (renamed from
  `RegistryTextField`), using registry Field context, FieldLabel, Input, and
  FieldError while retaining the native help paragraph. The number variant
  forwards native `type="number"`, `inputmode="numeric"`, and optional `min` / `max`;
  both new-link numeric fields use `min="1"` with no `max`.
- `links::PermissionSelect { id, name, value, help, error, binding }` renders
  the permission-level native `<select>` (`PERMISSION_LEVELS`), using registry
  Field context, FieldLabel, NativeSelect, native help, and FieldError.
- `links::RepositoryScopeGroup { name, repos, selected, help, error, onchange, onblur }`
  — the repository checkbox group; `onchange` receives `(repo_id, checked)`.

The server supplies preserved values without browser bindings or listeners.
Server-side rendering emits no listener attributes, so wired and unwired
markup is byte-identical for the covered values (tested for each component and
for the whole `LinkCreateForm`). The numeric restoration boundary below also
applies to the island's first-frame parity.

The production registry integration covers the summary Alert, submit Button,
description, max-use, and expiration Inputs, and permission NativeSelect.
`with_meta_values` supplies explicit control ID, name, and errors, plus required
state for description;
metadata drives `input-error` / `select-error` and `aria-invalid` (`"false"` when
valid), without duplicate control overrides.
FieldError is an always-mounted polite live-region `div` with nested error
`div`s; clearing an error empties the corresponding `description-error`,
`max_uses-error`, `expires_in_days-error`, or `permission-error` region rather
than removing it.

The island converts each numeric dioform
`ParsedTextBinding<CreateLinkForm, Option<u32>>` to a raw-text `Binding<String>`
and applies `commit_on_focus_exit`. The existing custom parsers and typed
`Option<u32>` model are unchanged: blank means unlimited max use or no expiration,
and expiration timestamp overflow is still checked by the shared server/browser
validator. Errors still arrive through `LinkFormValues` props, folded with parse
errors before visible validator errors, then passed into the same metadata.
Unbound SSR uses the preserved raw-value memo; bound browser Inputs read directly
from their bindings, including invalid raw text restored from a failed response
with non-empty errors. Chromium sanitizes `abc` out of the native number display,
but the binding retains it and its parse error until
edited; unchanged blur or unrelated edits do not silently turn it into `None`.
Invalid numeric props without response errors do not configure restoration.
Valid noncanonical text such as `"007"` or `" 7 "` still formats from the typed
model as `"7"` on mount; preserving that spelling or byte-identical markup for
it is not a guarantee of this migration.

Permission has exactly five options, with explicit `NativeSelectOption::form_value`
strings: `pull`, `triage`, `push`, `maintain`, and `admin`. Native POSTs submit
these strings, never positional indices. There is no placeholder or empty option.
Unsupported values such as `owner` or an empty string remain verbatim in the
props and dioform model for validation, but display as `pull`. Previously no
option had a `selected` attribute for unsupported values, leaving the browser to
display the first option. Now the shared SSR/island component explicitly selects
`pull`, preserving display and native POST semantics without silently repairing
the model. Unlike the Inputs' direct bound display, permission always uses a
controlled memo: NativeSelect writes the DOM value property, so passing an
unmatched raw value would blank the control. Real NativeSelect input writes
through the binding; unchanged focus exit does not copy the fallback into it.

With dioform 0.7, the application still owns stable IDs and explicit
`aria-describedby` and `aria-errormessage`: later sibling registration cannot
supply the controls' first-pass SSR associations, and native help does not
register with metadata. The help `p`
retains its styling because registry FieldDescription forces the unsuitable
`label` class even with appearance disabled. Metadata reduces duplication; it
does not eliminate all application wiring. Only the new-link numeric fields
migrated; legacy `FieldKind::Number` controls elsewhere, textarea, and checkboxes
keep their existing native rendering and listeners. Installed upstream source
and installation instructions are unchanged, with no new dependencies. See
[component provenance and update instructions](src/components/README.md) and the
[island binding adapter](../ghinvite-island/README.md#registry-input-integration).

The page emits the island's mount points after the page header:

```html
<div id="link-form-island"><form …>…</form></div>
<script type="application/json" id="link-form-props">{…LinkFormIslandProps…}</script>
<script type="module" src="/assets/ghinvite-island.js"></script>
```

The ids and the module `src` are `link_form::LINK_FORM_ISLAND_ROOT_ID`,
`LINK_FORM_ISLAND_PROPS_ID` and `LINK_FORM_ISLAND_MODULE_SRC`. The data block
is written with `link_form::json_for_script_block`, which escapes every `<`
as `\u003c` so a description containing `</script>` cannot close it. Neither
script is inline executable code, so the page stays clean under
`script-src 'self'`; if the module is missing the browser 404s it and the
plain form keeps working.

## Consumers

- `ghinvite-web` re-exports this crate as `ghinvite_web::views::*`, adds
  `views::render` (the `dioxus_ssr` renderer; `dioxus-ssr` is deliberately not
  a dependency here), and re-exports `flash::{Flash, FlashLevel}` from
  `ghinvite_web::session`.
- `ghinvite-island` (the browser island, `dioxus-web` + the `dioform` facade)
  depends on this crate directly and renders `links::LinkCreateForm` with a
  `links::LinkFormHandlers` bundle of listeners; its parity test asserts the
  island's first frame equals the server's HTML for the covered response values.

## Checks

```bash
cargo test -p ghinvite-ui                                    # rendered-markup tests
cargo check -p ghinvite-ui --target wasm32-unknown-unknown   # browser build
```

The [browser regression suite](../../tests/browser/README.md) exercises the real
page and island under the production CSP, including native POST fallback,
validation timing, error associations, and desktop/mobile light/dark layouts.

Always `-p`, never `cargo build --workspace --target wasm32-unknown-unknown`:
feature unification would drag Worker-only dependencies into the browser
build and vice versa.

## Styling

Utility classes used here are compiled into `crates/ghinvite-web/assets/styles.built.css`
by Tailwind, which finds this crate through the `@source "../../ghinvite-ui/src";`
directive in `crates/ghinvite-web/assets/styles.css`. After adding or changing
classes, run `npm run build:css` in `crates/ghinvite-web` and commit the result
(see that crate's README).

`ConsoleLayout` supplies the mobile viewport meta tag. The approval checkbox's
label uses `whitespace-normal` so its text wraps at phone widths; retain both
when changing the form layout.
