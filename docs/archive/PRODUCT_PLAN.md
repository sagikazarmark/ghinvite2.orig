# GitHub Invitation Link App Product Plan

## Product Thesis

Build an invitation-link-based access workflow for GitHub repositories. A trusted user installs a GitHub App on an org or user account, creates restricted invitation links, and lets recipients request access with their own GitHub identity. The app handles approval, sends GitHub invitations, tracks status, and provides an audit trail.

The sharp v1 promise should be: "Give a contractor, student, candidate, or partner access to the right GitHub repositories without sharing PATs, manually re-inviting people, or losing the approval/audit trail."

## Recommended V1 Scope

Include:

- GitHub login for all users.
- GitHub App installation for repository access management.
- Dashboard for installed accounts and repositories visible to the installation.
- Create invitation links with selected repositories, permission level, expiration, max uses, and approval policy.
- Recipient page that requires GitHub login before request submission.
- Manual approval queue.
- GitHub repository invitations through the Collaborators API.
- Per-request status tracking: requested, pending approval, approved, GitHub invitation created, accepted, expired, declined, failed, revoked.
- Audit log covering all security-relevant events.
- Polling-based reconciliation for pending GitHub invitations.

Exclude from v1:

- Notifications beyond GitHub's own invitation notification.
- Editing existing links.
- Automatic cascade-cancel of pending GitHub invitations when an invitation link is revoked.
- Team membership and organization membership as first-class grant types.
- SCIM or Enterprise Managed User support.
- JIT time-bound automatic removal after access is accepted.
- Multi-approver policies.
- Slack/email approval notifications.

## Confirmed GitHub Facts

- GitHub Apps have no permissions by default. The app must request repository, organization, and account permissions explicitly.
- Installation access tokens are created with `POST /app/installations/{installation_id}/access_tokens` and expire after one hour.
- Installation tokens can be scoped down to a subset of repositories and permissions, but cannot exceed the installation's granted repositories or permissions.
- To add a repository collaborator, use `PUT /repos/{owner}/{repo}/collaborators/{username}`.
- For organization-owned repositories, adding a collaborator this way does not add the user to the organization. It creates outside collaborator access unless enterprise-managed-user rules apply.
- Repository invitation creation returns a repository invitation object with `id`, `invitee`, `inviter`, `permissions`, `created_at`, `expired`, `url`, and `html_url`.
- Existing collaborator permission changes may return `204` instead of an invitation object.
- Pending repository invitations can be listed with `GET /repos/{owner}/{repo}/invitations` and canceled with `DELETE /repos/{owner}/{repo}/invitations/{invitation_id}`.
- Repository invitees can accept or decline invitations through `/user/repository_invitations/{invitation_id}` when authenticated as themselves.
- Adding outside collaborators may be restricted by org or enterprise policy.
- Repository invitation rate limit: 50 invitations per repository per 24 hours, with no limit for inviting existing organization members to an organization repository.
- Organization invitations can be created with `POST /orgs/{org}/invitations`, but GitHub documents that the authenticated user must be an organization owner.
- Organization invitations expire automatically after seven days.
- GitHub webhooks include `member` events for repository collaborators. The `member.added` action corresponds to a user accepting a repository invitation.
- GitHub webhooks include `organization` events with actions such as `member_invited`, `member_added`, and `member_removed`.
- GitHub App installation lifecycle webhooks are always sent for installation creation, deletion, suspension, unsuspension, and repository selection changes.

## Important Open API Verification

These should be verified with a throwaway GitHub org before implementation locks in:

- Whether installation access tokens with repository `administration:write` can create and cancel repository invitations in every relevant org setting.
- Whether the GitHub App installation actor appears as the inviter and whether that is acceptable in the product/audit model.
- Whether an installation token can call organization invitation endpoints in practice, or whether org membership invitations require a user token from an org owner.
- Which exact app permissions are required for the collaborator, invitation-listing, invitation-canceling, and member webhook endpoints. The likely high-risk permission is repository `Administration: write`, and webhooks likely require organization `Members: read`.
- Whether repository invitation IDs returned at creation can always be correlated with later list calls and webhook-driven acceptance.
- How GitHub behaves when adding a collaborator who already has access through org base role, team membership, or existing direct repo permission.
- How SAML SSO, 2FA requirements, outside-collaborator restrictions, and seat availability failures are represented in API responses.

## Access Model Recommendation

Use repository-level outside collaborator invitations in v1.

Do not model organization membership or team membership as a v1 grant type. They are valuable, but they are a different authorization product: org owners, teams, SCIM, role restoration, SAML, seats, and broad org visibility all become part of the blast radius.

V1 grant object:

- Account: GitHub org or user where the app is installed.
- Repositories: one or more repositories selected from the installation's accessible repos.
- Permission: read, triage, write, maintain, or admin, with `admin` optionally disabled by product policy.
- Recipient: GitHub user id and login from OAuth.
- Delivery: GitHub repository collaborator invitation.

Recommended default permission choices:

- Enable `read`, `triage`, and `write`.
- Hide or require an extra confirmation for `maintain`.
- Disable `admin` in v1 unless the app is explicitly positioned for high-trust internal admin workflows.

## Link Security Recommendation

Treat invitation links as bearer capabilities, but not as sufficient authorization to receive access.

Required controls:

- High-entropy opaque token in URL.
- Store only a hash of the token server-side.
- Require GitHub login before a request can be submitted.
- Validate expiration and revoked state before showing sensitive details and again at submission.
- Enforce max uses atomically.
- Prevent duplicate active requests for the same user/link.
- Record IP, user agent, GitHub user id, and login in audit logs.

Repo-name visibility decision:

- Recommended v1: show repository owner/name and permission after GitHub login, before request submission.
- For stricter environments, add a link option `hide_repo_details_until_approved` later.
- Do not show private repo names to anonymous users.

Max-use decision:

- Count a use when a request is approved and GitHub invitation creation is attempted, not when the link is merely opened.
- Also keep `request_count` separately for abuse monitoring.
- If approval is disabled, request submission and use consumption happen in the same transaction/workflow step.

## Authorization Model Recommendation

There are two kinds of users:

- Installers/admins: users who can install the GitHub App and manage invitation links for an installed account.
- Recipients: users who only land on a link and request access.

V1 admin authorization should be based on GitHub-side authority, not an app-local manual role system.

Recommended rule:

- A user can manage an installed account if GitHub indicates they have sufficient authority over that account and the installation.
- For orgs, require org owner status or app manager/install authority where GitHub exposes enough signal.
- For user-owned repos, the account owner can manage links.
- Cache admin eligibility briefly for UI speed, but re-check before sensitive mutations.

Reason:

- The app grants real GitHub access. App-local roles can drift from GitHub authority and become dangerous unless backed by a strong organization model.

Future option:

- Add app-local delegated admins, but only after a verified owner enables it per installed account.

## GitHub Invitation State Machine

Request states:

- `requested`: recipient submitted a request.
- `pending_approval`: waiting for app admin approval.
- `denied`: admin denied the request.
- `approved`: admin approved, but GitHub invite not yet created.
- `github_invite_pending`: one or more GitHub invitations were created and are awaiting acceptance.
- `active`: GitHub confirms access is active for all requested repositories or enough repositories depending on policy.
- `partially_active`: some repositories accepted/active and some still pending or failed.
- `expired`: GitHub invitation expired or app link/request expired before invitation creation.
- `revoked`: app admin revoked request or canceled pending GitHub invitations.
- `failed`: GitHub API failed in a non-retryable way.

Per-repository invitation states:

- `not_started`
- `create_pending`
- `pending`
- `accepted`
- `declined`
- `expired`
- `canceled`
- `failed`
- `already_had_access`

Policy recommendation:

- Track per-repo invitations even when the user-facing request is one logical request.
- Consider the request `active` only when every selected repo is `accepted` or `already_had_access`.
- Show `partially_active` if multi-repo grant has mixed outcomes.

## Webhooks vs Polling

Recommended v1: polling first, webhook-ready schema.

Why polling first:

- The product needs a reliable audit trail more than instant status.
- Pending invitation lists are directly queryable per repository.
- It avoids early complexity around webhook signature validation, idempotency, event ordering, retries, and matching ambiguous events.
- The workflow can schedule polling until GitHub's seven-day expiry window is passed.

Polling algorithm:

- After GitHub invitation creation, persist returned invitation IDs per repo.
- Poll pending invitations for those repos every few hours, with jitter.
- If invitation ID still appears and `expired == false`, remain pending.
- If invitation ID appears with `expired == true`, mark expired.
- If invitation ID no longer appears, check collaborator permission for that user on that repo.
- If collaborator permission is present and at least the expected permission, mark accepted/active.
- If invitation no longer appears and collaborator access is absent, mark declined_or_canceled_unknown unless a local revoke/cancel event exists.
- Stop after GitHub's invitation expiry window plus buffer.

Recommended future webhook adoption:

- Subscribe to `member`, `organization`, `installation`, and `installation_repositories` events.
- Use webhooks as fast-path signals that trigger reconciliation, not as sole source of truth.
- Still poll as fallback for missed or ambiguous events.

## Restate Architecture Recommendation

Use Restate for durable orchestration and concurrency. Use Postgres as the queryable product source of truth and audit/event store.

Do not make Restate the only frontend source of truth in v1.

Reasons:

- The dashboard needs filtered lists, joins, pagination, search, and audit history.
- Audit logs need durable, queryable, append-only semantics.
- Restate state is excellent for workflow coordination, but the product still needs relational projections.
- Debugging and support are much easier with SQL-visible state.

Recommended split:

- Postgres owns users, installations, links, requests, invitation rows, audit logs, and UI projections.
- Restate owns durable execution: approving requests, creating GitHub invitations, waiting for external events, polling, revoking, and idempotent retries.
- Application server exposes UI/API and writes intent rows, then invokes Restate workflows.
- Restate writes all state transitions to Postgres inside durable side-effect steps.

Restate services:

- `InvitationLinkObject(link_id)`: serializes link use checks, max-use accounting, revocation, and request creation.
- `InvitationWorkflow(request_id)`: handles approval wait, GitHub invitation creation, polling/reconciliation, and terminal state.
- `InstallationObject(installation_id)`: serializes installation metadata refresh, repository selection changes, and sync jobs.
- `GithubWebhookService`: validates webhook requests and signals workflows or installation objects. Optional in v1.

## Data Model Sketch

Tables:

- `users`: GitHub user id, login, avatar, last login.
- `github_app_installations`: installation id, account id, account login, account type, repository selection, permissions JSON, suspended/deleted flags.
- `installation_repositories`: installation id, repo id, owner, name, full name, private, archived, current access flag.
- `invitation_links`: id, installation id, creator user id, token hash, name, expires_at, max_uses, uses_count, approval_required, revoked_at, created_at.
- `invitation_link_repositories`: link id, repo id, permission.
- `access_requests`: id, link id, requester user id, status, requested_at, approved_at, approved_by, denied_at, denied_by, terminal_at.
- `request_repository_invitations`: request id, repo id, permission, GitHub invitation id, GitHub html url, status, created_at, last_checked_at, terminal_at, failure_code, failure_message.
- `audit_events`: id, actor user id nullable, installation id, link id nullable, request id nullable, repo id nullable, event_type, event_time, ip, user_agent, metadata JSON.
- `oauth_sessions`: tower-sessions storage or equivalent.

Event types:

- `user.logged_in`
- `installation.created`
- `installation.repositories_changed`
- `link.created`
- `link.revoked`
- `link.opened`
- `request.created`
- `request.approved`
- `request.denied`
- `github.invitation.created`
- `github.invitation.accepted`
- `github.invitation.expired`
- `github.invitation.canceled`
- `github.invitation.failed`
- `request.revoked`

## User Flows

Admin onboarding:

- User logs in with GitHub.
- Dashboard shows install/connect GitHub App CTA.
- User installs app on org/user account and selects repositories.
- App receives installation webhook or callback and syncs repositories.
- User selects installed account and sees invitation links.

Create link:

- Admin selects installed account.
- Admin selects repositories.
- Admin chooses permission level.
- Admin sets expiration and max uses.
- Admin chooses approval required or auto-approve.
- App creates link and records audit event.
- Admin copies share URL.

Recipient request:

- Recipient opens link.
- If anonymous, app shows minimal page and requires GitHub login.
- After login, app validates link again.
- Recipient reviews repo names and permission unless link hides details.
- Recipient submits request.
- If approval is required, status becomes pending approval.
- If not, workflow proceeds to GitHub invitation creation.

Approval:

- Admin sees pending request with GitHub identity, target repos, permission, link, timestamps, and prior requests by same user.
- Admin approves or denies.
- Approval starts or signals `InvitationWorkflow`.
- Workflow creates GitHub invitations and records per-repo results.

Reconciliation:

- Workflow polls pending invitations.
- Accepted invitations become active when collaborator permission confirms.
- Expired or missing invitations become terminal with a precise or conservative reason.
- Audit log records each transition.

Revocation:

- Link revocation prevents future requests.
- Request revocation cancels only pending app-side requests in v1.
- Pending GitHub invitation cancellation can be manual per request if implemented; do not cascade from link revocation automatically in v1.

## Adjacent Projects And Patterns

GitHub native invitations:

- Strength: official, familiar, no extra product.
- Weakness: no reusable link workflow, no approval queue separate from GitHub admins, weak product-level audit trail, cumbersome for repeated cohorts.

GitHub Classroom:

- Strength: link-based onboarding into repositories/classrooms, excellent for cohorts.
- Weakness: education-specific workflow, not a general-purpose access approval/audit product.

GitHub reinvite scripts/tools:

- Strength: solves expired invitation pain.
- Weakness: operational utility, not a secure delegated access workflow.

Opal, Apono, Entitle/BeyondTrust, ConductorOne, access-management tools:

- Strength: approval workflows, time-bound access, auditability, broader identity integrations.
- Weakness: heavier enterprise model, often less suited to simple invitation-link distribution, may require significant setup.

GitHub Entitlements and Terraform-style access management:

- Strength: declarative, reviewable, infrastructure-as-code friendly.
- Weakness: slower for ad hoc external recipients; not recipient-driven.

Product gap:

- A focused GitHub access-link product can sit between raw GitHub invitations and heavyweight access management: low setup, OAuth identity, GitHub App permissions, reusable/revocable links, explicit approval, and auditability.

## Feature Backlog

Near-term:

- Link templates.
- Duplicate request detection and resend flow.
- Manual GitHub invitation refresh/retry.
- Pending invitation cancellation per request.
- Basic email or Slack notification for pending approvals.
- Org policy diagnostics for outside collaborator restrictions, rate limits, 2FA, SAML, and seats.
- Recipient-facing status page.
- Export audit log CSV.

Medium-term:

- Webhook fast-path reconciliation.
- Team-based grants.
- Organization membership grants.
- Time-bound access with automatic removal.
- Auto-approval for allowlisted domains or GitHub org membership.
- App-local delegated admins.
- Link-level repo detail hiding.
- Signed approval comments/reasons.

Long-term:

- SCIM-aware enterprise mode.
- SAML SSO diagnostics.
- Access reviews and recertification.
- Terraform/provider integration.
- Slack/Linear/Jira integrations.
- Policy engine for approval routing.

## Research Questions

Product phrasing:

- Is this primarily "GitHub invite links", "GitHub access requests", or "temporary GitHub access"?
- Is the target buyer an open-source maintainer, small company admin, classroom organizer, hiring team, or enterprise platform/security team?
- Is the killer use case one-off external access or repeated cohort onboarding?
- Is the product safer when it never grants org membership in v1?
- Should v1 optimize for self-hosted/internal deployment or SaaS?

Security:

- Are private repository names considered sensitive before approval?
- Should links require manual approval by default?
- Should auto-approve links be allowed at all for private repos?
- Should max uses count approvals, requests, accepted invitations, or created GitHub invitations?
- Should links have a maximum lifetime enforced by product policy?
- Should the app allow write/maintain/admin grants by default?

GitHub behavior:

- Can installation tokens create repo invitations across personal repos and org repos with the expected permissions?
- What exact errors occur under outside collaborator restrictions, SAML, 2FA, seat limits, EMU accounts, and existing access?
- Can organization invitation endpoints be used by GitHub Apps, or only by user tokens with org owner authority?
- How consistently can repository invitation IDs be reconciled after acceptance/decline/expiration?

Architecture:

- Should Postgres be the read model and audit source of truth? Recommended: yes.
- Should Restate own every state transition? Recommended: yes for request/invitation flows.
- Should the web server call GitHub directly? Recommended: only for login and read-only UI sync; mutating GitHub operations should go through workflows.
- Should webhooks be v1? Recommended: no, except installation lifecycle if easy; use polling for invitation state.

UX:

- What does the recipient see before login?
- Should the recipient have a durable status page after request submission?
- Should admins approve one request across all repos or per repo?
- Should denied requests include a reason visible to the recipient?
- What happens if the recipient already has access?

## Decision Interview With Recommended Answers

1. Who is the first user?

Recommended answer: small-team GitHub org admins who repeatedly invite external collaborators, contractors, candidates, students, or community contributors to private repos.

2. Is this for organizations only or personal repositories too?

Recommended answer: support both if GitHub App installation makes it natural, but design around org-owned repositories because that is where access delegation and audit matter most.

3. What is the v1 grant type?

Recommended answer: repository collaborator invitation only. No org membership or team membership in v1.

4. Should users see repo names before requesting?

Recommended answer: only after GitHub login. Anonymous users should not see private repo names.

5. Should users see repo names before approval?

Recommended answer: yes in v1. The recipient needs informed consent about what access they are requesting. Add a hide-details option later for sensitive environments.

6. Should manual approval be required by default?

Recommended answer: yes. Auto-approve is useful but should be opt-in and visually risky.

7. Who can approve?

Recommended answer: users who still have GitHub-side authority over the installed account. Avoid app-local admins in v1.

8. Should one approval cover all selected repos?

Recommended answer: yes. The link is the access package; per-repo approval adds complexity and weaker UX. Track per-repo GitHub outcomes internally.

9. Should max uses count request submissions or approved grants?

Recommended answer: approved grants or auto-approved GitHub invite attempts. Count raw submissions separately for abuse.

10. Should links be editable?

Recommended answer: no in v1. Create a new link. Editing old links complicates auditability and recipient expectations.

11. What does revoking a link do?

Recommended answer: it stops new requests only. It does not remove already granted access and does not cascade-cancel pending GitHub invitations in v1.

12. What does revoking a request do?

Recommended answer: if app-side pending, mark revoked. If GitHub invitation pending and cancellation is implemented, cancel that invitation. If already accepted, v1 should not remove access automatically unless explicit access removal exists.

13. Should accepted access be removable by this app?

Recommended answer: not in initial v1 unless positioned as access lifecycle management. Removing collaborators has surprising fork/package/project side effects and needs stronger confirmation/audit.

14. Should webhooks be used immediately?

Recommended answer: use installation lifecycle webhooks if needed, but use polling for invitation acceptance/expiration in v1. Add member webhooks later as a fast path.

15. Should Restate be the frontend source of truth?

Recommended answer: no. Use Postgres for UI/audit projections and Restate for durable workflows.

16. What is the most dangerous permission?

Recommended answer: repository `Administration: write`, because it can manage collaborators and other repo settings. The product must explain this during app installation and scope installation to selected repositories where possible.

17. Should install tokens be stored?

Recommended answer: no. Generate one-hour installation tokens on demand. Store installation id and app private key securely.

18. Should OAuth user tokens be stored?

Recommended answer: store only if needed for GitHub-side admin verification or recipient invitation acceptance helper flows. Prefer short-lived sessions and minimal account data in v1.

19. What should the audit log guarantee?

Recommended answer: every intent and every external side effect has an append-only event: link creation/revocation, request, approval/denial, GitHub invite create/cancel, reconciliation result, failures.

20. What is the v1 success metric?

Recommended answer: an admin can create a link and a recipient can request, receive, and accept access with full audit visibility, without the admin creating a PAT or manually touching GitHub invitations.

## Recommended Next Implementation Spike

Before building the UI, write a narrow GitHub API spike:

- Create a test GitHub App with selected repository access.
- Request minimum likely permissions for repo collaborator management and member webhooks.
- Install it on a throwaway org with one private repo.
- Use app JWT to create an installation token.
- Call add collaborator for a test GitHub user.
- Persist the returned invitation id.
- List pending repo invitations and verify the id appears.
- Accept the invitation as the recipient or manually in GitHub.
- Verify list-pending behavior after acceptance.
- Verify collaborator permission check after acceptance.
- Cancel a second pending invitation and verify list/check behavior.
- Repeat under an outside-collaborator-restricted org setting if available.

This spike should settle the highest-risk API assumptions before committing to the full Restate workflow implementation.
