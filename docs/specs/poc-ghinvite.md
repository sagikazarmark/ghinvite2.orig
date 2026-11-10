# F0: Harvest of the ghinvite PoC

Source: `sagikazarmark/ghinvite2.orig` at `89a67fa` (593 commits, 2026-05-04 → 2026-09-25). I read the code and docs and did not build or run anything. Paths are relative to the PoC repo root unless they start with `/home/laborant/specs.orig`. **Inference** marks a conclusion I drew rather than one the PoC states.

## 1. Summary

- The PoC is a production-grade **repo-collaborator-only** slice of P1. Admins create invitation links for one or more repos, with one permission level, max uses, expiry and optional approval. Requesters sign in with GitHub and request access. Approved requests fan out to one GitHub collaborator invitation per repo. There's an audit log and a request queue.
- The stack is Rust end to end: two Cloudflare Workers (web + Restate service endpoint) sharing one D1 database, KV for sessions, Dioxus SSR with exactly **one** browser island, and Restate Cloud as the runtime. Auth is **GitHub OAuth directly (no Clerk)**. Admin rights come from GitHub (org owner, or the owner of a personal account).
- The **Restate Rust SDK runs on Workers** through a fork patch (`sagikazarmark/restate-sdk-rust@e882e9e`, upstream PR restatedev/sdk-rust#128, **still open**), in request-response mode. This was verified locally in workerd/Miniflare against real Restate 1.7.9. **No remote/production deployment has been verified yet** (issue #61).
- About 70% of the engineering effort went into correctness under failure: authoritative admission in Virtual Objects, replay-safe receipts, projections to SQL, an "outcome unknown" model for GitHub writes, and an HTTP-attempt fence in D1. The architecture was reworked three times; ADR 0009 is the latest.
- **Installation tokens are never journaled**: they are minted and used inside the same `ctx.run` closure, and only outcomes are journaled. **User OAuth tokens** live in a 30-day encrypted session (XChaCha20-Poly1305, KV). There is **no refresh-token handling**.
- **Missing relative to the notes:** org-level links, team assignment, multi-app split, email-domain restriction, access expiry, pause vs. revoke, revoking pending invitations on link revoke, bulk invite, role precedence, resend, and RBAC beyond "GitHub admin".
- **Liftable:** the `ghinvite-github` crate (JWT, transport, redaction, installation client) is close to a reusable C1/C5 base. The token cache is per invocation on Workers, so it has little effect there.

## 2. What runs today, and on what

| Concern | What the PoC does | Evidence |
|---|---|---|
| Hosting | Two Workers: `ghinvite-web` (axum + SSR, OAuth, webhooks) and `ghinvite-restate-svc` (Restate endpoint). Native binaries (`*-server`) exist for dev/tests only | `README.md` §Architecture; `wrangler/web.toml`; `wrangler/restate-svc.toml`; `crates/ghinvite-web-server`, `crates/ghinvite-workflows-server` |
| Restate on Workers | `restate-sdk` 0.12 **patched** via `[patch.crates-io]` to `sagikazarmark/restate-sdk-rust` rev `e882e9e` ("Wasm clocks and input draining: restatedev/sdk-rust#128"). The patch uses `web_time` for `Instant`/`SystemTime` (std aborts on wasm32), skips the `tokio::time::timeout` around `drain_input` on wasm, and selects getrandom JS backends. Upstream PR #128 is **OPEN** (checked 2026-10-15). Upstream has since released v0.12.1 (2026-09-22) | `Cargo.toml` (last lines); `Cargo.lock` `restate-sdk 0.12.0 (git…e882e9e)`; `docs/worker-admission-gate.md` "Pins" ("The original SDK 0.10.0 compiled but panicked in `ctx.run`'s native `Instant`") |
| Protocol mode | `ProtocolMode::RequestResponse`, because Workers buffer the request body (no bidi streaming). Lazy state is enabled on the link object to keep request bodies small (measured: 7 round trips, ~35 KB max request for a fresh rejection) | `crates/ghinvite-workflows-worker/src/lib.rs:94-103`; `crates/ghinvite-workflows/src/admission.rs:77`; `docs/worker-admission-gate.md` (measurements) |
| Restate runtime | Restate Cloud in production (identity-key signing `RESTATE_IDENTITY_KEY`, ingress bearer key `GHINVITE_RESTATE_API_KEY`); Docker `restate:1.7.9` locally | `crates/ghinvite-workflows/src/lib.rs:53-81`; `compose.yaml`; `docs/restate-ingress-gate.md` |
| Database | D1 (prod), SQLx/SQLite (native). All SQL lives in `ghinvite-core`; adapters only bind and execute (ADR 0005). A shared conformance suite runs against both, including D1 in workerd | `crates/ghinvite-core/src/storage/`; `crates/ghinvite-storage-d1/src/wasm_impl.rs`; `migrations/0001_initial.sql` (12 tables) |
| Sessions | `tower-sessions` over KV (`SESSIONS` binding) on Workers and in-memory SQLite natively. Complete records are encrypted with XChaCha20-Poly1305 under `GHINVITE_SESSION_SECRET`, with deny-only revocation markers | `crates/ghinvite-web-worker/src/lib.rs:21-60`; `crates/ghinvite-web/src/session_store.rs`; ADR 0002 |
| UI | axum + `dioxus_ssr`, with native `<form method=post>` for every mutation. One `dioxus-web` island (new-link form) served from Workers Static Assets with dioform validation. CSP `script-src 'self' 'wasm-unsafe-eval'` | ADR 0001; `crates/ghinvite-ui`, `crates/ghinvite-island`; `scripts/build-island.sh` |
| Auth | **GitHub App user OAuth directly**: `/login`, `/oauth/callback`, `scope=read:user read:org`. **No Clerk anywhere** (grep finds nothing). Admin = `GET /user/memberships/orgs/{login}` → active `admin` role, cached 60 s in the session; personal account = same user ID | `crates/ghinvite-github/src/oauth.rs:57,132-160`; `crates/ghinvite-web/src/middleware/auth.rs:14-51` |
| GitHub App | **One App**: Repository Administration RW, Metadata R, optional Organization Members R plus the `member` webhook. A Setup URL (`/setup/github`) handles install/update redirects | `README.md` §GitHub App setup; `crates/ghinvite-web/src/routes/setup.rs` |
| Secrets | Worker secrets: App private key (base64 PEM), OAuth client secret, webhook secret, session key, Restate ingress API key, Restate identity key. The App ID is a plain var. There's no KMS or envelope encryption | `wrangler/*.toml` comments; `crates/ghinvite-workflows-worker/src/lib.rs:34-56` |
| Build/CI | Dagger modules plus GH Actions: fmt/clippy/test, per-crate wasm32 builds (never `--workspace`), island bundle with a size budget, real Restate gate (`scripts/test-restate.sh`), Worker/D1 gate (`tests/worker`, Miniflare), Playwright browser tests | `README.md` §CI; `.github/workflows/ci.yml`; `tests/worker/*.mjs`; `tests/browser/*.mjs` |
| Deploy status | **Production rollout blocked** under #61. Local Worker/D1 gate (#59) is complete. Remote signatures, routing, CPU/subrequest budgets and D1 restore haven't been verified | `docs/deploy.md` §Rollout status; `docs/worker-admission-gate.md` §Remaining rollout gates |

## 3. Feature coverage against the P1 notes

Legend: **covered** = implemented with tests; **partial** = some machinery exists but not the notes' feature; **missing** = no code.

| Notes feature (`/home/laborant/specs.orig/githubstuff.md`) | Status | Evidence / comment |
|---|---|---|
| Org-level links (invite into an org) | **missing** | There's no org invitation API call: `InstallationClient` only has repo collaborator endpoints (`crates/ghinvite-github/src/installation.rs:363-520`). `CONTEXT.md` defines Invitation Link as "repository collaborator access" only |
| Repo-level links | **covered** | `CreateLink.repos` (`crates/ghinvite-core/src/invitation_link.rs:18-30`); `PUT /repos/{o}/{r}/collaborators/{u}` (`installation.rs:363`) |
| Multi-repo links | **covered** | Repository Scope is a non-empty set; one `RepositoryDelivery/<request>:<repo>` object per repo (ADR 0009; `crates/ghinvite-workflows/src/delivery.rs`) |
| Per-repo permission levels | **missing** (single level per link) | `CreateLink.permission: Permission` (one value); `InvitationLinkRepo` has only `repo_id`, `repo_full_name` (`invitation_link.rs:152-155`) |
| Team auto-assignment | **missing** | No team API anywhere ("team" appears only in unrelated docs) |
| Max uses | **covered** | `max_uses: Option<u32>`; a use is a *created request*, and declined/expired requests don't refund it (`CONTEXT.md` Invitation Link; ADR 0003) |
| Link expiry | **covered** | `expires_at`; `Inactive::check` (`invitation_link.rs:100-110`) |
| Email-domain restriction | **missing** | No email logic. OAuth scope is `read:user read:org` (`oauth.rs:57`) |
| Access expiry (auto-removal after X) | **missing** | No removal of collaborators anywhere |
| Pause vs. permanent revoke | **partial** | Only a permanent `revoke` (`crates/ghinvite-workflows/src/admission.rs:323-357`). There's no reversible pause |
| Revoke pending invites on link revoke | **missing** (by decision) | ADR 0003: "Link expiry/revocation does not change existing requests." `RepositoryDelivery::cancel` exists (`delivery.rs:126`; `settlement.rs` `Withdrawal::Cancel` → `delete_invitation`), but no web route calls it. Request cancellation UI is explicitly deferred (ADR 0003 §Cancellation scope) |
| Audit log | **covered** | 18 event types (`crates/ghinvite-core/src/audit.rs:85-103`); `/console/accounts/{login}/audit` with cursor paging (`docs/audit-log.md`); stable event IDs survive replay |
| Approval mode | **covered** | `approval_required`; approve/decline routes (`crates/ghinvite-web/src/routes/console.rs:65-70`); 7-day decision deadline `PENDING_LIFETIME` (`admission.rs:26`), arbitrated by processing time |
| Link editing rules | **covered (stricter than notes)** | Guardrails are immutable from creation. Only description and internal note can change (`crates/ghinvite-ui/src/link_edit.rs`; `CONTEXT.md` "Guardrails are fixed… create a new invitation link"). The notes' "editable until first request" rule isn't implemented |
| Bulk invite | **missing** | None. Note: per-repo delivery objects plus plan submission (`submit_plan` in `delivery.rs`) are a reusable fan-out core (see C8) |
| Role precedence (keep higher existing role) | **missing; possible downgrade risk** | Delivery always PUTs the link's permission. It doesn't read the existing role first (`delivery.rs:654-700`). `CollaboratorRole::matches` treats a higher role as "not a match" (`installation.rs:50-62`) and only uses it in read-only reconciliation. **Inference:** for an existing collaborator, GitHub's PUT updates the permission, so a lower-permission link could downgrade a user. This needs a desk check |
| Invite acceptance tracking | **covered** | Signed `member`/`added` webhook → `RepositoryDelivery::on_webhook`, plus `Reconcile::daily_run` sweep over `list_invitations` (`docs/invitation-settlement.md`; `crates/ghinvite-workflows/src/reconcile.rs`) |
| Own invite expiry (revoke if not accepted) | **partial** | `tick_expire` exists and settles Expired only with list evidence (`settlement.rs:360-450`). "Automatic lifecycle cadence remains in #86" (open). **Inference:** nothing schedules `daily_run`: there's no cron trigger in `wrangler/*.toml` and no self-rescheduling |
| Resend | **missing** | The UI copy tells requesters *not* to resubmit (`crates/ghinvite-ui/src/invitation.rs:152`). ADR 0004: "introduces no manual resend UI" |
| Two GitHub Apps (org vs repo) | **missing** | One App with repo Administration RW (`README.md`) |
| Onboarding / install selection | **partial** | GitHub's own install screen plus Setup URL redirect → `Installation::onboard` → `AccountInstallation` (`routes/setup.rs`; `docs/installation-availability.md`). There's no in-app choice of apps. A repo picker for large installs is open (#111) |
| RBAC for link creators | **missing (GitHub-derived only)** | Org owner or personal-account owner only (`middleware/auth.rs:14-51`). Delegation to non-admins isn't possible |
| Requester justification | **covered** | `justification: Option<String>`, normalized (`crates/ghinvite-core/src/admission.rs:30-40`) |
| "Already used the link" page | **covered** | One pending/approved request per requester per link; the status page is driven by authoritative reads (`CONTEXT.md` Invitation Request; `routes/invitation.rs`) |
| Admin notifications | **missing** | #46 is open (channel not chosen) |

## 4. ADRs and decisions

All ADRs are in `docs/adr/`. `docs/adr.bak/` is an older copy (0003, 0004, 0006 and 0008 differ, and it lacks the second 0009).

| ADR | Gist | Why it matters for feasibility |
|---|---|---|
| 0001 SSR-first with Dioxus islands | Dioxus fullstack doesn't compile for `wasm32-unknown-unknown` (`dioxus-server` → axum/tokio-net/mio), so the PoC uses SSR plus native forms, with Dioxus only as fresh-render islands. Amended: the island must "take over" live DOM input, and a second island needs its own ADR | **Hard platform limit:** Dioxus fullstack + Workers is a no-go. One island cost about 1,000 LOC plus 2,400 LOC of tests, 4.4× the form it enhances. The named escape hatch is a console SPA on Static Assets with a JSON API (13–20 engineer-days) |
| 0002 Session protection | Complete session records (including the GitHub user token) are encrypted with XChaCha20-Poly1305 using a single app key. Revocation markers live in KV, with a fixed 30-day lifetime. RustCrypto was chosen over cryptbox because cryptbox's binding API doesn't accept arbitrary AAD | **Evidence for C2/C3:** cryptbox was already evaluated and set aside here. Workers KV eventual consistency is accepted for sign-out |
| 0003 Restate-authoritative admission | A link-keyed Virtual Object is the admission authority. SQL/D1 is an async projection. Replay-safe operation IDs, no use refunds, a 7-day decision deadline with processing-time arbitration, lazy split-key state | Shows that the notes' "VirtualObject blocking handler for in-flight vs. revoke" race works (tested). It also costs a lot: Restate state becomes business-critical data with no automatic expiry |
| 0004 Delivery identity and uncertain results | Bind immutable numeric requester ID and re-validate the login before PUT. 201/204/definitive error are facts; timeouts become *outcome unknown*, with no automatic repeat PUT. Amended: accept on a ≤5-min availability observation, reject only on a live read | Shows that GitHub writes can't be exactly-once with Restate. A D1 "attempt fence" is needed. The same applies to P2 merges |
| 0005 Core owns SQL | All SQL and row decoding lives in core, and the D1/SQLx adapters only bind values. D1 quirks: `results::<T>()` panics on bad rows; numbers above 2^53 can't be bound | **Liftable pattern** for any D1+SQLx product. It does duplicate bind lists (~84 per adapter) |
| 0006 Delivery vs. settlement checks stay distinct | Delivery, settlement and availability each run their own prerequisite check | Mostly internal. Partly superseded by 0009 |
| 0007 Attempt continuations | The browser retains sealed exact input (XChaCha20, session key, scope as AAD) so an uncertain Restate call can be retried with the same operation ID. Outcome semantics differ per flow | **Pattern for C5:** recoverable form posts over Restate ingress |
| 0008 Concrete LinkAuthority over ingress | No traits or in-memory fakes for Restate calls. Tests use a fake HTTP ingress server instead | Testing strategy for any web→Restate app |
| 0009 (two files) Canonical link ID; request-owned lifecycle; unified RepositoryDelivery | Link ULID = public code = object key. `InvitationRequest` VO owns decisions and deadlines (delayed messages replace workflow sleeps and promises). `RepositoryDelivery/<req>:<repo>` owns create plus settlement | Current architecture. It shows the PoC moved **away from Restate Workflows and promises toward short exclusive VO handlers plus delayed sends**, which is a useful default for P1/P2 |

Other decision-bearing docs: `docs/network-deadlines.md` (30 s GitHub, 15 s Restate; a timeout means outcome unknown), `docs/installation-availability.md` (installation replacement, tombstones, 60 s recheck), `docs/delivery-recovery.md`, `docs/admission-v1.md`.

## 5. Domain glossary seeds (from `CONTEXT.md`)

- **Account**: the GitHub account (org or personal) where the app is installed.
- **Account Admin**: has GitHub-derived authority over the account (org owner, or the personal-account owner).
- **GitHub User**: a person before a request exists. **Requester**: a person after submitting one. "Recipient" isn't canonical.
- **Invitation Link**: shareable URL with **guardrails** (max use, expiration, permission level, repository scope, approval policy), all fixed at creation. **Description** (≤120 chars, admin-only, editable) and **Internal Note** (admin-only, editable).
- **Invitation Code**: the code segment of the URL. It's now the link ULID. Never call it "slug".
- **Max use / uses**: a use is a *created invitation request*, never refunded.
- **Revoke**: the canonical lifecycle verb. It stops new requests and doesn't touch existing ones.
- **Invitation Request**: one per requester per link while pending/approved. It can be auto-approved, or manually approved/declined/expired/cancelled.
- **Invitation Request Admission**: the authoritative accept/reject, judged at admission time and replay-stable.
- **Invitation Request Decision Deadline**: admitted_at + 7 days, independent of link expiry.
- **Approval Policy**: auto vs. admin decision.
- **GitHub Invitation**: per-repository collaborator invitation.
- **Repository Access**, **Permission Level** (pull/triage/push/maintain/admin), **Repository Scope**, **Repository Identity** (owner + name, numeric ID used for matching), **Available Repositories** ("recent" observation is ≤5 min).
- **Delivery Outcome Unknown**, **Blocked Delivery**, **Throttled Delivery**: distinct non-failure delivery states.
- **Console** (admin workspace), **Public Surface**, **Invitation Request Flow**, **Audit Log**.
- **Ghinvite Session**, **Sign Out**, **Global Session Invalidation**.

For our specs, the vocabulary to adopt is: guardrails, use, revoke, admission, decision deadline, outcome unknown, blocked/throttled. The vocabulary to extend is: org-level links, teams, pause.

## 6. Workarounds and hints at platform limits

1. **Dioxus fullstack doesn't run on Workers** (ADR 0001): "`dioxus-server` hard-depends on `axum/tokio` and `tokio-tungstenite`, which pull `tokio/net` and `mio` and fail to compile for `wasm32-unknown-unknown`." Result: SSR plus islands, and an island-takeover protocol to avoid losing input. `cfg(target_arch="wasm32")` means "Workers" in server crates and "browser" in UI crates, so builds must be per crate (`README.md`). This affects any Dioxus+Workers product (P1, P2, P6 dashboard).
2. **Restate Rust SDK on wasm32** needs a fork: `std::time::Instant/SystemTime::now()` aborts at runtime on wasm32, and `tokio::time::timeout` panics without a runtime (commit message of `e882e9e`). `cargo check` doesn't catch either problem. The fix is upstream PR #128, **unmerged**. Workers buffer bodies, so the endpoint uses `ProtocolMode::RequestResponse` (`crates/ghinvite-workflows-worker/src/lib.rs:94-103`). That costs suspension and replay round trips per lazy state read, so `enable_lazy_state(true)` is a deliberate trade-off (ADR 0003).
3. **`Send` shims:** `KvStore`, `D1Database` and futures are `!Send`. The PoC uses `unsafe impl Send` wrappers (`WasmSend`, `KvSessionStore`) justified by single-threaded wasm (`crates/ghinvite-storage-d1/src/wasm_impl.rs:28-40`; `crates/ghinvite-web-worker/src/lib.rs:39-45`).
4. **Crypto/randomness on wasm:** JWT RS256 is signed by hand with `rsa` because `ring` (jsonwebtoken default) doesn't target wasm cleanly (`crates/ghinvite-github/src/jwt.rs:1-5`). getrandom 0.2 and 0.3 both need JS backends plus a `--cfg` flag (`.cargo/config.toml`; `crates/ghinvite-github/Cargo.toml`).
5. **reqwest timeouts on wasm:** `ClientBuilder::timeout` is native-only, so a per-request `RequestBuilder::timeout` is applied on wasm (`crates/ghinvite-github/src/transport.rs:247-353`; `docs/network-deadlines.md`).
6. **D1 quirks:** `D1Result::results::<T>()` panics on bad rows, so the adapter reads `serde_json::Value` first. Numbers are bound as JS doubles, so the adapter refuses u64 above 2^53−1 (ADR 0005).
7. **Tracing:** workerd lacks `performance.mark/measure`, so tracing goes through a custom console writer (`crates/ghinvite-workflows-worker/src/lib.rs:110-120`).
8. **Is the installation token ever journaled? No.** `InstallationClient::installation_token` (`installation.rs:310-321`) mints or caches inside the client. Every call that needs a token runs inside one `ctx.run(...)` closure (for example `guarded_github_create`, `delivery.rs:264-302`, which calls `attempt()` at `delivery.rs:563`). The closure returns only an `Attempt` receipt or outcome, and only that is journaled. The token is minted just before the fence, *inside* the same closure: "Mint the PUT's credential before the fence… A token this fresh outlives the PUT, which then reuses it from the cache" (`delivery.rs:654-667`). Error values never keep upstream bodies (`crates/ghinvite-github/src/redact.rs` module docs: "keeps **no upstream text at all**"), so tokens can't leak into Restate terminal errors either. `tests/credential_disclosure.rs` enforces this.
   - Consequence: the notes' C3 problem is **sidestepped**, not solved. Nothing secret crosses a journal boundary, so there's no `TokenExpired` loop. The cost is an extra mint whenever a different invocation needs a token.
   - **Inference:** the `TokenCache` (`token_cache.rs`) sits on `InstallationClient`, which is built **per `fetch`** on the Worker (`crates/ghinvite-workflows-worker/src/lib.rs:74`). The doc comment at `installation.rs:109` says "Construct once per worker invocation". So on Workers the cache lasts at most one HTTP request from Restate, and every Restate invocation that touches GitHub mints a new token. The notes' rate-limit concern is unaddressed at scale.
9. **User tokens** never reach Restate. Web passes `{account_id, user_id}` assertions (`AccountAdmin`, `invitation_link.rs:8-13`). The token sits in the encrypted session (`crates/ghinvite-web/src/session.rs:15`). **No refresh-token logic exists** (grep for `refresh_token` finds nothing). **Inference:** if the GitHub App has user-token expiration on, admin checks fail after 8 h within a 30-day session. The README doesn't say which setting is assumed.
10. **Exactly-once GitHub writes are impossible.** The PoC adds a D1 HTTP-attempt fence (`delivery_attempts` table; `claim_delivery_attempt`, `delivery.rs:669`) to cover the gap between "GitHub responded" and "journal acknowledged". Ambiguity becomes *outcome unknown* plus read-only reconciliation (ADR 0004). Restate alone isn't enough for safe external writes.
11. **Restate Workflow retention and promises were abandoned** in favour of persistent VOs with delayed messages (ADR 0009). Reason: workflow retention isn't the business lifetime, and late promise resolution recreates orphan state (ADR 0003 notification proof).
12. **Scheduling:** there's no Workers cron and no self-rescheduling `Reconcile::daily_run` (**inference**, from `wrangler/*.toml` and `reconcile.rs`). It's an open item (#86). The user's `restate-cron` would fit here.

## 7. Shared components in disguise

| PLAN component | PoC code | What it has | Liftability |
|---|---|---|---|
| **C1 Installation token broker** | `crates/ghinvite-github/src/{jwt.rs, token_cache.rs, installation.rs, transport.rs, error.rs, pagination.rs, redact.rs}` | Wasm-safe RS256 App JWT (9 min, 60 s backdate); installation-token mint plus in-process cache (60 s buffer); `HttpTransport` seam with reqwest (native + wasm) and `MockTransport`; rate-limit classification (`Error::RateLimited`, `RateLimit`); exhaustive `Link` pagination where a partial walk is an error; credential-safe errors | **High (library).** About 5.5 kLOC, depends only on `ghinvite-core` for `Permission`, which is easy to cut. Missing for C1: a cross-invocation cache (a Restate VO, DO or KV), multiple App identities, repo-scoped or permission-downscoped tokens, and app-level secondary rate-limit handling. Pattern to keep: "token lives only inside `ctx.run`" |
| **C2 User token vault** | `crates/ghinvite-web/src/session_store.rs`, `session.rs`, `routes/oauth.rs`, `github/src/oauth.rs` | OAuth code exchange, `UserApiClient` (`/user`, org membership, user installations), encrypted session record with revocation | **Low.** Tokens are session-scoped, with no refresh and no rotation. C2's hard part (single-use refresh rotated exactly once) isn't there. The encryption envelope (ADR 0002) is reusable |
| **C3 Journal secrets** | `crates/ghinvite-github/src/redact.rs` (+ `tests/credential_disclosure.rs`); `crates/ghinvite-web/src/attempt_continuations.rs` (sealed payloads) | Design *avoidance*: no secret enters the journal or errors. Redaction keeps no upstream text | **Medium as a pattern, low as code.** There's no codec or encryption of journal values. Redact is a small module that can be lifted. It's evidence that C3 may be optional for installation tokens (see §8) |
| **C5 App scaffolding** | `routes/setup.rs`, `routes/webhook.rs` (HMAC `X-Hub-Signature-256`, `octoevents`), `workflows/src/installation.rs` + `availability.rs` (Installation/AccountInstallation VOs, replacement and tombstones, 60 s recheck), `middleware/{auth,csrf,csp}.rs`, `restate_client.rs`, `attempt_continuations.rs`, worker entry crates, `wrangler/*.toml`, `.dagger/` | Install/update redirect, installation convergence, webhook verification and routing receipts, console admin extractor, CSRF, CSP, Restate ingress client with deadlines, Worker wiring | **Medium.** Solid but tied to ghinvite types. Single App only, so the multi-app split, Clerk↔GitHub identity linking and an onboarding UI are all new work. `availability.rs` (1.2 kLOC) is the most reusable installation-state machine |
| **C6 Authz** | `crates/ghinvite-web/src/middleware/auth.rs:14-51` (`check_admin`, `RequireConsoleAdminOf`) | GitHub-derived authority: org owner via `GET /user/memberships/orgs/{login}`, 60 s cache, numeric-ID binding. Concealing 404 for non-admins | **Low–medium.** A simple base. No roles, delegation or per-repo scoping |
| **C7 Audit** | `crates/ghinvite-core/src/audit.rs`, `storage/audit_{read,write}.rs`, `ghinvite-ui/src/audit.rs`, `docs/audit-log.md` | Typed events (actor kind, target kind, event type), stable IDs for replay-dedup, projection that never drops events, cursor paging (25/page), safe metadata rendering | **Medium–high.** The event enum is domain-specific, but the envelope, stable-ID dedup, paging and the "audit is retained, not cache" rule generalize |
| **C8 Bulk ops** | `delivery.rs` (`RepositoryDelivery` per item, `submit_plan`, `DeliveryRecovery`), `request_owner.rs` (immutable approved manifest + per-repo submission progress), `throttle.rs` | Fan-out of an immutable plan to per-item VOs with per-item receipts; blocked/throttled/unknown states; rechecks honouring GitHub's `Retry-After` | **Medium.** It's the right *shape* for P1 bulk invite and P2 bulk merge (manifest → per-item owner → receipts → projection). It has no review step and no cross-item rate budget |
| Storage traits (D1/SQLx) | `crates/ghinvite-core/src/storage/*`, `ghinvite-storage-{d1,sqlx}` | Role-split ports, core-owned SQL, a conformance suite run on both, D1 batch projection with revisions | **Medium (pattern).** A generic "D1+SQLx with shared conformance suite" template is worth extracting. The tables themselves aren't |
| Platform: Restate on Workers | `crates/ghinvite-workflows-worker/src/lib.rs`, SDK fork, `tests/worker/*` (Miniflare + real Restate harness) | Working endpoint wiring and a test harness that observes protocol frames | **High.** It belongs in `restate-toolkit` or a template, and depends on upstreaming PR #128 |

## 8. Feasibility evidence against the PLAN §7 seed risks

| Seed risk | PoC answer | Status |
|---|---|---|
| P1: org invites, team assignment, role precedence doable with app permissions | Repo collaborator invites only, with a single App holding repo Administration RW. No org/team calls. Role precedence isn't handled, and PUT may downgrade (§3) | **Open.** Repo part proven; org/team/precedence need a desk check and a spike |
| P1: invite acceptance observable (webhook or polling) | `member`/`added` webhook (needs Org Members R) plus a `list_invitations` sweep with full pagination. The README notes personal-account webhook limits (`docs/invitation-settlement.md#member-added-webhooks`) | **Answered for repo invites** (code + tests). Org-invite acceptance (`organization`/`member_added`) is open. Scheduling the sweep is open (#86) |
| P1: email-domain restriction via verified emails | Not attempted | **Open** |
| P1: 50/day invite limit vs. bulk | Throttled Delivery is modeled. GitHub rate-limit responses become retries at GitHub's stated time, never failures (`CONTEXT.md` Throttled Delivery; `delivery.rs`) | **Partially answered.** The mechanism exists; the bulk budget and UX don't |
| P1/P2: secrets in the Restate journal (C3/C4) | The installation token never leaves `ctx.run`, and errors are redacted (§6.8) | **Partly answered:** for installation tokens, a codec isn't needed if the broker is "mint inside the run". **Open** for P2 user tokens, which must be stored durably for refresh, and for any secret that must survive across invocations |
| P2: refresh-token rotation survives crashes (C2) | Not present | **Open** |
| P2: fan-out within Workers CPU/subrequest limits | Local measurement only (7 round trips, ~35 KB for one admission). Remote CPU/subrequest budgets are an explicit #61 gate | **Open** |
| All: Clerk + `dioxus-clerk` on Workers; is Clerk still wanted? | The PoC uses GitHub OAuth directly with GitHub-derived admin, and works on Workers. Clerk was never tried | **Open (decision).** The PoC proves Clerk isn't *needed* for "GitHub admins create links". Clerk only becomes necessary for RBAC or delegation (C6) |
| All: Restate Rust SDK on Workers, patch status and upstream plans | Works on SDK 0.12.0 + fork `e882e9e`, request-response mode, verified locally in workerd with real Restate 1.7.9 (`docs/worker-admission-gate.md`). PR #128 still open; upstream is at v0.12.1 | **Answered locally; remote unverified.** Upstream status is the risk: we carry a fork |
| (implicit) Dioxus on Workers | Fullstack doesn't compile; SSR + islands works (ADR 0001) | **Answered.** Constrains P2's dashboard UX (filtering and bulk selection are the ADR's "escape hatch" triggers) |
| (implicit) D1 as projection store | Works, with conformance on real D1 in workerd. Remote D1 restore isn't validated | **Answered locally** |

## 9. Open questions for the user

1. **Keep or reset?** Is the PoC the P1 codebase to extend (org links, teams, two apps), or a reference to mine? Its correctness machinery is heavy, so extending it is costly but safe.
2. **Restate SDK fork:** is PR restatedev/sdk-rust#128 expected to merge? Should we rebase the fork on v0.12.1, or vendor it in `restate-toolkit`?
3. **Clerk:** the PoC dropped it (or never adopted it) in favour of GitHub OAuth. Was that deliberate? Does P1 v1 need non-admin link creators? If not, Clerk can wait.
4. **User-token expiry:** is the PoC's GitHub App configured with user-token expiration off? The 30-day session with no refresh suggests so. P2 can't rely on that.
5. **Role precedence:** have you seen GitHub's collaborator PUT downgrade an existing higher role? Should precedence be "never lower", as the notes say?
6. **Link revoke semantics:** the PoC decided revoke never touches existing requests or invites. Do you still want "revoke + cancel pending invites" and "pause" from the notes?
7. **Guardrail immutability:** the PoC forbids editing guardrails entirely. Do you accept that over the notes' "editable until first request" rule?
8. **Token cache:** do you accept one mint per Restate invocation, or should C1 be a shared broker (VO/DO/KV) from the start?
9. **Production rollout (#61):** has any remote Workers + Restate Cloud deployment been run, even outside the repo's gate? It's the biggest unverified platform claim.
10. **`docs/adr.bak/` and duplicate ADR 0009 numbering:** can the older copy be ignored as stale?
