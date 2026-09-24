# Recoverable browser admission (#57)

The web `AppState::new` routes creation/admission/metadata/revocation to
`InvitationLink`, and decisions/progress to `InvitationRequest`. `build_endpoint`
binds these owners and independent projectors and repository delivery services on
private Restate ingress, per [ADR 0009](adr/0009-unified-repository-delivery.md).
Missing authority is never reconstructed from SQL.

## Browser protocol

Fresh forms contain a canonical `operation_id`, distinct from the server-generated
accepted request ID. Missing IDs, overflow, aliases and malformed IDs return 400;
ASCII case normalizes. Justification trims outer whitespace, with missing/empty/
whitespace-only represented as absent. It is bounded at 16 KiB. Authenticated
immutable requester ID is asserted by the web handler; session data and timestamps
are never admission input.

Submission first calls `prepare_attempt`, binding normalized input to that ID in
the link object, then calls `admit`. Preparation is **not acceptance**. It stores
`attempt/<id>` and a per-requester `latest-attempt/<user>` recovery pointer.
Prepared attempts have no automatic expiry. Two tabs can have distinct
IDs. Each can recover by its URL; opening the base invitation link recovers the
latest prepared/decided attempt. Older URLs remain valid after the pointer moves.
Preparation and admission both compare retained input; editing an ID's input
conflicts rather than silently replacing the ID.

The native form action includes `?operation_id=<id>`, so a lost response leaves a
recoverable browser URL. An authenticated GET of that URL uses exclusive
`requester_page` to return the original receipt and separately materialize current
request expiry. Another requester receives generic 404; a cross-user command gets
generic conflict. The operation ID is not a credential. OAuth returns preserve the
attempt URL. No SQL link/request row is needed.

Unknown-result forms retain normalized input and ID, make the original text
read-only, and offer **Retry same attempt**, **Recover original attempt**, and
**Start a fresh attempt with edited input** when an authoritative page read
confirms fresh eligibility. Pending/approved suppression and inactive links hide
that action, including direct `fresh=true` navigation. During an outage only
same-attempt recovery is offered. If preparation never reached ingress, a separate
current-page lookup restores the fresh action once authority is available again.
The fresh form explicitly allocates a
new ID and retains a link to the exact original, including across tabs. Before
contacting ingress, the web caller retains normalized input as a sealed attempt
continuation in SQL, scoped to session, user and canonical link ID and identified by operation.
Continuations expire with the browser session, are not login sessions and cannot
confer authentication. One immutable record per operation prevents concurrent
tabs or session saves from erasing each other's input. The most recently retained
continuation is only a navigation aid; exact attempt URLs remain independent.
This supplies reload/navigation recovery even if
preparation never reaches Restate. Retained link-owned state provides permanent
cross-session recovery once input reaches authority.
No acceptance is claimed for locally saved or prepared input.

| Native response | Meaning |
| --- | --- |
| 303 to attempt URL | Accepted receipt confirmed; authority and downstream arrangement are durable |
| 409 with rejection copy | Definitive recorded business rejection |
| 409 with conflict copy | ID already bound to different input; recover original or explicitly start fresh |
| 400 | Invalid structure/input; no replacement ID generated |
| 404 | Missing or inaccessible authoritative fact |
| 502 with preserved form | Outcome unknown; retry same identity/input |
| 200 on status GET | Original receipt plus separate current status, or prepared/unknown attempt |

No v1 browser mutation uses a queued-send acknowledgement as success. Accepted
admission is not proof of approval or GitHub delivery. Lists/audit remain eventual.
All pages retain session authentication, CSRF, private/no-store and native forms.

The shared confirmation components show GitHub identity, repository scope,
permission, approval policy and optional justification guidance. Wrong-account
sign-out returns to the account-neutral invitation URL; receipt URLs remain
available for their owning requester. Oversized justification is redisplayed with
an associated editable field error without allocating a replacement operation.
Pending current status reloads every 20 seconds through native meta refresh and
provides manual recovery; terminal status stops automatic refresh.

`requester_page.can_start_fresh` is advisory, derived from authoritative link
guardrails and the requester's current blocker (even when recovering an older
receipt). Fresh inactive visits return the generic invitation-flow 404;
existing authorized attempts and requests remain recoverable. Submission still
reaches the replay lookup before admission eligibility, independently of
page-time hints.

## Link routing and account admins

The application allocates a ULID before first creation submission. Its canonical
26-character representation is the Invitation Code, public `/i/<code>` locator,
creation identity and `InvitationLink` object key. Public lookup parses that code
locally and calls the link directly; there is no code registry or SQL dependency.
Accepted textual variants canonicalize before routing and browser-continuation
lookup. Malformed and overflowing IDs are rejected. A valid uninitialized ID is
missing; requester reads and submissions never initialize or reserve it.

Creation retains its normalized input and original receipt independently of
invocation cleanup. Same-input replay returns that receipt even after expiration,
revocation or metadata edits; changed input conflicts. Acknowledgement follows
authoritative state writes and durable projection/audit dispatch, not SQL application.

The existing creation form carries a stable link ID and validation-time anchor
in its native action URL, including in island props. This preserves the absolute
expiration across retries. Current GitHub-derived account authorization and
repository selection still run before creation. Metadata/detail/revoke use the
link object with immutable account/user assertions rather than projected link
authorization. Metadata has a separate optional current snapshot: retained
creation input never changes; the snapshot carries metadata only once it has
been edited.
The shared SQLx/D1 projection updates metadata using monotonic link revisions.

## Verification

```sh
cargo test -p ghinvite-web --test invitation_resolution
cargo test -p ghinvite-web --test console_flow
bash scripts/test-restate.sh authoritative_admission
bash scripts/test-restate.sh canonical_link
# From tests/browser:
npx playwright test --config admission.config.mjs
```

HTTP tests use the production router and fake Restate transport to lose a committed
acknowledgement. Playwright drives that router with JavaScript disabled through
two tabs, retry, reload, navigation and explicit editing. The `canonical_link`
runtime test drives authenticated web creation and requester submission through
real Restate and the production GitHub HTTP clients against a stub, with delayed
projection. Real-Restate facade cases cover normalization, direct lookup,
creation interruption/cleanup, projection outage, retained attempts,
retry after revoke and requester confidentiality; the existing runtime cases cover
deadlines, terminal replay, races and interrupted writes/sends. Both Worker targets
are typechecked; actual local Worker/D1 acceptance is covered by the
[completed #59 gate](worker-admission-gate.md). Remote rollout remains blocked by #61.
