# Invitation Context Loading Design

## Goal

Introduce a Restate service module that loads the common Invitation Context used by GitHub Invitation workflow and reconciliation paths.

The module should centralize the lookup chain for GitHub Invitation, Invitation Request, Invitation Link, repository, requester, installation, and Account facts while preserving existing create, cancel, expire, webhook, and reconciliation behavior.

## Current State

`crates/restate-svc/src/github_invitation.rs`, `crates/restate-svc/src/invitation_request.rs`, and `crates/restate-svc/src/reconcile.rs` repeatedly reconstruct overlapping lookup chains:

- `GithubInvitation` row to `InvitationRequest` row.
- `InvitationRequest` row to `InvitationLink` row.
- `InvitationLink` row to selected repository metadata.
- `InvitationRequest.requester_id` to requester `User`.
- Installation id to `Account` / installation row.

Those paths currently classify missing rows by manually converting `None` into `HandlerError::Storage(storage::Error::NotFound)` and classify broken domain relationships with ad hoc `HandlerError::Invariant` messages. The behavior is mostly consistent, but the rules are duplicated and easy to drift.

## Chosen Approach

Add `crates/restate-svc/src/invitation_context.rs` as the canonical Restate module for this lookup chain.

Expose small async loader functions that take `&AppState` and IDs already present in the workflow inputs. Keep the result structs plain and specific to Restate service needs rather than moving this responsibility into `domain` or `storage`.

This is the smallest change that gives one classification boundary without changing storage APIs, database schema, Restate payloads, GitHub API calls, or audit event shape.

## Detailed Design

Add result structs for the contexts currently needed:

- `InvitationRequestContext` loads an `InvitationRequest`, its `InvitationLink`, and the requester `User`.
- `GithubInvitationContext` loads a `GithubInvitation`, its `InvitationRequest`, its `InvitationLink`, the matching `InvitationLinkRepo`, the parsed `RepositoryIdentity`, the requester `User`, and the installation `Account`.
- `GithubInvitationAccountContext` loads a `GithubInvitation`, its `InvitationRequest`, its `InvitationLink`, the requester `User`, and the installation `Account` without resolving repository metadata. This preserves webhook behavior, because webhook audit only needs the account chain and historically did not require `InvitationLink.repos` to contain the GitHub Invitation repo.

The module should provide loaders shaped around existing callers:

- `load_invitation_request_context(state, request_id)` for `invitation_request::build_dispatch_inputs`.
- `load_github_invitation_context(state, invitation_id, expected_installation_id)` for `github_invitation::{cancel_logic,tick_expire_logic}`.
- `load_github_invitation_context_for_account(state, account, row)` for `reconcile::reconcile_single`, where the daily sweep already has the active installation row and pending GitHub Invitation row.
- `load_github_invitation_account_context(state, invitation_id)` for webhook handling, because webhook inputs identify the GitHub Invitation row but do not carry an installation id and do not need repository resolution.
- `load_installation_account(state, installation_id)` for `github_invitation::create_logic`, which only needs the installation/account row for audit classification.

Each loader should return owned domain rows. This keeps call sites simple, avoids lifetime complexity around `Arc<dyn Storage>`, and matches current code that already clones or moves repository strings into GitHub client calls.

## Error Classification

The module owns classification for the repeated lookup chain:

- Missing `GithubInvitation`, `InvitationRequest`, `InvitationLink`, requester `User`, or installation row returns `HandlerError::Storage(storage::Error::NotFound)`.
- A GitHub Invitation whose `repo_id` is not present in the Invitation Link repository set returns `HandlerError::Invariant` in full GitHub Invitation context loaders.
- An invalid `repo_full_name` returns `HandlerError::Invariant` from full GitHub Invitation context loaders.
- A mismatched expected installation id returns `HandlerError::Invariant` because it means the workflow input is pointing at a different installation than the loaded Invitation Link/Account chain.
- A mismatched expected account id returns `HandlerError::Invariant` because reconciliation is iterating one active installation account and must not audit against another account's link.

Storage database errors and corrupt rows should continue to propagate through `HandlerError::Storage` unchanged so terminal/transient behavior remains controlled by `HandlerError::is_terminal`.

## Consumer Changes

Update `github_invitation.rs` so:

- `on_webhook_logic` uses the account context loader for account id resolution before emitting accepted or declined audit events, without introducing a repository invariant into webhook handling.
- `cancel_logic` uses the context loader for request, link, repository, and account information before calling GitHub delete and emitting cancellation audit.
- `tick_expire_logic` uses the context loader for request, link, repository, and account information before listing GitHub invitations and emitting expiration audit.
- `create_logic` keeps its input-driven GitHub call path and replaces `lookup_account_id_for_installation` with `load_installation_account`.

Update `invitation_request.rs` so `build_dispatch_inputs` uses the request context loader to build one `CreateInvitationInput` per Invitation Link repo.

Update `reconcile.rs` so `reconcile_single` uses the account-aware context loader and keeps the existing reconciliation behavior: no change when the invitation is still pending, accepted when the requester is a collaborator, cancelled otherwise, terminal errors logged and skipped by the daily sweep.

## Testing

Add focused unit tests in `invitation_context.rs` covering:

- Complete GitHub Invitation context loads the expected invitation, request, link, repo, requester, and account.
- Webhook account context loads without requiring a matching Invitation Link repository.
- Missing Invitation Request returns `Storage(NotFound)`.
- Missing Invitation Link returns `Storage(NotFound)`.
- Missing requester returns `Storage(NotFound)`.
- Missing repository in the Invitation Link returns `Invariant`.
- Invalid repository full name returns `Invariant`.
- Missing installation returns `Storage(NotFound)`.
- Wrong expected installation or account returns `Invariant`.

Keep existing behavior tests in `github_invitation.rs`, `invitation_request.rs`, and `reconcile.rs` passing. Add or adjust tests only where needed to prove the new centralized classification does not regress behavior.

Run:

```bash
cargo test -p restate-svc invitation_context github_invitation invitation_request reconcile
cargo fmt --check
```

Before completion, run the wider relevant suite:

```bash
cargo test -p restate-svc
```

## Non-Goals

This change does not modify storage trait methods, database migrations, domain structs, Restate payload shapes, GitHub client behavior, audit event names, Web routes, or UI behavior.

This change also does not introduce a generic cross-service context framework. The scope is the Invitation Context lookup chain used by GitHub Invitation workflow and reconciliation paths.
