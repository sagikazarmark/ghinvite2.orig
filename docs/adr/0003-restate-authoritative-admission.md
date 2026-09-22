---
status: accepted
date: 2026-09-14
---

# Restate-authoritative admission with asynchronous database projections

For [#48](https://github.com/sagikazarmark/ghinvite2.orig/issues/48), we chose a link-ID-keyed Restate Virtual Object as the authority for invitation request admission and SQLx/D1 as asynchronously updated records and projections. Keeping admission state with its durable execution simplifies concurrency and recovery, and lets admission and revocation proceed while database writes are unavailable. We accept projection lag and the operational responsibility of retaining authoritative business state in Restate.

**The architectural direction and product rules are accepted; the production protocol retains explicit proof obligations and open questions.** A bounded state-and-dispatch sequence passed the native runtime proof below on 2026-09-14; this is not an assertion that production admission or the D1/Worker integration is implemented. The [maintainer-approved refinement](https://github.com/sagikazarmark/ghinvite2.orig/issues/48#issuecomment-5666916141) supersedes earlier database-authoritative and all-effects-in-one-database-transaction proposals in that issue. Keep #48 open until the remaining protocol decisions, verification, and implementation-ticket breakdown are recorded.

## Authority and ownership

| Owner | Responsibility |
|---|---|
| `InvitationLink/<link_id>` Virtual Object | Authoritative guardrails, metadata, revocation, uses, admission outcomes, and request state needed for pending/approved repeat suppression and decision deadlines. Exclusive handlers serialize commands for that link. |
| `InvitationRequest/<request_id>` workflow | Waiting for an admin decision or deadline and orchestrating downstream GitHub invitations. Requests authoritative lifecycle transitions from the link object. |
| Separate Restate projection execution | Retry-safe SQLx/D1 writes, including request records, link records, and audit events. It does not decide admission. |
| SQLx/D1 | Queryable records for Console lists, audit, and other read views. Projected eligibility is advisory, not permission to admit or approve. |

**Update (2026-09-21):** These owners are implemented as the `InvitationLink` Virtual Object, the `InvitationRequest` workflow and the `InvitationProjection` projector. The earlier SQL-driven writers and the cutover path were removed before launch; nothing had been deployed.

This decision covers invitation-link and invitation-request admission/lifecycle state; it does not move all application data, sessions, or GitHub invitation state into the link object.

All eligibility-affecting commands for a link use the same canonical object key, validated against the command's link ID. Account authorization is still required and is not replaced by key validation. Admission does not read projected database state to reconstruct current eligibility during normal operation.

Request workflows receive the authoritative data needed to run, including request/link identity, requester, repository scope, permission, approval policy, admission time, and any decision deadline. They must not require a projected request row to exist before starting. The link object never waits for a request workflow's approval lifecycle or database projection.

## Accepted admission contract

- Check current expiration, revocation, remaining uses, and pending/approved requester eligibility at the authoritative admission decision, after any queueing. An expired link is ineligible at or after its expiration time.
- One accepted admission creates one request and consumes one use. Decline, request expiry, and cancellation do not refund it.
- A pending or approved request blocks a fresh request from the same requester through the same link. A declined, expired, or cancelled request permits a fresh attempt subject to current link eligibility.
- Give each attempt a stable operation identity before its first submission. Same identity and canonical business input return the original outcome; different input conflicts. Record definitive rejections as well as accepted outcomes. Transient infrastructure failure is not a definitive rejection.
- Recover a recorded outcome before checking present eligibility. Replay does not consume another use, change the original deadline, or turn an accepted request into a rejection after link expiry/revocation. A fresh attempt after a rejection needs a fresh identity.
- Canonical input includes the authenticated requester, link, normalized justification, and command format version. Retry-varying transport timestamps and Restate invocation IDs are not business input.
- Set a pending request's absolute decision deadline from its admission time and the pending-request lifetime. Do not cap it at link expiration. Link expiry/revocation does not change existing requests.
- Record stable identities for the admission audit event and the transition that consumes the final use. Replay and projection retries must not duplicate either event.

### Ordering examples

| Scenario | Required result |
|---|---|
| Two different requesters compete for one remaining use | Exactly one request is admitted; the other is rejected as exhausted. |
| Revocation takes effect before admission | Admission is rejected, even if SQL still displays an active link. |
| Admission takes effect before revocation | The request remains admitted and its lifecycle continues. |
| Submission waits until after link expiration | A previously undecided attempt is rejected using admission-time eligibility. |
| A requester with an approved request submits a fresh attempt | It is rejected without consuming another use. |
| An accepted attempt is replayed after acknowledgement loss and link revocation | It returns the original accepted outcome and request identity. |
| A definitively rejected attempt is replayed after eligibility changes | It returns its original rejection; a fresh identity is needed for a new attempt. |
| Link expires tomorrow, but the admitted request's deadline is later | The pending request keeps its independent deadline. |
| SQLx/D1 is unavailable after acceptance | Projection retries; acceptance remains binding and later revocation can still take effect in Restate. |

## Command completion and reads

**A command succeeds when its authoritative Restate transition is durable and its downstream work is durably arranged. Database projection completion is not part of command success.** This applies to admission, revocation, and metadata changes.

Revocation and metadata updates hold link exclusivity only for their authoritative work and durable dispatch. They do not wait for earlier request workflows or projections. Guardrails remain immutable; changing them requires a new link.

Public admission outcomes distinguish:

- **Accepted:** stable request identity, admission time, and deadline if pending. Admission is not a guarantee of GitHub invitation delivery.
- **Rejected:** a recorded business reason, such as expired, revoked, exhausted, or an existing pending/approved request.
- **Operation conflict:** the identity was already bound to different canonical input.
- **Outcome unknown/temporarily unavailable:** transport or infrastructure prevented confirmation; retry the same operation rather than presenting a definitive rejection.

A replay returns the original admission outcome; current lifecycle status is a separate read. An ingress acknowledgement that work was queued is not an accepted admission result. Exact wire types, HTTP mappings, and rejection precedence remain to be specified.

Immediate post-command views use the command result or an authorized authoritative Restate status read. Database-backed lists and audit views may lag. Missing projected rows cannot be interpreted as proof that an acknowledged request does not exist. Preliminary web eligibility checks must not bypass replay or payload-conflict handling.

## Draft state and command design

The logical state belongs together in the link object; this table is not a commitment to one large serialized K/V value.

| State | Required contents |
|---|---|
| Link | Identity/account scope, fixed guardrails and repository scope, metadata, revocation, uses, monotonic projection revision. |
| Operation outcome | Operation identity, canonical input or versioned fingerprint, accepted/rejected result, stable request/event identities, authoritative decision time. |
| Request eligibility/lifecycle | Exact request and requester identities, pending/approved/declined/expired/cancelled state, absolute deadline where applicable, transition identity/revision. |
| Downstream continuation | Sufficient durable information to finish projection and workflow dispatch after interruption without making admission again. |

The intended small command interface consists of link initialization, `admit`, `revoke`, metadata update, short request lifecycle transitions, and authorized status reads. Concrete command names and encoding are implementation choices.

Pending-to-approved remains blocking. Decline/expiry/cancellation releases eligibility only through an authoritative transition. A late transition for an older request must not release the blocker belonging to a newer request. Request workflows wait and orchestrate, but do not independently decide authoritative request state and then asynchronously clear a second blocker map.

### Selected production layout: lazy, split-key state

Keep all authoritative keys in the **same link-ID object**, but separate growing history from the hot link record:

| Key (logical encoding) | Value and access |
|---|---|
| `link` | Link identity/account, fixed guardrails, metadata, revocation, uses, and monotonic revision. No requester list or outcome history. Read for new admission and link changes. |
| `op/<operation_id>` | Immutable canonical input/fingerprint and original accepted/rejected outcome. Direct lookup before eligibility. |
| `request/<request_id>` | Requester identity, authoritative lifecycle state, admission time/deadline, current revision, and transition/dispatch identity needed for safe recovery. Direct lookup by request. |
| `blocker/<requester_id>` | The exact currently pending/approved request ID. At most one pointer per requester/link; absent when retry-eligible. |

Use canonical typed identifiers, not arbitrary delimiter-containing strings. The proof reuses operation identity as request identity for simplicity; the approved production contract below separates them. Its link-local operation key binds requester as input, and the same raw operation ID under another link is a different scoped operation.

### Approved operation identity and retention contract

The maintainer [approved the identity/retention and deadline defaults](https://github.com/sagikazarmark/ghinvite2.orig/issues/48#issuecomment-5667650036) after reviewing the explicit recommendation list. Link-scoped operation identity, separate request identity, and no automatic outcome expiry are accepted; unimplemented normalization, privacy, and retention-expiry verification remain proof obligations.

**Identity is `(invitation_link_id, admission_operation_id)`.** Use a strictly parsed ULID for the operation ID, generated before the first submission and encoded canonically for the object-state key. Parsing different textual representations of the same ULID must resolve to the same identity. Within a link, requester identity belongs to the bound input, not to a separate operation namespace. Thus reusing an operation ID for another requester cannot create a second request under the same scoped identity. Reusing the raw ID under a different link is a distinct operation; there is no global operation-ID registry.

**Accepted request IDs are separate server-generated ULIDs.** Generate the request ID as part of the complete journaled accepted decision, store it in the outcome, and derive request-state/workflow/event identities from that recorded value. A retry must never generate a replacement for a committed request. A rejected operation has no created request ID. This avoids turning independently scoped client operation IDs into colliding global request IDs and keeps operation replay distinct from request lifecycle status. Random ID uniqueness follows the existing ULID convention; projection key conflicts with different immutable content are invariant failures, not successful deduplication.

#### Input binding and validation

For command version 1, compare a typed canonical input containing:

- command kind/version (`admit`, version 1);
- resolved immutable invitation link ID;
- authenticated immutable GitHub requester ID;
- optional justification, with leading/trailing whitespace trimmed and missing, empty, or whitespace-only input represented as absent. Preserve internal whitespace and character content.

Keep this canonical value in the operation record initially, rather than depending on incidental serialized JSON bytes or an unspecified hash. If a fingerprint replaces it, specify the canonical encoding and digest version first. Restate invocation IDs, trace IDs, session/CSRF tokens, user login strings, form render times, submission times, and authoritative admission time are excluded from input equality. Validate and canonicalize at the authoritative command interface as well as at the trusted web caller. Keep version 1 canonicalization compatible for retained outcomes; deploying a new version must not silently reinterpret old receipts as new operations.

Authenticate and authorize access to the command/result before returning any stored outcome. An operation ID is not a credential. For the same link/ID under a different authenticated requester, do not disclose the previous request, outcome, justification, or requester identity; return only the permitted generic conflict/not-found response after the existing access checks. Approval-policy changes are not retry payload changes: guardrails are immutable link state and admission outcomes preserve the policy used at acceptance.

Malformed/missing operation IDs and invalid command structure are validation failures before admission and consume no use. Do not silently replace a malformed ID. A failure before a valid authoritative decision does not create a business outcome record. Once a valid command is decided, both acceptance and eligibility rejection bind its input and identity under the approved no-automatic-expiry policy.

#### Browser and transport behavior

Generate the operation ID when rendering a fresh submission form and preserve it with the same business input on uncertain transport failure. Retrying that attempt reaches the authoritative outcome lookup before current link/repeat eligibility shortcuts. If the user edits the justification after an uncertain result, retain the original attempt for status/recovery and treat the edited form as an explicitly fresh attempt with a fresh ID. Do not automatically change IDs merely to get past a conflict. Separate tabs may have distinct IDs; pending/approved suppression still prevents another admitted request for the same requester/link.

The current route in [`routes/invitation.rs`](../../crates/ghinvite-web/src/routes/invitation.rs) already generates a form ID and preserves it in its failed-POST response, but silently replaces invalid IDs and includes a newly generated submission timestamp on each retry. It also short-circuits on projected link/request eligibility. Production implementation must rename/distinguish the hidden operation ID from the accepted request ID, reject invalid IDs, exclude transport time from equality, and route replay through the authoritative command. Recovery across reload/navigation needs a concrete status/attempt lookup in the web-read ticket; the hidden field alone does not guarantee that continuity.

Use the link object's application-level outcome lookup for business replay. Do not set the bare business operation ID as an ingress deduplication key if doing so bypasses payload comparison. A normal call may have a different transport invocation ID on retry; link exclusivity and the operation record still recover its result. Runtime-generated deduplication for retries of the same transport invocation is compatible with this contract. The new command version must actually reach the application to detect same-ID/different-version conflicts.

#### Retention and cleanup

**Initial policy: no automatic expiry of authoritative admission outcomes**, including rejected outcomes. Link expiration/revocation, request completion, session expiry, database projection success, and completed-workflow/invocation journal cleanup do not remove them. An authenticated replay returns the original admission outcome, not the current request state, and never starts a completed workflow again simply because its runtime retention elapsed.

Keep the minimal replay record: canonical input/version, outcome/reason, admission decision time, and accepted request ID/deadline/policy result where relevant. Delivery bookkeeping and historical request payloads have separate retention needs; projection completion is not permission to discard replay identity. A database copy does not become the admission authority if the Restate record is missing. A restored or unexpectedly missing authoritative state must enter the defined recovery path, not silently bootstrap a fresh attempt from a lagging projection.

This policy deliberately accumulates state. Lazy lookup bounds normal access, not retained record count. A future finite replay window, account/link deletion, or archival scheme requires an explicit amendment specifying how an old identity remains distinguishable from a never-used one. A tombstone can prevent duplicate execution but cannot return the original outcome unless enough data or an authoritative archive remains. Never implement TTL eviction that turns an old operation ID into a fresh operation. Capacity/input bounds and operational monitoring remain rollout requirements, rather than an implicit permission to evict outcomes.

#### Examples and verification still needed

| Attempt | Required result |
|---|---|
| Same link/ID/requester; `" access "` followed by `"access"` | Same canonical input; return the recorded outcome. |
| Same link/ID/requester; absent followed by whitespace-only justification | Same canonical input. |
| Same link/ID; changed justification, requester, or command version | Conflict without new effects; never disclose another requester's stored result. |
| Same raw operation ID on two different links | Two distinct operations; accepted requests receive different server-generated IDs. |
| Accepted operation replayed after decline and a later fresh request | Return the original accepted request identity; do not create another request or modify the current blocker. |
| Definitive rejection replayed after eligibility becomes available | Return the original rejection; only a new ID starts a fresh attempt. |
| Runtime has cleaned up the completed workflow and invocation journal | Retained object outcome still answers replay; it does not dispatch the workflow again. |
| Missing or malformed hidden operation ID | Validation error; no silently generated replacement or use consumption. |

The existing proofs cover same-input replay, changed justification conflict, and rejection replay after blocker release. Add versioned canonicalization/normalization, requester-confidentiality, cross-link ID separation, browser attempt continuity, and replay after actual runtime retention expiry when implementing this contract. No additional production code or retention test is implied by this documentation.

**Enable lazy state explicitly** via Rust SDK `ServiceOptions::enable_lazy_state(true)` when binding the link object. Default eager loading sends the object's entire state to each execution, defeating split-key bounded access. In request-response mode, lazy reads introduce suspension/replay round trips. This is the accepted technical tradeoff for not transferring historical requests/outcomes with every command; measure actual Worker cost before optimizing read scheduling.

A normal new admission performs direct reads of operation, link, and requester blocker, plus the blocking request if its lifecycle/deadline must be inspected. It never enumerates all state keys or scans history. The complete journaled decision contains only these touched records' after-images and bounded projection/workflow messages. An accepted attempt writes link, request, blocker, and operation outcome in deterministic order, then sends downstream work. A rejection records its outcome without rewriting history; advancing a link revision for rejection is optional and is used only for simplicity in the proof. Mutable request projections carry their own revisions.

This is bounded **per-command access and payload**, subject to input/repository-scope limits, not bounded total storage. One operation record per retained attempt and one request record per admitted request still accumulate. Preserve outcomes without automatic deletion until retention/replay semantics are approved. Approved blockers remain while they confer repeat suppression. Capacity measurement, key/value limits, and retention remain production work; do not use an unbounded serialized map as a shortcut.

### Approved deadline arbitration and overdue-request contract

The maintainer approved the seven-day initial lifetime, processing-time deadline arbitration, overdue expiry on fresh admission and authoritative status, and auto-approval within admission with no pending deadline. The approval is linked above. Cancellation permissions and recovery/migration details remain open; approval of behavior does not assert runtime verification.

**Initial lifetime: seven days for newly admitted manual-approval requests**, expressed as an application-wide policy value rather than a per-link editable guardrail. Snapshot `decision_deadline = admitted_at + pending_lifetime` in each accepted pending request. Existing requests keep their recorded deadline when the policy changes. Test fixtures may use short lifetimes. Do not infer this deadline from current link expiration or a workflow's start/restart time. Migration must explicitly assign deadlines to historical pending requests rather than silently recomputing them on read.

**An admin decision is timely only if the link object's complete journaled transition decision evaluates it at `decision_time < decision_deadline`.** At equality or later, a still-pending request expires; approval/decline does not win. Use the same complete-decision clock pattern as admission: sample current time with the transition computation, journal its complete result, then apply the touched state and arrange downstream work. Once a timely approval/decline is durable, replay completes it even if execution resumes after the deadline. Client click time, HTTP receipt time, a caller-supplied `decided_at`, and the order a workflow promise happens to resolve cannot backdate the authoritative decision.

This deliberately accepts processing-time semantics: a command submitted before the deadline but queued until afterward can be too late. Honoring submission/ingress time instead would require an authoritative receipt-time protocol and different ordering rules; it must be a separate product choice, not accidental use of a payload timestamp.

#### One request transition authority

- Approve/decline commands must obtain the link object's authoritative transition result before reporting success. The workflow's durable promise is a coordination mechanism, not proof of approval.
- Prefer a short link command for the decision, followed by a durable one-way notification to the request workflow. A forwarding workflow handler may call the link object, but the link handler must never wait for that workflow's completion or callback. This prevents a circular wait and keeps SQL projection out of the critical path.
- A timer, admin command, or new admission may discover that a pending request is overdue. They all use the same transition logic and stable request-expiration event identity. Exactly one logical pending-to-expired transition releases its blocker; late notifications are harmless.
- A terminal request is never overwritten by a late decision or timer. Return its authoritative current state, distinguishing an already-completed matching decision from an incompatible action. Precise lifecycle operation identity/payload-conflict semantics remain part of the lifecycle command interface; admission-operation receipts alone do not solve decision replay.
- GitHub dispatch is authorized only by the recorded approved request transition. Record auto-approval as approved during admission, with no pending deadline or timer. This accepted representation is not yet implemented in production handlers.

#### Late timers and fresh admission

For a **new** valid admission operation, directly read the request named by the requester's blocker. If it is pending and the complete decision's sampled time is at/after its deadline, include its expiration and conditional blocker release in the same bounded decision as evaluation of the new admission. Do not scan other requesters or wait for a scheduled timer.

Expiration does not refund the old use. Evaluate new admission against the current link state and remaining uses after recognizing that release. A revoked, expired, or exhausted link still rejects the new request, but the discovered overdue request is expired and its event/projection is durably arranged. Thus a decision may expire the old request and reject the new attempt; its complete journaled result must contain both effects. It may touch one old request and one new request, still bounded independently of history.

**Recorded admission replay remains first.** Replaying an existing accepted or rejected operation returns its original outcome without opportunistically creating a new request or re-running its admission eligibility. It is not the overdue-cleanup trigger; a fresh operation, timer, or lifecycle/status command performs that work.

For an immediate authoritative status query, use the same short exclusive expiration check before returning a still-pending request. This keeps the displayed actionable state consistent with the deadline. Such a handler can materialize expiry and durably arrange notifications/projection; it is not a shared read-only handler. Database-backed lists may temporarily show an overdue pending row, but commands always enforce the deadline.

#### Timer scheduling and timestamps

Persist the absolute deadline and carry it in the request workflow's immutable input. On initial startup or after an early wake-up, compute remaining wait from current time, not submission time; if due, immediately ask the link object to expire. After any wake-up, recheck through the authoritative transition. An early timer does not expire the request, and a delayed timer does not authorize late approval. The pinned Rust SDK exposes relative `sleep`; its suspension/replay timing still needs a focused timer test before claiming an exact wake-up time. No exact scheduler-latency guarantee is part of the contract.

Represent expiration's effective time as the stored deadline, while retaining the actual transition-evaluation time for recovery/diagnostics. An expiration discovered tomorrow was effective at yesterday's deadline; do not label tomorrow's processing time as a new decision window. Approved/declined decisions use their authoritative evaluation time. Stable audit IDs ensure late materialization cannot create multiple expiration events.

Cancellation eligibility and actors remain a separate product question. Recommended precedence, if cancellation of pending requests is supported: after the deadline an overdue request becomes expired rather than being relabeled cancelled; before it, an authorized cancellation releases eligibility without a refund. Cancelling a Restate invocation is not a business cancellation and must not bypass this state machine.

#### Deadline examples and required proof

| Ordering | Required result |
|---|---|
| Approval is evaluated just before the deadline and durably decided; execution resumes afterward | Complete approval and its original timestamp; the timer cannot overturn it. |
| Admin clicks before the deadline, but the link evaluates the command at/after it | Expire the pending request and report that approval/decline was too late. |
| Timer and admin command both become runnable after downtime past the deadline | Expiration wins regardless of which workflow future is polled first. |
| Timer fires early | Request remains pending; schedule/retry the wake-up for the remaining duration. |
| Fresh attempt arrives after old pending deadline, timer still delayed, link eligible with uses remaining | Expire old request and admit the new one; total uses increases by one, without refund. |
| Same situation, but link is revoked or exhausted | Expire old request and reject new admission; no new use. |
| Old admission is replayed after its request expired and another request was admitted | Return the original admission receipt; leave the newer blocker untouched. |
| A late timer targets an old request after readmission | Return the old terminal state; never release the newer request's blocker. |

Extend the native proof with a controlled clock for exact-before/equal/after comparisons and runtime interruption around complete transition journaling, old-request expiry, blocker release, new admission, and dispatch. Verify one expiration audit intent and exactly one new use, including expiry-plus-rejection. Add real timer/decision races and delayed workflow startup separately from pure comparisons. The existing proof's explicit `expired` transition does not validate deadline arbitration or these combined transitions. Worker/D1 execution remains deferred.

### Coherent multi-key transitions and reads

Keep authoritative reads that combine link, request, outcome, or blocker records **exclusive** on the link object. They queue behind unfinished mutation/replay and cannot observe half-applied state. This includes immediate request status and replay lookup. Single-record shared reads could be added later with deliberately weaker in-flight semantics, but are not the initial authoritative interface.

Journal the bounded decision before applying its after-images. Apply each state write in deterministic order and recover using the original invocation's journaled reads/decision. A retrying writer continues to hold exclusivity. Consequently, another admission, lifecycle transition, or authoritative status read cannot slip between the writes. This supplies coherent command/read behavior without claiming the separate state writes form one rollback transaction.

For a lifecycle transition, read the exact request and its requester blocker before writing. Pending → approved retains the pointer; pending → declined/expired/cancelled clears it only if it still names that request. Write the terminal request and pointer change within the same exclusive invocation. The transition's replayed original read must reach any unfinished pointer update; a fresh duplicate sees the terminal request and does not release a newer request's blocker. Production must also journal transition time/result and arrange its versioned projection/audit before completing.

The [split-state proof](../../crates/ghinvite-workflows/tests/admission_protocol_proof/split_state.rs) verifies multi-key interruption recovery and these pointer rules. The original aggregate proof below retains evidence for admission clock/dispatch and SQL outage. Exclusive authoritative reads may wait during a stuck link mutation; asynchronous database projection still does not hold that mutation open. Administrative kill/purge remains outside ordinary replay recovery and requires a repair procedure before reopening the object to commands.

### Durable transition and dispatch: verified bounded sequence

The [protocol proof](../../crates/ghinvite-workflows/tests/admission_protocol_proof.rs) exercises this sequence on Restate 1.7.9 with Rust SDK 0.10.0:

1. In an exclusive link handler, `ctx.get` the coherent authoritative state and check the operation outcome. A fresh invocation with an existing identity compares canonical input and returns the recorded result or conflict.
2. For a new operation, immediately await `ctx.run("decide_admission")`. Its closure takes the immutable state snapshot, samples current time, evaluates eligibility, and returns the **complete decision and resulting state**, including outcome, uses, revision, and deadline. There are no external reads or Restate context calls in the closure.
3. The durable decision journal result is the logical admission point. Its eligibility time is the clock sample made while computing that decision; it is not a separately journaled timestamp reused to make a later undecided admission. If the result never reaches the journal, recomputation may use fresh time; once it does, replay must apply that exact decision even after expiration. There is no promise that physical clock sampling and journal persistence happen at an identical wall-clock instant.
4. Apply the result with one `ctx.set("state", snapshot)` in the bounded proof. This materializes the binding decision as a coherent object-state value. Before this state entry is applied, a shared status read can still see the prior snapshot while admission is in flight; after it is applied, status can expose the accepted state even if dispatch is still being recovered. Command success is returned only after the sends below. A production split-key layout needs its own coherent-read and recovery design.
5. Call the projection service using `.send()` and await only its `invocation_id()`, which confirms the durable send identity without waiting for projection completion. For acceptance, do the same for workflow startup keyed by the stable request identity. SDK operations occur in deterministic order.
6. Return the original outcome. Database writes and workflow execution are separate durable continuations.

On an interrupted invocation, `ctx.get` replays its original read, and `ctx.run` replays its decision. Execution therefore reaches the unfinished state/send operations rather than branching early on the newly written outcome. Previously journaled state changes and sends replay without repeating their logical effects. A separately submitted retry is another exclusive invocation and waits behind the original, then reads the completed outcome. The proof uses application-level identity comparison, not ingress idempotency caching that could bypass payload validation.

Exclusive handlers and durable state are not an arbitrary handler-wide rollback guarantee or a transaction across objects and external databases. This recovery argument depends on ordinary durable replay of the original invocation. Administrative kill/purge, state editing, incompatible deployment changes, and independent restores need explicit recovery procedures; a killed partial transition cannot simply be treated as a completed admission because an outcome exists.

The timing proof interrupts both before and after the decision, waits past link expiration, then resumes: the undecided attempt expires, while the journaled acceptance retains its earlier admission time. This was verified in request-response protocol mode on native; the actual Worker clock execution still needs verification.

Restate transport deduplication is not a substitute for canonical-input comparison: a cached response can prevent a changed payload from reaching the handler. Explicit object state must retain the business replay contract independently of completed-invocation/workflow retention.

## Asynchronous projection protocol

Projection work executes separately from the authoritative link object. A regular service or keyed projection object can implement it; the choice must not couple database retries to link command availability.

- Give every mutable record a monotonic revision or equivalent conditional transition. A late snapshot cannot resurrect an active link after revocation or revert a terminal request to pending.
- Deduplicate immutable request/admission records and audit events by stable logical identities. Do not increment a projected use counter once per delivery; duplicate delivery must have no accounting effect.
- State projection and audit delivery have different semantics: skipping a stale snapshot must not discard an as-yet-unrecorded historical audit event.
- Handle request creation, later lifecycle transitions, link records, and referenced account/user rows arriving in different orders. A newer snapshot must contain enough data to create a missing row, or the projector must durably arrange prerequisite writes. Foreign-key failures are not evidence of business rejection.
- Use SQLx transactions or D1 batches where useful to expose coherent projected records. These are local projection mechanisms, not the admission authority.
- Retry ambiguous external writes safely, including a write that committed before acknowledgement was lost and an obsolete execution's late write.
- Make permanently failing projection work observable and repairable/redrivable without changing the authoritative outcome or duplicating effects. Temporary database outage does not trigger admission compensation.

Restate durably retains unfinished execution, but its journal is not automatically an indefinite domain event archive. Rebuilding projections after data loss requires an explicit snapshot/event retention and recovery design. Database projection eventual success assumes dependencies recover and invalid writes are repaired; no fixed lag bound is promised here.

## Concrete questions to resolve before implementation

### Proposed production projection interface

Use a separate ordinary Restate service with one internal `apply_transition(envelope)` command. The link object sends to it durably and does not await completion. The service may process different envelopes concurrently; correctness comes from conditional database writes and stable identities, not assumed delivery order. A keyed projector is unnecessary initially unless measured database contention warrants serialization. This is the concrete implementation recommendation, not an additional verified production adapter.

**Update (2026-09-21):** `InvitationProjection` is now a Virtual Object keyed by link ID rather than an ordinary service. The link object still sends without awaiting completion, but the per-key queue applies one link's transitions in send order, and a failing transition holds back only that link's later transitions. Revision checks remain as a guard against manual redrives.

Each versioned envelope contains:

- schema version and a stable transition ID generated/recorded with the authoritative decision;
- account/link identity and a full current link snapshot with its monotonic revision;
- a bounded list of touched request snapshots, each with immutable creation facts, current state, absolute deadline, and its own revision;
- immutable audit events with stable event IDs, actor/target, safe event-specific metadata, and effective/evaluation timestamps as defined above;
- the immutable parent/relationship facts needed to make these writes valid, or explicit prerequisite identities for durable dependency resolution.

Admission normally touches one request; overdue expiry plus readmission touches at most the old and new requests. A rejection without lifecycle effects need not create a projected request. Never include all operation/request history in an envelope. Operation receipts stay authoritative in the link object; projecting copies is optional and must not gate replay.

For each mutable row, insert if absent, replace only when the incoming revision is greater, and treat equal revision with equal content as replay. Equal revision with different authoritative content or an immutable identity/account mismatch is an invariant failure to repair, not an ignorable duplicate. Lower revision snapshots cannot undo newer state. Request state and link state use separate revision comparisons: receiving a newer link snapshot must not suppress a still-needed request snapshot from an older envelope.

**Update (2026-09-21):** With the projector serialized per link and no other writer of these rows, links and requests no longer retain snapshot content or identity columns. An equal-revision snapshot is a no-op rather than a content comparison; immutable identity conflicts are still checked against the stored columns, and audit events still compare content.

Always attempt all immutable audit insertions independently of snapshot freshness. The same event ID with identical content is replay; different content conflicts. Derive exhaustion identity from the logical link exhaustion transition and expiration identity from the exact request expiration transition, not whichever invocation happens to publish them. This prevents old snapshots arriving late from losing historical events.

Apply one envelope's related writes in a SQLx transaction or a fixed D1 batch where practical. Encode conditional writes so a stale/equal snapshot does not block missing event insertion. Validate conflicts inside the atomic application rather than relying on a post-commit row count to roll back. Actual D1 verification is deferred, so the concrete portable statements remain an implementation task.

Parent records require special care: the projector cannot assume an installation/user/link row from another execution has already arrived. Envelopes can create immutable relationship stubs using only verified identity facts; they must not overwrite newer user profiles, installation status, or account authority with admission-time snapshots. Alternatively, resolve prerequisite projection through a separate durable call in the projector. Choose the exact schema/stub strategy during migration design. Waiting for prerequisites is acceptable in projection, never in the authoritative link command, and must not introduce a dependency cycle.

Keep retryable storage failure retryable, including ambiguous acknowledgement loss. Invariant/encoding failures retain inspectable failed work for repair and redrive; do not acknowledge projection success and drop the envelope. Operational inspection should correlate transition ID, target identity, and retry status without logging private request payloads. Runtime journal retention is not a replacement for a projection rebuild plan after independent database loss.

### Selected workflow notification protocol: direct promises

The native notification experiment below resolves the earlier mailbox question: **use direct workflow promises initially; do not introduce a separate mailbox object.** Restate 1.7.9 accepts a shared-handler promise resolution before workflow startup, and the later run consumes that fact. A waiting workflow also receives it, and interrupted resolution recovers. Retention cleanup removes the promise, but a late shared-handler notification is still callable and does not start the run handler. The authoritative terminal record already lives in the link object and supplies long-lived recovery truth.

The production protocol is:

1. The link durably sends stable request-workflow startup during admission. A terminal transition durably sends a notification to that workflow's shared handler, without awaiting workflow completion. These sends may execute in either order.
2. The notification is a wake-up carrying request/link identity and immutable decision identity/revision. The shared handler resolves one named terminal promise. Repeated identical facts are accepted; a different fact is not permission to change the request's state.
3. At startup, the workflow reads authoritative request status from the link. If terminal, it proceeds directly; if pending, it waits on the promise or deadline. A terminal transition between that read and the wait is retained by the promise.
4. After notification or timer wake-up, obtain/validate the authoritative terminal fact from the link before dispatch. A timer can ask the link to materialize expiry; a notification never decides approval. Only approved authoritative state permits GitHub dispatch.
5. Retain admission outcomes and lifecycle/dispatch checkpoints in link-owned state. Replaying admission never restarts its workflow. Explicit post-retention workflow redrive must consult those checkpoints before external effects; automatic run re-creation is not a supported replay mechanism.

The proof uses `peek_promise` to compare identical/conflicting terminal facts. This is **not an atomic compare-and-set across concurrent shared handlers**. Production correctness depends on the link emitting only one immutable terminal decision, private/trusted notification ingress, and the workflow verifying authority. Do not claim the proof's sequential conflicting-payload test establishes concurrent arbitrary-writer conflict handling.

A late notification after workflow cleanup can recreate orphaned promise state. It does not resurrect the run handler in the tested runtime, but cleanup of that orphan state needs a retention/operations policy. A link-owned consumed/dispatch checkpoint can suppress deliberate redelivery; its exact atomic update and recovery protocol remains part of lifecycle implementation. Errors or absent promises after cleanup are not evidence that the authoritative decision disappeared.

The following mailbox design is retained only as a considered alternative, superseded as the initial recommendation by the direct-promise evidence. Revisit it if a concrete delivery or cleanup requirement cannot be met with link-owned checkpoints and direct signals.

#### Considered alternative: request-keyed mailbox

The link object owns terminal request decisions; the workflow consumes them to stop waiting or dispatch GitHub invitations. A notification contains the exact request/link IDs, terminal state, request revision, decision identity/time, and required immutable workflow context. It is a durable fact, not another instruction to decide approval. Only the authoritative link path may emit it.

Startup and a terminal notification can race: auto-approval occurs during admission, a timer may run late, or an admin may decide before workflow startup completes. A direct call to an unstarted or retention-expired workflow promise must not be assumed reliable without proof.

The alternative transport is a small request-ID-keyed result mailbox Virtual Object, holding only the immutable terminal result and at most the current workflow waiter registration. This is delivery state, not a second admission/lifecycle authority. It has no use counter, requester blocker, or permission to decide transitions.

1. The link durably sends workflow startup and, when terminal, `publish(result)` to the mailbox. These sends may execute in either order. The link waits only for durable dispatch identities.
2. The workflow creates a durable awakeable and calls `subscribe(waiter_id)` on its mailbox before waiting. The short exclusive mailbox handler either records the waiter or returns the already-published result immediately. Never hold mailbox exclusivity while waiting for a result or for the workflow to finish.
3. `publish` records the terminal result and resolves any registered waiter using durable context operations. Same result/decision identity is replay; a different terminal result for the same request is an invariant conflict. A resumed publish must complete an unfinished waiter resolution even if result state was already written.
4. The workflow races its notification wait against its deadline wake-up. If the timer wins, it asks the link to evaluate expiration and consumes that authoritative result, which may already be approved. If a notification wins, it consumes the terminal fact. Neither path invents a terminal state based on polling order.
5. Only an approved fact authorizes downstream dispatch. Persist stable dispatch identities and completion evidence so ordinary replay cannot repeat logical dispatch. Admission replay returns its receipt and does not launch another workflow.

Retain the terminal mailbox result independently of completed-workflow retention initially. This lets a late duplicate notification be answered without resurrecting a completed workflow. An already-completed workflow is not automatically restarted by mailbox publication. Registration cleanup, stale/unavailable awakeable resolution, mailbox state retention, and administrative recovery must be specified and tested. In particular, late resolution failure must not replace or discard the terminal fact; use the durable result read on recovery. A workflow recreated after retention requires an explicit redrive protocol and dispatch checkpoint check, not automatic re-execution of GitHub work.

This adds one small durable delivery module per request. Direct promise evidence now removes the assumed need for an extra startup mailbox. The mailbox itself has not been implemented or verified; it would introduce waiter-resolution and cleanup obligations in addition to authoritative link records. The selected direct approach still requires production link-status/dispatch-checkpoint integration, beyond the standalone promise proof.

### Cancellation scope: deferred new actions

[`RequestState::Cancelled`](../../crates/ghinvite-core/src/invitation_request.rs) is currently reserved and has no v1 request-cancellation command. Its comment and the historical v1 spec describe future cascading link revocation; that assumption is superseded by the approved rule that link revocation does not change existing requests. GitHub invitation cancellation is a separate existing lifecycle and does not cancel an approved invitation request or refund its use.

**New request-cancellation UI/permissions are deferred from this admission work.** The maintainer instructed publication of the implementation breakdown with this recommended scope on 2026-09-14. Preserve cancelled records and their retry eligibility during migration, reads, and state-machine tests, but do not invent a caller able to produce cancellation. Approval/decline remain account-admin decisions; decline already closes pending work from that role. The published lifecycle and migration tickets carry this scope explicitly.

If request withdrawal is wanted now, decide explicitly whether the requester, an account admin, or both may cancel a pending request. Then define audit actor/reason visibility and command replay. The recommended state rule is pending-only cancellation before its deadline, expiry at/after the deadline, no use refund, and no cascade to an approved request/GitHub invitation. Administrative Restate cancellation/kill remains operational recovery and is not a domain cancellation command.

## Approved dispatch recovery and migration contract

**Update (2026-09-21):** The legacy writers and cutover path were removed before launch, so the controlled writer cutover, historical data treatment, and pre-cutover rollback steps below no longer apply; nothing had been deployed. The retained dispatch plan, receiving-side receipt, projection prerequisite and post-authority recovery rules still apply.

The maintainer [confirmed this contract](https://github.com/sagikazarmark/ghinvite2.orig/issues/48#issuecomment-5668073287), including stable dispatch plans and create outcomes, controlled write maintenance, preservation of established historical deadlines, and existing identity-parent prerequisites. This is not a completed migration or verified external-effect protocol; concrete recovery mechanics and the migration rehearsal remain implementation work.

### Retained dispatch plans and receiving-side receipts

Distinguish **approved**, **durably submitted**, and **GitHub create result confirmed**. None implies the next. In particular, a `Sent` GitHub invitation awaits its recipient, but its create operation is complete.

Before first fan-out, a short exclusive link command creates or returns a retained dispatch plan for the approved request: approval identity, immutable repository scope/permission/requester identity, and one stable GitHub invitation ID per repository. Generate IDs once in the complete journaled plan decision and retain them beyond workflow cleanup. A different plan for the same approval conflicts. Preserve existing invitation IDs on import. Store per-repository entries separately if needed; bound repository scope at link creation rather than growing the hot link header with dispatch history.

The workflow obtains that same plan on replay/redrive and durably sends each create command to its stable invitation object ID. Record `submitted` only after the durable send identity is confirmed. If acknowledgement of submission is lost, resending the same command must be safe at the receiving object. A submitted checkpoint is not a successful GitHub response, and a missing workflow journal is not permission to allocate new invitation IDs.

Suggested retained per-repository checkpoints: `planned`, `submitted` (with original invocation identity when available), and `create_resolved` (with outcome identity). The workflow can finish when fan-out is durably arranged and recorded; delivery continues independently. Explicit repair checks receiving-side outcomes before resending retained commands. Admission replay only returns its receipt and never initiates redrive.

The existing [`GithubInvitation::create_logic`](../../crates/ghinvite-workflows/src/github_invitation.rs) skips a repeated create only when the stored invitation state is terminal. `Sent` is not terminal in [`InvitationState`](../../crates/ghinvite-core/src/github_invitation.rs), so a replay can repeat the collaborator PUT despite a prior successful create. Fix this before considering post-retention redrive safe.

**Retain a create-operation receipt separately from invitation lifecycle**, keyed by stable invitation ID and bound to immutable input. Results include created pending invitation with upstream ID, already collaborator, and definitive failure. Once recorded, replay returns that result without another PUT regardless of later sent/accepted/declined/expired/cancelled state. A future deliberate resend is a new domain operation, not recovery of the original create. Link dispatch checkpoints do not replace the receiving receipt.

GitHub success before receipt persistence is still ambiguous: Restate cannot atomically commit GitHub and its journal. Retain attempt intent/identity; reconcile collaborator access and pending invitations for the immutable requester before retrying an uncertain create. Absence in a list is not by itself proof that an earlier call never succeeded or that its result was not subsequently acted on. Specify handling of unresolved ambiguity rather than claiming exactly-once HTTP effects. Test acknowledgement loss, recipient action, and API errors. Requester login is mutable addressing data; define lookup/validation against immutable GitHub identity instead of treating a stale login as identity.

Initially the GitHub adapter may retain database-backed create receipts, provided missing projected request/link prerequisites are retryable dependencies rather than terminal not-found. Database outage may delay delivery, but never moves external calls or projection waits into the authoritative link handler. Choosing Restate state for GitHub create receipts is a separate explicit implementation decision. Neither receiving-side receipt implementation nor GitHub ambiguity reconciliation has been proven by the admission experiments.

### Controlled writer cutover

Use write maintenance rather than simultaneous old/new authoritative writers:

1. **Inventory.** Collect link/request counts and uses, pending/approved conflicts, pending deadlines, per-repository invitation IDs/results, queued/running Restate invocations, and pinned deployments. Record an immutable versioned migration manifest with source identities/checksums and per-link progress. Endpoint replacement cannot arbitrarily change old journal semantics.
2. **Quiesce.** Stop new old-path admission/link/admin-decision commands and automatic request transitions. Let short commands finish. Pending workflows can wait days and cannot simply drain during a short window: inventory and pause/suspend them under a validated procedure, accounting for queued timers/decisions. A pause does not fence already-issued SQL/GitHub requests. Establish that all old writers are quiescent before import; late commands must route compatibly or fail as obsolete rather than resume independent database-authoritative mutation.
3. **Checkpoint.** Capture coordinated Restate/database backups or exports and outstanding invocation metadata after quiescence. Record uncertain external effects. A database backup alone cannot recover new authoritative Restate outcomes.
4. **Import once.** Initialize each link with existing IDs, guardrails, metadata, revocation, uses, request states/blockers, and known dispatch IDs/results. Import requires the expected migration identity, rejects conflicting initialized state, and resumes partial work from the manifest. Mark a link ready only after all its state keys are imported; do not overwrite live state from a stale SQL snapshot.
5. **Transfer lifecycle work.** Preserve request identity and terminal results. Start versioned replacement lifecycles only for pending work or unfinished approved dispatch after old execution is fenced. Retain dispatch plans and receiving outcomes; never blindly fan out again. Retire old invocations only after their obligations and uncertain effects are accounted for. Operational kill is not domain cancellation. Validate exact management commands/routing in a disposable rehearsal before deployment.
6. **Verify and open.** Check counts, eligibility, deadlines, revisions, dispatch identity and representative replay. Route all relevant commands to canonical link keys. Deploy authoritative immediate reads with cutover; database readers must tolerate projection lag.

### Historical data treatment

- **Uses mismatch:** all created requests count, including declined/expired/cancelled. Report and reconcile discrepancies from evidence; do not reduce counters, fabricate requests, or expand max use automatically. An already-over-limit link cannot admit more.
- **Multiple blocking requests:** preserve records and flag ambiguous eligibility. Do not pick a pending row while ignoring approved history or delete rows to satisfy a new index. Keep unresolved links closed to new admission pending reconciliation; define any exceptional historical blocker representation in the migration ticket.
- **Historical deadlines:** preserve each already-established pending deadline, including the old link-expiry cap, while applying the new independent lifetime to new admissions. Recover journaled deadlines if absent from SQL. Missing/unreliable deadlines need an explicit mapping decision; never give every migrated pending request a fresh seven days. Known terminal states are preserved.
- **Legacy operation identity:** old request IDs can identify accepted attempts, but exact original input and rejected outcomes may not have been retained. Define a versioned legacy replay compatibility path based on available evidence. Do not fabricate payload equality or promise replay of previously unrecorded rejections. Strict new operation semantics begin at a versioned cutover.
- **GitHub delivery:** preserve existing per-repository IDs. `Sent` with an upstream ID is create-success evidence. `Sending`, incomplete fan-out, or missing records require Restate/GitHub reconciliation before redrive, not fresh IDs and unconditional PUTs.

### Projection prerequisites

Retain the current installation/user parent model initially rather than inserting incomplete stubs into tables whose readers require full profile/account fields. Normal identity rows come from their existing owners. A missing parent becomes retryable projection dependency/repair, not fabricated authority or compensation of admission.

The envelope carries a complete authoritative link snapshot and its touched request snapshots. Apply link/repository records before requests within the transaction/batch even if an earlier link-created envelope is delayed; honor independent revisions and never overwrite newer parent/account metadata from admission-time data. On database restore, recover installation/user prerequisites from their owners or a compatible backup before admission projection replay. Dependency recovery must not wait cyclically on the same projector.

If an identity cannot be recovered, leave projection observably pending for repair. Admission/revocation continue from Restate. Identity retention/deletion must respect these references. Portable statements, equal-version conflict checks, and actual D1 validation remain implementation work.

### Rollback and destructive recovery

Before new-authority writes, a failed cutover can restore its coordinated checkpoint and resume old writers through the verified procedure. **After any new Restate-authoritative admission/transition, rolling back to database-authoritative writers is unsafe**: projection may omit uses/outcomes. Prefer a compatible forward fix or maintenance with explicit reverse reconciliation. Missing SQL rows after restore never establish fresh admission eligibility.

For killed/purged invocations or independent Restate restore, close affected commands until partial keys, sends, receipts, and external effects are reconciled. Ordinary replay proofs assume the original journal survives. Restore projections from retained authoritative snapshots and event history where available; current snapshots cannot reconstruct every historical audit event. A recoverable event source/database backup is required independently of completed-workflow journal retention.

Required rehearsal: legacy pending/approved/terminal records, `Sent` and ambiguous `Sending` delivery, queued old revoke/decision, interrupted import, identical/conflicting manifest replay, delayed parent projection, and attempted rollback after a new admission. Assert no restored eligibility, lost history, or repeated confirmed create and explicit handling of uncertainty. No migration, runtime pause/purge, or external reconciliation was performed in this session.

## Remaining implementation decisions

**Sequencing update (2026-09-14):** The maintainer [deferred actual Worker/D1 verification](https://github.com/sagikazarmark/ghinvite2.orig/issues/48#issuecomment-5667489260) for now. Continue contract and lifecycle design using the recorded native evidence. Worker clock/endpoint and D1 adapter verification remain explicit follow-ups; native request-response/SQLx success does not satisfy them. This deferral does not establish production readiness or by itself close #48.

| Question | Proposed next step / approval needed |
|---|---|
| What are the production capacity limits and retention policy? | Lazy split-key state and exclusive coherent reads passed the focused proof. Verify actual runtime/Cloud key/value limits, input/scope bounds, storage growth, and Worker round-trip cost before rollout. |
| Does decision-time sampling work on the actual Worker target? | The clock is sampled inside the complete decision closure; before/after-decision expiration recovery passed natively in request-response mode. Verify Wasm clock behavior and the actual Worker endpoint. |
| What identity/retention verification remains? | The contract is approved. Implement normalization/privacy/cross-link/retention-expiry verification; the current proof deliberately reuses operation/request IDs. |
| What deadline implementation verification remains? | Lifetime/arbitration and overdue materialization are approved; narrow native checks passed below. Verify production workflow notifications, configuration/migration, revoked/expired-link readmission, and lifecycle operation replay. |
| Future cancellation scope? | New cancellation command/UI is deferred; preserve existing cancelled-state semantics. A future withdrawal feature must choose actors and audit visibility explicitly. |
| Finalize projection envelope and prerequisites? | Recommend an ordinary service with per-record versioned snapshots plus immutable audit events. Settle parent-row schema/dependency strategy and verify portable conditional writes. |
| What notification integration remains? | Direct promises were selected after native early/late/interrupted delivery proof. Implement authoritative status verification, trusted ingress, dispatch/consumption checkpoints, and orphan-promise cleanup/redrive policy. |
| How do authoritative status reads work during projection lag? | Specify authorization, lookup by link/request identity, and post-command SSR navigation, including freshly created links absent from SQL. |
| What can be rebuilt after retention, administrative kill/purge, or independent restore? | Define Restate backup/state retention and projection rebuild/redrive procedures. Workflow restart after completed-workflow retention must not repeat downstream GitHub effects. |

## Migration and implementation seams

**Update (2026-09-21):** The legacy writers and cutover path were removed before launch. The files and existing-data cutover described below no longer exist or apply; #58's cutover tooling was deleted.

Current code differs from this decision:

- [`commands.rs`](../../crates/ghinvite-web/src/commands.rs) sends link commands with account keys and request commands with request keys. [`invitation_link.rs`](../../crates/ghinvite-workflows/src/invitation_link.rs) validates account-keying for metadata and creates link IDs inside its handler.
- [`invitation_request.rs`](../../crates/ghinvite-workflows/src/invitation_request.rs) performs database-backed admission inside the request workflow, uses submission-derived time, caps deadlines at link expiry, and writes lifecycle state/audit directly.
- [`storage/mod.rs`](../../crates/ghinvite-core/src/storage/mod.rs) and its SQLx/D1 adapters expose request insertion/use increment separately from preceding eligibility checks. The [initial schema](../../migrations/0001_initial.sql) enforces pending-only uniqueness.
- Web link resolution and submission currently depend on projected request/link records. They must support authoritative replay and post-command reads before asynchronous projection is enabled.

Initialize existing link state once from a reconciled database snapshot during a controlled writer cutover. Include consumed uses, revocation, pending/approved eligibility, existing request identities, and deadline treatment. Inspect historical duplicates before changing uniqueness constraints; do not delete historical requests or refund uses to make migration pass. Old account-keyed commands and old request workflows must drain or be redirected so they cannot remain independent writers after cutover. Runtime admission must not repeatedly bootstrap from stale projections.

Plan schema additions for projection revisions, durable event identities, and request deadlines. The authoritative operation ledger belongs in Restate; a database copy is optional projected history. Exact migrations depend on the chosen state/projection protocol.

After resolving the questions above, split narrow Batch 3 tickets around these testable interfaces:

1. Link-owned state/commands and one admission/revocation/replay slice, with runtime crash-recovery verification.
2. Asynchronous projection through both storage adapters, with duplicate/reordering/acknowledgement-loss verification and schema changes.
3. Request lifecycle commands and independent deadlines, with repeat suppression and durable workflow-start verification.
4. Web command outcomes and authorized immediate status reads under projection lag.
5. Existing-data/in-flight-writer cutover and projection repair/rebuild procedures.

### Published implementation tickets

The maintainer approved publication of the seven-part breakdown on 2026-09-14. Each ticket delivers a verifiable command-to-result slice; early slices use an isolated versioned path until writer cutover. GitHub native blocking links mirror this table.

| Ticket | Delivery | Blocked by |
|---|---|---|
| [#53](https://github.com/sagikazarmark/ghinvite2.orig/issues/53) | Authoritative link creation/admission/replay/revocation and status through real Restate | None; #47 runtime gate is complete |
| [#54](https://github.com/sagikazarmark/ghinvite2.orig/issues/54) | Command-to-queryable projection/audit with outage and parent-dependency recovery | #53 |
| [#55](https://github.com/sagikazarmark/ghinvite2.orig/issues/55) | Pending decisions, overdue expiry/readmission, direct notifications, and projected status/audit | #53, #54 |
| [#56](https://github.com/sagikazarmark/ghinvite2.orig/issues/56) | Approved request to per-repository delivery with retained plans/create receipts | #55, #49 |
| [#57](https://github.com/sagikazarmark/ghinvite2.orig/issues/57) | Recoverable native browser submission and authoritative status under projection lag | #55 |
| [#58](https://github.com/sagikazarmark/ghinvite2.orig/issues/58) | Resumable writer cutover and native migration/recovery rehearsal | #56, #57 |
| [#59](https://github.com/sagikazarmark/ghinvite2.orig/issues/59) | Actual Worker/D1 integration and rollout verification | #58; explicitly deferred pending maintainer resumption |

#49 remains the source for installation loss, recipient identity, and ambiguous outbound-result policy. #56 depends on that decision rather than silently establishing those policies here. #53–#58 carry `ready-for-agent` with their blocking edges; only the unblocked frontier is actionable. #59 is `ready-for-human` for the explicit scheduling deferral. Ticket publication does not claim implementation or deployment verification. The ticket-writing workflow leaves parent #48 unchanged; its final acceptance/closure is a separate tracker step.

## Required focused proof

Use the project's real Restate runtime gate and pinned SDK, including the Worker request-response path; pure helper tests cannot prove durable dispatch or object serialization. Exercise SQLx and the actual D1 adapter.

1. Initialize a link with one remaining use and keep database projection unavailable.
2. Race two requesters. Exactly one admission succeeds; request count/uses and replay outcomes are coherent in authoritative state.
3. Interrupt execution around decision persistence, state updates, projection send, workflow send, and response delivery. Replay the same operation, including with a different payload, without skipping unfinished continuation or consuming another use.
4. Revoke while the projector is retrying. Revocation completes; a fresh admission is rejected; replay of the original acceptance remains accepted. Also test revocation ordered before the first admission.
5. Restore projection and inject duplicate, reordered, and commit-before-acknowledgement-loss deliveries. Verify convergence to one request, one use, each logical audit event once, no state regression, and durable request-workflow startup.
6. Verify approved-repeat suppression; fresh attempts after decline/expiry/cancellation; a late transition for an old request; queued admission past link expiry; and a pending deadline after link expiry. Include replay of definitive rejections after eligibility changes.

### Proof results, 2026-09-14

Command: `bash scripts/test-restate.sh admission_protocol_proof` — **passed**, one bounded scenario, 8.32 seconds after compilation/runtime startup. The existing `bash scripts/test-restate.sh` gate also passed, as did targeted Clippy with `-D warnings` and shell syntax validation.

Environment: the Compose-pinned Restate **1.7.9** image/digest, Rust SDK **0.10.0** from `Cargo.lock`, local Docker, and SQLx SQLite. The environment provided Rust **1.94.0** rather than the repository/CI toolchain **1.92.0**. The proof endpoint buffers requests and advertises **request-response** mode like the Worker, but runs natively.

Evidence:

- A retryable injected interruption first lets the earlier journal prefix reach Restate. On its next attempt, the proof aborts the SDK task and drops its response future at each of four checkpoints: after decision, after state write, after projection send, and after workflow send. Restate retries via a new endpoint invocation and completes the original operation.
- A real SQLite trigger rejects projection writes throughout admission/revocation/startup. SQL remains empty while all four accepted operations and revocations complete. The separate projector retries independently.
- Same-input acceptance and rejection replay remain stable; changed canonical input returns HTTP 409. Two independent requests racing for the last use yield accepted/exhausted.
- Interruption before the decision followed by expiration rejects admission; interruption after a durable decision followed by expiration preserves acceptance and the original timestamp/deadline.
- After removing the failing trigger, a projection commits and deliberately loses its acknowledgement. Retry converges. Explicit duplicate delivery of older snapshots after newer revoked revisions cannot restore active state or duplicate audit rows.
- Final assertions: **five requests, five uses, fourteen logical audit events, and five observed workflow starts**, including the concurrent final-use winner. Each faulted link remains revoked at its latest projected revision.

Scope: this is a disposable protocol experiment with test-only handlers, a small aggregate, and a scratch SQL schema. It does not invoke production admission or the production storage trait. It aborts endpoint execution tasks, not the Restate server or the host process. It verifies startup of a minimal workflow consumer, not approval/cancellation or exactly-once external GitHub effects. Actual D1 adapter/Worker execution, lifecycle repeat suppression, schema/foreign-key migration, retention, administrative kill/restore, and large state layout remain unverified. Wrangler is not installed in this environment; D1 conformance was not run. These gaps keep #48 open.

### Split-state follow-up proof, 2026-09-14

The same command, extended with `admission_protocol_proof/split_state.rs`, **passed in 12.70 seconds** including the original aggregate/outage scenarios. It registers a separate test-only object with lazy state enabled and exclusive status reads, using the same runtime, SDK, native request-response endpoint, and scratch SQL schema.

- Aborted/replayed executions after each of link-header, request-record, requester-blocker, and operation-outcome writes recover a complete admission. An exclusive status invocation submitted during each interruption remains blocked until recovery and then observes matching outcome/request/blocker and exactly one consumed use.
- A separate interruption after writing a terminal request but before clearing its blocker recovers the pointer update. Explicit declined/expired/cancelled transitions release blocking without refunding a use. A new operation is admitted afterward; the old rejected operation still replays its rejection.
- Approved requests continue suppressing admission, and stale terminal updates to the old request do not clear a newer request's blocker. These are state-transition tests, not proof of production timer arbitration, authorization, cancellation commands, or lifecycle audit/projection.
- Each of four split-key links finishes with two admitted request records and two consumed uses. The projector receives only the current link snapshot and touched request outcome, not accumulated history.
- Each link contains an unrelated roughly 256 KiB history value. The endpoint observes raw Restate request bodies: the distinctive history payload never arrives, and the largest split-object invocation body is **2,176 bytes**. This verifies that lazy loading is actually effective in the pinned native request-response path; it is not a large-scale capacity benchmark or Wasm performance result.

The selected layout resolves the growing-snapshot/coherent-read question for ordinary replay. Actual Worker/D1 execution, production-sized records, retention/migration, lifecycle projection, and kill/restore recovery remain open. All earlier proof limits still apply except the now-tested narrow split-state access and blocker transitions.

### Deadline follow-up proof, 2026-09-14

`bash scripts/test-restate.sh admission_protocol_proof` **passed in 18.84 seconds**, including the earlier aggregate and split-key scenarios and the new [deadline extension](../../crates/ghinvite-workflows/tests/admission_protocol_proof/deadlines.rs). It uses the same pinned native request-response runtime and scratch SQL projector.

- A test-controlled clock verifies approval and decline strictly before, exactly at, and after the deadline. At/after expiry, effective expiration time remains the stored deadline rather than delayed processing time.
- Task abort/replay before transition decision uses the later time and expires; interruption after a timely journaled approval preserves approval on replay past the deadline.
- Combined old-request expiration plus fresh admission recovers interruptions after old-request expiry, link/blocker/use state, new-request state, and durable projection send. A separate exhausted-link case expires the old request but rejects the fresh attempt without consuming a new use. Each checks one projected expiration event, unchanged old admission replay, and stale timer protection for the newer blocker.
- Early expiration checks stay pending; authoritative status materializes due expiry. Auto-admission records approved state with no deadline.
- Real durable sleeps race a post-deadline approval command; both observe expired. A workflow started after its deadline also expires immediately through the link command rather than granting a fresh lifetime.

This is test-only state-machine evidence, not production handlers. The extension uses a fixed requester and a compact link/blocker record plus split request/operation keys; the separate split-state suite proves separate blocker-key recovery. Its operation/request IDs are deliberately conflated and its projection carries link counters and event identities, not a full lifecycle read model. It does not verify revoked/expired-link combined rejection, exact wake-up latency, workflow notification delivery, cancellation permissions, retained-ID behavior after runtime cleanup, or actual Worker/D1. Those remain explicit gaps. The recorded approval settles the product rules independently of these proof limits.

### Notification follow-up proof, 2026-09-14

`bash scripts/test-restate.sh admission_protocol_proof` **passed in 21.96 seconds**, including all earlier scenarios and [notifications.rs](../../crates/ghinvite-workflows/tests/admission_protocol_proof/notifications.rs). Targeted Clippy with `-D warnings` passed. The proof configures two-second completion retention on its workflow and a one-second cleanup scan on the disposable smoke runtime only. The pinned runtime otherwise scans hourly, with an initial delay: simply waiting two seconds did not exercise cleanup. See pinned [cleaner implementation](https://github.com/restatedev/restate/blob/v1.7.9/crates/worker/src/partition/cleaner.rs).

- Notification before startup returns HTTP 200, and promise inspection returns the recorded fact while the run counter remains zero. Starting the workflow consumes it without a second notification.
- A separately started waiting workflow receives a later fact. After promise resolution, a retryable interruption followed by endpoint task abort recovers the notification and completes the waiting run.
- Sequential identical publication returns already-delivered; a different retained fact returns 409. Arbitrary concurrent conflicting writers are not tested or supported by that peek-then-resolve check.
- Actual cleanup is observed as the promise becoming absent, **not** a failed shared-handler HTTP status. After cleanup, late notification returns HTTP 200 and recreates the promise. The run counter stays unchanged: notification does not restart the workflow.

The first experiments failed because they expected shared handlers to become unavailable and then expected immediate retention cleanup under the default hourly scan. The corrected proof measures promise absence and accelerates the real cleaner; it does not simulate cleanup or infer it from elapsed time. Two-second retention and one-second scanning are proof settings, not production policy.

This establishes native direct-promise mechanics, not full link-to-workflow integration or exactly-once GitHub delivery. Post-retention orphan-state cleanup, authoritative fact verification, consumption/dispatch checkpoints, notification/timer integration in the production workflow, and administrative redrive remain implementation obligations. Worker/D1 stays deferred.

## Alternatives and consequences

**Database-authoritative admission coordinated by Restate** was considered. It is closer to the current implementation and provides database read-after-write visibility, but couples admission/revocation to database availability and requires a cross-system committed-outcome recovery protocol. It was not chosen as the target architecture.

**Waiting for SQL projection inside the link handler** would simplify some immediate reads, but a failing write would block later revocation and admission. Asynchronous projection is deliberate, not an optimization to remove casually.

Restate state becomes critical business data. Per-link serialization limits a hot link's throughput, retained requester/operation history can grow on unlimited links, and object state layout/retention must be designed accordingly. Projection adds read-lag, ordering, and repair work, but external database failures no longer participate in the admission decision.

## Historical reconciliation and references

The original v1 design spec (§8 request timer, §9 workflow, and Q8) and the Restate handlers implementation plan (Plan 3) (both removed; see git history) retain the historical link-expiry deadline cap and older workflow/storage assumptions. This ADR supersedes those admission/deadline rules; their original bodies remain historical evidence. The current domain language is in [CONTEXT.md](../../CONTEXT.md).

Restate's [database integration guidance](https://docs.restate.dev/guides/databases) describes object-state consistency and retry-safe external writes; its [state documentation](https://docs.restate.dev/develop/ts/state) distinguishes object-state retention from workflow retention. These explain the architectural choice; implementation details must be checked against this repository's pinned Rust SDK/runtime rather than inferred from examples for other SDKs.
