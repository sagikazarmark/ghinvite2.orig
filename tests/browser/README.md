# Browser Regression Tests

Playwright tests the registry integration through the production new invitation
link page: `LinkCreateFormPage`, the application's `Field` wrapper with registry
Field context, FieldLabel, Input, and FieldError for description; Field context,
FieldLabel, NativeSelect, and FieldError for permission; registry numeric Inputs
and FieldErrors for max use and expiration; and the real browser island.
There is no substitute UI, mock island, or test-only CSS.
The local Node server requires no GitHub login, database, Worker, or Restate.

## Run

Requirements: Node.js 20+, the repo's Rust toolchain with the
`wasm32-unknown-unknown` target, and Dioxus CLI **0.7.9** (`dx` on `PATH`).
From the repository root:

```bash
npm ci --prefix crates/ghinvite-web
npm run build:css --prefix crates/ghinvite-web
scripts/build-island.sh
npm ci --prefix tests/browser
npm exec --prefix tests/browser -- playwright install --with-deps chromium
npm test --prefix tests/browser
```

Rebuild CSS and the island after changing production components. The test server
re-runs `cargo run -p ghinvite-island --example ssr_fixture` at startup, so SSR is
always current. Missing CSS/bundle files or a changed CSP constant format fail
startup rather than silently using fallback assets. Build the island on its own
with `-p`, not a workspace-wide wasm build (ADR 0001).

Run one project, repeat for flakes, or inspect a failure:

```bash
npm test --prefix tests/browser -- --project=desktop-light
npm test --prefix tests/browser -- --project=desktop-light --grep 'numeric max_uses' --workers=2
npm test --prefix tests/browser -- --project=desktop-light --grep 'browser rejection' --workers=2
npm test --prefix tests/browser -- --grep 'numeric' --workers=2
npm test --prefix tests/browser -- --project=mobile-light --project=mobile-dark
npm test --prefix tests/browser -- --repeat-each=3 --workers=2
npm run test:headed --prefix tests/browser -- --project=mobile-dark
npm exec --prefix tests/browser -- playwright show-report tests/browser/playwright-report
```

Playwright owns `127.0.0.1:4173`, never reuses an existing server, and shuts it
down afterward. For manual inspection, `npm run serve --prefix tests/browser`
starts the same server. Generated reports, screenshots, and traces are ignored.
No fixture HTML is written to `dist/public`, where it could shadow real routes
if accidentally deployed.

## Fixture Contract

- `/`: fresh form with three real repository choices.
- `/failed`: existing `--with-errors` fixture, with every validation error.
- `/preserved`: `--preserved-values`, rejected description with otherwise valid
  values, checked approval, and multiple selected repositories.
- `/permission-invalid`: `--permission-only-invalid`, raw `owner` with a
  permission error and summary, a valid description, and repository 10 selected.
- `/permission-empty`: `--permission-only-invalid --empty-permission`, the same
  permission-only failure with an empty raw value.
- `/permission-unvalidated`: `--permission-only-invalid --unvalidated`, raw
  `owner` without initial errors, for unchanged focus-exit validation.
- `/max_uses-invalid`: `--numeric-only-invalid`, raw `abc` with only a max-use
  error and summary, a valid description, and repository 10 selected.
- `/expires_in_days-invalid`: `--numeric-only-invalid --expiration`, the same
  numeric-only failure for expiration instead of max use.
- `/rejection-mixed`: `--browser-rejection`, a client-reproducible whitespace
  description error alongside a synthetic server-only permission error and
  form-level summary message. Permission `push` is valid to the client.
- `/rejection-max_uses` and `/rejection-expires_in_days`: `--browser-rejection
  --parse-blocked` (plus `--expiration` for the latter), the mixed rejection
  with raw `abc` and its parse error on the named numeric field.
- `/rejection-form`: `--browser-rejection --form-only`, valid values with only
  a synthetic form-level rejection, for an unchanged native POST retry.
- `/assets/*`: real JS/Wasm staged by `scripts/build-island.sh`, with Wasm MIME.
- `/static/styles.css` and `/static/app.js`: production CSS and theme script.
- `POST /console/accounts/acme/links`: JSON echo of ordered form entries,
  preserving duplicate keys. It deliberately does not implement validation or
  create links. Server validation remains covered by Rust route tests.

HTML is the fixture's unmodified production renderer output. The HTTP CSP is
read from `ghinvite-web/src/middleware/csp.rs`; no relaxed test policy or
`bypassCSP` option is used.

The `Transport fixture:` messages are deliberately fabricated response payloads
to exercise dioform 0.7 `BrowserRejection` transport. They do not represent a
duplicate-description feature or any additional domain validation. The fixture
injects only values/errors into the production renderer; lifecycle behavior
comes from the real compiled island bundle. When another session owns the
production migration/build, wait for its staged bundle before running these
tests; rebuilding SSR at server startup does not rebuild Wasm.

## Coverage

- Successful Wasm mount with one form, no runtime/CSP errors, and real assets.
- Description validates on focus exit, including an unchanged empty field, not
  during initial typing. Correct-invalid-correct transitions update metadata's
  error color and `aria-invalid` (`"false"` when valid), retain unique IDs and
  label targeting, and update explicit help/error associations.
- Description, permission, and numeric FieldErrors are always-mounted polite live-region
  `div`s with nested error `div`s. Correction empties them instead of removing them.
- Numeric controls retain unique IDs/names, native help paragraphs, label click
  targeting, minimum 1, and explicit ARIA associations in mounted and SSR-only
  modes. Valid controls report `aria-invalid="false"` without error styling.
- Repeated invalid raw edits (`0`, `00`, `000`) keep the same parse error and
  retain their text through unrelated renders. Invalid-valid-empty-invalid-valid
  transitions update metadata without replacing the error region.
- Native-valid `4294967296`, `1e2`, and `1.0` are rejected by the numeric parsers
  and block progressive POST. Expiration also rejects `4294967295`, which fits
  `u32` but overflows chrono's timestamp range. Max use accepts that `u32` limit.
- Corrected numeric values and explicitly cleared optional numbers submit
  exactly once under their native names in mounted, no-JS, and blocked-bundle
  modes. The POST echo asserts empty strings, not Rust `None`; server-side
  optional-value semantics remain the responsibility of the Rust tests.
- Permission renders exactly five options with explicit `form_value` strings:
  `pull`, `triage`, `push`, `maintain`, and `admin`. Each submits exactly once as
  `permission` in mounted, JavaScript-disabled, and bundle-blocked modes, never
  as a positional index. There is no placeholder or empty option.
- Raw `owner` and empty permission survive mounting in dioform while the select
  displays `pull`. Native validity passes, but progressive submit stays blocked;
  unchanged focus exit does not repair the model. Choosing another supported
  option and then `pull` clears the error and allows native POST.
- An unvalidated unsupported permission becomes invalid on unchanged focus exit.
  Malformed DOM input (`owner`, empty, or `0`) cannot silently repair it. Native
  input writes through the binding; both registry adapters use
  `commit_on_focus_exit` to ignore native-change commit and call `commit()` then
  `focus_exit()` on blur.
- Native `required` and `min` constraints block invalid submissions.
- Whitespace description and missing repository scope pass native constraints
  but are blocked by dioform; correcting them allows a document-navigation POST,
  not fetch/XHR. Tests never bypass constraints with `form.submit()`.
- Failed values, summary, inline errors, and `aria-describedby`/`aria-invalid`
  survive mounting, as do description's and permission's `aria-errormessage`.
  Valid controls are not reset while correcting a failed one.
- Mixed client/server-only rejection messages and numeric raw parse failures
  retain the same messages and error associations in mounted, no-JS, and
  blocked-bundle modes. Summary entries and inline messages are not duplicated.
- Parse-blocked progressive preflight retains restored field and form errors
  across repeated submit attempts. Correcting either numeric field clears its
  error; the next submit retires server-only rejection messages and validates
  the untouched whitespace description on the client before permitting POST.
- Editing a related field clears its restored error immediately, before blur;
  other field errors and the form-level rejection remain. Unrelated rerenders
  neither replay the original rejection nor restore invalid numeric text.
- A server-only form rejection permits an unchanged, native-valid retry as a
  document-navigation POST in all three modes. Field edits alone retain that
  form-level message until fresh preflight retires it.
- JavaScript-disabled and bundle-blocked forms remain usable and submit natively.
- Label clicks, keyboard Space, repeated repository keys, checked approval and
  unchecked omission, and empty optional numbers keep native behavior.
- Desktop (1440 x 1000) and mobile Chromium (390 x 844), light and dark:
  responsive navigation, horizontal overflow, reachable/named controls, keyboard
  focus order, and targeted axe label/button/ARIA rules.

Important boundaries:

- These tests cover the application's explicit `aria-describedby` and
  `aria-errormessage`, not automatic first-pass SSR sibling registration. Input
  and NativeSelect render before help/error siblings; native help does not
  register with Field metadata and stays a `p` to avoid FieldDescription's forced
  `label` styling.
- Native constraint failures need not show dioform errors: the browser can stop
  before `submit` fires. Tests assert native validity, not validation-popup text.
- Chromium sanitizes `value="abc"` in a number input to empty. The all-errors
  fixture checks the preserved props and visible error, not impossible DOM text.
  Numeric-only fixtures additionally prove that unchanged blur and correction of
  other controls cannot silently repair the raw invalid model: native validity
  passes but progressive POST stays blocked until the numeric control is edited.
  To explicitly clear a sanitized-empty field, tests enter a number first, then
  clear it; filling an already empty DOM input need not emit an input event.
  Decimal rejection uses `1.0`, which satisfies native integer-step constraints,
  rather than mistaking native rejection of a fraction for progressive validation.
  Unavailable repository IDs are not selectable controls. Unsupported permission
  remains raw in props/dioform, but a controlled memo displays `pull` in both SSR
  and the browser, unlike description's direct bound display. The old markup
  omitted `selected` for invalid permission and relied on the browser's first
  option; now shared SSR/island markup explicitly selects `pull`. Display and
  native POST semantics are preserved, not the old attribute shape. Without the
  island, an unchanged fallback submits the displayed `pull` natively; with the
  island, dioform still rejects the raw invalid model until a supported input.
- The app reads `ghinvite-theme` from local storage, rather than the OS color
  scheme. Tests set that preference before loading production `app.js`. With JS
  disabled the production SSR theme stays light, even in a dark-named project.
- Mobile projects enable Chromium's `isMobile` and `hasTouch` with a 390 x 844
  phone viewport, not just a resized desktop window. Production `ConsoleLayout`
  emits `width=device-width, initial-scale=1`; the fixture does not inject it.
  The responsive test catches a missing viewport meta through navigation and
  control geometry. No claim of physical-device or iOS/WebKit coverage is made.
- Accessibility checks cover the migrated form's names, error wiring, and valid ARIA,
  not a full WCAG/contrast audit. Layout uses geometric assertions rather than
  platform-dependent pixel snapshots. Failure screenshots/traces support review.

## CI

The existing **Island bundle** job builds the real island once, builds production
CSS, installs the lockfile-pinned Playwright/Chromium version, and runs all four
projects. It uploads HTML reports, screenshots, and traces on failure. Two workers
and one CI retry limit resource contention while retaining failure diagnostics.
