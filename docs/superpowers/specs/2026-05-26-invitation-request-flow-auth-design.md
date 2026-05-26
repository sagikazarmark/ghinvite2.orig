# Invitation Request Flow Auth Design

## Context

Issue #22 requires every invitation page under `/i/{code}` to require a signed-in GitHub user before showing details about the invitation request flow. Current behavior requires sign-in for `/i/{code}/request` and `/i/{code}/pending/{request_id}`, but unauthenticated `/i/{code}` resolves the code and reveals repository and permission details for active links and 404 state for inactive or unknown links.

The canonical domain language is `Invitation Request Flow` for the public journey, `Invitation Code` for the code segment, and `Invitation Request` only after a requester has submitted a request. `CONTEXT.md` now records the resolved repeatability rule: a requester may retry after `Declined`, `Expired`, or `Cancelled`, but may not create another request after a `Pending` or `Approved` request for the same invitation link.

## Goals

- Require sign-in before any `/i/{code}` page reveals code validity, link status, repositories, permission level, or request state.
- Make `/i/{code}` the single requester-facing page for an invitation code.
- Merge the current landing and request form content into one signed-in page.
- Show the signed-in requester their current non-repeatable request status when a `Pending` or `Approved` request exists.
- Allow retry after the newest relevant prior request is `Declined`, `Expired`, or `Cancelled`.
- Keep inactive-link details concealed behind the generic invitation-flow 404.

## Non-Goals

- No separate requester status page.
- No historical request archive on requester-facing pages.
- No compatibility route for `/i/{code}/request`.
- No preservation of unauthenticated POST bodies across OAuth.
- No change to account-admin request review behavior.
- No change to GitHub invitation sending behavior.

## Route Design

`GET /i/{code}` is the only supported requester page for an invitation code.

Unauthenticated `GET /i/{code}` redirects to `/login?return_to=/i/{code}` before resolving the invitation code. This applies equally to valid, malformed, unknown, expired, revoked, and exhausted links.

Unauthenticated `POST /i/{code}` redirects to `/login?return_to=/i/{code}` before resolving the invitation code. The original POST body is not preserved; after OAuth the requester lands on the signed-in form or status page.

Signed-in `GET /i/{code}` looks up the link and the signed-in requester's own requests for that link. It renders the merged form, the current status, or the generic invitation-flow 404. The active-link check happens after existing non-repeatable requester status is considered.

Signed-in `POST /i/{code}` submits a new invitation request only when the requester has no `Pending` or `Approved` request for that link and the link is active. If a non-repeatable request already exists, it redirects back to `/i/{code}` without creating another request.

Nested routes under `/i/{code}/...`, including `/i/{code}/request` and `/i/{code}/pending/{request_id}`, are not supported requester pages. They still require sign-in first and then return the generic invitation-flow 404.

## Request State Behavior

If the signed-in requester has a `Pending` request for the invitation link, `/i/{code}` shows the pending status inline and does not show the form.

If the signed-in requester has an `Approved` request for the invitation link, `/i/{code}` shows the approved status inline and does not show the form.

If the signed-in requester has no `Pending` or `Approved` request, but the newest historical request is `Declined`, `Expired`, or `Cancelled`, `/i/{code}` shows the form with a brief notice explaining the previous outcome and that a new request can be submitted.

If multiple historical retryable requests exist, the page shows only the newest status notice. It does not show full request history.

If the invitation link is inactive but the signed-in requester already has a `Pending` or `Approved` request, `/i/{code}` still shows that existing status. Invitation link expiration, revocation, or exhaustion stops new requests but does not change existing requests.

If the invitation link is inactive and the signed-in requester has no `Pending` or `Approved` request, `/i/{code}` shows the generic invitation-flow 404. It does not say whether the link is expired, revoked, exhausted, malformed, or unknown.

## Page Design

The signed-in form combines the current landing and request form content:

- Repository list.
- Permission level.
- Signed-in GitHub identity.
- Optional `Justification` textarea.
- Submit button.

The page should continue using invitation-flow layout and calm, operational copy. The requester should understand which repositories and permission level they are requesting before submitting.

The status state should reuse the existing request status copy where possible, but render at `/i/{code}` rather than on a separate pending URL.

## UI Design Direction

Physical scene: a requester opens a repository access link from chat or email on a laptop or phone, is not yet sure what access is being requested, and needs one calm confirmation surface after GitHub sign-in.

Register: product. The interface should serve the access decision rather than sell ghinvite. Keep the Stripe-style operational tone from `PRODUCT.md`: precise, quiet, structured, and production-grade.

Color strategy: restrained. Use the existing `ghinvite` OKLCH theme and semantic state colors. Primary color belongs on the single submit action and focus states. Status notices may use semantic info, warning, success, or error treatments, but the text label must carry the meaning so status does not rely on color alone.

Theme: preserve the existing invitation-flow theme and user-selected theme sync. Do not introduce a new dark or marketing-style treatment for this page.

Layout should stay single-surface:

- Use one `InvitationLayout` `mac-panel` as the containing surface.
- Do not introduce nested cards for repositories, identity, or status.
- Put the page in task order: heading, signed-in identity, access summary, prior-status notice if present, justification field, submit action.
- Use varied spacing so the access summary and form read as separate steps without making the page feel like a landing page.
- Keep the panel compact on desktop and fully usable on mobile. The submit action can be full-width on small screens and right-aligned on larger screens.

Access summary should be scannable:

- Show repository names as a compact list, not a grid of cards.
- Show permission level near the repository list, preferably as a restrained badge or property row.
- Keep repository names selectable and readable if they are long. Truncate only when the full value remains available through normal browser selection or wrapping.

Status states should feel like workflow outcomes:

- `Pending`: show a warning or info notice titled `Awaiting review`, with a short explanation that account admins have the request.
- `Approved`: show a success notice titled `Approved`, with copy directing the requester to GitHub notifications and email for GitHub invitations.
- `Declined`, `Expired`, and `Cancelled`: show a compact prior-status notice above the form, then keep the form active.
- Avoid making retryable prior states look like fatal errors. The notice should explain context, not block action.
- Do not make `Create your own invitation link` a primary action in requester status states. It competes with the requester's current task.

Copy should be direct and specific:

- Page heading: `Request repository access`.
- Helper copy: `Review the repositories and submit a request as @login.`
- Submit button: `Submit request`.
- Retry notice examples: `Your previous request was declined. You can submit a new request.` and `Your previous request expired. You can submit a new request.`
- Signed-in identity copy should keep the existing escape hatch: `Not you? Sign out and sign in again.`

Accessibility requirements:

- Keep the `Justification` label associated with the textarea.
- Preserve keyboard-visible focus on links, buttons, and the textarea.
- Keep status labels visible in text, not only color.
- Keep body copy below 75 characters per line where possible.
- Avoid layout shifts or animated page-load effects; any motion should be limited to existing hover or focus feedback.

## Data Flow

`GET /i/{code}`:

1. Load session.
2. If unauthenticated, redirect to `/login?return_to=/i/{code}` without validating the code.
3. Parse the invitation code and look up the invitation link after authentication.
4. If the code is malformed or no link exists, render the generic invitation-flow 404.
5. Load requests for the invitation link.
6. Select only requests belonging to the signed-in requester.
7. Prefer a `Pending` request, then an `Approved` request, as non-repeatable status states.
8. If a non-repeatable request exists, render status even if the invitation link is inactive.
9. If the link is inactive and no non-repeatable request exists, render the generic invitation-flow 404.
10. If no non-repeatable request exists and the link is active, keep the newest retryable request as an optional form notice.
11. Render status, form, or generic invitation-flow 404.

`POST /i/{code}`:

1. Load session.
2. If unauthenticated, redirect to `/login?return_to=/i/{code}` without validating the code or reading the form as a durable intent.
3. Parse the invitation code and look up the invitation link after authentication.
4. If the code is malformed or no link exists, render the generic invitation-flow 404.
5. Check the signed-in requester's existing requests for the link.
6. If a `Pending` or `Approved` request exists, redirect to `/i/{code}` without dispatching a command.
7. If the link is inactive, render the generic invitation-flow 404.
8. Trim blank justification to `None`.
9. Dispatch `SubmitInvitationRequest` with the existing generated request id pattern.
10. Redirect to `/i/{code}` after successful dispatch.

## Error Handling

Pre-auth invitation-flow routes do not reveal whether the invitation code exists or whether the nested path was once valid. They redirect to login first.

Signed-in invalid, unknown, expired, revoked, or exhausted links render the generic invitation-flow 404 unless the requester has an existing `Pending` or `Approved` request for that link.

Storage or command failures use the existing web error and flash-message patterns. Submission command failure should redirect back to `/i/{code}` with an error flash so the requester can retry.

## Testing

Route tests should cover:

- Unauthenticated `GET /i/{code}` redirects to login before resolving active, malformed, unknown, expired, revoked, and exhausted codes.
- Unauthenticated `POST /i/{code}` redirects to login with `return_to=/i/{code}`.
- Signed-in `GET /i/{code}` shows the merged request form for an active link with no non-repeatable request.
- Signed-in `GET /i/{code}` shows `Pending` status instead of the form when the requester has a pending request.
- Signed-in `GET /i/{code}` shows `Approved` status instead of the form when the requester has an approved request.
- Signed-in `GET /i/{code}` shows a retry notice and form when the newest historical request is `Declined`, `Expired`, or `Cancelled`.
- Signed-in `GET /i/{code}` shows only the newest retryable status notice when several historical retryable requests exist.
- Signed-in `GET /i/{code}` shows existing `Pending` or `Approved` status even if the invitation link is inactive.
- Signed-in `GET /i/{code}` returns the generic invitation-flow 404 for inactive links with no non-repeatable requester request.
- Signed-in `POST /i/{code}` dispatches `SubmitInvitationRequest` only when no `Pending` or `Approved` request exists.
- Signed-in `POST /i/{code}` redirects back to `/i/{code}` without command dispatch when a `Pending` or `Approved` request already exists.
- Nested `/i/{code}/...` routes redirect unauthenticated users to login first, then return the generic invitation-flow 404 after sign-in.

Verification commands:

- `cargo fmt --all --check`
- `cargo test -p web invitation_resolution`
- `cargo test -p web route_smoke`
- `cargo test -p web`

## Risks

The main risk is leaking code validity before authentication by resolving the invitation code too early. Tests should assert redirects happen without storage lookup for unauthenticated invitation-flow routes.

Another risk is selecting the wrong request when multiple historical requests exist. The implementation should centralize requester request selection so `GET` and `POST` agree on which states are non-repeatable and which prior state notice to show.

A third risk is accidentally preserving old `/request` or `/pending` behavior. Route tests should assert those nested paths authenticate first and then return the generic invitation-flow 404.
