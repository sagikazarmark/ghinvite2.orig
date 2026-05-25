I'd like to create an application that allows me to share GitHub repositories with others.

Here is how I imagine it would work:

- I login with my GitHub account (oauth)
- I authorize a GitHub application to gain acceess to manage members for repositories in an organization (use GitHub app, so we can issue short-lived installation tokens for repos automatically without users having to share a PAT or anything else)
- I create a new invitation link: selecting the repositories (within the org/user) to get access to, membership level. Other settings for the link: each invitation request requires manual approval, expiration, max uses. (In the future maybe others?)
- I send the link to the users I want the repo(s) to be shared with

- Users then go to that link
- They also log in with their GitHub account
- They click request invitation (they can review which repos they are gaining access to? is that secure?)
- If the link is expired or max used, etc, then error
- If the link requires approval, the user sees pending request page (waiting approval from an admin; in the future, send notification, in v1 not necessary)
- Admin approves
- Invitation is sent to repos

I think there is an expiration on invitations automatically by github. (check this)
If the expiration is reached, check if the invitation is still pending or not. If it was accepted, invitation is closed.
If it was expired, invitation is marked as expired.

An invitation can also be revoked (is this the right word?) by an admin (while it's still pending). (THis may be problematic, because we may not have up-to-date information if an invitation was accepted or not, more about this later)

An invitation link can also be revoked (is this the right word?) by an admin. In the future, the admin may also select cancelling/revoking any pending invitations that haven't been accepted yet. Probably not needed in v1.

In v1, I don't think editing links (eg. updating max use, updating expiration) is necessary (may even be less secure). Admins can always create new links.

Technically, there is no difference between admins and users: everyone can use the platform for invitation. There is an "admin" dashboard for authorizing the app and managing invitation links. When users go to an invitation link, they only see that, no dashboard is involved.

Speaking of managing invitation links: admins can create and revoke. But they can also see who used those links (in some view).

Also (and this is very important): there needs to be an audit log in the app to see when someone requested an invite, who and when approved it (if approval was necessary), when the invitation was accepted (if we have that info), etc.

Invitation state on GitHub's side: theoretically, GitHub allows sending webhook events about member additions. The GitHub App can set that webhook up. When a member is added to the repo, GitHub sends a webhook. We check against org id and invitee (Do we get the invitation ID?). We try to identify the invitation based on those details and mark an invitation accepted and closed. But for this, we need webhooks which may be an overkill for v1. An alternative is periodically checking non-accepted invitations with the github API. Need a decision and good arguments why.


Questions:
- Who can approve requests, revoke links, etc? Org admins? ANyone who can grant app access? Within this application, how do we authorize users?
- Can a github app invite members into an organization?
- Can a gh app be installed multiple times on an org? At the same time or at a later time (eg. access revoked then allowed again)? If the latter, is that two separate installations?


Tech stack:

I have two concrete ideas:

Most of the invitation logic explained above can be implemented using Restate.

An invitation link can be modeled as a virtual object tracking uses, expiration, etc.

An invitation request can be a call on that object that kicks off a workflow: listens for external events (revocation, approval, GitHub webhook)

The biggest question: Should Restate be the single source of truth for information for the frontend (ie. talk to restate directly to list invitation links), or maybe have a database that tracks a projection state (written by restate exclusively). (A database is necessary anyway for audit logs)

For the frontend: I'd like to use dioxus with tailwindcss and daisyUI, combined with tower-sessions for user login, octocrab for talking to the github API, oauth2 crate for gh oauth login.

User gets do the dashboard. At the top, org selector. In empty state: authorize? Otherwise a global list of invitation links?

Select an org, get to invitation links. Be able to go to audit log.


Help me turn these ideas into:
- Research questions: how should these features be phrased? What other features could be added to the list?
- Research other projects: I mentioned a few, but I'M sure there are others. Do a very detailed research
- A list of questions about user flows, architecture, etc
- Gather information available on the internet about this topic


Interview me relentlessly about every aspect of this plan until we reach a shared understanding. Walk down each branch of the design tree, resolving dependencies between decisions one-by-one. For each question, provide your recommended answer.

