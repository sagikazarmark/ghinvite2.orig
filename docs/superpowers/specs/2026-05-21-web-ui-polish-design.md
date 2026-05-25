# ghinvite Web UI Polish: Design

**Status:** approved for implementation planning  
**Date:** 2026-05-21  
**Scope:** Product UI polish for the existing Dioxus SSR web views in `crates/web/src/views` and the route/read-model support needed to render safer admin and recipient workflows.

## 1. Product Goal

The current web UI is functional but reads as default DaisyUI scaffolding. This polish pass should make ghinvite feel trustworthy for high-stakes GitHub collaborator access decisions while keeping the existing Dioxus SSR, Tailwind, and DaisyUI stack.

The work is a product UI hardening pass, not a visual redesign or design-system extraction. It should improve safety, clarity, accessibility, and responsive behavior with the smallest correct changes.

## 2. Design Principles

- **Access decisions must feel consequential.** Creating an invitation link, approving a request, and stopping a link all affect GitHub repository access. The UI must make those consequences visible before the user acts.
- **Keep familiar product affordances.** Use standard forms, tables, buttons, alerts, and navigation. Do not invent custom interaction patterns for basic admin workflows.
- **Prefer clear copy over decorative styling.** The interface should gain specificity through labels, helper text, summaries, and state copy rather than heavy visual treatment.
- **Make mobile structurally usable.** The dashboard can remain denser on desktop, but all primary navigation and actions must remain reachable on small screens.
- **Improve accessibility while touching the forms.** Inputs should have visible labels and stable associations where practical.

## 3. Target Surfaces

This spec covers these existing surfaces:

- Home page: `crates/web/src/views/home.rs`
- Shared chrome: `crates/web/src/views/layouts.rs`, `crates/web/src/views/components.rs`
- Dashboard overview: `crates/web/src/views/dashboard.rs`
- Link creation and detail: `crates/web/src/views/links.rs`, `crates/web/src/routes/dashboard.rs`
- Approval queue: `crates/web/src/views/requests.rs`, `crates/web/src/account_admin_reads.rs`, `crates/web/src/routes/dashboard.rs`
- Settings: `crates/web/src/views/settings.rs`
- Recipient invitation flow: `crates/web/src/views/invitation.rs`

## 4. Link Creation

### Behavior

The new-link form should default to safer values:

- Permission: `pull`
- Expiration: `30` days
- Max uses: blank remains unlimited, but the form must explain that blank means unlimited
- Approval required: unchecked by default, preserving the current v1 behavior

The route currently constructs `LinkFormValues::default()`. That default should carry the safer form values so the initial render and validation errors can reuse the same shape.

### UI

The form should become a guided access setup rather than a raw config list:

- Add a short page intro explaining that the link grants repository collaborator access.
- Add helper text under permission, max uses, expiration, approval mode, internal note, and repositories.
- Add explicit risk copy near elevated permissions (`maintain` and `admin`). Static server-rendered copy is acceptable for this pass; no client-side dynamic warning is required.
- Keep the repo picker simple, but show a selected-repository count or explanatory empty state if no repos are available.
- Use explicit labels for controls. Placeholder-only labeling is not acceptable.

## 5. Link Detail

The link detail page should become the operational hub for an invitation link.

### Required Content

Show these facts without requiring admins to remember what the slug means:

- Share URL
- Active or inactive status
- Permission level
- Uses and max uses
- Expiration or "No expiration"
- Approval mode
- Repository list
- Internal note when present

The existing `InvitationLink` domain object already includes most of this data. Render it directly from `LinkDetailPage` where possible rather than introducing a new read model.

### Actions

- Add a "Copy link" control if it can be done without requiring hydration. If clipboard behavior requires client JavaScript outside the current architecture, use a copy-friendly readonly input and make "Open recipient preview" the reliable action.
- Add "Open recipient preview" linking to the public invitation URL.
- Replace "Revoke this link" with "Stop accepting new requests".
- Add consequence copy: stopping a link prevents new requests but does not cancel requests or invitations already in progress in v1.
- The destructive action may remain a POST button, but it must be visually and textually separated from informational actions.

### Route Messages

Flash copy should match the user-facing terminology:

- Success: "Link stopped accepting new requests."
- Error: "Could not stop this link. Please try again."

## 6. Approval Queue

The approval queue must give admins enough context to approve safely.

### Data

Extend `AccountAdminPendingRequestRow` to include invitation-link context needed by the queue:

- Permission
- Repositories, or at minimum repository count plus names when available
- Link expiration
- Approval mode if useful for clarity

This data is already available while hydrating each pending row from the associated `InvitationLink`. Prefer extending the existing row over adding a second storage query path.

### UI

Each request row should show:

- Requester login
- Link slug and link detail URL
- Requested permission
- Repository names or a compact repository summary
- Optional justification
- Created time
- Approval consequence: approving sends GitHub collaborator invitations for the listed repositories

The approve and decline forms can remain inline POST buttons. On small screens, the content and actions should stack instead of squeezing into a single row.

## 7. Dashboard Overview And Navigation

### Overview

The overview should feel less like two generic metric cards and more like an admin starting point:

- Keep pending request and active link counts, but pair each with a clear action.
- Improve empty state copy for no recent links. It should tell the admin what creating a link does.
- Make the summary grid responsive: single column on small screens, two columns on medium and above.

### Navigation

The dashboard sidebar is currently hidden below `md`. Add a mobile replacement so account navigation remains reachable:

- A compact horizontal tab row below the top nav is preferred.
- It should include Overview, New link, Requests, Audit, and Settings.
- If Audit remains a 501 route, label it "Audit (coming soon)" or remove it from the mobile row and de-emphasize it in the sidebar. Do not make it look equally ready.

## 8. Recipient Flow

Recipient pages should reassure users before and after they request access.

### Landing

- Keep the focused card layout.
- Make the requested permission and repository list easier to scan.
- Preserve the sign-in CTA.
- Add concise copy explaining that GitHub sign-in confirms the recipient identity.

### Request Form

- Keep the wrong-account guard: "Signed in as @login" plus sign-out link.
- Add a visible label for the optional justification textarea.
- Clarify that the justification is optional and visible to account admins.

### Pending Status

- Replace bare "Refresh to check status" copy with a small status explanation and a visible link/button to refresh the current page.
- For approved status, tell the user to check GitHub notifications and email if GitHub sends one.
- For declined, expired, and cancelled states, keep copy short and specific.

## 9. Settings And Home Copy

### Settings

Settings is read-only in v1, but it should not feel like a placeholder. It should present useful account status:

- Account login and type
- Repository access scope
- Note that repository selection changes happen through the GitHub App installation settings for now
- "Edits coming in v1.1" may remain, but it should be secondary

### Home

Keep the current simple marketing page, but avoid promising an admin-facing audit trail if the audit UI is not available yet. Phrase audit as captured history rather than a fully browsable UI.

## 10. Accessibility And Responsive Requirements

- Dashboard summary grids and link detail grids must use `grid-cols-1 md:grid-cols-2` or equivalent.
- Request rows must stack actions on small screens.
- Form controls should have visible labels and, where Dioxus markup makes it practical, matching `for` and `id` attributes.
- Avoid placeholder-only labels.
- Avoid relying solely on low opacity for important secondary text.
- Keep focusable actions as native links and buttons.

## 11. Out Of Scope

- New brand identity, custom theme tokens, or replacing DaisyUI.
- Client-side hydration for dynamic field warnings.
- Full copy-to-clipboard JavaScript if it does not fit the current server-rendered architecture.
- Audit log implementation.
- Search, filtering, or pagination for very large repository lists.
- Confirmation modals. If confirmation is needed, use inline consequence copy before the existing POST action.

## 12. Testing Strategy

- Run `cargo fmt` after code changes.
- Run `cargo test -p web` to cover route/view changes and account-admin read-model tests.
- If CSS classes are changed or added, run `npm run build:css` in `crates/web`.
- Verify generated CSS changes only if `styles.built.css` is tracked or expected in this repository.

## 13. Acceptance Criteria

- New invitation links render with `pull` and `30` day expiration defaults.
- The link detail page no longer uses "revoke" in user-facing copy.
- Link detail shows permission, usage, expiration, approval mode, repositories, and a recipient preview action.
- Pending request rows include permission and repository context before approve/decline actions.
- Dashboard navigation remains available on mobile widths.
- Main grids and request rows have small-screen layouts.
- Recipient request textarea has a visible label.
- Pending recipient status includes a visible refresh affordance.
- Settings reads as an account status page rather than a placeholder.
- The home page does not imply a finished audit-log UI.
