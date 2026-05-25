# Main Page And Account Console Split Design

## Context

Issue #16 asks to reorganize the application into two clearer surfaces:

- A user-facing public surface for the main page and recipient invitation flow.
- An admin-facing Account Console for account admins.

`CONTEXT.md` defines the relevant domain language: Public Surface, Recipient Flow, Account Console, Invitation Link, and Invitation Code. User-facing copy may call an invitation link an invitation link when speaking to non-admins.

The current web app routes account-admin pages under `/accounts/{login}` and redirects signed-in users from `/` to the first active account. The new design removes that redirect, moves account-admin pages under `/console`, and keeps `/` as the public entry point for everyone.

This design intentionally supersedes older dashboard/account-route wording in prior specs. The new canonical route namespace is `/console/accounts/{login}`.

## Goals

- Make `/` a user-facing home page for both signed-out and signed-in users.
- Move account-admin pages into the Account Console namespace under `/console`.
- Provide a generic authenticated `/console` entry point that can redirect, show an account picker, show an empty state, or show an inline load error.
- Preserve authorization concealment after authentication for account-scoped console URLs.
- Replace plain browser-facing 404s with real 404 pages where safe.
- Keep the recipient flow focused by removing normal header controls from invitation pages.
- Rename dashboard concepts to console across code, tests, CSS, and user-facing admin-shell naming.

## Non-Goals

- No compatibility routes or redirects for old `/accounts/...` URLs.
- No tests for old `/accounts/...` behavior.
- No server resolver endpoint for Invitation Code entry.
- No URL/link parsing in the Invitation Code input.
- No new setup-incomplete recovery flow for installations visible to GitHub but absent from local storage.
- No ADR. The route split is captured through domain language and this spec.

## Chosen Approach

Use `/console` as the authenticated boundary and `/console/accounts/{login}` as the account-scoped console namespace.

Alternatives considered:

- Link the header directly to the first account console. Rejected because it hides multiple-account context and makes the global header depend on account selection.
- Keep `/accounts/...` as compatibility redirects. Rejected because the desired split is explicit and old route tests should not remain.
- Add a server-side Invitation Code resolver. Rejected because the desired shortcut should directly navigate to `/i/{code}` without validation or a separate endpoint.

## Routing

Public routes:

- `GET /` always renders the public home page. It never redirects signed-in users to an account console.
- Recipient flow routes remain under `/i/{slug}`.
- OAuth, install, setup, health, webhook, static, and fallback routes keep their existing public or infrastructure roles unless noted below.

Console routes:

- `GET /console` is the generic Account Console entry point.
- Account-scoped console routes move from `/accounts/{login}` to `/console/accounts/{login}`.
- Existing account-scoped subroutes keep their shape below the new prefix, for example `/console/accounts/{login}/links/new`, `/console/accounts/{login}/requests`, `/console/accounts/{login}/audit`, and `/console/accounts/{login}/settings`.
- After GitHub App setup returns successfully, redirect to `/console/accounts/{account_login}`.

Removed routes:

- `/accounts/...` is no longer an account-console namespace.
- The app should not generate `/accounts/...` links.
- Tests should not assert old `/accounts/...` behavior.

## Authentication And Authorization

All `/console...` URLs redirect unauthenticated users to `/login?return_to=` plus the URL-encoded original console path before account authorization checks run.

`return_to` validation should allow `/console` and `/console/...` while preserving open-redirect and traversal protections. Existing invitation and setup return targets remain allowed.

Logout ignores `return_to` entirely and always redirects to `/`.

After authentication:

- `/console` derives the accounts the user can administer.
- `/console/accounts/{login}` validates that the user is an Account Admin for that account.
- Unknown accounts, uninstalled accounts, and non-admin access render a real generic 404 page, not console chrome.
- Missing nested console pages render a console-shell 404 only after the user has already been verified as an Account Admin for that account.

This preserves the existing concealment rule for account-scoped resources while making `/console` a normal authenticated entry point.

## Console Account Discovery

`/console` should derive account choices from the intersection of:

- GitHub App installations visible to the signed-in GitHub user through `/user/installations`.
- Active installations stored locally.

Then filter to accounts where the signed-in user is currently an Account Admin:

- For personal-account installations, accept when `session.login == account_login`.
- For organization installations, use the existing GitHub organization membership/admin check.

States:

- Exactly one administered account: redirect to `/console/accounts/{login}`.
- Multiple administered accounts: render an account picker sorted alphabetically by login.
- Zero administered accounts: render a focused empty state with primary `Install GitHub App` linking to `/install` and secondary `Go home` linking to `/`.
- GitHub installation discovery failure: render an inline error state on `/console` with retry, install, and home recovery actions.
- Admin verification failure for any account: fail the account picker with an inline retryable error state instead of rendering a partial list.

The account picker should show account login and account type labels using glossary language: `Personal account` and `Organization`.

## Home Page

The home page remains part of the Public Surface.

Signed-out home:

- Concise explanatory text about requesting or managing GitHub repository access.
- Primary action: `Sign in with GitHub`.
- No feature-card section.
- No Invitation Code shortcut.

Signed-in home:

- First task: open an invitation by Invitation Code.
- Field label: `Invitation Code`.
- Placeholder: `Paste code`.
- Helper text: `Enter the code from an invitation link to request repository access.`
- Button: `Open invitation`.
- Behavior: trim leading/trailing whitespace, preserve case, require non-empty, then navigate to `/i/{encodeURIComponent(code)}`.
- Empty input shows an inline client-side message such as `Enter an invitation code first.` in an `aria-live="polite"` region.
- Enter key in the input triggers the same behavior as the button.
- The entered value is always treated as a code. Do not parse full URLs or links.
- Second task: create an invitation link.
- Show a vertical separator between tasks.
- Action label: `Create invitation link`.
- Action target: `/console`.

The shortcut can use a small inline script in the Dioxus view. The current CSP helper is not applied globally, and the app already contains inline scripts. Do not expand CSP work in this issue.

## Header And Layouts

Normal header:

- Used on public home, public real 404 pages, and console pages.
- Signed-out controls: theme toggle and `Sign in`.
- Signed-in controls: theme toggle, small primary `Console` button linking to `/console`, low-emphasis `@login`, and `Sign out`.

Recipient flow layout:

- Do not render the normal header.
- Render only a small upper-left ghinvite logo link to `/`.
- No theme toggle, no sign-in/sign-out controls, and no Console button.
- Keep the invitation flow focused on completing the access request.

Console layout:

- Rename dashboard shell concepts to console shell concepts.
- The account sidebar appears only after account context is selected.
- Console sidebar links keep admin-facing domain language: `Overview`, `New invitation link`, `Pending requests`, `Audit log`, and `Settings`.
- Console routes and links use `/console/accounts/{login}`.

## Invitation Status Pages

Request status pages are authenticated recipient-flow pages.

They should keep state-specific actions where useful, such as `Check again` for pending/processing states.

Every request status state should also show exit actions:

- Primary: `Create your own link`, linking to `/console`.
- Secondary: `Go home`, linking to `/`.

## 404 Behavior

Real browser-facing 404 pages should be rendered where safe.

Rules:

- Unknown public page `GET` routes render a public real 404 page.
- Recipient-flow failures render a recipient-layout 404 page, using the focused logo-only invitation layout.
- Console URLs redirect unauthenticated users to login first.
- Authenticated but unauthorized or unknown console account contexts render a generic real 404 page with normal public/app header controls, not account sidebar chrome.
- Verified account admins hitting missing nested console pages render a console-shell 404 page.
- Console-shell 404 copy: `This console page is not available.`
- Public 404 primary action: `Go home`.
- Non-GET action failures and asset-like misses can remain plain 404 responses.

## Repo-Wide Rename

Rename dashboard concepts to console concepts across the repo.

Expected scope:

- Route modules and comments, for example `dashboard` route module becomes `console`.
- View modules and components, for example `DashboardLayout` becomes `ConsoleLayout`.
- Test names and test files where appropriate.
- CSS class names, for example `dashboard-frame` becomes `console-frame`.
- User-facing admin-shell wording where `dashboard` appears.

Keep canonical domain terms intact:

- `Account Console` for the admin-facing account workspace.
- `Invitation Link` in admin-facing console copy.
- `Invitation link` as public/home copy when speaking to non-admins.

## Testing

Update tests to assert the new behavior only.

Coverage should include:

- Signed-out `/` renders concise public home copy and sign-in CTA.
- Signed-in `/` does not redirect and renders the Invitation Code shortcut plus `Create invitation link` action.
- The Invitation Code view includes the client-side empty-code error region and navigation script behavior markers.
- Normal header signed-in controls include `Console`, `@login`, and `Sign out` outside recipient pages.
- Recipient layout does not render normal header controls and keeps only logo navigation.
- `/console` unauthenticated redirects to `/login?return_to=/console`.
- `/console/accounts/{login}` unauthenticated redirects to login with the original console path.
- `/console` redirects to the only administered account.
- `/console` renders an alphabetized account picker for multiple administered accounts.
- `/console` renders a no-accounts empty state when no administered active installations remain.
- `/console` renders inline retryable error states for GitHub discovery/admin-verification failures.
- Account-scoped console routes render existing pages under `/console/accounts/{login}`.
- Authorized missing nested console routes render console-shell 404 with no active sidebar item.
- Authenticated unauthorized/missing account contexts render a generic real 404 without console sidebar chrome.
- Setup return redirects to `/console/accounts/{account_login}`.
- Logout always redirects to `/`.
- `validate_return_to` accepts `/console` and `/console/...` but rejects external URLs and traversal.

Do not add or keep tests whose purpose is to assert old `/accounts/...` behavior.

## Acceptance Criteria

- `/` no longer redirects signed-in users to an account page.
- Signed-out home is reduced to concise explanatory copy and sign-in.
- Signed-in home has a Invitation Code shortcut and a separated `Create invitation link` action.
- All generated account-admin links use `/console/accounts/{login}`.
- `/console` handles one, many, zero, and error account-discovery states.
- All `/console...` URLs authenticate before account authorization checks.
- Unauthorized or missing console account contexts do not reveal account-specific chrome.
- Recipient pages use focused logo-only chrome.
- Invitation status pages include `Create your own link` and `Go home` exits.
- Dashboard naming is replaced with console naming across routes, views, tests, CSS classes, and UI shell concepts.
- Existing domain language remains consistent with `CONTEXT.md`.
