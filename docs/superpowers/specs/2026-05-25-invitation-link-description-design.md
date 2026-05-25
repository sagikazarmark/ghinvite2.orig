# Invitation Link Description Design

**Status:** approved for user review
**Date:** 2026-05-25
**Scope:** Add required admin-only invitation-link descriptions and move internal notes into the top section of the new-link form.

## Goal

Make invitation links easier for account admins to identify without exposing admin context to requesters.

Issue #18 asks for a friendly identifier for invitation links and for the internal note to move into a top section of the link form. The resolved model is a required short `Description` plus an optional freeform `Internal note`.

## Current State

`CONTEXT.md` defines `Invitation Link`, `Invitation Code`, and `Internal Note`. Before this design, invitation links only had an opaque generated invitation code plus optional `internal_note` metadata.

The create form in `crates/web/src/views/links.rs` places `Internal note` inside the `Request handling` section. The route in `crates/web/src/routes/console.rs` trims `internal_note` and stores non-empty values. Storage persists `internal_note` on `invitation_links`, and detail pages render it when present.

The Console overview currently uses the invitation code as the primary identifier in the recent-links table. Link detail pages also use the invitation code as the page heading and browser title. Requester-facing pages do not render `internal_note`.

## Chosen Approach

Add a required `description: String` field to the canonical invitation-link model.

- Every invitation link has a description.
- Description is admin-only and is never rendered in the invitation request flow.
- Description identifies the purpose or audience of the invitation link, for example `AI coding workshop`.
- Description is short single-line text, up to 120 characters.
- Description may change after creation as domain metadata, but this issue does not implement edit flows.
- Duplicate descriptions are allowed.
- Description is not included in audit metadata.

Keep `Internal note` as optional freeform admin-only context. It remains separate from description and may contain longer multiline notes.

This design uses the full domain field approach. Web-only metadata would leave the canonical `InvitationLink` unable to answer how admins identify a link. Reusing `internal_note` would collapse the distinction between a short required identifier and optional freeform notes.

## Domain And Storage

Update `domain::InvitationLink` with:

```rust
pub description: String,
```

Update every creation boundary that constructs an invitation link:

- `crates/web/src/commands.rs`: `CreateInvitationLink`
- `crates/restate-svc/src/invitation_link.rs`: `CreateLinkInput`
- `crates/restate-svc/src/invitation_link.rs`: transition from `CreateLinkInput` to `DomainInvitationLink`
- Test fixtures across web, storage, and Restate crates

Update storage row shapes and SQL in both adapters:

- `crates/storage/src/records.rs`
- `crates/storage/src/sqlx_impl.rs`
- `crates/storage-d1/src/bind.rs`
- `crates/storage-d1/src/wasm_impl.rs`

Add migration `migrations/0002_invitation_link_description.sql`.

The migration should add the required column:

```sql
ALTER TABLE invitation_links ADD COLUMN description TEXT NOT NULL DEFAULT '';
```

Then it should backfill existing rows with SQLite/D1-portable SQL equivalent to:

```sql
-- Pseudocode for the required transformation, not a fixed implementation.
source = coalesce(nullif(trim(internal_note), ''), slug)
normalized = collapse_whitespace_runs_to_single_spaces(source)
description = substr(trim(normalized), 1, 120)
```

The implementation can use a recursive CTE or another portable SQL strategy to collapse repeated whitespace runs exactly. A fixed-depth `replace(value, '  ', ' ')` chain is not sufficient unless it guarantees normalization for all stored text lengths.

Required backfill behavior:

- Use `internal_note` when it is present and non-blank.
- Otherwise use the invitation code.
- Replace newline, carriage-return, tab, and repeated whitespace runs with single spaces.
- Trim the result.
- Truncate the result to 120 characters.

The migration intentionally includes a data backfill `UPDATE` in addition to the schema `ALTER TABLE`, because required descriptions need valid values for legacy invitation links.

Application validation owns the stricter single-line and length rules for new writes. SQL only enforces non-null persistence.

No Restate backward-compatibility shim is required for old create payloads. The web app and Restate service are internal components that ship together in this repository.

## Create Form

Move invitation-link admin metadata into a top form section named `Link details`.

Field order:

- `Description`
- `Internal note`
- `Access configuration`
- `Request handling`
- `Repository scope`

`Description` is a required text input:

```text
Label: Description
Placeholder: AI coding workshop
Helper text: Visible only to admins. Use a short purpose or audience for this invitation link.
```

The input should use `name="description"`, `type="text"`, `required`, and `maxlength="120"`.

`Internal note` remains an optional textarea:

```text
Label: Internal note
Placeholder: Why this link exists
Helper text: Optional admin-only notes. Not visible in the invitation request flow.
```

Empty or whitespace-only internal notes continue to become `None`. Multiline internal notes are preserved.

## Validation

The create route validates description before dispatching the command.

Rules:

- Trim leading and trailing whitespace before storing.
- Reject empty descriptions after trimming.
- Reject descriptions longer than 120 characters after trimming.
- Reject descriptions containing newline characters.
- Do not silently truncate new submissions.

Validation failures should follow the existing create-link pattern and return plain `400 Bad Request` responses. Inline form error rendering is out of scope.

Existing invalid permission and empty repository validation keep their current plain `400 Bad Request` behavior.

## Console Display

Use the existing restrained Console visual vocabulary. This is a product UI change, not a redesign.

Overview recent-links table:

- Replace the primary `Code` column with `Description`.
- Render description as the linked primary text.
- Render the invitation code below it as secondary monospace metadata.
- Keep `State` and `Uses` columns.

Link detail page:

- Use description as the `h1`.
- Use description in the browser title, for example `{description} · {login}`.
- Show `Invitation code` near the header as secondary metadata.
- Keep the share URL section unchanged.
- Keep the internal note section when an internal note exists.

Requester-facing pages:

- Do not render description.
- Do not render internal note.
- Continue explaining repository access through repository scope and permission.

## Audit

Do not include description text in `invitation_link.created` metadata.

Audit should continue recording operational facts such as permission, approval policy, max use, and repository count. Description is mutable admin metadata and may contain sensitive context. Logging it would duplicate stale sensitive text.

This issue does not add edit flows or metadata-update audit events. A future edit issue may add a coarse `invitation_link.metadata_updated` event that records changed field names only, not text values.

## Testing Strategy

Add or update focused tests:

- `crates/web/src/views/links.rs`: create-form rendering includes `Description`, `required`, `maxlength="120"`, `AI coding workshop`, `Link details`, and `Internal note` in the top section.
- `crates/web/src/routes/console.rs` or existing route tests: missing/empty, over-120-character, and newline descriptions return `400 Bad Request`.
- Web command/fake-command tests: successful create passes the trimmed description into `CreateInvitationLink`.
- `crates/storage/src/tests.rs` or Sqlx storage tests: invitation-link storage round-trips `description`.
- `crates/storage-d1`: update row bindings and SQL coverage so D1 insert/select includes `description`.
- `crates/restate-svc/src/invitation_link.rs`: create transition stores `input.description` on the domain invitation link and audit metadata does not contain description text.
- `crates/web/src/views/console.rs`: overview uses description as the primary recent-link text and code as secondary metadata.
- `crates/web/src/views/links.rs`: detail page uses description as the heading/title and shows invitation code as metadata.
- `crates/web/src/views/invitation.rs`: requester-facing render tests do not expose description or internal note.

Run targeted checks while implementing:

```bash
cargo test -p storage
cargo test -p restate-svc invitation_link
cargo test -p web links console
```

Before completion, run:

```bash
cargo test
```

## Non-Goals

- Do not add edit UI for description or internal note.
- Do not add audit events for description or internal-note changes.
- Do not include description text in any audit metadata.
- Do not add uniqueness constraints for description.
- Do not add inline validation or form re-rendering on validation errors.
- Do not show description or internal note in the invitation request flow.
- Do not introduce a new visual system for the Console.

## Acceptance Criteria

- Account admins must provide a valid description when creating an invitation link.
- Description is persisted on invitation links and round-trips through Sqlx and D1 storage.
- Existing invitation links receive a backfilled description from normalized internal note or invitation code.
- Console recent-links lists use description as the primary identifier and invitation code as secondary metadata.
- Link detail pages use description as the page title/header and show invitation code as metadata.
- Internal note appears in the top `Link details` section of the create form as optional freeform text.
- Requester-facing pages do not expose description or internal note.
- Audit metadata does not include description text.
