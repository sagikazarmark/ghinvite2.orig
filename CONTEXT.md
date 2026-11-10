# ghinvite Context

## Glossary

### Account
A GitHub account where ghinvite is installed. An account may be a personal GitHub account or a GitHub organization.

### Installing Owner

For a repository-app installation on an organization, the Installing Owner is
recorded from the sender of an authenticated installation-created or accepted-
permissions webhook. Setup callbacks contain verified installation facts but
cannot name an installer. App-scoped installation state and its revisioned SQL
projection retain this evidence. With only the repository app installed, this
owner can list the account's Console records. Creating or changing a link also
requires current administrator authority over every repository target.

### Repository Admin Delegation

An owner-controlled, default-off account setting that permits repository
administrators to manage links targeting repositories they administer. A link
spanning several repositories is visible only when the user administers all of
them. Counts, request queues and audit pagination use the same scope. Search
results are candidates for selection and do not grant authority.

Repository-only Bulk batches use the same complete-target authority for audit
history. Batch, request and access-removal events remain discoverable without an
invitation link; organization-bearing batches remain outside delegated scope.

### Console Authority Evidence

GitHub observations obtained with read-only installation credentials, bound to
numeric installation, account, repository and user identities. Evidence is
cached independently of browser sessions for less than five minutes; future
timestamps and expired observations confer no authority. A bounded read budget
can leave access pending. Pending evidence never authorizes a mutation.

The Console navigation's pending-request badge is a display of scoped projected
requests, shared by desktop and mobile navigation. Counts include a request only
when the viewer can administer every target in its immutable manifest. Partial
scope, count unavailability and search-first delegated discovery display an
unknown count; none is represented as zero or used as authorization evidence.
The badge never spends additional repository-read budget during discovery.

Existing-link authority and repository availability are separate: a personal
owner can stop links or decide historical requests after uninstall, while new
creation requires current installation selection. Delivery checks availability
again before an external effect.

### App-scoped Installation

The logical app (`repo` or `org`) and numeric installation ID together identify
an installation. Account installation authority is separately addressed by app
and numeric account ID. The same numeric ID in two app scopes never shares
state. Setup verifies the numeric App ID with that app's JWT; webhook verification
uses only that app's configured current/previous secrets.

### Personal Account
A GitHub account owned by an individual person, as distinct from a GitHub organization.

### GitHub User
An individual GitHub identity that can sign in, create invitation links, request access, or decide invitation requests.

### Ghinvite Session
A browser's continuity with ghinvite across requests, which may include a signed-in GitHub user's identity. It is distinct from the user's GitHub authorization and repository access.

### Sign Out
Ending the current browser's signed-in ghinvite session. Other browsers' sessions, GitHub authorization, and repository access are unaffected.

### Global Session Invalidation
Ending all existing ghinvite sessions so users must sign in again. It does not revoke GitHub authorization or repository access.

### Requester
A GitHub user who has submitted an invitation request.

`Recipient` is not a canonical domain role in ghinvite. Use `GitHub User` before an invitation request exists, and `Requester` after one exists. Admin-facing UI copy may use recipient informally for the person an account admin shares a link with. GitHub invitation integration may use recipient for the GitHub user who receives a collaborator invitation.

Requester profile facts are advisory. The approval Console can display a cached
GitHub avatar and account creation date, but numeric GitHub identity and current
complete-target authority remain decisive. Account age is never inferred from
local sign-in time. Avatar bytes are served through an authorized same-origin
route; browsers do not contact third-party image hosts.

### Account Admin
A GitHub user with current GitHub-derived authority to administer an account in ghinvite. For an organization account this is an organization owner; for a personal account this is the same GitHub user who owns the account.

### Invitation Link
An invitation link is an account-admin-created offer of access to immutable targets and their grants. Its internal Link ID addresses the authority and Console records. Its public URL carries an independent random Link Secret in the fragment. The URL is shown only after creation or rotation; the Console cannot reconstruct it. UI may use `link` alone when the context clearly establishes `invitation link`.

Public/requester-facing copy should usually say repository access. Admin/domain/GitHub-facing copy may say collaborator access when GitHub-specific precision matters.

An invitation link's max use is the maximum number of invitation requests that may be created through the invitation link. Its uses value is the number of invitation requests already created through it.

Use `max use` and `uses` in domain and UI language. A use means a created invitation request, not a page view, click, approval, or sent GitHub invitation.

A declined, expired, or cancelled invitation request still counts as a use; its outcome does not refund the use.

Invitation-link guardrails are the max use, expiration, targets and their individual grants, approval policy, invitation lifetime, and hourly abuse threshold. Guardrails are fixed when the invitation link is created. To use different guardrails, an account admin creates a new invitation link.

Invitation link expiration or revocation stops new invitation requests from being created through that invitation link. It does not change existing invitation requests or GitHub invitations.

User-facing UI should describe invitation-link expiration as stopping acceptance of new invitation requests when space allows.

`Revoke` is the canonical invitation link lifecycle verb. User-facing UI should describe the effect as stopping acceptance of new invitation requests.

Every invitation link has a description. An invitation link's description is admin-only context that identifies the purpose or audience of the invitation link. It is not visible in the invitation request flow. Descriptions are short single-line text, up to 120 characters, and may change after the invitation link is created.

Admin-facing invitation-link lists use the description as the primary identifier and the internal Link ID as secondary metadata. They never redisplay the secret.

### Internal Note
Optional freeform admin-only context attached to an invitation link. An internal note is not visible in the invitation request flow and may change after the invitation link is created.

### Link ID
The internal ULID identifying the invitation-link authority and Console record. It conveys no public access and is never accepted as an invitation code. Legacy `/i/{id}` paths return a neutral 404.

### Link Secret
A 256-bit random bearer value, encoded as 43 canonical base64url characters without padding. The public URL is `/i#<secret>`. The edge hashes the decoded 32 bytes with SHA-256; only that hash and its generation enter storage, sessions, continuations, or Restate. Native form submission carries the secret in a CSRF-protected request body. The browser erases the fragment immediately and proceeds through `/join` using its session selection.

### Secret Generation
The monotonically increasing version of the link's current secret. Rotation replaces the current hash, increments the generation, and invalidates older capabilities for fresh admission. It preserves original operation-bound admission and rotation receipts. The hash routing index is independent of link projection; only the link authority decides whether a hash and generation remain valid.

### Invitation Code
The Link Secret entered manually to open an invitation. It is the fragment value only, not an internal Link ID or the full invitation URL. UI may use `code` alone when nearby context establishes `invitation code`.

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
A requester's request for access through an invitation link. It may be auto-approved or wait for an account admin decision. A requester may have at most one pending or approved invitation request for a given invitation link at a time. Admin UI may use `Requests` as a compact label when the Console context makes the meaning clear.

A requester may create a new invitation request through the same invitation link after a previous invitation request is declined, expired, or cancelled. A requester may not create a new invitation request through the same invitation link while the latest invitation request is pending or approved. A request that becomes Lapsed permits a new request under the current link guardrails and the re-request limit.

An auto-approved invitation request is approved by invitation-link policy without account-admin review. Auto-approval does not guarantee that every GitHub invitation is sent successfully.

A reviewer may explicitly include a requester message in a new decline decision.
This optional public message is separate from historical administrator-only decline
notes: old notes are never published or backfilled. The message is normalized and
bounded to 500 characters, retained with the exact decision operation, and shown
only in that requester's authenticated status or to an authorized administrator.
A replay cannot replace or add text to an already completed decision.

An expired invitation request is one whose account-admin decision deadline passed before approval or decline. This is distinct from invitation link expiration and GitHub invitation expiration.

In the Console, `Requests` usually means the queue of pending invitation requests awaiting account-admin decision, not a complete invitation request archive.

### Invitation Request Admission
The authoritative acceptance or rejection of an attempt to create an invitation request, based on eligibility at admission time rather than submission time. Accepted admission creates one invitation request and consumes one use; replaying the same attempt preserves its outcome even after invitation link expiration or revocation.

Admission is distinct from approval: an admitted invitation request may still await an account admin decision.

### Invitation Request Decision Deadline
The time by which a pending invitation request must be approved or declined, established at admission using the pending-request lifetime. This deadline is independent of invitation link expiration and is not shortened by invitation link revocation.

### Approval Policy
The invitation-link setting that determines whether invitation requests are auto-approved or wait for an account-admin decision.

An invitation link's approval policy is fixed when the invitation link is created.

### GitHub Invitation
A repository collaborator invitation sent through GitHub for one repository as the result of an approved invitation request.

An approved invitation request may produce repository invitations and, with organization delivery enabled, an organization invitation with its attached teams. UI may summarize those GitHub invitations, but the domain object remains repository-scoped.

### Repository Access
A GitHub user's ability to collaborate on a repository at a specific permission level.

### Delivery Outcome Unknown
A repository delivery whose GitHub effect cannot yet be confirmed or ruled out.
It is distinct from a definitive delivery failure and does not authorize sending
another invitation.

### Delivery Generation

A generation identifies one repository delivery for a request and target.
Its canonical key is the request ID, numeric repository ID and delivery
generation. A later attempt within that delivery does not change this key.

Requester and Console status reads bind the owner snapshot to the request,
link, account, requester, immutable target and grant, and current generation.
A missing, unavailable or mismatched owner snapshot leaves projected history
explicitly historical; it does not confirm that history as the current outcome.
These reads never authorize another external write.

### Attempt Generation

A monotonic generation of the durable external-write fence within a delivery.
A positive rejection can release only its own generation. An uncertain attempt
remains fenced and permits read-only recovery, never an automatic repeat write.

### Effective Role

The requester's observed GitHub repository permission. Built-in roles are ranked
read, triage, write, maintain and admin. Custom roles and an unspecified base
role are unranked. A target's grant is a built-in role and may differ between
repositories in the canonical target model.

### Already Satisfied

A delivery whose pre-write observation establishes sufficient repository access.
The delivery does not issue a write or lower the requester's access.

### Upgraded

A delivery whose collaborator write returned 204 after observing an insufficient
built-in role. The retained result records the prior role and target grant.

### Already Pending

A delivery tracking a matching preexisting GitHub invitation with sufficient
permission, including one whose lower permission this delivery raised with a
PATCH. It is foreign for cancellation: stopping tracking does not delete it or
claim that GitHub cancelled it.

### Blocked Delivery
A repository delivery waiting for an unavailable prerequisite before an external
effect can safely proceed. The prerequisite may be an installation, a repository,
a verified identity, or GitHub's own willingness to answer. Approval and the
original repository scope remain in force while delivery is blocked.

### Throttled Delivery
A repository delivery GitHub answered with a rate limit instead of a decision.
It is a form of blocked delivery: nothing was created and nothing was refused on
the merits, so it is neither a delivery failure nor a delivery outcome unknown,
and it is retried once the limit GitHub named has had time to clear.

A rate limit is never evidence that an invitation cannot be sent, so it is never
presented to an account admin or a requester as a definitive failure.

### Target

An immutable access destination and grant on an invitation link. A repository
target identifies its account and repository numerically, retains a display
name, and grants one built-in role. The implemented M0 delivery path requires a
non-empty, duplicate-free set belonging to one account. Explicit organization
delivery composition additionally accepts organization and team targets. A mixed
link retains both app installation identities; an organization-only link uses its
primary installation identity for the organization app.

### Link State

M0 links are Active or Revoked. Expiration and exhausted uses separately stop
fresh admission. Revocation preserves already admitted requests and their
operation receipts. Pause, automatic pause and cascade are M1 lifecycle work.

### Delivery

One request's durable attempt to fulfill one target's grant, addressed by request,
target and delivery generation. Its retained owner holds the create receipt and
any settlement evidence. Projection lag cannot authorize another external write.

### Reconciler

A background observer of tracked repository and organization deliveries. SweepSchedule owns the
account's tracked set, schedule token and next deadline. It schedules hourly
while deliveries are tracked and daily while idle; new tracking can bring an idle
deadline forward. Reconcile reads the complete pending-invitation list once per
repository and sends observations to individual delivery owners. SQL projections
do not discover work or decide settlement. A failed read leaves tracking intact
for a later sweep. With organization delivery enabled, organization onboarding also
starts the idle schedule. OrganizationReconcile completes the pending and failed
invitation lists once per tracked organization, then sends the same observation
to each owner. Owners verify current numeric installation, organization and
requester identity before using absence, and reject stale observations. A failed
invitation timestamp alone does not establish expiry. Uncertain writes require
positive recovery evidence and retain their fences. Organization/member webhooks
request fresh observation; their payloads cannot establish acceptance.

Requester refresh addresses the generation retained by the request owner and is
limited to once per sixty seconds per delivery. Dispatch progress includes this
generation so delayed SQL cannot choose an older delivery for refresh.
An authenticated requester can open an owned request directly by Request ID
without retaining a browser-selected invitation link. This read cannot create a
new request or recover a link capability. Its target and generation come from
the request owner; the delivery owner independently decides whether observation
is still tracked. A refresh schedules read-only reconciliation and reports that
it was requested, rather than promising immediate GitHub access.

### Permission Level
The built-in repository role granted by one repository target. Different targets on one link may grant different roles. Grants are fixed at creation; a delivery-local permission describes only its own target.

### Available Repositories
The repositories a GitHub App installation currently makes available to ghinvite for an account.

The caller supplies app-scoped availability evidence with its observation time. Admission may reject on relevant negative evidence no older than five minutes. Missing or stale evidence does not reject, and the admission handler performs no GitHub I/O. Delivery checks current prerequisites before an effect. Evidence is part of an attempt's immutable input, so retries cannot silently substitute a newer observation.

### Repository Scope
The non-empty set of repositories selected on an invitation link for which invitation requests may request repository access.

Repository scope is fixed when the invitation link is created.

### Repository Identity
The GitHub owner and repository name pair that identifies a repository for collaborator invitations while preserving the display full name. It has exactly one owner and one repository name; both are non-empty.

## P1 implementation boundaries

Canonical Targets replaces the global-grant facade in link, request initialization,
and projection models. The creation form, island and recovery paths retain an
explicit role for each selected repository. Set all changes form values and never
creates or recovers a retained mutation. Organization/team delivery is available
under explicit composition; the default repository deployment refuses those
targets. Local implementation and runtime tests do not establish live activation.

- **Organization Role**: the grant is Member; observed Admin or team Maintainer membership is preserved rather than downgraded. Organization and team create receipts and settlements remain distinct.
- **Email Domain Restriction**: immutable normalized rules, sign-in collection, requester confirmation, journaled admission and SQL evidence are implemented under the explicit sign-in composition. Live sign-in app permission verification remains open. See the domain evidence section below.
- **Invitation Lifetime**: immutable lifetime between one hour and seven days, with a retained own-expiry timer beginning at a positively observed invitation creation time. Uncertainty never invents that time or authorizes a repeated write. Requester expiry is advisory: it uses separately retained GitHub creation timing and the exact own-lifetime policy, never approval or confirmation time. Pre-existing invitations keep GitHub’s seven-day deadline; exact attached organization teams share it. Missing historical timing remains unavailable, and passing the displayed deadline does not settle an invitation.
- **Pause**: manual pause prevents fresh admission; automatic pause also holds qualifying unfinished deliveries. See Link Pause and Abuse Hold below.
- **Aborted**: a final pre-write stop. A revoked qualifying hold can abort without making an invitation; uncertain writes continue read-only recovery instead.
- **Held**: an unfinished delivery waiting on an exact automatic-pause epoch. A later resume releases that epoch; confirmed receipts remain unchanged.
- **GitHub Invitation**: a positively observed upstream invitation, distinct from the application request and from eventual membership or repository access.
- **Invite Budget**: implemented under explicit M1 composition, including authenticated cap settings and immutable link shares; production activation remains pending.
- **Sign-in App / Org App / Repo App**: explicit app slots, permission profiles and encrypted token contexts keep sign-in, organization membership and repository collaboration separate. Real app registration and live permission checks remain open.
- **Eligibility Warning**: advisory organization facts shown in creation and link administration, with installation-bound freshness and explicit unknowns. A requester sees a two-factor notice only for a fresh known requirement. Warnings do not authorize a write or block link creation or admission. Live minimal-app field visibility remains open.
- **Bulk Invite Batch**: a bounded administrator list with a mandatory retained review, stable row numbers and explicit removed/duplicate rows. Its durable coordinator retains exact operation receipts, the reviewed submission and its downstream handoffs. Requests have a BatchRef, no invitation-link identity and no consumed use. The creator remains distinct from the currently authorized submitting administrator. Ordinary link initialization refuses batch origins; a separately enabled private batch initializer binds the original approval and immutable targets. SQLite and actual D1 verify retained row, installation and grant bindings. Per-target read-only review and native Console entry/review/submit/cancel/recovery forms are implemented. Session-bound encrypted continuations retain original allocations after uncertain replies; an atomic display claim permits one personal-link response across concurrent retries. Bounded progress reads at most64 delivery owners per page and marks partial rows explicitly. Complete browser acceptance, owner callbacks and fixed handler journal coverage remain M2 integration work. Normative definition in P1 spec §2.
- **Reviewed Email Address**: the protected address associated with an opaque email row during the fixed review period, belonging to the account review so another currently authorized administrator may submit it. Removed or expired review rows cannot start delivery; expiration of a review does not change an already submitted delivery.
- **Email Invite Delivery**: an organization invitation addressed through an opaque EmailRef, without a requester, request, Lapsed derivation or access expiry. Its durable owner uses the encrypted address vault, a distinct write fence, real invitation budget and upstream-ID-only reconciliation. Native Restate and actual D1 tests cover receipt/audit replay, uncertain writes and bounded cleanup. Signed Worker email-owner integration verifies real batch activation, uncertain fences, exact refunds and full retained privacy scans. Final browser acceptance and fixed named-journal coverage remain M2 work. P1 sends no email itself; personal links are distributed by the administrator. Normative definition in P1 spec §2 and encrypted-retention amendment in §11.4.


### Daily Delivery Summary

The first successful daily observation of blocked repository, organization, team
and email steps in the SQL projection, grouped by kind and reason for the current Console reader's authorized
scope. Its observation time is explicit. A missing or stale summary is distinct
from an observed zero; neither a zero nor a recent observation proves that the
projection includes every retained workflow fact. Current authorization filters
historical items before aggregation. Recording a summary changes no delivery
state and sends no GitHub request.

### Delivery Step Observation

A monotonic operational observation of an independent repository, organization,
team or email step. Its account, target and source are explicit: request-based
deliveries retain their request and generation; Bulk sources retain their batch
and row, and email deliveries have no fabricated request or invitation link.
Uncertainty onset is retained once; successful observation time only
advances. Neither field changes the immutable delivery receipt or authorizes a
new GitHub write. Missing or future timestamps mean incomplete evidence. Reports
and daily summaries fail rather than silently truncate when their complete-step
bound is exceeded.

### Operational Dispatch Signal

A bounded log record of an actual broker transport attempt, configured-app
signature failure, or rejected edge binding check. It carries UTC time and only
explicit app/installation or fixed bucket/reason dimensions. Coalesced callers
are not extra attempts. These records support externally aggregated rates;
missing collection is unknown, not a zero. They are independent of durable
Delivery Step Observations and never authorize or retry a write.

### Link Pause and Abuse Hold

Manual pause stops fresh admission without changing existing requests or deliveries.
Automatic pause is decided inside the exclusive admission owner when a rolling
hour would exceed its immutable threshold. Its captured admission window holds
qualifying work before an external write. Successful resume releases that hold;
terminal revoke aborts it. Expired/exhausted resume is a typed refusal and never
changes guardrails. Full revisioned snapshots and hold epochs prevent delayed
messages from reviving released holds. See [link lifecycle](docs/link-lifecycle.md)
and [delivery control](docs/delivery-control.md) for receipt and uncertainty rules.


### Invite Budget

The app/account authority that reserves invitation and content-write capacity
before a delivery can claim an external-write fence. Repository PUT reserves a
repository invitation slot; PATCH and cancellation reserve content capacity only.
The delivery retains the complete reservation input before calling the budget.
A retry reuses that input, and an interrupted unused reservation is released only
with durable no-write evidence and a readable absent fence. A changed PATCH-to-PUT
plan must acquire a repository invitation reservation.

A directly observed collaborator PUT204 refunds only invitation capacity. Content
usage and spacing remain charged. A lost response or later observation cannot
supply that refund evidence. Rate-limit closures and positive refund/rejection
evidence are retained before cross-object settlement calls. GitHub rate limits
close content capacity; a repository PUT rejected as spam closes its invitation
bucket, for twenty-four hours when no retry time is supplied.

`AppState::with_invite_budget` explicitly enables S3 and binds the real budget
and asynchronous projection in the standard endpoint. Default M0 composition
remains unchanged while SP-08 gates production activation. An active organization
owner can confirm the organization cap in Settings. The canonical repo account
authority retains the operation, current owner authorization, revisioned policy
and audit event before applying the change to the organization budget. Completed
operations return their original result before attempting unrelated pending work.
A new operation finishes an interrupted authorized change before changing policy;
retrying that interrupted operation does not authorize or apply it twice. Cap
changes preserve charged usage and per-link shares.

The Settings Invite Budget panel reads SQL snapshots, scoped to the account and
the owner's authority. Only a current organization owner sees organization-app
usage. Counts, closures and next-slot times are explicitly recorded observations,
not permission to make a write. Invitation counts cover twenty-four hours;
content-write counts retain their separate minute and hour windows. Missing data
is not zero usage. A content-write closure retains its own upstream deadline,
separate from the next slot imposed by spacing or rolling windows; the panel
displays both. Expiry clears the recorded closure on the next budget observation.
A budget refusal retains its reason with the delivery outcome.
Link-share exhaustion is distinct from a GitHub rate limit: waiting requester
pages explain automatic retry, while link and Settings pages show scoped,
explicitly delayed share-exhaustion notices. A newer delivery projection clears
the notice; replaying an old throttled projection cannot restore it. New journal
paths require ID-21 version discipline.


### Approved Dispatch

The request owner retains one approval manifest before sending delivery work.
The manifest contains a separate immutable command for each repository and, when
selected, one organization command containing the exact team set. Every command
retains its app installation, requester, target grants, approval time, delivery
generation, budget share and independently allocated invitation identity. Recovery
resends these commands without rebuilding them from SQL or current link metadata.

Repository and organization delivery owners have separate service namespaces.
An equal numeric repository and organization ID therefore identifies two distinct
deliveries. Request controls and revocation manifests retain target kind and every
known delivery generation. The current link control is sent before each create
submission. Organization dispatch progress means planned or submitted; it does
not prove that organization or team access was granted.

Organization delivery composition requires the explicit shared setting
`GH_ORG_DELIVERY_ENABLED=true` with the complete `repo-and-org` app stage. Native
and Worker workflow endpoints bind the organization owner and its real budget;
web applies the same capability to fresh organization target selection. Turning
the setting off cannot safely roll back retained organization workflows: endpoint
pinning or draining is required, as for other journal-path changes.


### Lapsed Request and Final Results

Delivery owners notify the request owner with target kind, numeric identity and
generation. The request owner reads the retained delivery snapshot and matches its
complete command to the approved manifest. These messages contain no trusted
result, and delayed SQL does not decide completion. Repository, organization and
team targets each have a final result: Satisfied, LapsedInvite, Declined or
Unfulfillable. Missing or uncertain targets are not final.

An approved request becomes Lapsed only after every target is final, at least one
invitation lapsed, and no target was satisfied. Mixed satisfied/lapsed requests
remain Approved. Own expiry and a disappearing tracked foreign invitation can
produce LapsedInvite; an owned invitation declined in GitHub remains Declined.
Existing AlreadySatisfied team membership retains that final result even when its
recorded membership state was pending. A newly created pending team membership
waits for its independent settlement.

Lapse preserves the original approval decision and admission receipt. Projection
includes the retained approval proof and applies approval plus lapse atomically,
so reordered delivery does not block an exclusive projector waiting for its own
inbox. The separate RequestLapsed audit event retains completion time.

A new request after lapse consumes another use. The link records the last accepted
re-request and permits at most one per seven days. That limit survives decline or
expiry of an intervening request. When invitation lifetime is less than seven days,
these re-requests require approval even on an auto-approve link. Refused and accepted
operation receipts remain immutable after the cooldown ends. Requester pages show
the retained retry time and offer Request again only under current eligibility.


### Email Domain Restriction and Domain Evidence

A link may require one of at most 32 distinct email-domain rules. Exact rules
match only the named domain; `*.example.com` matches descendants and excludes
`example.com` itself. Rules use non-transitional UTS #46 ASCII normalization,
lowercase and removal of the terminal root dot. The core uses the IDNA crate's
strict STD3/hyphen checks with root-dot-aware DNS-length validation; invalid
labels and addresses supplied as domains are refused. Rule order and duplicates
are normalized before creation identity is retained.

Domain Evidence contains the Link ID, only matching verified email domains and
the time they were checked. It contains no email address. The sign-in adapter
must exclude GitHub noreply addresses. Admission independently requires evidence
for this link, checked no more than ten minutes ago and not in the future on the
journaled admission clock; every retained domain must match a rule and be
non-noreply. Missing or stale evidence requires another sign-in; fresh unmatched
evidence is a domain mismatch. These refusals consume no use.

An attempt retains its exact evidence. Starting an attempt does not freeze
freshness; admission checks again when deciding. Once admission is recorded,
replay and request initialization preserve the original evidence and decision
time, even if the evidence subsequently expires. Re-authentication supplies a
new operation rather than changing an existing operation's evidence. Unrestricted
requests retain no domain evidence. Live sign-in permission and re-authentication
behavior remain subject to SP-19/SP-19a; local protocol tests do not establish them.


An anonymous offer read proves possession of the current hash/generation and
reveals only public offer fields: account, targets, approval requirement, email
rules and paused state. It accepts no requester identity and cannot read attempts
or request history. The signed-in requester page remains identity-bound.

### Organization Eligibility Facts and Capacity Hint

Organization eligibility facts are advisory, optional observations bound to an
org installation. Seats, 2FA requirements and base repository permissions may be
unavailable to the minimal app. Missing facts mean unknown; they never establish
permission or capacity. The retained account cache lasts one hour from the start
of its read and refreshes on authenticated installation events. Delivery owners
still use fresh checks before writes.

After validating the requester and link capability, the requester-page owner
reads the same cache and rechecks its account, link-bound organization installation
and freshness against the page's journaled time. Its public page carries only the
optional two-factor requirement, without seats or other administrative facts.
Known true produces an acceptance notice; unknown or false produces no notice.
This is the organization's requirement, not evidence about the requester's own
two-factor status, and never becomes an admission guardrail. Canonical suspension
invalidates the cached notice even before an asynchronous refresh arrives.

A capacity hint is an authenticated organization member-removal event, not an
invitation acceptance or evidence that a seat is free. Its removed user is not a
requester identity. The account inbox captures the current installation's tracked
workset and exact payload-hash receipt; bounded one-way fanout only releases
NoSeat deliveries from their daily sleep. Each awakened delivery repeats its full
protocol. Positive external team management can block a missing membership;
existing sufficient membership stays satisfied, and absence of a management
field does not prove that a team is unmanaged.


### Delivery Transition Audit

A delivery audit episode begins when the owner enters a blocked reason, an
uncertain outcome or a hold epoch. Rechecking the same condition does not create
another episode. A release names the original hold epoch; an abort caused by
revocation is not a release. The owner stores the current snapshot and pending
audit facts together, and preserves the fact's original time through interruption
and delayed SQL projection. A newer snapshot does not erase earlier audit history.

### Access Grant

A generation-bound record of access that P1 positively created and the requester
accepted. It carries the original link or batch origin and acceptance time.
Preexisting, foreign pending and upgraded access never grants P1 removal rights.
An optional immutable access lifetime schedules removal; without a lifetime the
record may still be selected in an explicit remove-on-revoke preview.

### Access Removal

A separate effect that removes P1-created accepted access without changing the
original delivery receipt or settlement. Its current-role check preserves a
later organization owner, team maintainer or higher/custom repository role.
A failed or uncertain DELETE does not prove removal and cannot authorize another
DELETE. Reissue must stop or finish an older grant's pending removal first.
Repository removal may leave inherited access; organization removal has broader
team and repository consequences. These caveats belong in confirmation UI.

### Bulk Item Progress

Bulk item notifications carry identities rather than trusted results. The batch
owner binds the row to its immutable submission, reads the request's current
approved manifest, then reads the notified delivery and validates its complete
command. Final results retain target kind and delivery generation. Reissue sends
a current-manifest wake; delayed older-generation messages cannot restore old
results. Bounded progress pages expose retained results only when their complete
generation set still matches the current request owner.

Email items retain their own typed outcome and settlement. A disappeared email
invitation is not proven acceptance. Notifications are one-way after producer
state is durable. The batch retains an acknowledged handoff cursor for each
item category. Recovery validates the exact child command before resending an
email already handed off; a retired mailbox is never reactivated. Missing or
expired recovery is a blocked email outcome, not a reason to lose the accepted
batch receipt or prevent cancellation. Pending notifications and email recovery-mailbox retirement
are drained on exact replay; retirement uses the original terminal timestamp.
A temporarily unavailable owner schedules a fresh identity-only wake after one
minute, since replaying a journaled failed read cannot make that read fresh.
