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
| `v1/link` | Link identity/account, fixed guardrails, metadata, revocation, uses, and monotonic revision. No requester list or outcome history. Read for new admission and link changes. |
| `v1/op/<operation_id>` | Immutable canonical input/fingerprint and original accepted/rejected outcome. Direct lookup before eligibility. |
| `v1/request/<request_id>` | Requester identity, authoritative lifecycle state, admission time/deadline, current revision, and transition/dispatch identity needed for safe recovery. Direct lookup by request. |
| `v1/blocker/<requester_id>` | The exact currently pending/approved request ID. At most one pointer per requester/link; absent when retry-eligible. |

Use canonical typed identifiers and versioned encoding, not arbitrary delimiter-containing strings. The proof reuses operation identity as request identity for simplicity; the production proposal below separates them. Its link-local operation key binds requester as input, and the same raw operation ID under another link is a different scoped operation. These precise identity/retention choices remain proposed for confirmation.

### Proposed operation identity and retention contract

The following completes the engineering recommendation for identity and retention. Same-input replay, changed-input conflict, remembered rejections, and stable outcomes after expiry/revocation are already approved. The precise scope, encoding, ID separation, and no-expiry policy below are proposed for maintainer confirmation; they are not additional runtime results.

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

Malformed/missing operation IDs and invalid command structure are validation failures before admission and consume no use. Do not silently replace a malformed ID. A failure before a valid authoritative decision does not create a business outcome record. Once a valid command is decided, both acceptance and eligibility rejection bind its input and identity permanently under the proposed retention policy.

#### Browser and transport behavior

Generate the operation ID when rendering a fresh submission form and preserve it with the same business input on uncertain transport failure. Retrying that attempt reaches the authoritative outcome lookup before current link/repeat eligibility shortcuts. If the user edits the justification after an uncertain result, retain the original attempt for status/recovery and treat the edited form as an explicitly fresh attempt with a fresh ID. Do not automatically change IDs merely to get past a conflict. Separate tabs may have distinct IDs; pending/approved suppression still prevents another admitted request for the same requester/link.

The current route in [`routes/invitation.rs`](../../crates/ghinvite-web/src/routes/invitation.rs) already generates a form ID and preserves it in its failed-POST response, but silently replaces invalid IDs and includes a newly generated submission timestamp on each retry. It also short-circuits on projected link/request eligibility. Production implementation must rename/distinguish the hidden operation ID from the accepted request ID, reject invalid IDs, exclude transport time from equality, and route replay through the authoritative command. Recovery across reload/navigation needs a concrete status/attempt lookup in the web-read ticket; the hidden field alone does not guarantee that continuity.

Use the link object's application-level outcome lookup for business replay. Do not set the bare business operation ID as an ingress deduplication key if doing so bypasses payload comparison. A normal call may have a different transport invocation ID on retry; link exclusivity and the operation record still recover its result. Runtime-generated deduplication for retries of the same transport invocation is compatible with this contract. The new command version must actually reach the application to detect same-ID/different-version conflicts.

#### Retention and cleanup

**Proposed initial policy: no automatic expiry of authoritative admission outcomes**, including rejected outcomes. Link expiration/revocation, request completion, session expiry, database projection success, and completed-workflow/invocation journal cleanup do not remove them. An authenticated replay returns the original admission outcome, not the current request state, and never starts a completed workflow again simply because its runtime retention elapsed.

Keep the minimal replay record: canonical input/version, outcome/reason, admission decision time, and accepted request ID/deadline/policy result where relevant. Delivery bookkeeping and historical request payloads have separate retention needs; projection completion is not permission to discard replay identity. A database copy does not become the admission authority if the Restate record is missing. A restored or unexpectedly missing authoritative state must enter the defined recovery path, not silently bootstrap a fresh attempt from a lagging projection.

This policy deliberately accumulates state. Lazy lookup bounds normal access, not retained record count. A future finite replay window, account/link deletion, or archival scheme requires an explicit amendment specifying how an old identity remains distinguishable from a never-used one. A tombstone can prevent duplicate execution but cannot return the original outcome unless enough data or an authoritative archive remains. Never implement TTL eviction that turns an old operation ID into a fresh operation. Capacity/input bounds and operational monitoring remain rollout requirements, rather than an implicit permission to evict outcomes.

#### Examples and verification still needed

| Attempt | Required result under this proposal |
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

**Sequencing update (2026-09-14):** The maintainer [deferred actual Worker/D1 verification](https://github.com/sagikazarmark/ghinvite2.orig/issues/48#issuecomment-5667489260) for now. Continue contract and lifecycle design using the recorded native evidence. Worker clock/endpoint and D1 adapter verification remain explicit follow-ups; native request-response/SQLx success does not satisfy them. This deferral does not establish production readiness or by itself close #48.

| Question | Proposed next step / approval needed |
|---|---|
| What are the production capacity limits and retention policy? | Lazy split-key state and exclusive coherent reads passed the focused proof. Verify actual runtime/Cloud key/value limits, input/scope bounds, storage growth, and Worker round-trip cost before rollout. |
| Does decision-time sampling work on the actual Worker target? | The clock is sampled inside the complete decision closure; before/after-decision expiration recovery passed natively in request-response mode. Verify Wasm clock behavior and the actual Worker endpoint. |
| Confirm the proposed operation identity/retention contract? | Link-scoped operation IDs, requester bound in canonical input, separate server-generated request IDs, and no automatic outcome expiry are specified above. Confirm these choices and implement normalization/privacy/cross-link/retention-expiry verification. |
| What pending lifetime is configured, and when is an admin decision timely? | Current code has seven days, but configuration scope and deadline-edge arbitration need confirmation. Recommend enforcing the stored deadline at the authoritative transition; timers only wake work. |
| Does an overdue request block a fresh attempt until its timer runs? | Recommend admission first materialize an overdue pending request's expiry, so scheduler lag does not extend blocking. This behavior needs approval. |
| How are auto-approval and cancellation represented? | Specify whether auto-approval is part of the admission transition; define cancellation actors and allowed states. Existing cancellation terminology does not by itself implement a cancellation command. |
| Service or keyed object for projection; snapshots or ordered deltas? | Choose the smallest protocol that handles reordered writes, missing parents, and audit completeness. Verify on actual SQLx and D1 execution paths. |
| How do authoritative status reads work during projection lag? | Specify authorization, lookup by link/request identity, and post-command SSR navigation, including freshly created links absent from SQL. |
| What can be rebuilt after retention, administrative kill/purge, or independent restore? | Define Restate backup/state retention and projection rebuild/redrive procedures. Workflow restart after completed-workflow retention must not repeat downstream GitHub effects. |

## Migration and implementation seams

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

These are candidate ticket seams, not published tickets or a claim that the protocol has passed its gate. Establish dependencies from the proof rather than executing them as independent migrations.

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

## Alternatives and consequences

**Database-authoritative admission coordinated by Restate** was considered. It is closer to the current implementation and provides database read-after-write visibility, but couples admission/revocation to database availability and requires a cross-system committed-outcome recovery protocol. It was not chosen as the target architecture.

**Waiting for SQL projection inside the link handler** would simplify some immediate reads, but a failing write would block later revocation and admission. Asynchronous projection is deliberate, not an optimization to remove casually.

Restate state becomes critical business data. Per-link serialization limits a hot link's throughput, retained requester/operation history can grow on unlimited links, and object state layout/retention must be designed accordingly. Projection adds read-lag, ordering, and repair work, but external database failures no longer participate in the admission decision.

## Historical reconciliation and references

The [v1 design](../superpowers/specs/2026-05-04-ghinvite-v1-design.md) (§8 request timer, §9 workflow, and Q8) and [Plan 3](../superpowers/plans/2026-05-04-ghinvite-restate-handlers.md) retain the historical link-expiry deadline cap and older workflow/storage assumptions. This ADR supersedes those admission/deadline rules; their original bodies remain historical evidence. The current domain language is in [CONTEXT.md](../../CONTEXT.md).

Restate's [database integration guidance](https://docs.restate.dev/guides/databases) describes object-state consistency and retry-safe external writes; its [state documentation](https://docs.restate.dev/develop/ts/state) distinguishes object-state retention from workflow retention. These explain the architectural choice; implementation details must be checked against this repository's pinned Rust SDK/runtime rather than inferred from examples for other SDKs.
