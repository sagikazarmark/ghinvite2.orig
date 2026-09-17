# Recoverable browser admission (#57)

`AppState::with_admission(client)` opts an **isolated, migrated** web deployment
into v1 admission, creation, metadata, revocation and decisions together. The
native and Worker entry points do not enable it. Bind `admission_v1::bind` (which
also binds `InvitationCodeV1`), the projector, lifecycle and delivery consumers
on private Restate ingress. Never mix legacy and v1 writers for the same link.
Migration #58 must populate invitation-code routing for imported links before
opening browser traffic; missing authority is never reconstructed from SQL.

## Browser protocol

Fresh forms contain a canonical `operation_id`, distinct from the server-generated
accepted request ID. Missing IDs, overflow, aliases and malformed IDs return 400;
ASCII case normalizes. Justification trims outer whitespace, with missing/empty/
whitespace-only represented as absent. It is bounded at 16 KiB. Authenticated
immutable requester ID is asserted by the web handler; session data and timestamps
are never admission input.

Submission first calls `prepare_attempt`, binding normalized input to that ID in
the link object, then calls `admit`. Preparation is **not acceptance**. It stores
`v1/attempt/<id>` and a per-requester `v1/latest-attempt/<user>` recovery pointer.
There is no automatic expiry and no request-history UI. Two tabs can have distinct
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
contacting ingress, the web caller explicitly saves normalized input in a separate
protected session-backend record keyed by session, user, code and operation. These
30-minute continuations are not login sessions and cannot confer authentication.
Independent keys prevent concurrent tabs or session saves from erasing each
other's input. A separate latest pointer is only a navigation aid; exact attempt
URLs remain independent. This supplies reload/navigation recovery even if
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
receipt). Older responses missing this field default to false. Fresh inactive
visits return the generic invitation-flow 404; existing authorized attempts and
requests remain recoverable. Submission still reaches the replay lookup before
admission eligibility, independently of page-time hints.

## Link routing and account admins

Creation allocates a random invitation code in the journaled creation decision.
Before acknowledgement, the link registers its immutable code-to-ID mapping in
`InvitationCodeV1`. The registry never calls a link, preventing a synchronous
exclusive-object cycle. Resolution needs no SQL and always routes commands to the
canonical link ID. Registry state is retained alongside link authority.

The existing creation form carries a stable link ID and validation-time anchor
in its native action URL, including in island props. This preserves the absolute
expiration across retries. Current GitHub-derived account authorization and
repository selection still run before creation. Metadata/detail/revoke use the
link object with immutable account/user assertions rather than projected link
authorization. Metadata has a separate optional current snapshot: retained
creation input never changes, and older snapshot encodings remain readable.
The shared SQLx/D1 projection updates metadata using monotonic link revisions.

## Verification

```sh
cargo test -p ghinvite-web --test invitation_resolution
cargo test -p ghinvite-web --test console_flow
bash scripts/test-restate.sh authoritative_admission
# From tests/browser:
npx playwright test --config admission.config.mjs
```

HTTP tests use the production router and fake Restate transport to lose a committed
acknowledgement. Playwright drives that router with JavaScript disabled through
two tabs, retry, reload, navigation and explicit editing. Real-Restate facade cases
cover normalization, fresh-code resolution, projection outage, retained attempts,
retry after revoke and requester confidentiality; the existing runtime cases cover
deadlines, terminal replay, races and interrupted writes/sends. Both Worker targets
are typechecked; actual Worker/D1 runtime acceptance remains #59.
