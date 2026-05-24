# ghinvite Context

## Glossary

### Account
A GitHub account where ghinvite is installed. An account may be a personal GitHub account or a GitHub organization.

### Personal Account
A GitHub account owned by an individual person, as distinct from a GitHub organization.

### GitHub User
An individual GitHub identity that can sign in, create share links, request access, or decide invitation requests.

### Account Admin
A GitHub user with current GitHub-derived authority to administer an account in ghinvite. For an organization account this is an organization owner; for a personal account this is the same GitHub user who owns the account.

### Share Link
A shareable URL created by an account admin that lets a GitHub user request repository collaborator access for the repositories and permission level configured on the link. Public use of a share link means recipient use of that same shareable URL, not a separate kind of link. User-facing copy may call this an invitation link when speaking to non-admins.

### Share Link Code
The short code from a share link that a recipient can enter to open the recipient flow.

### Public Surface
Unauthenticated or broadly accessible pages outside a specific account console.

### Recipient Flow
The public share-link journey where a GitHub user reviews and submits an invitation request.

### Account Console
The admin-facing, account-scoped workspace where account admins manage share links, requests, and settings.

### Audit Log
An account-scoped history of access workflow events that account admins use to understand what happened in the account.

### Invitation Request
A recipient's request for access through a share link. It may be auto-approved or wait for an account admin decision. A recipient may have at most one pending invitation request for a given share link at a time.

### GitHub Invitation
A repository collaborator invitation sent through GitHub for one repository as the result of an approved invitation request.

### Repository Identity
The GitHub owner and repository name pair that identifies a repository for collaborator invitations while preserving the display full name. It has exactly one owner and one repository name; both are non-empty.
