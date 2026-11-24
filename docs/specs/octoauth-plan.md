# octoauth (gh-toolkit2): build plan

| Field | Value |
|---|---|
| Project | octoauth, the rewrite of the L2 GitHub App toolkit (working name gh-toolkit2) |
| Status | 2026-10-20. Phases A–D done. ghinvite runs on the octoauth crates and gh-toolkit is deleted (`plans/ghinvite-toolkit-migration.md`) |
| Decisions | ADR 0003 (amended 2026-10-20), ADR 0014 (scope: client-agnostic auth and tokens; webhooks are octoevents) |
| Baseline | `implementation/gh-toolkit2` at `124dbd5` (now `implementation/octoauth`): crates `gh-toolkit2` (App JWT, installation tokens, `Permissions`, IDs, errors) and `gh-token-provider` (`InstallationTokenProvider`, `AppTokenProvider`) |
| Reference | `implementation/gh-toolkit` (the old toolkit; source to port from, then delete) |
| Spec | `specs/components/L2-github-app-toolkit.md` describes gh-toolkit; deltas D-21 and D-22 record what this rewrite changes. This plan does not rewrite the spec |

**Conventions.**
- Task IDs: `G2-nn` (core and provider work), `GL-nn` (later crates), `GM-nn` (ghinvite migration).
- One task is one agent session and one PR. A task that grows past that is split before it starts.
- Paths starting with `gh-toolkit/` are under `implementation/gh-toolkit/crates/gh-toolkit/`. Paths starting with `gh-toolkit2/` are under `implementation/gh-toolkit2/crates/gh-toolkit2/`, which becomes `implementation/octoauth/crates/octoauth/` after G2-00.
- **UNVERIFIED** marks a claim no source or test backs yet.

---

## 1. Goal and scope

The project helps build GitHub Apps, much like Probot but with less syntactic sugar. Per ADR 0014 it is **GitHub App authentication and installation tokens**, and it depends on no GitHub API client. Consumers pick their own client (octocrab, reqwest, anything else), get a token from a provider, and attach it themselves. Webhooks use octoevents directly.

### 1.1 Names

The crates join the `octo*` family next to `octoevents`, `octoslash` and `octodelivery`. None of the names below was registered on crates.io on 2026-10-20 (`octoapp` and `octokey` were taken). The workspace stays in `implementation/` for now and moves to its own repository later.

| Crate | Working name | State | Scope |
|---|---|---|---|
| `octoauth` | `gh-toolkit2` | Exists | App JWT, installation tokens, `Permissions`, IDs, errors, and the public `ApiError` classifier (G2-01) |
| `octotoken` | `gh-token-provider` | Exists | `InstallationTokenProvider`, `AppTokenProvider`, and the `with_token` retry helper (G2-05) |
| `octoscrub` | `gh-redact` | Later (GL-01) | `scrub` for application log layers |
| `octologin` | `gh-oauth` | Later (GL-02) | User sign-in: web flow with state and PKCE, user API |

Workspace directory: `implementation/octoauth`.

### 1.2 Out of scope (ADR 0014)

- a GitHub API client or wrapper;
- typed endpoints and pagination;
- the governor;
- ETag caching, GraphQL, quota observation;
- a webhook crate;
- GitHub Enterprise Server;
- the shared encrypted `TokenStore` (ADR 0003 decision 3, amended). That comes later, when a second service needs shared tokens.

## 2. Phases

| Phase | Tasks | Gate to the next phase |
|---|---|---|
| A. Core hardening | G2-00 … G2-04 | Renamed; classifier public; secret-type and disclosure tests green on native and wasm32; octocrab example builds |
| B. Retry helper | G2-05 | ADR 0003 acceptance check passes as a unit test |
| C. Crates ghinvite needs | GL-01, GL-02 | Both build for wasm32 and pass their ported tests |
| D. ghinvite migration (**done**) | G2-06, GM-00 … GM-10 (`plans/ghinvite-toolkit-migration.md`) | ghinvite's CI is green without `gh-toolkit`, and `implementation/gh-toolkit` is deleted |

G2-00 comes first. After it, phases A, B and C can run in parallel. Phase D needs all three.

## 3. Tasks

### 3.1 Phase A: core hardening

**G2-00. Rename.**
- **What:**
  - `implementation/gh-toolkit2` → `implementation/octoauth`;
  - crate `gh-toolkit2` → `octoauth`;
  - crate `gh-token-provider` → `octotoken`;
  - `DEFAULT_USER_AGENT` → `octoauth/<version>`;
  - `octotoken` re-exports `octoauth` (today `pub use gh_toolkit2`);
  - the docs.
- **Also update the references** in ADR 0003 (amendment), ADR 0014, the L2 spec D-21/D-22 and `plans/README.md`. Add "(formerly gh-toolkit2)" at first mention.
- **Done when:** `cargo test --workspace` and `cargo check --target wasm32-unknown-unknown` pass, and `rg 'gh.toolkit2|gh.token.provider'` finds no matches outside the history notes.

**G2-01. Public response classifier (`ApiError`).**
- **What:** a public type that classifies a GitHub error response from its parts and carries no upstream text. Any client can use it; token creation uses it too.
- **Design:**

  ```rust
  // octoauth
  pub enum ErrorBody<'a> {
      Bytes(&'a [u8]),                                                     // reqwest, raw body
      Parsed { message: Option<&'a str>, errors: &'a [serde_json::Value] }, // octocrab's GitHubError
      Empty,
  }

  #[non_exhaustive] #[derive(Clone, Debug, PartialEq, Eq)]
  pub enum ApiError {
      Unauthorized,
      Forbidden { reason: ForbiddenReason },
      NotFound,
      Conflict,
      Validation { codes: Vec<ValidationCode> },
      RateLimited { scope: RateLimitScope, retry_after: Option<Duration> },
      Server { status: u16 },
      Status { status: u16 },
  }

  impl ApiError {
      pub fn classify(status: http::StatusCode, headers: &http::HeaderMap, body: ErrorBody<'_>) -> Self;
      pub const fn code(&self) -> &'static str;   // Display prints this
      pub const fn is_unauthorized(&self) -> bool;
      pub const fn is_transient(&self) -> bool;
      pub fn suggested_wait(&self) -> Option<Duration>;
  }
  ```

- **Why it is shaped this way:**
  - `http` 1.x types are what reqwest 0.12 and octocrab 0.54 both expose. So it becomes a public dependency, and neither client needs adapting.
  - `ErrorBody::Parsed` exists because octocrab hands over `message` and `errors` already parsed, not the raw body. Neither caller has to re-serialize.
  - `ApiError` is separate from token creation's `Error`. A 404 or 422 from token creation means `InstallationGone` or `PermissionsNotGranted`, which aren't generic meanings. `Error` is built from `ApiError` inside `Installation::access_token`. `ForbiddenReason` and `RateLimitScope` are shared.
  - Classification order: rate-limit evidence first, then status.
  - Inspection is capped at 64 KiB of body and 32 `errors` entries.
  - Transport failures (not sent, no response) stay with the client and are not classified here.
- **Port from:**
  - `gh-toolkit2/src/http.rs` L199-261: rate-limit evidence, `retry-after` and reset measured against the response's own `Date`.
  - `gh-toolkit/src/error.rs` L574-676:
    - add `ForbiddenReason::OutsideCollaboratorsRestricted` (four exact templates);
    - match the IP allow list against its full template;
    - add `Spammed` and `BelowBasePermission` (the `Cannot assign X permission of Y` template) as `ValidationCode` values.
  - `gh-toolkit/src/redact.rs` L150-182: `ValidationCode` is an enum of GitHub's documented codes (`already_exists`, `missing`, `missing_field`, `invalid`, `custom`, `unprocessable`) plus `Unrecognized`. Codes are deduplicated, and recognized ones sort before `Unrecognized` ahead of the cap.
- **Not included:** an octocrab conversion. The ten-line bridge from `octocrab::Error::GitHub` lives in the G2-04 example.
- **Done when:**
  - every rate-limit case from `gh-toolkit/src/transport.rs` L796-1047 and every refusal case from the `gh-toolkit/src/error.rs` tests passes against `ApiError::classify`, through both `ErrorBody::Bytes` and `ErrorBody::Parsed`;
  - `Installation::access_token` and `is_suspended` go through it with no behaviour change (existing tests unchanged);
  - `cargo check --target wasm32-unknown-unknown` passes.

**G2-02. Compile-time secret-type tests.**
- **What:** prove at build time that:
  - `InstallationToken` is not `Serialize` or `Display`;
  - `SigningKey` is not `Clone`, `Serialize` or `Display`;
  - neither has a public field.
- **Port from:** `gh-toolkit/tests/ui/*` and `gh-toolkit/tests/secret_types.rs` (trybuild). Use `static_assertions::assert_not_impl_any!` where trybuild isn't needed.
- **Done when:** removing a guarantee (for example, adding `#[derive(Serialize)]`) fails the test suite.

**G2-03. Credential-disclosure tests for token creation.**
- **What:** extend `gh-toolkit2/src/installation/tests.rs` `credentials_never_leak_into_diagnostics` with the cases from `gh-toolkit/tests/credential_disclosure.rs` that apply to token creation:
  - a gateway that echoes the `Authorization` header in the body;
  - malformed success bodies containing a `ghs_` marker;
  - a 422 whose sub-codes must survive while the message must not.

  Run the same cases against `ApiError::classify`.
- **Done when:** no `Display`/`Debug` output of any error, and no captured `tracing` output, contains the marker or the reflected header.

**G2-04. Documentation and examples.**
- **What:**
  - `examples/octocrab.rs`: an octocrab client given an octoauth token. octocrab is a dev-dependency only. It includes the `octocrab::Error` → `ApiError` bridge.
  - `examples/reqwest.rs`: the same with plain reqwest.
  - A crate-level doc section "Using a token with a client".
- **The docs must state:**
  - Build the octocrab base client **without auth**, then derive a client per token (`.user_access_token(token)`). Configuring auth on the base client and then deriving sends two `Authorization` headers.
  - Avoid octocrab's default `RetryConfig::Simple(3)`, which retries POSTs immediately. Use `OctocrabBuilder::new_empty().with_service(..)` or disable retries.
  - octocrab buffers bodies with no size limit. Cap them in the service if responses are untrusted.
  - Convert `octocrab::Error` (its `Debug` carries GitHub's message and headers) to `ApiError` before it reaches a log, a Restate journal or an octoevents `DispatchError` (ADR 0003 decision 1; ADR 0014 consequences).
  - Use `octotoken::with_token` (G2-05) for the 401 retry.
- **Done when:** both examples build (`cargo build --examples`), and the octocrab example runs against a wiremock GitHub in a test.

### 3.2 Phase B: retry helper

**G2-05. `octotoken::with_token`.**
- **Why build it** (instead of documenting the manual pattern):
  - **The natural mistake is silent.** After a 401, calling `token()` again returns the same cached, rejected token. Only `replace(&rejected)` is correct, and nothing in the types prevents the mistake.
  - It is generic over the error type, so it needs no client dependency.
  - The ADR 0003 acceptance check (401 → one retry → success; 401 → retry → 401 is terminal) is tested once here, not in every app.
- **Shape:**

  ```rust
  pub async fn with_token<P, T, E, F, Fut>(
      provider: &P,
      installation: InstallationId,
      scope: &TokenScope,
      is_unauthorized: impl Fn(&E) -> bool,
      call: F,
  ) -> Result<T, WithTokenError<E>>
  where
      P: InstallationTokenProvider,
      F: FnMut(InstallationToken) -> Fut,
      Fut: Future<Output = Result<T, E>> + MaybeSend;

  #[non_exhaustive]
  pub enum WithTokenError<E> { Token(TokenError), Call(E) }
  ```

  How it runs:
  1. Get `token()` and run `call`.
  2. If `is_unauthorized(&err)`, call `replace(&token)` and run `call` once more.
  3. A second 401 is returned to the caller as `Call(err)`.

  It never sleeps and never retries anything else. With `ApiError` the predicate is `ApiError::is_unauthorized`.
- **Contract, stated in the docs:** a closure makes **one GitHub request**, or only requests that are safe to repeat. A 401 on request N re-runs requests 1…N-1. Inside Restate, call it inside the `ctx.run` that makes the request, and never return the token from it (ADR 0003 decision 1).
- **Done when:**
  - unit tests cover: 401 → `replace` → success; 401 → `replace` → 401 terminal; a non-401 error is not retried; a `TokenError` from `replace` comes back as `Token`;
  - concurrent callers that hit 401 on the same token converge on one replacement (an existing `replace` guarantee, exercised through the helper);
  - it builds on wasm32 (closure futures not `Send` there; `MaybeSend`).

### 3.3 Phase C: crates ghinvite needs

**GL-01. `octoscrub`.**
- **What:** a new crate with `scrub()`, the regex safety net for log layers.
- **Port from:** `gh-toolkit/src/redact.rs` L36-59, covering `gh[surop]_…`, `github_pat_`, JWT-shaped strings, `Bearer`/`Basic` values, and OAuth query and JSON parameters.
- **Do not port:** `upstream_diagnostic`. Errors keep no upstream text, so it isn't needed.
- **Done when:** `gh-toolkit/tests/redaction_backstop.rs` passes, including the idempotence and no-allocation cases. The crate builds for wasm32.

**GL-02. `octologin`.**
- **What:** user sign-in for GitHub Apps (ADR 0007).
  - `authorize()` with 32-byte state and a PKCE S256 verifier, and no `scope`.
  - `PendingAuthorization::verify` with constant-time comparison.
  - `exchange()`: secret form body, zeroized; an error in a 200 body is still an error; codes from an allowlist.
  - `get_user`, `get_user_emails` (verified only), `revoke_token`.
  - Errors classified with `octoauth::ApiError`.
- **HTTP:** plain reqwest, like the core (ADR 0014: no client dependency).
- **Port from:** `gh-toolkit/src/signin.rs`.
- **Do not port:**
  - `legacy_oauth.rs`;
  - the acting-grant and rotating-credential parts (`signin/acting.rs`, `signin/rotating.rs`); they come when P2 needs them (L1).
- **Done when:** `gh-toolkit/tests/signin_contract.rs` cases for sign-in pass, minus acting and rotating. It builds for wasm32.

### 3.4 Phase D: ghinvite migration

Planned in full in **`plans/ghinvite-toolkit-migration.md`**:
- **Decisions PD-1…PD-9.** PD-7 needs the user.
- **One toolkit prerequisite, G2-06.** octoauth gains `Installation::get()` and `App::credential()` for app-authenticated reads.
- **Eleven tasks, GM-00…GM-10.**

The outline that stood here changed in one important way. ghinvite does **not** adopt octocrab (PD-1). It keeps its own GitHub client, `ghinvite-github`, moved from gh-toolkit's transport, REST layer and endpoints, and only tokens, webhooks, sign-in and the small modules move to the new crates. The reasons:
- its write fencing needs the transport's "not sent" vs "no answer" distinction;
- its endpoint helpers carry dozens of edge cases;
- about 80 test files depend on `MockTransport` and the stub.

## 4. Deferred: extract when a second consumer needs it

Each of these starts in ghinvite (phase D). It moves into this workspace only when a second app needs the same code (ADR 0014 decision 5).

| Candidate | Starts in | Trigger to extract |
|---|---|---|
| Workers transport for octocrab | Nowhere yet (ghinvite keeps its own client, migration PD-1) | The first Workers app using octocrab |
| octocrab adapter: hardened service stack, `From<octocrab::Error>` for `ApiError` | Nowhere yet (ghinvite does not use octocrab) | Two apps writing the same bridge |
| octoevents glue: per-app `Verifier` from app config, handler adapter that provides an installation client | ghinvite GM-06 | Two apps with the same webhook wiring; this is where "Probot-like" would land |
| A pluggable sender for octoauth and octologin | Nowhere yet (migration PD-2) | A consumer that must route GitHub traffic through something reqwest cannot be (a service binding, an egress proxy) |
| Shared encrypted `TokenStore` for `octotoken` | Nowhere yet | Several services must share tokens, or measured token-creation volume calls for it (ADR 0003 decision 3) |
| Acting-grant OAuth and the L1 `Refresher` adapter | Nowhere yet | P2 M1 (L1) |

## 5. Decision: webhook event-type mix-ups stay out of octoevents

**The risk.** GitHub signs the body only. An attacker holding one captured, signed delivery can resend it under a different `X-GitHub-Event` header (`octoevents/docs/security.md` L18-38).

**What gh-toolkit did.** It rejected `installation` and `installation_repositories` payloads that carried any of 13 keys belonging to other event types (`gh-toolkit/src/webhook.rs` L333-370).

**Recommendation:** don't add that check to octoevents. Confirm through the API in the app instead.
- **The check is a heuristic.** A per-kind blocklist has to be maintained for every event kind against payloads GitHub keeps extending. It only proves the body doesn't look like a few other kinds; it never proves the header is right.
- **Only destructive actions need protection, and for those the API is authoritative.** For ghinvite that means `installation.deleted` and `installation.suspend`. `octoauth::Installation::is_suspended` already answers both: `Err(InstallationGone)` confirms a deletion, `Ok(true)` confirms a suspension. No new code is needed.
- **Scope of the attack.** It needs a signed delivery from the same app's webhook, for example from leaked logs. Routing a non-destructive event wrongly at worst triggers a refresh.
- **Today's ghinvite** starts the uninstall straight from the webhook (`ghinvite-workflows/src/availability.rs` L743), relying on gh-toolkit's check. Migration task GM-06 replaces that check with the API confirmation through `Installation::get()` (G2-06).

octoevents is unchanged, and its security guide already gives this advice.

## 6. Known limitations carried forward

- **Redirects on Workers.** reqwest's fetch backend follows redirects, so on Workers a redirected request has already been re-sent before it fails as `Redirected`. This applies to token creation (octoauth `lib.rs`) and the sign-in code exchange (octologin). It is accepted, with no pluggable sender (migration PD-2):
  - token creation's body holds no secret, and fetch drops `Authorization` across origins;
  - GitHub does not redirect the OAuth endpoints, so a redirect implies a compromised TLS connection.

  ghinvite's own API transport refuses redirects.
- **Two HTTP stacks on Workers** in consumers with their own client: reqwest for the toolkit's requests, the consumer's transport for API calls. In ghinvite that consumer transport is `WorkerFetchTransport` in `ghinvite-github`.

## 7. Decisions log

| # | Question | Decision (2026-10-20) |
|---|---|---|
| OQ-1 | Crate names and location | `octoauth`, `octotoken`, `octoscrub`, `octologin` (§1.1); stays in `implementation/octoauth` for now, own repository later |
| OQ-2 | Classifier shape | `ApiError::classify(StatusCode, &HeaderMap, ErrorBody)`, separate from token creation's `Error` (G2-01) |
| OQ-3 | Retry-on-401 helper | Build `octotoken::with_token` (G2-05) |
| OQ-4 | Event-type check in octoevents | No; confirm destructive actions through the API (§5, migration GM-06) |
| OQ-5 | Pluggable sender for octoauth/octologin | No; accept the Workers redirect limitation and revisit on a concrete need (migration PD-2) |
| OQ-6 | ghinvite's GitHub client | Its own `ghinvite-github`, moved from gh-toolkit; no octocrab (migration PD-1) |

## 8. Progress (as built)

Phases A and B are done in `implementation/octoauth`. Verification:
- `cargo test --workspace --all-features --all-targets` and the doc tests pass natively.
- `cargo clippy --workspace --all-targets --all-features`, `cargo doc` with `-D warnings` and `cargo fmt --check` are clean.
- `cargo check --workspace --target wasm32-unknown-unknown` passes, with and without `--all-features`.

The test suites themselves run natively only: wiremock and tokio's `net` feature don't build for wasm32. That relaxes the Phase A gate's "green on native and wasm32" to "libraries check on wasm32".

| Task | State | Departures from the task text |
|---|---|---|
| G2-00 | Done | The internal module `http` became `transport`, so the crate name `http` (now a public dependency) is unambiguous |
| G2-01 | Done | See the notes below the table |
| G2-02 | Done | `static_assertions` (`assert_not_impl_any!`) instead of trybuild. "No public field" is not tested: `InstallationToken`'s field is private by construction, and `TokenParts` has public fields by design. `TokenParts` is also asserted not `Clone`, `Serialize`, `Deserialize` or `Display`. Removing a guarantee was checked to fail the build |
| G2-03 | Done | The reflected-credential cases run against token creation (`installation/tests.rs`) and `ApiError::classify` (`api/tests.rs`). Neither crate emits `tracing` events, so no log capture is needed |
| G2-04 | Done | See the notes below the table |
| G2-05 | Done | No `MaybeSend` bound on the closure's future: auto traits flow through `async fn`. The native `tokio::spawn` test proves `Send`, and the library checks on wasm32. The concurrent test sleeps 1.1 s before rejecting the first token, because a replacement must expire later and expiry is per second |

**G2-01 notes:**
- **Spam is its own variant.** It is `ApiError::Spammed` rather than a `ValidationCode`, as in gh-toolkit, because GitHub can say `spammed` on a 403 as well as a 422.
- **Template refusals.** `ValidationCode` also has `OutsideCollaboratorsRestricted` and `CannotAssignBelowBase`. On a 422, the outside-collaborator template is a validation code; on a 403 it is a `ForbiddenReason`.
- **`Display` adds the status** for `Server` and `Status` (`gh:api:server:502`), like `Error::Unexpected`. `code()` stays fixed.
- **Only the first 32 `errors` entries are inspected.** Codes are deduplicated, with `Unrecognized` last.
- **Two intended behaviour changes in token creation:**
  - Error bodies over 64 KiB are no longer read for rate-limit wording. Before, any size was read.
  - A 403 carrying an outside-collaborator template is now `Forbidden { reason: OutsideCollaboratorsRestricted }`.

**G2-04 notes:**
- **Location.** The examples live in `octotoken`, the crate that sees both crates. Each is registered with `test = true` and tested against a wiremock GitHub.
- **octocrab as a dev-dependency.** It is native-only, with `default-client`, `rustls`, `rustls-ring` and `timeout` (`jwt-rust-crypto` was dropped on 2026-11-24: octotoken mints the tokens, and it pulled `jsonwebtoken` 10 with `rsa` 0.9). Leaving out `retry` and `follow-redirect` is how the example disables both.
- **The 401 test** checks that each request carries exactly one `Authorization` header.
- **Bridge caveat:** octocrab puts a non-JSON body's raw text in `message`. Such a body then names a reason, unlike in the `Bytes` path. The effect: a 403 with an exhausted quota and an HTML body is `Forbidden` through octocrab but `RateLimited` through `Bytes`.

**Review follow-ups applied:**
- Token creation's error mapping matches on `ApiError` variants instead of a second status switch.
- Two near-miss refusal texts were added to the tests, including the 422 variants.
- The workspace path dependency now carries a version.

### Phase C (2026-10-20)

Both crates are in the workspace, which now lists its members as `crates/*`. Both build for wasm32, and the same checks as above pass.

| Task | State | Departures from the task text |
|---|---|---|
| GL-01 `octoscrub` | Done | The patterns and tests are ported unchanged, and the crate docs list exactly what is scrubbed. `bounded_upstream_code` is not ported; the callback's OAuth `error` code is classified by `octologin::OAuthErrorCode::from_github` instead |
| GL-02 `octologin` | Done | See the notes below the table |

**GL-02 notes:**
- **API shape.** One `SignIn` per app, with a builder. Its methods replace the old free functions: `authorize`, `exchange`, `user`, `emails` and `revoke(token)` (`revoke` takes the token by value). `PendingAuthorization` keeps public fields so a store can rebuild it, as ghinvite does.
- **Emails.** `emails` returns verified addresses only, as the task asks. It reads one page of 100 and fails with `Incomplete` if there are more. gh-toolkit followed every page; here a partial list is an error, and no pagination code is needed.
- **Errors.** New variants:
  - `ForeignToken`: revoking another app's token, refused before sending.
  - `InvalidInput`: a malformed code or verifier, refused before sending.
  - `Redirected`, `Incomplete` and `Randomness`.
  - HTTP errors are `octoauth::ApiError`.
- **Own transport.** `src/transport.rs` partly duplicates octoauth's. It zeroizes request and response bodies, and grows the buffer by hand so a reallocation leaves no copy behind. Secret strings (form body, `Basic` and `Bearer` values) are allocated once at their final size. Error bodies over 64 KiB are not parsed for an OAuth `error`.
- **Tests.** The sign-in contract cases are ported: state, PKCE and no scope; redirect URI validation; the whole flow with header and form checks; provider errors in any status; malformed answers; input refused without sending; another app's token. New tests cover the email page limit, redirects, transport failures and identity checks. Compile-time secret-type assertions cover `UserToken`, `PendingAuthorization`, `AuthorizeUrl` and `SignIn`.

**Open items for phase D:**
- **Redirect re-sends on Workers.** On Workers, `fetch` follows a 307/308 from the token endpoint and re-sends the form body, client secret included, before octologin discards the answer as `Redirected`. octoauth's token creation has the same documented limitation. *Resolved:* accepted, with no pluggable sender (migration plan PD-2, OQ-5).
- **Duplicated code.** octologin repeats octoauth's validators (base URL, client ID, user agent) and parts of its transport. Extract them into octoauth, or a small internal crate, if a third copy appears.

**Also fixed in octoauth.** Its response buffer was a plain `Vec`, though a token response carries the installation token. It now uses octologin's zeroizing reader with manual growth.
