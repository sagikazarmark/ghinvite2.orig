# ghinvite: migration from gh-toolkit to the octoauth crates (phase D)

| Field | Value |
|---|---|
| Parent | `plans/octoauth-plan.md` §3.4 (phase D) |
| Status | **Done, 2026-10-20.** All tasks G2-06 and GM-00…GM-10 are complete (§7); gh-toolkit is deleted |
| Decisions | ADR 0003 (amended 2026-10-20), ADR 0014 (+ naming amendment) |
| Baseline | `implementation/ghinvite` and `implementation/octoauth` at `57657f8` |
| Evidence | Two inventories of ghinvite's gh-toolkit use (2026-10-20 session), summarized in §1. File references are relative to `implementation/ghinvite/crates/` unless stated; `wf` = `ghinvite-workflows/src`, `web` = `ghinvite-web/src` |

**Conventions.** Task IDs `GM-nn` replace the outline in the parent plan. One task is one agent session and one PR; a task that grows past that is split along the seams named in it. Every task keeps the full ghinvite CI green (`.github/workflows/ci.yml`: workspace tests, fmt, clippy `-D warnings`, the `integration` feature, the wasm32 builds, the Miniflare runtime suites, `scripts/test-restate.sh`). **UNVERIFIED** marks a claim no test or source backs yet.

---

## 1. What ghinvite uses today

ghinvite is about 162k lines of Rust. About 107 files name `gh_toolkit`, about 35 of them production sources.

| Area | gh-toolkit API | Where and how much |
|---|---|---|
| **HTTP transport** | `ReqwestTransport`, `WorkerFetchTransport` (gh-toolkit-worker), all behind `Arc<dyn HttpTransportDyn>` | Built in all four binaries: `ghinvite-web-server/src/main.rs:51`, `ghinvite-web-worker/src/lib.rs:309`, `ghinvite-workflows-server/src/main.rs:72`, `ghinvite-workflows-worker/src/lib.rs:53`. Both Workers build theirs per request |
| **App registry** | `AppRegistry`, `AppConfig`, `AppSlot`, `SigningKeys` (`kid:base64;…`), `WebhookSecrets`, `ClientId`/`ClientSecret`, `AppJwtSource` | `ghinvite-config/src/apps.rs`. Three slots: `signin` (OAuth only), `repo`, `org` (stage `repo-and-org`) |
| **Token broker** | `Broker` with `with_token` (one 401 retry), `call` (no replay), `ensure`, `ensure_fresh`, `invalidate_installation`, `read_only()` | Workflows: 14 `with_token`, 12 `call` (2 in dead code, `wf/settlement/withdrawal.rs`), 7 `ensure`, 1 each `ensure_fresh`/`invalidate_installation` (`wf/availability.rs:1211-1221`). Web: 2 `with_token` (`web/authority/mod.rs:774`, `bulk_review.rs:300`) on a per-request read-only broker. Seven scope profiles (`wf/github_profiles.rs`, `web/authority/mod.rs:35-39`) |
| **App-JWT reads** | `get_installation`, `list_installation_requests` | 9 + 1 sites (e.g. `web/installed_apps.rs:99, 135`, `wf/availability.rs:192`) |
| **REST layer** | `Rest`, `RestEnv`, `PathTemplate`, `Credential`, `QuotaBucket`, pagination with `Partial`, typed endpoints, `roles`, `payloads` | About 40 endpoint helpers across `wf` and `web/authority`, with many edge-case guarantees (exact statuses, `total_count` checks, validated cursors persisted in the AuthorityCache) |
| **Errors** | `ApiError` (incl. `NotSent`, `Ambiguous`, `Decode{what}`, `Partial`, `NotAllowed`, `Gone`, `Forbidden{Permission}`), `MintError`, `BrokerError`, `CallError` | Retry and outcome tables in `wf/error.rs:32-110`, `wf/repository_protocol.rs:610-721`, `wf/cancellation_protocol.rs:228-288`, `wf/access_grant/protocol.rs:358-416`, `wf/org_delivery/protocol.rs:483-874`, `wf/email_delivery/protocol.rs:246-317`, `web/error.rs` |
| **Webhooks** | `WebhookRouter`, `VerifiedDelivery` (with `dedupe.payload_sha256`), `events::{InstallationEvent, InstallationRepositoriesEvent}`, `TracingSink` | `web/routes/webhook.rs`, `webhook_organization.rs`; octoevents 0.3 directly for `EventKind` only |
| **Sign-in** | Typed `signin::*` (opt-in), `legacy_oauth::*` (**production default**: `GH_SIGNIN_ENABLED = "false"` in `wrangler/web.toml`) | `web/signin/*`, `web/routes/oauth.rs`, `routes/oauth/typed.rs`; about 28 web test files sign in through the legacy helper |
| **Governor** | `WriteLedger`, `BucketSpec`, `Decision`, `SlotId`, `content_write_buckets` | `wf/invite_budget/model.rs` only; the ledger is **persisted Restate state** |
| **Small modules** | `email_invitation::{Address, create}`, `avatar::{canonical_url, get}`, `redact::scrub` (2 sites), `bounded_upstream_code`/`OAUTH_ERROR_CODES`, `jwt::{Clock, SystemClock}` | email-vault, `web/email_rows.rs`, `wf/email_delivery/protocol.rs:236`, `web/routes/console/request_history.rs:428` |
| **Test fixtures** | `MockTransport`/`Expectation` (feature `test-mock`), `stub::router` and the `stub` example binary (`test-stub`) | ~50 test files script `MockTransport`; ~28 use `Harness::start*` with the stub and its control routes (`/identity`, `/calls`, `/reset`, `/outcomes`, …); Miniflare suites build the stub binary (`tests/worker/build-stub.mjs`); `ghinvite-web-worker/src/broker_fixture.rs` tests broker coalescing on Workers |
| **Tooling** | `scripts/check-toolkit-boundary.{sh,py}`, `verify-toolkit-features.sh`, `check-toolkit-error-codes.py`, `test-toolkit-*.py` | CI and Dagger |

## 2. Decisions

| # | Question | Recommendation |
|---|---|---|
| PD-1 | Which GitHub client does ghinvite use? | **Its own crate, `ghinvite-github`, moved from gh-toolkit's transport, REST layer and endpoints. No octocrab in ghinvite.** See below |
| PD-2 | Do octoauth and octologin get a pluggable sender (the parent plan's open item)? | **No.** See below |
| PD-3 | How do token futures reach `ctx.run`, which requires `Send` futures (`restate-sdk-rust/src/context/mod.rs:968-972`), on Workers where octotoken's are not `Send`? | Wrap them in `send_wrapper::SendWrapper` on wasm32 (already a ghinvite-web dependency with the `futures` feature), replacing gh-toolkit's `unsafe` `SendFut`. A Worker isolate is single-threaded, so the wrapper's thread check never fires |
| PD-4 | Token cache on Workers | One `AppTokenProvider` per app in an isolate-scope `static` (`OnceLock`), not per request (both Workers build per request today). On Workers concurrent misses may create duplicate tokens: no cross-request coalescing (ADR 0003 amendment). The Miniflare `coalesce` case changes its expectation accordingly |
| PD-5 | Several signing keys | octoauth signs with one key. Use the **first** configured key and drop gh-toolkit's second-key fallback. Rotation: add the new key in GitHub, deploy it first in the list, then remove the old key in GitHub. The `kid:base64;…` env format stays, so deployments need no change |
| PD-6 | App-authenticated reads (`get_installation`, `list_installation_requests`) | Add them to octoauth first (prerequisite **G2-06**, §3): `Installation::get()` and `App::credential()` |
| PD-7 | Legacy OAuth is the production default; typed sign-in waits on live gates SP-19/SP-19a | **Decided by the user (2026-10-20): switch production to typed sign-in and drop legacy OAuth now** (GM-09), without waiting for SP-19/SP-19a. Typed sign-in becomes the only path in the next deploy |
| PD-8 | `installation.deleted` acts on the signed payload alone and retires the installation for good (`wf/availability.rs:743`, `continue_uninstall`) | Confirm with `octoauth::Installation::get()` before acting (parent plan §5). Done in GM-06 |
| PD-9 | Persisted formats | Every serialized value that crosses the move keeps its exact serde shape: the `WriteLedger` in Restate state, `RepositoryCursor`/`TeamCursor` in the AuthorityCache (KV on Workers), `PendingSignIn`, the webhook dedupe keys, and every `ctx.run` result type. GM-00 pins them with golden tests. If one must change, in-flight Restate invocations are drained before the deploy |

**PD-1 in detail.** ADR 0014 leaves the client to the consumer. For ghinvite the existing code is the better client:
- **Write fencing needs "not sent" vs "sent, no answer".** Every write path releases its budget and storage fence only when the transport proves the request never left (`ApiError::NotSent`, at 7 sites). octocrab reports transport failures as opaque service errors and cannot make that distinction.
- **The endpoint helpers carry the edge cases.** For example: `put_collaborator`'s 201-with-invitation vs 204, exact-status checks returning `Decode`, exhaustive pagination that fails with `Partial` on SSO partial results, `total_count` and duplicate-ID checks, cursors re-validated before they are followed. Rewriting them on octocrab would re-derive each one.
- **The tests depend on the seam.** About 50 test files script `MockTransport`, and about 28 drive the stub. Keeping the transport trait keeps them.
- **Workers.** octocrab on Workers is UNVERIFIED and would need a custom tower service and executor anyway.

As a result, the parent plan's deferred "Workers transport for octocrab" and "octocrab adapter" lose ghinvite as their first consumer (parent plan §4).

**PD-2 in detail.** On Workers, `fetch` follows redirects, so a redirected request has already been re-sent when octoauth or octologin discards the answer. The exposure is small:
- **Token creation** sends no secret in its body. `fetch` drops `Authorization` when a redirect crosses origins, and a same-origin redirect stays on `api.github.com`.
- **The sign-in code exchange** carries the client secret in its body, and a 307/308 re-sends it. GitHub does not redirect these endpoints, so a redirect needs a compromised TLS connection to github.com, which exposes far more than this request.

A pluggable sender would also break the rule that the toolkit owns its own requests' redaction and bounds. **Revisit** when a consumer must route GitHub traffic through something reqwest cannot be, such as a service binding or an egress proxy.

## 3. Prerequisite: G2-06 (octoauth)

**G2-06. App-authenticated reads.**
- **What:**
  - `Installation::get() -> Result<InstallationInfo, Error>`: `GET /app/installations/{id}`, typed, with token creation's error mapping. `InstallationInfo` has:
    - `id`, `app_id`;
    - `account { id, login, kind: User | Organization | Unknown }`;
    - `permissions` (GitHub's permission names and access as strings, since GitHub adds permissions faster than an enum can follow);
    - `repository_selection`;
    - `suspended_at: Option<SystemTime>`.
  - `is_suspended` becomes a thin wrapper over `get`.
  - `App::credential() -> Result<AppCredential, Error>`: the cached app JWT for app-level endpoints octoauth does not wrap, such as `GET /app/installation-requests`. `AppCredential` is a secret type: not `Clone`, `Serialize` or `Display`, redacted `Debug`, `expose_secret()`. Its docs say clock-skew correction only happens on octoauth's own requests, and that a 401 with it should be retried once after `App::installation(..).get()` has re-learned the skew.
- **Watch out:** ghinvite checks `id`, `app_id`, `account.id`, `account.account_type == "Organization"`, `suspended_at` and `permissions.contains_key("members")`. All of them must be readable from `InstallationInfo`.
- **Done when:**
  - wiremock tests cover the typed fields, unknown account kinds and permissions, a malformed 200 (`Decode`), and the error mapping;
  - `AppCredential` is in `tests/secret_types.rs`;
  - `cargo check --target wasm32-unknown-unknown` passes.

## 4. Tasks

### GM-00. Baseline and golden formats

- **What:** before anything moves, pin what must not change.
  1. Record the CI baseline: test counts per crate, and the green Miniflare and Restate suites.
  2. Inventory every gh-toolkit type reachable from a serialized value: Restate state, `ctx.run` result types, AuthorityCache entries, SQL/D1 columns, session values. Start from `wf/invite_budget/model.rs:35-66` (`WriteLedger`, `SlotId`), the cursor types in `web/authority/*`, and `PendingSignIn` (`web/signin/mod.rs:23-30`).
  3. Add golden tests: serialize a representative value of each, compare with a checked-in JSON fixture, and deserialize the fixture back.
- **Done when:** the golden tests pass on the current code and are listed in this plan. Every later GM task must pass them unchanged (PD-9).

### GM-01. Create `ghinvite-github`

- **What:** a new crate `crates/ghinvite-github`, copied from gh-toolkit and trimmed to what ghinvite uses. No ghinvite crate switches to it yet.
- **Keep (copied from `implementation/gh-toolkit/crates/`):**
  - `transport.rs`: the `HttpTransport` trait, `Request`/`Response`, `ReqwestTransport`, `NotSent` vs `SentNoResponse`, deadlines, size bounds.
  - `gh-toolkit-worker/src/lib.rs`: `WorkerFetchTransport` with manual redirects and abort-on-drop, behind a wasm32-only module.
  - `rest.rs`, `rest/listing.rs`, `pagination.rs`: `Rest`, `PathTemplate`, `Partial`.
  - The used parts of `endpoints*.rs`: the list in §1, plus `Endpoint` metadata for the profile test.
  - `roles.rs`, `payloads.rs`, `email_invitation::create` (GM-07 moves `Address`), `avatar.rs`.
  - `error.rs`'s `ApiError`, `CallError` and `ValidationCode`, with every variant ghinvite matches.
  - `mocks.rs` (feature `test-mock`) and `stub.rs` plus the `stub` example (feature `test-stub`).
- **Change while copying:**
  - **Credentials.** The sealed `Credential` trait gets implementations for `InstallationCredential` (wrapping `octoauth::InstallationToken`, `QuotaBucket::Installation` from its `app_id`/`installation_id`) and `AppCredential` (wrapping G2-06's `octoauth::AppCredential`). No user-token credential: sign-in moves to octologin (GM-08).
  - **Error classification.** `ApiError::from_response` takes its rate-limit, 403 and 422 classification from `octoauth::ApiError::classify` and adds ghinvite's own variants (`Partial`, `NotAllowed`, `Gone`, `Decode{what}`, `Forbidden{Permission}`). That is one copy of the evidence rules instead of two.
  - **`Send`.** `unsafe` `SendFut` becomes `send_wrapper` (PD-3).
  - `getrandom` and `observations` preflight are dropped unless a kept module needs them.
- **Drop:** `broker`, `jwt`, `app`, `installation`, `observations`, `rest/etag.rs`, `graphql`, `endpoints/admin.rs`, `endpoints/pulls*`, `legacy_oauth`, `signin`, `webhook`, `governor`, `redact`, `wasm_smoke`, the extraction scripts and API snapshots.
- **Tests:** port gh-toolkit's tests for the kept code: transport certainty and deadlines, endpoint contract tests, pagination, the stub contract, and the `transport.mjs`/`deadlines.mjs` Worker cases.
- **Done when:** `cargo test -p ghinvite-github --all-features` passes, the library builds for wasm32, and the crate has no gh-toolkit dependency. If it outgrows one PR, split it into **GM-01a** (transport, Worker transport, REST, errors, mocks) and **GM-01b** (endpoints, roles, payloads, stub).

### GM-02. Apps and the token context

- **What:**
  - **Config.** `ghinvite-config` builds one `octoauth::App` per installed slot from the existing variables (first key, PD-5), next to the old `AppRegistry` (both exist until GM-10).
  - **Token context.** A `GithubApps` context in `ghinvite-github` replaces `RepositoryGithub`'s `Broker` + `AppJwtSource` (`wf/state.rs:21-47`). It holds one `AppTokenProvider` per app, in an isolate-scope static on Workers (PD-4) and in `AppState` natively. Its operations:

    | Operation | Replaces | Behaviour |
    |---|---|---|
    | `read(installation, scope, \|rest\| …)` | `with_token` | `octotoken::with_token` with `ApiError::Unauthorized` as the predicate |
    | `write_once(installation, scope, \|rest\| …)` | `call` | `token()`, one call, never replayed; returns the token's generation so the caller can `replace` it on a 401, exactly as today's generation reject |
    | `ensure(installation, scope)` | `ensure` | `token()` |
    | `refresh(installation, scope)` | `invalidate_installation` + `ensure_fresh` | `replace(token())` |
    | `installation(app, id).get()` | the app-JWT `get_installation` reads | G2-06 |
    | `app_credential(app)` | `list_installation_requests`'s credential | G2-06 |

  - **Scope profiles** become `TokenScope` values built once. A test asserts the read profiles grant only read access, and the profile test in `wf/github_profiles.rs:40-138` is ported to `ghinvite-github`'s `Endpoint` metadata. This replaces the compile-time `ReadOnlyBroker`/`ReadKey` check.
  - **Token errors.** `TokenError` replaces `MintError`. The mapping keeps `wf/error.rs`'s retry rule (retry only `Transient` and `RateLimited`, which is `octoauth::Error::is_transient`):

    | gh-toolkit `MintError` | octoauth `Error` |
    |---|---|
    | `Transient { kind }` | `Transient { kind }` |
    | `RateLimited { scope, retry_after }` | `RateLimited { scope, retry_after }` |
    | `PermissionsNotGranted` | `PermissionsNotGranted` |
    | `InstallationGone` | `InstallationGone` |
    | `InstallationSuspended` | `InstallationSuspended` |
    | `MintForbidden { reason }` | `Forbidden { reason }` |
    | `AppAuth(..)` | `AppCredentialsRejected { .. }` / `Signing(..)` |
    | `Decode`, `Unexpected` | `Decode`, `Unexpected`, `Redirected` |

  - **Tests.** A `FixedTokens` provider implementing `octotoken::InstallationTokenProvider` replaces the mint expectations in `MockTransport` scripts. `InstallationTokenProvider` is not object-safe, so `GithubApps` uses a small object-safe trait with boxed futures and a blanket impl over providers. The stub's token route (`POST /app/installations/{id}/access_tokens`) already answers in the shape octoauth parses.
- **No call site switches in this task.**
- **Done when:** `GithubApps` has unit tests for each operation, including 401 → `replace` for `read` and no replay for `write_once`. Config tests cover the first-key rule.

### GM-03. Workflows on `GithubApps`: the repo app

- **What:** switch the repo-slot paths:
  - `wf/repository_protocol.rs` (`with_token` 190, 349; `call` 466, 513; `ensure` 461, 495);
  - `wf/cancellation_protocol.rs`;
  - `wf/access_grant/protocol.rs` (repo parts);
  - `wf/settlement.rs`, `wf/settlement_observation.rs`, `wf/reconcile.rs`.

  Also:
  - Delete the dead `wf/settlement/withdrawal.rs` path, which only its own tests reach.
  - Replace the credential-binding check in `settlement_observation.rs:108-115` with `InstallationCredential`'s app and installation IDs.
  - Wrap `ctx.run` bodies in `SendWrapper` on wasm32 (PD-3).
- **Keep:** the outcome tables in §1's error row. The 401 path still rejects the token generation and loops once.
- **Done when:** workflow unit and integration tests pass with `MockTransport` + `FixedTokens` and with the stub, along with the GM-00 golden tests and `scripts/test-restate.sh`.

### GM-04. Workflows on `GithubApps`: the org app and availability

- **What:** switch:
  - `wf/org_delivery/*`, `wf/org_reconcile.rs`, `wf/org_eligibility.rs`;
  - `wf/email_delivery/protocol.rs` (`email_invitation::create` from `ghinvite-github`);
  - `wf/availability.rs` and `availability/org_observation.rs`;
  - the org parts of `access_grant`.

  `refresh_installation_credentials` (`availability.rs:1211-1234`) uses `GithubApps::refresh`. The app-JWT `get_installation` reads use `Installation::get()`.
- **Watch out:** `Decode { what }` sentinels matched by string:
  - `"email_team_managed"` and `"email_no_seat"` (`email_delivery/protocol.rs:100-105`);
  - `"organization_requester_identity"` (`org_delivery/protocol.rs:144-146`).

  They keep working because `ghinvite-github` keeps `Decode { what }`. Turning them into typed errors is a follow-up, not part of this task.
- **Done when:** as GM-03, including the org integration tests. No `gh_toolkit` import is left in `ghinvite-workflows` except `governor` (GM-07).

### GM-05. Web and the Workers on `GithubApps`

- **What:**
  - **Web.** Switch:
    - `web/authority/mod.rs` and `bulk_review.rs`: per-request read-only broker → shared `GithubApps::read` with the console read scopes;
    - `web/installed_apps.rs`: `Installation::get()`, and `app_credential` for installation requests;
    - `web/state.rs`.
  - **Binaries.** Build `GithubApps` in all four binaries; Workers use the isolate-scope static (PD-4).
  - **Runtime fixture.** Rewrite `ghinvite-web-worker/src/broker_fixture.rs` and `tests/worker/broker.mjs` for `AppTokenProvider`. The `coalesce` case now expects concurrent misses to succeed, possibly with duplicate creations, and the next request to be a cache hit.
- **Watch out:** the web `with_token` closures call `self.reserve()` (the read budget, `authority/mod.rs:777`, `bulk_review.rs:303`). Under a 401 retry that would run twice: move `reserve()` before `read`.
- **Done when:** web tests, the wasm32 builds and every Miniflare suite pass. `AppJwtSource`, `Broker`, `RestEnv` and `HttpTransportDyn` from gh-toolkit are no longer used.

### GM-06. Webhooks on octoevents 0.5

- **What:**
  - **Verification.** Replace `WebhookRouter` with one `octoevents::Verifier` per app (secrets chained with `also` for rotation) and `octoevents::authenticate(&verifier, &headers, body)`.
  - **Keep the route's behaviour:** routing by the `/webhooks/{app}` path, the header pre-checks and their statuses, the 2 MiB body cap, and the 202 answers.
  - **Dedupe key.** Compute `Sha256::digest(body)` in ghinvite. It is the dedupe key: `webhook/{hex}` in Restate state, `payload_sha256` in `organization_webhook_receipts`, and must stay identical (PD-9).
  - **Decoders.** Move `InstallationEvent` and `InstallationRepositoriesEvent` (typed actions, sender, the foreign-key rejection) into `ghinvite-web`.
  - **Signature-failure event.** Replace `TracingSink`'s with a plain `tracing` event with the same fields.
  - **Confirm deletion (PD-8).** Before `continue_uninstall` (`wf/availability.rs:743`), call `Installation::get()`:
    - `Err(InstallationGone)` confirms;
    - a successful read means the installation still exists, so the delivery is logged and ignored;
    - a transient error is retried as other availability reads are.
- **Watch out:** the octoevents 0.3 → 0.5 breaks:
  - `Envelope::from_signed` → `authenticate`;
  - `meta` is now `WebhookMeta`;
  - `Envelope::new` changed;
  - new `ReceiveError::UnknownTarget`;
  - signature errors are `Missing | Malformed | Mismatch`.
- **Done when:**
  - `tests/app_webhooks.rs`, `tests/org_webhooks.rs`, `route_smoke.rs` and `wf/tests/retained_delivery.rs` pass unchanged (they sign with `hmac` directly);
  - new tests cover a deletion that the API does not confirm, and secret rotation;
  - octoevents 0.3 is gone from `Cargo.lock`.

### GM-07. Leftover modules

- **What:**
  - **Governor.** Move `gh-toolkit/src/governor.rs` and `governor/tests.rs` into `ghinvite-workflows` (e.g. `invite_budget/ledger.rs`). Its only dependencies are serde and `UnixTime = i64`; the serde shape must not change (PD-9, GM-00 golden test).
  - **Email address.** Move `email_invitation::Address` into `ghinvite-email-vault`, the vault's only gh-toolkit use.
  - **Scrubbing.** Replace `redact::scrub` with `octoscrub::scrub` (`wf/repository_protocol.rs:116`, `web/authority/bulk_review.rs:498`).
  - **OAuth error codes.** Replace `bounded_upstream_code(…, OAUTH_ERROR_CODES)` with `octologin::OAuthErrorCode::from_github` (`web/error.rs:96-101`).
  - **Clock.** Give `ghinvite-core` its own `Clock` trait and `SystemClock`, replacing `gh_toolkit::jwt::{Clock, SystemClock}` at its four sites.
- **Done when:** the moved tests pass, the golden tests pass, and none of these modules is imported from gh-toolkit.
- Independent of GM-01…GM-06: it can run any time after GM-00.

### GM-08. Typed sign-in on octologin

- **What:**
  - `SignInService` (`web/signin/mod.rs`) uses `octologin::SignIn`, built from the `signin` slot's client ID and secret and `{P1_HOST}/oauth/callback`, with the base URLs injectable for tests.
  - `PendingSignIn` storage is unchanged: its `state` and `verifier` fields map one-to-one onto `PendingAuthorization` (PD-9).
  - Adapt to octologin's differences:
    - `emails` returns verified addresses only (simplify `signin/domains.rs:37`), and a second page is `Incomplete`;
    - `User::created_at` is a `SystemTime`;
    - the callback's `error` goes through `OAuthErrorCode::from_github`.
  - `revoke` still runs on every path that obtained a token (`mod.rs:287-296`). On Workers the octologin futures go through `SendWrapper`, like the routes today (`web/routes/webhook.rs:174`).
- **Tests:** move `src/signin/tests.rs` (11 tests) and `tests/pending_signin.rs`'s end-to-end case from `MockTransport` to wiremock. The pagination test becomes an `Incomplete` test.
- **Done when:** typed sign-in tests pass natively and the web-worker wasm32 build passes. The legacy path is untouched.
- Independent of GM-01…GM-06: it can run any time after GM-00.

### GM-09. Remove legacy OAuth (PD-7 decided: now)

- **What:** once production runs typed sign-in, delete:
  - the legacy branch of `/login` and `/oauth/callback` (`web/routes/oauth.rs`);
  - `legacy_oauth::OAuthConfig` and `GHINVITE_GITHUB_CLIENT_ID`/`_SECRET` (`web/config.rs`);
  - the `access_token` session field and its stripping (`session.rs:14`, `session_store.rs:158, 184`);
  - the `legacy-p1-oauth` feature.

  Move the ~28 test files from the legacy sign-in helper (`tests/common/sign_in.rs`) to a typed helper on wiremock.
- **Done when:** web tests pass, and no route or config value reads the legacy variables.

### GM-10. Remove gh-toolkit

- **What:**
  - Drop `gh-toolkit` and `gh-toolkit-worker` from the workspace and every crate. Point `ghinvite-e2e` and the workflows `integration` feature at `ghinvite-github`'s stub.
  - Make `tests/worker/build-stub.mjs` build `ghinvite-github`'s stub example.
  - Delete the toolkit scripts (§1 Tooling) and their CI and Dagger steps.
  - Delete `ghinvite-config`'s old `AppRegistry`.
  - Delete `implementation/gh-toolkit`.
  - In the docs: mark the L2 spec as describing the removed toolkit (status line and §17), and update this plan and the parent plan's progress.
- **Done when:** CI is green, `cargo tree` shows neither crate, and no file in the repository outside history references `gh_toolkit` (`rg gh_toolkit` is empty).

## 5. Order and parallelism

```
GM-00 ──┬─► GM-01 ─► GM-02 ─┬─► GM-03 ─┬─► GM-05 ─► GM-06 ──┐
G2-06 ──┘                   └─► GM-04 ─┘                    │
GM-00 ─► GM-07 ─────────────────────────────────────────────┤
GM-00 ─► GM-08 ─► GM-09 (needs PD-7) ───────────────────────┴─► GM-10
```

- GM-00 comes first.
- G2-06 (in `implementation/octoauth`) can run in parallel with GM-00.
- GM-03 and GM-04 touch different files and can run in parallel.
- GM-07 and GM-08 are independent of the token work.
- GM-10 needs all the others, so it waits on PD-7.

## 6. Risks

| Risk | Mitigation |
|---|---|
| A serialized shape changes and in-flight Restate invocations or stored state fail to decode | GM-00 golden tests in every task; drain before deploy if a change is unavoidable (PD-9) |
| `NotSent` stops being produced on a path, so budgets and fences are never released | `ghinvite-github` keeps gh-toolkit's transport and its certainty tests (GM-01) |
| Token creation on Workers is not `Send` inside `ctx.run` | `SendWrapper` (PD-3); the wasm32 builds of both Workers in every task's CI |
| More token creations on Workers (no coalescing, isolate cache only) | Accepted by ADR 0003's amendment. Watch creation volume after GM-05; the shared `TokenStore` is the remedy (parent plan §4) |
| A 401 retry repeats a side effect inside a read closure | `with_token`'s contract is one request per closure. GM-05 moves `reserve()` out; GM-03/GM-04 review each `read` closure for side effects |
| Legacy OAuth blocks the final removal | PD-7; GM-09 is small once production has switched |
| The deleted-installation confirmation delays real uninstalls when the API is unavailable | The read is retried like other availability reads; the delay is bounded by Restate's retry policy |

## 7. Progress (as built)

### G2-06 (octoauth), 2026-10-20

Done as specified, with these additions.

**API:**
- `Installation::get() -> InstallationInfo`, with these fields:
  - `id`, `app_id`;
  - `account { id, login, kind: User | Organization | Enterprise | Unknown }`;
  - `permissions: BTreeMap<String, String>` and `permission(name)`;
  - `repository_selection: All | Selected | Unknown`;
  - `suspended_at`.
- `is_suspended()` is now a wrapper over `get()`.
- `App::credential() -> AppCredential`, with `app_id()`, `expires_at()` and `expose_secret()`. It is not `Clone`, `Serialize` or `Display`, and its `Debug` is redacted.
- `octotoken` re-exports all the new types.

**Behaviour:**
- **Answers about something else are refused.** A body naming another installation `id` or `app_id` is a `Decode` error, including in `access_token`'s suspension check.
- **Required fields:** `suspended_at` must be present (`null` is allowed), as before. `permissions` and `repository_selection` must be present.
- **Enterprise accounts:**
  - The account kind falls back to the top-level `target_type`, because GitHub's enterprise account object has no `type`.
  - An empty or missing `login` falls back to `slug`.

**Limitation:** `InstallationInfo` is `#[non_exhaustive]`, so other crates cannot build one. Add a constructor if `ghinvite-github`'s test fakes need one.

### GM-00 (ghinvite), 2026-10-20

**Golden tests.** Ten `golden_*` tests pin every persisted value that holds a gh-toolkit type. Fixtures live in `ghinvite-workflows/golden/` and `ghinvite-web/golden/`; `GOLDEN_BLESS=1` rewrites them, and by default they are only compared.

| Persisted value | Storage | Golden test |
|---|---|---|
| InviteBudget `State` (`WriteLedger`, `SlotId`) | Restate state `budget` | `golden_invite_budget_state_{org,repo_refund}` |
| `WriteLedger` with every internal state populated | inside the above | `golden_write_ledger` |
| `verify_onboard_identity` result `Option<GhAppInstallation>` | `ctx.run` journal (`wf/availability.rs:540`) | `golden_verify_onboard_identity_journal` |
| Console authority cache entries (`Membership`, `RepositoryPage`/`RepositoryCursor`, `TeamPage`/`TeamCursor`, `TeamIdentity`, `GhUser`) | KV / memory | `golden_authority_cache_console_entries` |
| Bulk preview cache entries | KV / memory | `golden_authority_cache_preview_entries` |
| `PendingSignIn` | SQL/D1 (encrypted) | `golden_pending_signin` |
| Webhook dedupe digest (`webhook/{hex}`) | Restate key | `golden_webhook_dedupe_key` |
| Org webhook receipt digest and event keys | SQL `organization_webhook_receipts` | `golden_organization_webhook_dedupe_keys` |
| Email vault plaintext (`Address` normal form) | SQL/D1 (encrypted) | `golden_address_canonical_form` |

Restate handler inputs and outputs and all other `ctx.run` results hold no gh-toolkit types: roles are mapped to core enums before they are returned.

**Not covered:**
- The Restate key strings built inline in handlers. They need a Restate server; the existing `installation_authority.rs:299` test asserts `webhook/{…}`.
- KV on Miniflare.

**Baseline (`cargo test --workspace`, native):** 1081 passed and 0 failed before GM-00; 1091 after.

| Crate | Passed |
|---|---|
| web | 527 (+9 ignored) |
| ui | 155 |
| core | 134 |
| workflows | 124 |
| storage-sqlx | 61 |
| island | 55 |
| email-vault | 15 |
| workflows-server | 11 |
| config | 5 |
| web-server | 4 |

**Not run locally:**
- the Restate integration job (needs `restate-server`);
- the Miniflare suites;
- the wasm32 and packaging builds;
- the docker observability checks.

**Pre-existing CI failure.** `clippy::nonminimal_bool` at `ghinvite-web/src/routes/onboarding.rs:160` fails `clippy -D warnings`. It is in unchanged code; fix it in the first GM task that touches ghinvite-web.

**Findings that change later tasks:**
1. **A `ctx.run` journals a gh-toolkit type** (GM-04, GM-06). `verify_onboard_identity` returns `Option<GhAppInstallation>`, with gh-toolkit's derive, as a journaled result. When it switches to `Installation::get()`, either map `InstallationInfo` into a ghinvite type with exactly the `GhAppInstallation` JSON shape, or drain in-flight availability invocations before deploying. Keeping the shape is the default (PD-9).
2. **`WriteLedger` is serialized from private gh-toolkit fields** (GM-07). Copy `governor.rs` with its serde attributes unchanged. The serialized form has these quirks:
   - a `{"state": …}` wrapper;
   - numeric map keys written as strings;
   - externally tagged enums;
   - `holds`, `hold_keys` and `commits` always present, even though ghinvite never takes a hold.
3. **Cursors are newtypes over private relative-path strings** (GM-01). `ghinvite-github` must keep their serde form, which is the bare string, and keep validating them when they are read back.
4. **The org identity cache entry is written as a `json!({"id","login"})` literal and read as `GhUser`** (GM-01). The replacement type must keep `avatar_url` and `type` optional (`serde(default)`).
5. **gh-toolkit's error `Display` text reaches the journal in terminal-error messages,** for example at `wf/reconcile.rs:243`. Nothing decodes it, but the wording will change once errors come from `ghinvite-github`. That is acceptable, and nothing parses it.

### GM-01, GM-07, GM-08 (2026-10-20)

The three tasks ran in parallel on separate branches (`gm01`, `gm07`, `gm08`) and were merged into `main`.

**State of `main` after the merge (native):**
- `cargo test --workspace`: 1254 passed, 0 failed, 9 ignored.
- `clippy --workspace --all-targets -D warnings` is clean, including the `integration` lint. The old `nonminimal_bool` error is fixed.
- fmt is clean.
- All CI wasm32 builds pass: ui, island, storage-d1, web-worker, workflows-worker, the island clippy, and `ghinvite-github`.

**Not run locally:** the Restate integration job, most Miniflare suites, and packaging. GM-08 did run the sign-in Miniflare suites on workerd: `pending-signin`, `domain-signin` and `signin-cutover`.

**GM-01: `ghinvite-github`.** It is a workspace member; no ghinvite crate uses it yet.
- **Contents:** about 5.5k lines moved from gh-toolkit:
  - transport, Worker transport, REST layer, pagination;
  - 26 of gh-toolkit's 44 endpoints;
  - roles, payloads, `email_invitation::create` (takes `&str`), avatar;
  - errors, mocks, and the stub with its example binary.
- **Credentials:** `InstallationCredential` wraps `octoauth::InstallationToken`, and `AppCredential` wraps `octoauth::AppCredential`.
- **Errors:** `ApiError::from_response` takes its classification from `octoauth::ApiError::classify`.
- **Other changes:**
  - `RestEnv::new(transport, api_base, user_agent)` replaces the registry lookup.
  - `send_wrapper` replaces gh-toolkit's `unsafe` `SendFut`, so the crate has `#![forbid(unsafe_code)]`.
  - No gh-toolkit dependency.
- **Tests:** 140 pass, counting all features. `tests/golden_shapes.rs` round-trips the GM-00 fixtures through the moved types.
- **Departures from gh-toolkit:**
  - unrecognized 422 codes sort last;
  - rate-limit wording past 64 KiB of body is not read;
  - no quota preflight;
  - the repository-admin guard is always on;
  - three stub-contract fixture cases were re-pointed from the dropped `ListUserInstallations`.
- **Left for GM-05:** the Miniflare `transport.mjs`/`deadlines.mjs` cases still exercise gh-toolkit-worker. The `github_profiles.rs` profile test moves in GM-02.

**GM-07: leftover modules.**
- **Governor:** now `ghinvite-workflows/src/invite_budget/ledger.rs`, with every serde attribute unchanged; the golden tests pass with their fixtures unchanged. One moved test lost its gh-toolkit `ApiError` half, which gh-toolkit's own tests cover.
- **Address:** now in `ghinvite-email-vault`, which no longer depends on gh-toolkit. Until GM-04 it is re-parsed into gh-toolkit's type at the `create` call site.
- **Scrubbing:** `octoscrub::scrub` replaces `redact::scrub`.
- **OAuth codes:** `octologin::OAuthErrorCode` backs `web/error.rs`'s `OAuthErrorCode`.
- **Clock:** `ghinvite_core::clock::{Clock, SystemClock}`. The remaining gh-toolkit clock uses are the ones passed into `Broker::new` (GM-02).

**GM-08: typed sign-in on octologin.**
- **Config:** `ghinvite-config` gives a `SignInApp` (client ID, secret, redirect, optional base URLs; `Debug` hides the secret). `SignInService::new(&SignInApp)` builds `octologin::SignIn`.
- **Tests:** they moved to wiremock. `golden_pending_signin` passes unchanged.
- **Behaviour changes:**
  - A callback `error` other than `access_denied` now shows the provider-error page, not "declined".
  - More than 100 email addresses gives `DomainUnavailable`, without reading page 2; the token is still revoked.
  - `GH_SIGNIN_CLIENT_SECRET` must be printable ASCII of at most 1 KiB.
- Legacy OAuth is untouched.

**Two toolkit fixes found by GM-08** (`implementation/octoauth`):
1. **`SignIn::exchange`'s future wasn't `Send` natively.** It held a form serializer across an await. Fixed; `tests/send_futures.rs` checks every request future at compile time.
2. **octoauth and octologin didn't link into a Worker.** Their wasm32 reqwest `stream` feature pulls in `wasm-streams` 0.4, which collides at link time with the 0.5 that `worker` 0.8 uses. On wasm32 both now read the body whole and then apply the size bound, after the `Content-Length` precheck; the buffer is zeroized. `octoauth/tests/wasm_dependencies.rs` guards against the dependency coming back.
   - **Limitation:** on Workers, a chunked response without `Content-Length` is buffered fully before it is refused. It is bounded by the Worker's memory, and only reachable from something impersonating GitHub.
   - With this fix, GM-07's interim local copy of the OAuth codes was replaced by octologin during the merge.

### GM-02 (2026-10-20)

`GithubApps` and the scope profiles are in `ghinvite-github`. No call site uses them yet.

- **Checks:**
  - `cargo test --workspace`: 1277 passed, 0 failed.
  - `ghinvite-github` with all features: 161 passed.
  - clippy, fmt and the wasm32 builds pass.
  - The GM-00 golden tests are unchanged.
- **API:**
  - `GithubApps::new(env, apps)`, or `with_sources(env, (InstalledApp, Arc<dyn TokenSource>)…)`.
  - `apps.app(AppSlot::Repo | Org)` returns `Result<GithubApp, AppUnavailable>`.
  - `GithubApp` has these operations:
    - `read`: retries once on a 401, through `replace`.
    - `write_once`: never replays; on a 401 it replaces the token before returning the error.
    - `ensure` and `refresh`.
    - `installation(id)` and `get_installation(id)`.
    - `app_rest()`: `Rest` with the app's JWT.
  - Closures receive an owned `InstallationRest`, which gives `rest()` and the credential's IDs, never the secret.
  - Errors are `TokenCallError<E> { Token(TokenError), Call(CallError<E>) }`.
- **Profiles:** `profiles::{REPO_READ, REPO_INVITE, ORG_READ, ORG_INVITE, CONSOLE_REPO_READ, CONSOLE_ORG_READ}` as `LazyLock<TokenScope>`. The endpoint↔profile coverage test is ported.
- **Config:** `WebApps.installed` and `MintApps.installed` hold `Vec<InstalledApp>` (one `octoauth::App` per slot). The signing key is the first `kid:` entry and the client ID is the JWT issuer. Errors name the variable, never its value. A second test key was added: `test-fixtures/github-app-rsa-2.pem`.
- **`Send` and Workers:** on wasm32 the providers' futures are boxed in `SendWrapper`, and a compiled-in check covers every operation on both targets.
  - Natively, the context owns one `AppTokenProvider` per app.
  - On Workers, providers live in an isolate-wide `thread_local`, keyed by slot and App ID. The first `App` built for a key wins for the isolate's life; a key rotation means a new deploy, and so new isolates.
- **Test support** (`test-mock`): `mocks::FixedTokens` (tokens `ghs_fixed_{app}_{inst}_{gen}`, call counters, scripted failures), `mocks::app(id, base)`, and `GithubApps::for_tests(&MockTransport, …)`.

**Notes for GM-03…GM-05:**
- **Error mapping:**
  - `BrokerError::Mint` becomes `TokenCallError::Token`; `BrokerError::Unauthorized` becomes `Call(Api(Unauthorized))`.
  - Retry a token error only when `TokenError::is_transient()`, which includes rate limits.
- **Test scripts:** drop the token-creation expectations from `MockTransport` scripts, and require `tokens.bearer(inst, gen)` instead.
- **`get_installation` doesn't go through `HttpTransport`.** It uses octoauth's own client, so `MockTransport` never sees it. Either use the stub or wiremock, or keep `endpoints::get_installation(&app.app_rest()?.rest(), id)` on the mock. The GM-00 note on `GhAppInstallation`'s journaled shape still applies.
- **`refresh` is per scope.** gh-toolkit's `invalidate_installation` covered every scope of the installation, so `refresh_installation_credentials` must refresh each scope it uses.
- **Read closures must have no side effects.** GM-05 moves `reserve()` out of them.
- **Token-creation base URL.** It is fixed when the `octoauth::App` is built, so stub-backed tests and runtime fixtures build apps with `parse_installed_github_apps` and the stub's URL.
- **Interim cost on Workers.** Until GM-10, Workers that parse config per request parse each private key twice: once for gh-toolkit, once for octoauth.

### GM-03…GM-06 (2026-10-20)

These ran in two parallel rounds on separate branches.
- **Round 1:** a shared prep commit (`RepositoryGithub.apps`, plus the new `HandlerError` variants `GithubApi`/`Token`), then GM-03 and GM-04.
- **Round 2:** GM-04b, GM-05 and GM-06. GM-04b removes the leftover scaffolding from the workflows crates.

The GM-05 and GM-06 agents were interrupted once. GM-05's work-in-progress was committed, rebased onto the new `main` and finished; GM-06 was restarted.

**State of merged `main`:**
- `cargo test --workspace`: 1295 passed, 0 failed, 9 ignored. All ten golden tests pass with their fixtures unchanged.
- Both clippy runs (`-D warnings`, including the integration lint) and `fmt --check` pass.
- The integration tests compile (`--no-run`), and the wasm32 builds pass: web-worker (default and `runtime-tests`), workflows-worker, storage-d1.
- GM-05 ran these Miniflare suites on workerd, all passing:
  - broker, transport, deadlines, operational-signals, smoke;
  - pending-signin, domain-signin, signin-cutover;
  - decision-queue, request-history, admission-telemetry, continuation-rotation;
  - email-vault, email-review, email-maintenance.
- **Not run:** the Restate integration tests and the admission, installation and journey Miniflare suites. They need `restate-server` or docker, which isn't available here.

**GM-03 (repo app) and GM-04 (org app, email, availability):**
- **Workflow code:** every workflow GitHub call goes through `RepositoryGithub.apps`. `with_token` became `read`, `call` became `write_once`, and installation reads go through `endpoints::get_installation(&app.app_rest()?.rest(), id)`. `verify_onboard_identity` therefore still journals the same `GhAppInstallation` shape.
- **Behaviour kept:** the outcome tables, `NotSent` releases and the 401 generation loop are unchanged.
- **Settlement and reconcile reads** keep their no-replay behaviour (`write_once` on `REPO_READ`).
- **Dead code:** the withdrawal path is deleted.
- **Credential refresh:** `refresh_installation_credentials` refreshes both of the app's scopes (write, then read). It fails and retries only on `octoauth::Error::Transient`, as before. It can create up to four tokens where gh-toolkit created one.

**GM-04b (workflows cleanup):**
- `RepositoryGithub` is now just `apps`, built by `new(transport, api_base, installed)` or `with_apps(apps)`.
- `HandlerError` lost its gh-toolkit variants, and `rate_limit()` returns a local `RateLimit`. `github_profiles.rs` is deleted.
- The `integration` feature and ghinvite-e2e use `ghinvite-github`'s stub, which matches gh-toolkit's routes and scripted outcomes.
- gh-toolkit is gone from the workflows binaries and ghinvite-e2e.

**GM-05 (web, Workers):**
- **`AppState`:** holds `github_apps` (a `WebGithub`). A separate gh-toolkit transport stays only for legacy OAuth.
- **Console reads:** they use `read` with the console scopes. `reserve()` now runs once before `read`, never inside the retried closure. Effects:
  - a 401 no longer spends a second read unit;
  - a failed token creation now spends one unit;
  - a busy limiter now stops a read before any token is created.
- **Binaries:** web-server builds `GithubApps` once; web-worker builds it per request, sharing token providers across the isolate.
- **Runtime fixtures:** `broker_fixture.rs` and `broker.mjs` now accept 1–20 creations for 20 concurrent misses (PD-4), then expect a cache hit.
- **Scripts:** `scripts/test-restate.sh` and `verify-native-d1-cutover.sh` build `ghinvite-github`'s stub.

**GM-06 (webhooks, PD-8):**
- **Verification:** one `octoevents::Verifier` per installed app, with all configured secrets chained by `also`, through `octoevents::authenticate`. The secrets come from a ghinvite-owned `WebhookSecrets`/`InstalledWebhooks` in `ghinvite-config`.
- **Route unchanged:** statuses, the 2 MiB cap, header pre-checks, 202 answers, the signature-failure events, and the `Sha256(body)` dedupe keys. The golden tests pass.
- **Decoders:** the installation decoders moved to `ghinvite-web/src/webhook_events.rs`.
- **Confirmed deletion (PD-8):** before uninstalling, `installation.deleted` reads the installation with the app JWT.
  - 404 confirms, and the uninstall proceeds.
  - A successful read means "not confirmed": no uninstall, and the dedupe receipt is kept.
  - A transient error is retried.
- **Replay-safe:** the check reuses the old `uninstall_time` step name. An old entry (a bare number) decodes as an unchecked, confirmed deletion, and new entries are JSON objects, so no drain is needed. A unit test decodes the old format.
- **Installation owner:** it marks itself uninstalled only when the deletion was confirmed.
- **Known edge:** if GitHub's API lags a real deletion, that delivery is ignored and its receipt blocks the same payload for 7 days. The installation still shows as unavailable on its next check.

**What still uses gh-toolkit** (removed by GM-09 and then GM-10):
- **Legacy OAuth:** `web/routes/oauth.rs`, `web/config.rs` (`OAuthConfig`), `web/state.rs` (`legacy_oauth_transport`), `web/error.rs` (`WebError::Github`), both web binaries (the transport), `tests/common/sign_in.rs`, `tests/oauth_flow.rs`, and the workflows integration tests that build web state with legacy sign-in.
- **The old `AppRegistry`:** `ghinvite-config/src/apps.rs`, `web/installed_apps.rs` and some test helpers.
- **Lockfile:** gh-toolkit-worker keeps octoevents 0.3 in `Cargo.lock`, because it turns gh-toolkit's default features back on.

### GM-09: remove legacy OAuth (2026-10-20, PD-7 decided by the user)

Commits `f4aa260` and `5000693`.

- **Removed:** the legacy branches of `/login` and `/oauth/callback`, `OAuthConfig`, the `GH_SIGNIN_ENABLED` switch, the `oauth_csrf` and `access_token` session fields, `WebError::Github`, the gh-toolkit transport kept for legacy OAuth, and the `legacy-p1-oauth` feature. Typed sign-in on `octologin` is the only sign-in path.
- **What deploys must change:**
  - **Set** `GH_SIGNIN_CLIENT_ID` and `GH_SIGNIN_CLIENT_SECRET`. Production startup fails without them. The sign-in app's callback URL is `https://{P1_HOST}/oauth/callback`.
  - **Delete** `GHINVITE_GITHUB_CLIENT_ID`, `GHINVITE_GITHUB_CLIENT_SECRET` and `GH_SIGNIN_ENABLED`; nothing reads them.
  - `wrangler/web.toml`, `docs/deploy.md`, the READMEs and `config.env.example` are updated.
- **Sessions:**
  - Sessions signed in through legacy OAuth stay signed in: same identity, CSRF token and lifetime.
  - Unknown fields are ignored on load, and the stored user token is dropped at the session's next write.
  - Sign-ins in flight during the deploy fail once and must restart.
  - Tokens that legacy OAuth issued are not revoked automatically; `docs/deploy.md` says how to revoke them on GitHub.
- **Tests:**
  - The ~28 web test files sign in through the typed flow: the test app relays the sign-in requests to the test's scripted GitHub.
  - `oauth_flow.rs` follows the typed rules: a bad callback consumes the pending sign-in, and a failed exchange is a 500 that still revokes the token.
  - Four legacy-only tests were dropped.
- **Checks:** 1296 tests passed. The Miniflare sign-in suites and 14 others pass on workerd, and so do 12 Playwright configs.

### GM-10: remove gh-toolkit (2026-10-20)

- **Config:** `ghinvite-config` owns the app configuration. It reads the same environment variables with the same validation rules, and produces what the code uses: `InstalledApp` (octoauth Apps), `SignInApp` and `InstalledWebhooks`, plus the slot lookup for webhook routing. The gh-toolkit `AppRegistry` is gone.
- **Dependencies:** `gh-toolkit` and `gh-toolkit-worker` are removed from every manifest, and `cargo tree` shows neither. `implementation/gh-toolkit` is deleted, along with its CI workflow (`.github/workflows/l2.yml`).
- **Scripts:** the toolkit scripts are deleted (`check-toolkit-boundary.{sh,py}`, `check-toolkit-error-codes.py`, `test-toolkit-*.py`, `verify-toolkit-features.sh`), together with their CI job in `.github/workflows/p1.yml`.
- **Boundary check:** the rule that mattered is now a test, `ghinvite-github/tests/dependency_boundary.rs`. It checks that the GitHub client depends on no product crate, and that the workspace resolves neither gh-toolkit nor the rotating-credential crates.
- **Docs:** current-state docs no longer describe gh-toolkit as in use. Dated verification records are left as history.
- **Checks on `main`:**
  - `cargo test --workspace`: 1301 passed, 0 failed. All ten golden tests pass with their fixtures unchanged.
  - Both clippy runs with `-D warnings`, fmt, and `cargo test -p ghinvite-workflows --features integration --no-run`.
  - The wasm32 builds: ui, island, storage-d1, web-worker (default and `runtime-tests`), workflows-worker.

**Not verified locally across phase D:** the Restate integration suites, and the Miniflare and Playwright suites that need `restate-server` or Docker. Run them in CI before the first deploy of this migration.
