# ghinvite Context

## Glossary

### Account
A GitHub account where ghinvite is installed. An account may be a personal GitHub account or a GitHub organization.

### Personal Account
A GitHub account owned by an individual person, as distinct from a GitHub organization.

### GitHub User
An individual GitHub identity that can sign in, create invitation links, request access, or decide invitation requests.

### Requester
A GitHub user who has submitted an invitation request.

`Recipient` is not a canonical domain role in ghinvite. Use `GitHub User` before an invitation request exists, and `Requester` after one exists. Admin-facing UI copy may use recipient informally for the person an account admin shares a link with. GitHub invitation integration may use recipient for the GitHub user who receives a collaborator invitation.

### Account Admin
A GitHub user with current GitHub-derived authority to administer an account in ghinvite. For an organization account this is an organization owner; for a personal account this is the same GitHub user who owns the account.

### Invitation Link
An invitation link is a shareable URL created by an account admin that lets a GitHub user request repository collaborator access for the repositories and permission level configured on the link. It is the same URL in admin-facing and requester-facing contexts. UI may use `link` alone only when nearby context clearly establishes `invitation link`.

Public/requester-facing copy should usually say repository access. Admin/domain/GitHub-facing copy may say collaborator access when GitHub-specific precision matters.

An invitation link's max use is the maximum number of invitation requests that may be created through the invitation link. Its uses value is the number of invitation requests already created through it.

Use `max use` and `uses` in domain and UI language. A use means a created invitation request, not a page view, click, approval, or sent GitHub invitation.

Invitation-link guardrails are the max use, expiration, permission level, repository scope, and approval policy. Guardrails are fixed when the invitation link is created. To use different guardrails, an account admin creates a new invitation link.

Invitation link expiration or revocation stops new invitation requests from being created through that invitation link. It does not change existing invitation requests or GitHub invitations.

User-facing UI should describe invitation-link expiration as stopping acceptance of new invitation requests when space allows.

`Revoke` is the canonical invitation link lifecycle verb. User-facing UI should describe the effect as stopping acceptance of new invitation requests.

Every invitation link has a description. An invitation link's description is admin-only context that identifies the purpose or audience of the invitation link. It is not visible in the invitation request flow. Descriptions are short single-line text, up to 120 characters, and may change after the invitation link is created.

Admin-facing invitation-link lists should use the description as the primary identifier and the invitation code as secondary metadata.

### Internal Note
Optional freeform admin-only context attached to an invitation link. An internal note is not visible in the invitation request flow and may change after the invitation link is created.

### Invitation Code
The short code from an invitation link that a GitHub user can enter to open the invitation request flow. An invitation code is the code segment only, not the full invitation link URL. UI may use `code` alone when nearby context clearly establishes `invitation code`. Do not use `slug` in domain or UI language for this value.

`Invitation` is not a standalone canonical noun in ghinvite. Use precise terms such as `Invitation Link`, `Invitation Code`, `Invitation Request`, `Invitation Request Flow`, or `GitHub Invitation`.

### Public Surface
Unauthenticated or broadly accessible pages outside an account-scoped Console page.

### Invitation Request Flow
The public invitation-link journey where a GitHub user reviews access being offered by an invitation link, signs in, submits an invitation request, or checks the request status.

The invitation request flow starts when a GitHub user opens a specific invitation link or invitation code. Entering an invitation code is a shortcut into the flow, not a separate flow.

Use `Invitation Request` vocabulary for this journey instead of standalone `Invitation` or `Recipient` vocabulary.

Broken or invalid invitation links are still handled as invitation request flow pages because the user was trying to open an invitation link or invitation code.

### Console
The admin-facing workspace where account admins manage invitation links, invitation requests, settings, and audit history for a GitHub account.

Use `Console` as the canonical term. Use account-scoped as a descriptive qualifier when needed; do not use `Account Console` as a separate canonical term.

### Audit Log
An account-scoped history of access workflow events that account admins use to understand what happened in the account.

Admin UI may use `Audit` as a compact label when the Console context makes the meaning clear.

### Invitation Request
A requester's request for access through an invitation link. It may be auto-approved or wait for an account admin decision. A requester may have at most one pending invitation request for a given invitation link at a time. Admin UI may use `Requests` as a compact label when the Console context makes the meaning clear.

A requester may create a new invitation request through the same invitation link after a previous invitation request is declined, expired, or cancelled.

An auto-approved invitation request is approved by invitation-link policy without account-admin review. Auto-approval does not guarantee that every GitHub invitation is sent successfully.

An expired invitation request is one whose account-admin decision deadline passed before approval or decline. This is distinct from invitation link expiration and GitHub invitation expiration.

In the Console, `Requests` usually means the queue of pending invitation requests awaiting account-admin decision, not a complete invitation request archive.

### Approval Policy
The invitation-link setting that determines whether invitation requests are auto-approved or wait for an account-admin decision.

An invitation link's approval policy is fixed when the invitation link is created.

### GitHub Invitation
A repository collaborator invitation sent through GitHub for one repository as the result of an approved invitation request.

An approved invitation request may produce multiple GitHub invitations, one per repository in the invitation link's repository scope. UI may summarize those GitHub invitations, but the domain object remains repository-scoped.

### Repository Access
A GitHub user's ability to collaborate on a repository at a specific permission level.

### Permission Level
The GitHub repository collaborator permission requested through an invitation link and used when sending GitHub invitations.

An invitation link's permission level is fixed when the invitation link is created.

### Available Repositories
The repositories a GitHub App installation currently makes available to ghinvite for an account.

### Repository Scope
The non-empty set of repositories selected on an invitation link for which invitation requests may request repository access.

Repository scope is fixed when the invitation link is created.

### Repository Identity
The GitHub owner and repository name pair that identifies a repository for collaborator invitations while preserving the display full name. It has exactly one owner and one repository name; both are non-empty.
