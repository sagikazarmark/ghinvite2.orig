# Repository Identity Design

## Goal

Deepen Repository Identity as a domain module for GitHub owner and repository name validation while preserving the repository full name string used by existing storage records and UI display.

This keeps product behavior and persistence unchanged while removing ad hoc repository full-name parsing from invitation workflow paths.

## Current State

`InvitationLinkRepo` stores repositories as `repo_id` plus `repo_full_name: String`. Storage adapters and database schemas persist that full-name string, and web views display it directly.

GitHub invitation workflow code currently parses repository full names close to GitHub API calls:

- `crates/restate-svc/src/github_invitation.rs` has a local `split_full_name` helper.
- `crates/restate-svc/src/reconcile.rs` calls `repo_full_name.split_once('/')` directly.

Those call sites only validate that a slash exists. They do not centralize the domain invariant that a repository identity has exactly one owner and one repository name, both non-empty.

## Chosen Approach

Add a minimal `domain::RepositoryIdentity` value type and parse stored full-name strings at GitHub-call boundaries.

Keep `InvitationLinkRepo.repo_full_name: String`, existing storage records, schemas, Restate payloads, and UI display behavior unchanged.

## Detailed Design

Add `crates/domain/src/repository_identity.rs` with a `RepositoryIdentity` type that owns the original display full name and exposes borrowed owner/name pieces.

The public interface should stay small:

```rust
pub struct RepositoryIdentity { ... }

impl RepositoryIdentity {
    pub fn parse(full_name: impl Into<String>) -> Result<Self, RepositoryIdentityError>;
    pub fn owner(&self) -> &str;
    pub fn name(&self) -> &str;
    pub fn full_name(&self) -> &str;
}
```

Validation rules:

- Accept exactly one `/` separator.
- Reject a missing separator.
- Reject an empty owner.
- Reject an empty repository name.
- Preserve the input full name exactly for display and audit metadata.

Do not add GitHub URL encoding or path construction to `RepositoryIdentity`. URL path encoding remains in the GitHub adapter through `github::util::path_segment` and existing `InstallationClient` methods.

Export `RepositoryIdentity` and its error type from `domain::lib`.

Adopt the type in Restate-side invitation paths before GitHub calls:

- `create_logic` parses `input.repo_full_name` once and passes `identity.owner()` and `identity.name()` to `add_collaborator`.
- `cancel_logic` parses the matched invitation-link repository full name before `delete_invitation`.
- `tick_expire_logic` parses the matched invitation-link repository full name before `list_invitations`.
- `reconcile_single` parses the matched invitation-link repository full name before `list_invitations` and `is_collaborator`.

Remove the local `split_full_name` helper and direct `split_once('/')` parsing from these paths.

## Error Handling

Invalid repository full names in workflow paths represent stored-data or command-payload invariants, not user-correctable request errors.

Map `RepositoryIdentityError` to the existing `HandlerError::Invariant` at Restate workflow boundaries. Preserve current terminal/transient retry behavior.

## Testing

Add domain unit tests for:

- valid identities expose owner, name, and preserved full name;
- missing slash is rejected;
- empty owner is rejected;
- empty repository name is rejected;
- multiple slashes are rejected so the type represents exactly one owner and one repository name;
- display full name is preserved exactly.

Update existing Restate service tests that exercise invalid repository full names so they now fail through `RepositoryIdentity` instead of a local helper.

Keep GitHub adapter path encoding tests in `crates/github` as the proof that URL encoding remains separate from domain parsing.

Run:

```bash
cargo test -p domain -p restate-svc -p github
```

## Non-Goals

This change does not modify database schemas, migrations, storage record shapes, Restate payload JSON, web display behavior, GitHub API endpoints, or GitHub path encoding.

It also does not thread `RepositoryIdentity` through `InvitationLinkRepo` or storage adapters. That broader typing change can be considered later if persisted repository modeling becomes richer.
