# Inline Description Validation Design

**Status:** approved
**Date:** 2026-05-26
**Scope:** Issue #24: add the first inline validation path for the new invitation link form, limited to invitation-link description validation.

## Goal

When an account admin submits an invalid invitation-link description, the Console should re-render the new invitation link form with inline validation instead of returning a plain `400 Bad Request` response. The form should preserve submitted values, expose accessible error associations, and keep valid submissions on the existing create command path.

## Current State

`crates/web/src/routes/console.rs` already validates descriptions as required, single-line, and 120 characters or fewer, but invalid values become `WebError::BadRequest` responses. `crates/web/src/views/links.rs` renders the server-side Dioxus form with native `required` and `maxlength` constraints, but it has no structured validation state, summary, field-level error, or `aria-invalid` association.

## Chosen Approach

Keep this slice small by adding structured description validation state directly to the existing create-link route and link form view model. This avoids a premature form framework while establishing the result shape that later validation slices can expand.

The route remains server authoritative:

- Parse the submitted form.
- Validate the raw description.
- On description validation failure, reload available repositories and render `LinkCreateFormPage` with preserved form values and validation errors.
- On validation success, continue to normalize the description and call the existing `create_invitation_link` command.

## Validation Behavior

Description validation uses canonical invitation-link description language from `CONTEXT.md`:

- An invitation-link description is required admin-only context.
- It identifies the purpose or audience of the invitation link.
- It must be short, single-line text.
- It must be 120 characters or fewer after trimming.

Invalid cases for this issue are blank, whitespace-only, multiline, and over-120-character descriptions. The route should re-render the form for each case rather than returning a plain `400 Bad Request` body.

## Form State And Rendering

The form view model will carry preserved submitted values for the fields already present in the form:

- `description`
- `internal_note`
- `permission`
- `approval_required`
- `max_uses`
- `expires_in_days`
- `selected_repo_ids`

The view will render:

- A top validation summary near the start of the form.
- A field-level description error below the description input.
- `aria-invalid="true"` on the description input when invalid.
- `aria-describedby` pointing at helper text and the error text when invalid.

Other validation failures keep their existing behavior in this issue unless needed to preserve the valid create path.

## Testing

Tests should cover externally observable behavior:

- Direct description validation for blank, whitespace-only, multiline, over-limit, and valid trimmed descriptions.
- Rendered form markup for summary, field error, preserved values, selected repository checkbox, `aria-invalid`, and `aria-describedby`.
- Route behavior for invalid description submissions returning an HTML form response, not a plain `400 Bad Request`.
- Route behavior for valid submissions still invoking the command facade and redirecting to the invitation link detail page.

No browser automation is needed because this slice preserves the server-rendered architecture.
