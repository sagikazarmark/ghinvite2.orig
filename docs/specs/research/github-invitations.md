# Desk check: GitHub invitations for P1 (GitHub Invite)

Checked on 2026-10-15 against current primary sources: docs.github.com (fetched as Markdown through `docs.github.com/api/article/body?pathname=…`), the OpenAPI description in `github/rest-api-description` (`descriptions/api.github.com/api.github.com.json`, info.version 1.1.4, REST API version `2026-03-10`), the public GraphQL schema (`docs.github.com/public/fpt/schema.docs.graphql`), and github.blog changelog. Nothing was created or changed on GitHub.

Confidence marks:
- **HIGH**: stated in so many words by a primary source.
- **MEDIUM**: follows from primary sources, but behaviour is implied rather than stated.
- **UNVERIFIED**: no primary source found. Needs a spike.

Short names for sources used throughout:
- **[members]**: https://docs.github.com/en/rest/orgs/members
- **[perms]**: https://docs.github.com/en/rest/authentication/permissions-required-for-github-apps
- **[collab]**: https://docs.github.com/en/rest/collaborators/collaborators
- **[repo-inv]**: https://docs.github.com/en/rest/collaborators/invitations
- **[teams]**: https://docs.github.com/en/rest/teams/members
- **[hooks]**: https://docs.github.com/en/webhooks/webhook-events-and-payloads
- **[openapi]**: https://github.com/github/rest-api-description/blob/main/descriptions/api.github.com/api.github.com.json
- **[invite-help]**: https://docs.github.com/en/organizations/managing-membership-in-your-organization/inviting-users-to-join-your-organization

---

## 1. Org invitations

**Answer.**
- **Endpoint:** a single `POST /orgs/{org}/invitations`. The body takes either `invitee_id` (the GitHub user ID, not a login) or `email`.
  - `role` is one of `admin`, `direct_member` (the default), `billing_manager` or `reinstate`.
  - `team_ids` (an array of integers) assigns teams at invite time.
- **App permission:** organization permission **"Members": write**. Installation access tokens (IAT) are accepted, so the app can call it without a user.
- **Related endpoints, all under the same "Members" permission:**
  - `PUT /orgs/{org}/memberships/{username}` (role `admin|member`) also invites a non-member.
  - `GET /orgs/{org}/invitations` lists pending invitations (read).
  - `DELETE /orgs/{org}/invitations/{id}` cancels one (write).
- **Rate limit:** 50 invitations per 24h per organization, or 500 if the org is more than a month old or on a paid plan. The docs phrase it as a limit on "organization owners". They don't say whether invitations made by an app count against the same per-org bucket. The bucket is most plausibly per org (UNVERIFIED). The endpoint also falls under the generic secondary content-creation limits.
- **Licences:** paid per-seat orgs need a free licence before an invitation can be created.

**Evidence.**
- [members] "Create an organization invitation":
  - "Invite people to an organization by using their GitHub user ID or their email address. In order to create invitations in an organization, the authenticated user must be an organization owner."
  - "`invitee_id` … Required unless you provide email." / "`email` … Required unless you provide invitee_id."
  - The `role` enum is "`admin`, `direct_member`, `billing_manager`, `reinstate`". `reinstate` "Only works if the invitee was previously part of your organization."
  - "`team_ids` (array of integers) Specify IDs for the teams you want to invite new members to."
  - "This endpoint triggers notifications. Creating content too quickly using this endpoint may result in secondary rate limiting." Response 422 means "Validation failed, or the endpoint has been spammed."
- [perms], section *Organization permissions for "Members"*:
  - `` `POST /orgs/{org}/invitations` | write | UAT, IAT ``
  - `` `DELETE /orgs/{org}/invitations/{invitation_id}` | write | UAT, IAT ``
  - `` `PUT /orgs/{org}/memberships/{username}` | write | UAT, IAT ``
  - `` `GET /orgs/{org}/invitations` | read | UAT, IAT ``
- [members] "Set organization membership for a user": "To prevent abuse, organization owners are limited to creating 50 organization invitations for an organization within a 24 hour period. If the organization is more than one month old or on a paid plan, the limit is 500 invitations per 24 hour period." [invite-help] says the same thing: "you can only create 50 organization invitations within a 24-hour period…"
- [invite-help]: "If your organization has a paid per-user subscription, an unused license must be available before you can invite a new member…"
- https://docs.github.com/en/rest/using-the-rest-api/rate-limits-for-the-rest-api: "no more than 80 content-generating requests per minute and no more than 500 content-generating requests per hour are allowed. Some endpoints have lower content creation limits."
- The list endpoint's role filter also shows `hiring_manager` as a returned role ([members], "List pending organization invitations"). You can't create invitations with it.

**Confidence.**
- Endpoint, parameters and the app permission for an IAT: **HIGH**.
- Whether the 50/500 limit counts app-made invitations in the same per-org bucket: **UNVERIFIED** (the wording is about "organization owners").
- Behaviour when inviting someone who is already a member (probably 422): **UNVERIFIED**.

## 2. Repo collaborator invitations

**Answer.**
- **Endpoint:** `PUT /repos/{owner}/{repo}/collaborators/{username}`, with a `permission` body. Valid values are `pull`, `triage`, `triage_plus`, `push`, `maintain`, `admin`, or any custom repository role name the org has defined. The default is `push`.
- **App permission:** repository permission **"Administration": write** (IAT accepted).
- **Status codes:**
  - **201** means an invitation was created.
  - **204** covers three cases: the user is already a collaborator (the permission is then updated, "with no other indication that the permission level changed"); the user is an org member (added directly, with no invitation); or the user already has access through a team.
- **Org members:**
  - The call adds them as a direct collaborator. It doesn't make them outside collaborators, and it doesn't add non-members to the org.
  - When an org base role is set, the granted role must be ≥ the base role. Otherwise the request fails with `Cannot assign {member} permission of {role name}`.
- **Email:** you can't invite to a repo by email. The path takes `username` only, and no email parameter exists.
- **Rate limit:** 50 invitations per repository per 24h. The limit doesn't apply to org members invited to an org repo.
- **Admin policy:** org/enterprise admins can forbid adding outside collaborators.
- **Changing a pending invitation:** `PATCH /repos/{owner}/{repo}/invitations/{id}` accepts only `read|write|maintain|triage|admin`. It takes no custom roles.

**Evidence.** All quotes are from [collab], "Add a repository collaborator", unless marked otherwise.
- "If the repository is owned by an organization, this API does not add the user to the organization - a user that has repository access without being an organization member is called an 'outside collaborator'…"
- "There are restrictions on which permissions can be granted to organization members when an organization base role is in place. In this case, the role being given must be equal to or higher than the org base permission. Otherwise, the request will fail with: Cannot assign {member} permission of {role name}"
- "The endpoint can also be used to change the permissions of an existing collaborator … The response will be a 204, with no other indication that the permission level changed."
- "You are limited to sending 50 invitations to a repository per 24 hour period. Note there is no limit if you are inviting organization members to an organization repository."
- `permission`: "We accept the following permissions to be set: pull, triage, triage_plus, push, maintain, admin and you can also specify a custom repository role name…" The default is `push`.
- 204 is the "Response when: an existing collaborator is added as a collaborator / an organization member is added as an individual collaborator / an existing team member (whose team is also a repository collaborator) is added as an individual collaborator".
- "Adding an outside collaborator may be restricted by enterprise and organization administrators."
- [perms], *Repository permissions for "Administration"*:
  - `` `PUT /repos/{owner}/{repo}/collaborators/{username}` | write | UAT, IAT ``
  - `` `PATCH …/invitations/{invitation_id}` | write ``
  - `` `DELETE …/invitations/{invitation_id}` | write ``
  - `` `GET /repos/{owner}/{repo}/invitations` | read ``
- [repo-inv], "Update a repository invitation": "`permissions` … Can be one of: `read`, `write`, `maintain`, `triage`, `admin`".
- [openapi]: the path parameter is `{username}`. There is no email-based repo-invite path. The full list of invitation paths is `/orgs/{org}/failed_invitations`, `/orgs/{org}/invitations[/{id}[/teams]]`, `/orgs/{org}/teams/{team_slug}/invitations`, `/repos/{owner}/{repo}/invitations[/{id}]`, `/teams/{team_id}/invitations` and `/user/repository_invitations[/{id}]`.

**Confidence.**
- Everything above: **HIGH**.
- Whether the base-role restriction also applies when the user already has a higher direct grant: **MEDIUM**.

## 3. Role precedence (keep the higher existing role)

**Answer.**
- **Repo, reading the current role:** `GET /repos/{owner}/{repo}/collaborators/{username}/permission` ("Metadata": read). It returns:
  - `permission`: the legacy level (admin/write/read/none; maintain is reported as write, triage as read).
  - `role_name`: includes custom roles.
  - Both are the **highest effective** role across repo, team, org and enterprise grants.
- **Repo, the catch:** that endpoint can't tell an org-level grant from a direct repo grant. So "current effective role" is easy to read, but "current direct grant" is not. `GET /repos/{owner}/{repo}/collaborators?affiliation=direct` is a candidate way to read direct grants (spike).
- **Org:** `GET /orgs/{org}/memberships/{username}` ("Members": read) returns `role` (admin|member) and `state` (active|pending).
- **Accidental lowering is possible on every write path:**
  - **Repo:** `PUT …/collaborators/{username}` silently *updates* an existing collaborator's permission and returns 204. It can therefore lower a direct grant.
  - **Org:** `PUT /orgs/{org}/memberships/{username}` with `role: member` demotes an owner ("If the authenticated user changes an owner's role to member, no email will be sent").
  - **Teams:** `PUT …/teams/{slug}/memberships/{username}` updates the role of an existing team member. Its default is `member`, so it can demote a maintainer.
  - **What P1 must do:** read the current role first and skip any write that would lower it. Role precedence is the app's job, and GitHub doesn't protect against it.
- **The author's question: org member + repo grant lower than the base permission.**
  - The API **rejects** it (`Cannot assign {member} permission of {role name}`). It doesn't silently lower anything, and effective access would have stayed at the base level anyway.
  - Base permissions apply only to members, not to outside collaborators, and a higher grant overrides the base.
  - P1 therefore has to handle a role below the base (or "none") as a **no-op**: skip it, or clamp it to the base. It must not show it as an error to the invitee.

**Evidence.**
- [collab], "Get repository permissions for a user":
  - "The calculated permissions are the highest role assigned to the collaborator after considering all sources of grants, including: repo, teams, organization, and enterprise."
  - "There is presently not a way to differentiate between an organization level grant and a repository level grant from this endpoint response."
- [perms], "Metadata": `` `GET /repos/{owner}/{repo}/collaborators/{username}/permission` | read | UAT, IAT ``
- [members], "Get organization membership for a user": "The state parameter in the response can be used to identify the user's membership status." And under "Set organization membership": "If the authenticated user changes an owner's role to member, no email will be sent."
- [teams], "Add or update team membership for a user": "If the user is already a member of the team, this endpoint will update the role of the team member's role." `role` has the default `member` and the values `member|maintainer`.
- https://docs.github.com/en/organizations/managing-user-access-to-your-organizations-repositories/managing-repository-roles/setting-base-permissions-for-an-organization:
  - "Base permissions do not apply to outside collaborators."
  - "If someone with admin access to an organization's repository grants a member a higher level of access for the repository, the higher level of access overrides the base permission."

**Confidence.**
- Read endpoints, the silent-update behaviour and base-role rejection: **HIGH**.
- Reliably separating direct grants from inherited ones: **UNVERIFIED** (spike).
- How to rank custom roles against each other (a custom role's base role plus its added permissions): **MEDIUM**. That needs `GET /orgs/{org}/custom-repository-roles` (not verified here).

## 4. Pending invitations: list, cancel, failed, resend, expiry

**Answer.**
- **Org invitations:**
  - `GET /orgs/{org}/invitations` lists pending ones, filtering by `role` and `invitation_source` (`all|member|scim`).
  - `DELETE /orgs/{org}/invitations/{invitation_id}` cancels one.
  - `GET /orgs/{org}/failed_invitations` returns `failed_at` and `failed_reason`.
  - `DELETE /orgs/{org}/memberships/{username}` also cancels a pending invitation for a known user.
  - `GET /orgs/{org}/invitations/{id}/teams` reads the teams attached to an invitation.
- **Repo invitations:** `GET /repos/{owner}/{repo}/invitations` (each item has an `expired` boolean), plus `PATCH` and `DELETE /repos/{owner}/{repo}/invitations/{id}`.
- **Resend:**
  - The REST API has **no resend or retry endpoint**. "Retry invitation" exists only in the web UI, on the Failed invitations page.
  - The API equivalent is to cancel (if still pending) and create a new invitation. That consumes rate limit and creates a new id.
  - GraphQL has no org or repo invite mutation either, only enterprise ones.
- **Expiry:**
  - Fixed at **7 days** for both org and repo invitations.
  - The REST API has no parameter, and the docs mention no setting to change it.
  - Our own shorter expiry has to be enforced by cancelling invitations ourselves.

**Evidence.**
- [openapi], the full list of invitation paths and methods:
  - `/orgs/{org}/failed_invitations` [get]
  - `/orgs/{org}/invitations` [get, post]
  - `/orgs/{org}/invitations/{invitation_id}` [delete]
  - `/orgs/{org}/invitations/{invitation_id}/teams` [get]
  - `/repos/{owner}/{repo}/invitations/{invitation_id}` [patch, delete]
  - … and no `resend` or `retry` path anywhere in the description.
- [openapi] schema `organization-invitation` has these properties: `id, login, email, role, created_at, failed_at, failed_reason, inviter, team_count, node_id, invitation_teams_url, invitation_source`. It has no `expires_at`.
- [openapi] schema `repository-invitation` has these properties: `…, created_at, expired, …`.
- [members]:
  - "The return hash contains failed_at and failed_reason fields which represent the time at which the invitation failed and the reason for the failure."
  - "If the specified user has been invited to the organization, this will cancel their invitation." (`DELETE /orgs/{org}/memberships/{username}`)
- [invite-help]:
  - "If an invitee does not accept the invitation within seven days, the pending invitation expires automatically."
  - "Invitations expire after 7 days. You can retry or cancel expired invitations, either one by one or in bulk." (UI steps under "Failed invitations")
- Changelog, 2020-02-05 (https://github.blog/changelog/2020-02-05-self-expiring-repository-and-organization-invitations/): "Invitations to join an organization or become a collaborator on a repository will expire seven days after they are created."
- GraphQL schema `type Mutation`: the only invite mutations are `inviteEnterpriseAdmin`, `inviteEnterpriseMember`, `cancelEnterpriseAdminInvitation` and `cancelEnterpriseMemberInvitation`. There are none for org members or repo collaborators.

**Confidence.**
- No resend API, fixed 7 days, and the list/cancel endpoints: **HIGH**.
- That expired org invitations show up in `failed_invitations`, and the exact `failed_reason` values: **MEDIUM/UNVERIFIED**. The UI text implies it.

## 5. Acceptance tracking (webhooks)

**Answer.**
- **Org:** the `organization` event, action `member_added` ("A member accepted an invitation to join an organization"). `member_invited` fires when an invitation is created, and `member_removed` fires on removal.
- **Repo:** the `member` event, action `added` ("A GitHub user accepted an invitation to a repository"), plus `edited` and `removed`.
- **Team:** the `membership` event, `added`/`removed`.
- **Subscription:** all three need the GitHub App to have the organization permission **"Members": read** (or higher).
- **Not observable by webhook:** invitation expiry, decline and cancellation. Reconcile by polling:
  - org: `GET /orgs/{org}/invitations` and `/failed_invitations`;
  - repo: `GET /repos/{owner}/{repo}/invitations` and its `expired` flag.
- **Where this leaves the repo-only app:**
  - The `member` event needs the *organization* "Members" permission. A repo-only app would still have to request org Members:read to observe repo acceptance.
  - Personal-account repos are a separate case: with no org permission, it's not clear the event can be subscribed to (UNVERIFIED).

**Evidence.**
- [openapi] `x-webhooks` descriptions:
  - `organization-member-added`: "A member accepted an invitation to join an organization."
  - `organization-member-invited`: "A member was invited to join the organization."
  - `member-added`: "A GitHub user accepted an invitation to a repository."
  - `member-edited`: "Permissions were changed for a collaborator on a repository."
  - `membership-added`: "An organization member was added to a team."
- [hooks], sections `organization`, `member` and `membership`: "To subscribe to this event, a GitHub App must have at least read-level access for the 'Members' organization permission." The `organization` payload's `membership` field is "Not present when the action is member_invited."
- [hooks] lists no event for invitation expired, declined or cancelled. The action lists for `organization` are `deleted, member_added, member_invited, member_removed, renamed`, and for `member` they are `added, edited, removed`.

**Confidence.**
- Event names and the required permission: **HIGH**.
- Whether `member.added` also fires for the 204 direct-add of an org member (no invitation), and whether the payload identifies which invitation was accepted: **UNVERIFIED**. A match on user id + repo is probably needed.
- That expiry is not webhooked: **MEDIUM** (absence of evidence).

## 6. Email-domain restriction

**Answer.**
- **Reading the user's emails:**
  - `GET /user/emails` works with a GitHub App **user access token** when the app has the account permission **"Email addresses": read**. That permission is granted when the user *authorizes* the app; the org owner doesn't grant it at install.
  - It lists all of the user's emails with the `primary`, `verified` and `visibility` fields.
  - **Private emails are included**, because it's the authenticated user's own list.
- **Native enforcement for org invites:** when an org invitation is created **by email**, GitHub itself only lets the invitee accept it from a matching *verified* email. So for org links, "invite by the user's verified in-domain email" gives a second, GitHub-enforced check.
- **Repos:** you can only invite by username (§2), so the app-side check is the only one there.
- **No native domain filter:** GitHub has no API or setting that restricts invitations by email domain. Org "verified domains" only drive the Verified badge and restrict *notification* emails to verified domains. They don't gate invitations.
- **Caveats:**
  - Whether `users.noreply.github.com` addresses show up in the list (and as `verified: true`) is **UNVERIFIED**. The domain check must ignore them anyway.
  - "Verified" means the user proved control of the mailbox at some point. GitHub doesn't re-verify it, and it doesn't mean the person is still employed at that domain.

**Evidence.**
- [perms], *User permissions for "Email addresses"*: `` `GET /user/emails` | read | UAT `` (UAT only, so it can't be called with an installation token).
- https://docs.github.com/en/rest/users/emails, "List email addresses for the authenticated user": "Lists all of your email addresses, and specifies which one is visible to the public." Its response schema has "`primary`: required, boolean / `verified`: required, boolean / `visibility`: required, string or null".
- https://docs.github.com/en/apps/using-github-apps/installing-a-github-app-from-a-third-party: "When you authorize a GitHub App, you grant the app access to your GitHub account, based on the account permissions the app requested… For example, you might grant the GitHub App permission to read your email addresses…"
- [invite-help]: "If you use an email address for the invitation, the invitee will only be able to accept the invitation if the email address matches with a verified email address associated with the invitee's personal account on GitHub."
- https://docs.github.com/en/organizations/managing-organization-settings/verifying-or-approving-a-domain-for-your-organization: "After verifying ownership of your organization's domains, a 'Verified' badge will display…" and "…verifying your domain and restricting email notifications to only verified email domains…" The page says nothing about restricting invitations.

**Confidence.**
- The endpoint, the permission and the `verified` field: **HIGH**.
- No native domain restriction: **MEDIUM** (absence of evidence across the invite docs).
- noreply handling: **UNVERIFIED**.

## 7. Access expiry (automatic removal)

**Answer.**
- **Org:** `DELETE /orgs/{org}/members/{username}` or `DELETE /orgs/{org}/memberships/{username}`, with "Members": write (IAT OK).
  - Removing a member also removes them from all teams and all org repo access.
  - Indirect membership through an enterprise team survives.
- **Repo:** `DELETE /repos/{owner}/{repo}/collaborators/{username}`, with "Administration": write (IAT OK).
  - The user may *still* have access through the org base permission or teams.
  - Side effects: forks are deleted or removed, issues are unassigned, the repo is unstarred.
  - Background propagation can lag.
- **Teams:** `DELETE /orgs/{org}/teams/{slug}/memberships/{username}`, with "Members": write.
- **Who does the timing:** scheduling is ours (a Restate timer). GitHub has no native expiring grant.
- **Policy decision:** removing a grant should restore any *previous* lower role rather than remove the user completely. That follows from role precedence (§3).

**Evidence.**
- [perms], "Members":
  - `` `DELETE /orgs/{org}/members/{username}` | write | UAT, IAT ``
  - `` `DELETE /orgs/{org}/memberships/{username}` | write | UAT, IAT ``
  - `` `DELETE /orgs/{org}/teams/{team_slug}/memberships/{username}` | write | UAT, IAT ``
- [perms], "Administration": `` `DELETE /repos/{owner}/{repo}/collaborators/{username}` | write | UAT, IAT ``
- [members], "Remove an organization member":
  - "Removing a user from this list will remove them from all teams and they will no longer have any access to the organization's repositories."
  - "If a user has both direct membership in the organization as well as indirect membership via an enterprise team, only their direct membership will be removed."
- [collab], "Remove a repository collaborator":
  - "If the user had their own fork of the repository, the fork will be deleted."
  - "A user can still have access to the repository through organization permissions like base repository permissions."
  - "Although the API responds immediately, the additional permission updates might take some extra time to complete in the background."

**Confidence.** **HIGH**.

## 8. Team membership for non-members

**Answer.**
- Yes. `PUT /orgs/{org}/teams/{team_slug}/memberships/{username}` called by an org owner (or with an app that has "Members": write) on a **non-member** sends an org invitation by email. The membership stays `pending` until the user accepts.
- Two alternatives exist:
  - `team_ids` on `POST /orgs/{org}/invitations` assigns teams in a single invitation;
  - teams can be assigned on the `organization.member_added` webhook.
- Team writes fail with an error if the team is IdP-synced (team sync).
- On existing members the role defaults to `member`, which can demote a maintainer (§3).

**Evidence.**
- [teams]:
  - "When an organization owner adds someone to a team who is not an organization member, this endpoint will send an invitation to the person via email. This newly-created membership will be in the 'pending' state until the person accepts the invitation…"
  - "When you have team synchronization set up for a team with your organization's identity provider (IdP), you will see an error if you attempt to use the API for making changes to the team's membership."
- [perms], "Members": `` `PUT /orgs/{org}/teams/{team_slug}/memberships/{username}` | write | UAT, IAT ``

**Confidence.**
- Documented behaviour: **HIGH**.
- That this path works the same way with an IAT, and counts against the org invitation limit: **UNVERIFIED**.

## 9. Bulk invite and resolving emails to users

**Answer.**
- **No bulk API:** neither REST nor GraphQL has a bulk invitation endpoint. A bulk invite is N sequential calls, throttled by:
  - the org limit (50 or 500 per 24h),
  - the repo limit (50 per repo per 24h, except org members),
  - the secondary limits (80 content-creating requests per minute, 500 per hour).
- **Emails, org invites:** don't resolve them to users. `POST /orgs/{org}/invitations` with `email` works directly, and GitHub matches the email against the invitee's verified emails at acceptance.
- **Emails, repo invites:** these need a username. Searching for users by email is unreliable:
  - `GET /search/users` "does not accept authentication and will only include publicly visible users".
  - `in:email` matches only the **public** email.
  - "For privacy reasons, you cannot search by email domain name."
- **What this means for bulk emails to repos:** don't resolve them. Use one of these instead:
  - send each person a personal P1 invite link, so they sign in and their identity (and verified emails) comes from their token; or
  - for org-owned repos, invite them to the org by email first.

**Evidence.**
- [openapi]: the invitation paths listed in §4 include no bulk endpoint. GraphQL `type Mutation` has no org or repo invite mutations (§4).
- https://docs.github.com/en/rest/search/search, "Search users": "This endpoint does not accept authentication and will only include publicly visible users. As an alternative, you can use the GraphQL API." It also states text match "for the issue login, public email, and name fields".
- https://docs.github.com/en/search-github/searching-on-github/searching-users: "With the `in` qualifier you can restrict your search to the username (`login`), full name, public email … For privacy reasons, you cannot search by email domain name."
- Rate limits: see §1 and §2.

**Confidence.**
- No bulk API and the limits of search: **HIGH**.
- Whether GraphQL user search finds private emails: **UNVERIFIED**, and unlikely.

## 10. Two-app split (no optional permissions)

**Answer.**
- **No optional permissions:**
  - A GitHub App's permissions are fixed per app registration.
  - Installing grants **all** the repository and organization permissions the app requested. The docs describe no per-permission opt-out.
  - When the app later asks for more permissions, an owner who doesn't approve keeps the *old* set. The installation stays all-or-nothing.
  - The docs never describe "optional permissions"; this is absence of evidence (MEDIUM).
- **The split helps least privilege and the install UX, but less than hoped:**
  - **Org-invite app:** needs org "Members": write. Only org owners can install it.
  - **Repo-invite app:** needs repo "Administration": write. That's broad: it also covers `DELETE /repos/{owner}/{repo}` and `PATCH /repos/{owner}/{repo}`.
    - Because it requests "Repository administration", **repo admins cannot install it themselves**. An org owner must install it, so the split doesn't let non-owners self-serve.
    - To observe repo acceptance through the `member` webhook it would *also* need org "Members": read (§5).
  - **What a separate repo app still buys:** it avoids asking owners for org-wide Members:write (which allows inviting org owners and removing members) when they only want repo links, and it works on personal accounts.
- **Email addresses:** the "Email addresses" account permission sits on whichever app does the user login. It's granted by each user at authorization, not by the org owner at install.

**Evidence.**
- https://docs.github.com/en/apps/creating-github-apps/registering-a-github-app/choosing-permissions-for-a-github-app:
  - "When a user installs an app on their user account or organization, they see and grant the repository and organization permissions that the app requested."
  - "When you do so, the owner of each account where the app was installed will be prompted to approve the new permissions. If the account owner does not approve the new permissions, their installation will continue to use the old permissions."
  - "Some webhooks and API access require 'Administration' permissions. If your app requires 'Administration' permissions, consider explaining this requirement on your app's homepage."
- https://docs.github.com/en/apps/using-github-apps/approving-updated-permissions-for-a-github-app: "If you do not approve the additional permissions, the GitHub App will still retain its current permissions."
- https://docs.github.com/en/apps/using-github-apps/installing-a-github-app-from-a-third-party: "Repository admins can install GitHub Apps in the organization that owns the repository if the app does not request any organization permissions or the 'Repository administration' permission."
- [perms], "Administration" also lists `` `DELETE /repos/{owner}/{repo}` | write | UAT, IAT `` and `` `PATCH /repos/{owner}/{repo}` | write ``.

**Confidence.**
- Fixed, all-or-nothing permissions and who can install: **HIGH**.
- That GitHub has no "optional permissions" feature: **MEDIUM**. No primary source mentions one, and the approval flow implies there isn't.

---

## Risk verdicts

| Risk | Verdict | Why |
|---|---|---|
| R-P1-org-invite | **feasible-with-changes** | Inviting, `team_ids` and role all work with "Members": write on an installation token. Role precedence must be enforced by the app with a read-before-write check, because every PUT (membership, collaborator, team) silently overwrites and can demote. A role below the org base permission is rejected, so treat it as a no-op. |
| R-P1-acceptance | **resolved-feasible** | `organization.member_added`, `member.added` and `membership.added` (all need org "Members": read) report acceptance. Expiry, decline and cancel aren't webhooked, so add a polling reconciler. Our own expiry works by `DELETE …/invitations/{id}` on a timer. |
| R-P1-email-domain | **resolved-feasible** | `GET /user/emails` with a user token and the "Email addresses": read account permission returns `verified` flags (private emails included). For org links, inviting by email adds GitHub's own verified-email check at acceptance. There's no native domain filter, and noreply handling still needs confirming. |
| R-P1-ratelimit | **feasible-with-changes** | No bulk API exists. The limits are 50 (new/free orgs) or 500 org invites per 24h, 50 per repo per 24h (org members exempt), plus 80/min and 500/h secondary limits. Bulk invite needs a queue with daily-budget backoff and "partially sent" UI. Whether app-made invites count in the same bucket is unverified. |
| R-P1-access-expiry | **resolved-feasible** | Org, team and repo removal endpoints all work with installation tokens (Members write, Administration write). Restate timers schedule them. Caveats: base or team access can remain after repo removal, removal deletes forks, and "restore previous role" needs stored prior state. |
| R-P1-two-apps | **feasible-with-changes** | Permissions are all-or-nothing per app, so a split is the only way to offer a lower-privilege repo-only option. But the repo app still needs Administration: write (owner-only install, can delete repos), plus org Members: read for `member` webhooks. The benefit is avoiding org Members: write, not letting repo admins self-serve. Decide on that basis. |

## Spikes to settle the open questions

**Setup (shared).**
- A throwaway **free** test org (≤ 1 month old, to see the 50/24h limit) and, if possible, a second older or paid org.
- Two or three secondary GitHub user accounts: one with only a noreply/private email, one with a verified custom-domain email.
- A test GitHub App with org "Members": R/W, repo "Administration": R/W, "Metadata": R, account "Email addresses": R, subscribed to the `organization`, `member` and `membership` events. Deliveries go to smee.io or a request bin.
- Set the org base permission to **Write** for S3.

| # | Question | Steps | Pass / fail |
|---|---|---|---|
| S1 | Do installation tokens create org invites with role and `team_ids`, and do teams attach on acceptance? | Using an IAT, `POST /orgs/{org}/invitations` with `invitee_id`, `role=direct_member` and `team_ids=[t]`. Accept as the test user. Check `GET /orgs/{org}/memberships/{u}` and team membership. | Pass: 201, then after acceptance active membership plus team. Fail: 403 or 404 for the IAT, or the team not attached. |
| S2 | Does inviting an existing member or owner fail or demote? | `POST /orgs/{org}/invitations` for an existing owner. Then `PUT /orgs/{org}/teams/{t}/memberships/{maintainer}` with no role. | Record the status codes. Pass (for the design): the invite is 422 and the team PUT demotes, which confirms read-before-write is needed. |
| S3 | Repo PUT for an org member below or above the base, and lowering a direct grant | With base=Write: PUT `permission=pull` for a member (expect the "Cannot assign" error), then `maintain`, then `push`. After each, GET `…/permission` and `GET …/collaborators?affiliation=direct`. | Pass: the error text matches, and the direct grant can be told apart from the base. Fail: a silent downgrade, or direct and inherited grants can't be told apart. |
| S4 | Webhooks on acceptance, direct-add and expiry | Create an org invite and a repo invite. Accept one, cancel one, decline one. Direct-add an org member to a repo (204). Leave one invite past 7 days, or check `failed_invitations` after a cancel. | Pass: `member_added` and `member.added` arrive with a user id that can be matched to the invitation, and the 204 direct-add is either observed or documented as not. Record what cancel and decline produce (expected: nothing). |
| S5 | Do app-made invites count toward the 50/24h per-org limit? | In the new free org, create 51 email invitations (to plus-addressed test emails) using the IAT, or 49 by a human owner and 2 by the app. Cancel them all afterwards. | Pass: the 51st fails with 422 "spammed" and the shared bucket is confirmed. Note the exact error body for the UI. |
| S6 | `GET /user/emails` contents with a GitHub App user token | Authorize the app as each test user. Call `/user/emails`. | Pass: private emails are listed with `verified`. Record whether the noreply address appears and how it's flagged. Fail: 403, or emails missing. |
| S7 | Repo-only app install by a repo admin | Register a second app with only repo "Administration" and "Metadata". Try installing it as a non-owner repo admin. | Expected per docs: blocked (owner approval request). Pass confirms that the two-app split doesn't enable admin self-serve. |

### Implementation follow-up: positive eligibility facts and retry policy (2026-10-16)

GitHub's organization invitation guide requires an available license for paid
per-user organizations and distinguishes invitations from Enterprise Managed
Users, which are provisioned through SCIM. Its organization member REST reference
explicitly says invitation operations do not work for EMU organizations. Neither
page supplies a stable error body or machine-readable refusal code for a no-seat
or EMU organization invitation. These sources establish the policy, not a tested
installation-token error classifier:

- [Inviting users to join an organization](https://docs.github.com/en/enterprise-cloud%40latest/organizations/managing-membership-in-your-organization/inviting-users-to-join-your-organization).
- [Organization member REST endpoints](https://docs.github.com/en/enterprise-cloud%40latest/rest/orgs/members?apiVersion=2022-11-28).

The [team membership REST reference](https://docs.github.com/en/rest/teams/members?apiVersion=2022-11-28)
says writes to IdP-synchronized team membership fail, but does not specify their
exact error payload. Generic403/422 responses, an organization login, and missing
billing fields therefore cannot establish NoSeat, IdpSyncedTeam, or
ManagedEnterprise. Likewise the failed-invitations endpoint documents
`failed_at`/`failed_reason`, but its example does not establish a finite expiry
reason vocabulary; failure-list presence alone is not Expired.

P1 currently accepts positive `plan.seats` and `plan.filled_seats` capacity facts
for NoSeat and retains a daily retry deadline. Repeated create calls or old wake
messages cannot shorten that deadline. The typed ManagedEnterprise refusal has a
no-self-retry contract, independently tested from wire classification. No live
probe or guessed error wording is claimed by those local tests; the error-body
mappings remain an explicit SP-01/SP-18 follow-up.

### Organization warning visibility and sync facts (2026-10-17)

The [Get an organization reference](https://docs.github.com/en/rest/orgs/orgs#get-an-organization)
explicitly requires the GitHub App **Organization plan** permission for plan
information. P1's minimal Members/Metadata installation profile does not request
that permission. Consequently paid-seat capacity, 2FA requirements, and base
repository permissions are optional token-visible facts; missing fields remain
unknown. The local warning cache does not claim that current installations can
read every field. Live visibility verification and any permission expansion are
separate decisions. An unknown base-permission enum is also unknown, never an
inferred access grant.

The [team-sync mapping endpoint](https://docs.github.com/en/enterprise-cloud@latest/rest/teams/team-sync)
supports installation tokens but requires **Members write even for its GET**.
It cannot be silently added to P1's read-only authority/cache profile. The
[GitHub Enterprise Server team response](https://docs.github.com/en/enterprise-server@3.21/rest/teams/teams)
documents `ldap_dn`; a nonempty value is positive external-management evidence.
P1 normalizes that field to a boolean and discards the DN. Its absence does not
prove that Cloud team synchronization is disabled. Existing sufficient membership
remains satisfied; missing membership with positive mapping is a typed blocked
team, and immutable org-invitation team sets are never silently shortened.

The [organization webhook reference](https://docs.github.com/en/webhooks/webhook-events-and-payloads#organization)
identifies the removed member through `membership.user`. A member-removal event
is an account capacity hint, not proof that this removed person requested an
invitation or that a seat is now available. P1's retained account inbox deduplicates
the authenticated payload hash and wakes only NoSeat deliveries for the captured
current installation; each delivery performs its full live protocol again.
