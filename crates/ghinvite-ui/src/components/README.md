# Installed Registry Components

Selected source installation from
[`sagikazarmark/dioxus-daisyui-components`](https://github.com/sagikazarmark/dioxus-daisyui-components),
not the DioxusLabs styled-component registry. No full registry Cargo dependency,
preview, examples, or CSS is included; the application owns integration and styling.

## Production Integration

`links::LinkCreateForm` uses the registry Alert for its error summary, Button for
submission, Input for description, max use, and expiration, and NativeSelect for
permission.
The original `ghinvite_ui::field::Field`
wrapper selects `FieldKind::RegistryText` or `FieldKind::RegistryNumber`, whose
isolated `RegistryInputField` (renamed from `RegistryTextField`)
keeps hooks separate from native control branches. It supplies registry Field
context with explicit ID, name, required state, and errors via `with_meta_values`,
and renders FieldLabel, Input, native help, and FieldError. Bound Input reads
directly from the context binding without a `value` override; unbound SSR uses a
prop-derived memo for preserved values.

`RegistryNumber` forwards native `type="number"`, `inputmode="numeric"`, and
optional `min` / `max`; the new-link fields retain `min="1"` and no `max`.
Their bindings expose raw `String` values from dioform's existing
`ParsedTextBinding<CreateLinkForm, Option<u32>>`, not numeric registry state.
Invalid text remains in the binding even when Chromium sanitizes `abc` to an
empty native number display. Errors still come from the application's props,
folded with parse errors before visible validator errors, then supplied through
`with_meta_values`. Blank-to-`None` parsing and the shared expiration timestamp
overflow validator are unchanged.

`links::PermissionSelect` supplies its own registry Field context with explicit
ID, name, and errors via `with_meta_values`, then renders FieldLabel,
`NativeSelect::<String>`, native help, and FieldError. Its five options use
explicit `form_value` strings: `pull`, `triage`, `push`, `maintain`, and `admin`,
not NativeSelect's positional defaults. No placeholder or empty option is added.

Unlike bound Inputs, permission retains a controlled display memo even in
the browser. Unsupported values such as `owner` and the empty string display
`pull` but remain raw in dioform for validation. NativeSelect sets the DOM value
property, so an unmatched value would otherwise blank the select. The old native
markup left all options without `selected` for invalid values; shared SSR/island
markup now explicitly selects `pull`, preserving the first-option display and
native POST semantics. Display normalization never writes back to the binding.

Metadata supplies the controls' ID, name, error color (`input-error` or
`select-error`), and `aria-invalid` (`"false"` when valid), plus Input's required
state. FieldLabel targets the metadata ID.
FieldError owns an always-mounted `div` with `aria-live="polite"` and nested
error `div`s; clearing errors empties the corresponding `description-error`,
`max_uses-error`, `expires_in_days-error`, or `permission-error` region rather
than removing it.

Two upstream limitations keep some wiring application-owned:

- Explicit `aria-describedby` and `aria-errormessage` preserve first-pass SSR
  associations: help/error siblings render after Input or NativeSelect, too late
  for their registration to supply the control's initial attributes. Native help does not register
  with metadata at all. Stable control and part IDs remain application-owned.
- Help stays a native `p` with the existing prose classes. FieldDescription
  forces daisyUI's `label` class even with `FieldDescriptionAppearance::None`,
  which does not preserve the application's help styling.

Metadata reduces duplication but does not remove all application wiring.
Only the new-link numeric fields migrated. Legacy `FieldKind::Number` controls
elsewhere, checkboxes, and textarea retain their native rendering and listeners.
No installed upstream source was edited, no dependencies were added, and the
source installation and update instructions below are unchanged.

The [island adapter](../../../ghinvite-island/README.md#registry-input-integration)
converts dioform's description, permission, max-use, and expiration bindings
through the shared `commit_on_focus_exit` helper. Writes preserve their origin; native-change commit
is ignored, and blur calls `commit()` then `focus_exit()`, including for unchanged
fields. NativeSelect writes on native input, not native change. See the
[browser regression suite](../../../../tests/browser/README.md)
for real-bundle, native POST, accessibility, and mobile coverage.

## Provenance

| Local directory | Upstream path | Exact commit |
| --- | --- | --- |
| `button/` | `src/components/button/` | `8bfd667ee335b4438f39808d6e9155dc808f18f4` |
| `alert/` | `src/components/alert/` | `8bfd667ee335b4438f39808d6e9155dc808f18f4` |
| `input/` | `src/components/input/` | `8bfd667ee335b4438f39808d6e9155dc808f18f4` |
| `native_select/` | `src/components/native_select/` | `8bfd667ee335b4438f39808d6e9155dc808f18f4` |
| `field/` | `src/components/field/` | `bb04b3e0d563d21c458fdd050ecd95f671c91408` |

Field is installed transitively at the exact revision declared by both Input's
and NativeSelect's `component.json`, not at the top-level registry revision. Each installed
directory contains upstream `component.rs` and `mod.rs`, byte-for-byte unchanged.
Their Git blob hashes were checked against the respective upstream trees.
The manifests exclude `component.json`, `README.md`, and `docs/` from installation.
Use the commit-pinned upstream paths above to consult those authoring files.

The root `mod.rs` is application-owned and contains shared application components,
the five registry module declarations, and an SSR smoke test. Keep application
adaptations outside the replaceable upstream directories.

## Licenses

The installed source is licensed under **MIT OR Apache-2.0**, at your option.
[`LICENSE-MIT`](LICENSE-MIT) and [`LICENSE-APACHE`](LICENSE-APACHE) preserve the
upstream license texts and copyright notice exactly. Both upstream commits have
identical license files; no upstream `NOTICE` file is present.

The Cargo dependencies remain separate dependencies under their own licenses:
`dioxus-field` 0.7.0 and `dioxus-primitives` (`MIT OR Apache-2.0`). No dependency
source was copied into these installed directories.

## Installation

Installed using Dioxus CLI **0.7.9** (`dioxus 0.7.9 (3e43ffa)`). With that version
of `dx` on `PATH`, run from `crates/ghinvite-ui`:

```sh
dx components add button alert input native_select \
  --git https://github.com/sagikazarmark/dioxus-daisyui-components.git \
  --rev 8bfd667ee335b4438f39808d6e9155dc808f18f4 \
  --module-path src/components
```

This is the initial-install command, for a tree without those component
directories. If converting the old layout, move `src/components.rs` to
`src/components/mod.rs` first; do not leave both module files in place.

NativeSelect alone was added to the existing installation with the same CLI at
`/tmp/opencode/dx`, without `--force` so the existing Field was not overwritten:

```sh
/tmp/opencode/dx components add native_select \
  --git https://github.com/sagikazarmark/dioxus-daisyui-components.git \
  --rev 8bfd667ee335b4438f39808d6e9155dc808f18f4 \
  --module-path src/components
```

Its manifest's Field pin matches the existing `bb04b3e0d563d21c458fdd050ecd95f671c91408`.
Its Cargo dependencies were already present; adding NativeSelect changed neither
the UI Cargo manifest nor the workspace lockfile.

The installer adds only these two direct dependencies to the UI Cargo manifest:

- `dioxus-field`: version `0.7.0`, default features disabled.
- `dioxus-primitives`: Git `https://github.com/DioxusLabs/components`, exact commit
  `bf007c15d0cf4d04d3181cc46cf12325aa773955`, version `0.0.1`, default features
  disabled, feature `router`, as declared by the registry manifests.

Keep the resulting workspace `Cargo.lock`. No workspace dependency declaration
was needed. The registry emits daisyUI classes and Tailwind utilities only; the
application remains responsible for its stylesheet and scanning these sources.

## Reinstallation And Updates

`dx components update` refreshes the registry cache, not installed source.
To reproduce this installation, run these commands from `crates/ghinvite-ui`:

```sh
dx components add field --force \
  --git https://github.com/sagikazarmark/dioxus-daisyui-components.git \
  --rev bb04b3e0d563d21c458fdd050ecd95f671c91408 \
  --module-path src/components
dx components add button alert input native_select --force \
  --git https://github.com/sagikazarmark/dioxus-daisyui-components.git \
  --rev 8bfd667ee335b4438f39808d6e9155dc808f18f4 \
  --module-path src/components
cargo check -p ghinvite-ui
cargo test -p ghinvite-ui
```

Install Field explicitly first because `dx` skips already-present transitive
components, even when `--force` overwrites the explicitly requested components.
For an intentional upgrade, inspect the new Input and NativeSelect manifests,
confirm their Field dependency pins agree, replace both commit
arguments with the reviewed registry and dependency pins, and review all source
and Cargo changes. Preserve any local work before using `--force`; it replaces
the selected component directories. Refresh this provenance and the license
copies from those exact commits. Do not use `--all` or add the registry's Cargo
library/preview dependency.

## API And SSR Notes

- Import through `ghinvite_ui::components::{button, alert, input, field, native_select}`.
  `components::field` is the registry wrapper, distinct from the existing
  application module `ghinvite_ui::field`.
- Input is the sagikazarmark native, field-aware implementation. `binding` takes
  `Option<dioxus_field::Binding<String>>`; `meta` takes `Option<FieldMeta>`.
  Resolution is explicit prop, then Field context, then standalone state. These
  are not dioform bindings; the island enables dioform's `dioxus-field` feature
  and adapts the converted binding for the application's validation timing.
- NativeSelect is generic over the option value type `T`; `binding` takes
  `Option<dioxus_field::Binding<Option<T>>>`, and controlled `value` takes
  `Option<ReadSignal<Option<T>>>`. Binding and metadata resolve from explicit
  props, then Field context, then standalone state. An explicit display value
  overrides the binding's read, not its writes. Native input maps the option's
  form string back to `Some(T)` and writes with user origin; empty or unknown
  strings are ignored. Native change calls commit; focus exit reports blur.
- Input's `value` is `Option<ReadSignal<String>>`, not a plain string or `Signal<String>`.
  For a signal named `value`, pass `value: Some(value.into())`. A binding can be
  passed as `binding: Some(value.into())` instead. The explicit `value` overrides
  what is displayed but input events still write through the resolved binding;
  controlled callers must keep their displayed value synchronized.
- Events are `on_change(EventHandler<String>)` on native input,
  `on_commit(EventHandler<()>)` on native change, and `on_focus_exit` on blur.
  There is no native `oninput`/`onchange` prop forwarding via extended attributes.
- Input forwards native/global attributes to the actual input and merges caller
  attributes after metadata; classes concatenate. Metadata supplies an ID and
  validity/state attributes even for standalone inputs. For stable server/client
  associations, supply explicit metadata IDs (and explicit part IDs when needed).
  Overriding just the native `id` does not update the ID that FieldLabel targets.
  Likewise, overriding `aria-invalid` alone does not set metadata invalidity or
  automatically select `input-error`.
- `required` and `disabled` are optional boolean props. `name`, `type`,
  `readonly`, ARIA, and data attributes use the native/global spread surface.
  The SSR smoke test covers explicit ID, name, type, value, required, readonly,
  ARIA/data attributes, and caller classes. Browser interactions, hydration, and
  compound Field label/error registration are not covered by that smoke test.
- Prefix/suffix adornments move daisyUI styling onto a `span.input` wrapper;
  native attributes still target the input. Use `wrapper_attributes` for the
  wrapper's layout/classes. Toggling adornment presence changes the element tree.
- Button has `onclick` and native/global attributes but no explicit appearance
  axis. Supply classes for ghost/outline variants and an explicit `type` where
  native submit behavior is not wanted. Alert defaults to horizontal and adds no
  live-region role; callers choose `role`/ARIA behavior.
