# 0014. gh-toolkit2 is client-agnostic GitHub App auth; webhooks are octoevents

- **Status:** Accepted (decided by the user in conversation)
- **Date:** 2026-10-20

## Context

- `implementation/gh-toolkit` (the L2 toolkit built inside P1) is about 27k lines of Rust plus about 6 MB of API snapshots and extraction tooling. Its only consumer is `implementation/ghinvite`.
- `implementation/gh-toolkit2` is the rewrite, about 5k lines. It has two crates:
  - `gh-toolkit2`: App JWT, installation token creation, typed `Permissions`, IDs, an error taxonomy, reqwest with an optional `reqwest-middleware`, and wasm32 support.
  - `gh-token-provider`: the `InstallationTokenProvider` trait and the cached `AppTokenProvider`.
- Consumers attach tokens to their own requests through `expose_secret()` (ADR 0003, amendment of 2026-10-20). ghinvite will eventually move to gh-toolkit2.
- **The goal:** a set of crates that help build GitHub Apps, much like Probot but with less syntactic sugar.
- **octocrab 0.54.5** (released 2026-10-17) was evaluated against its source at tag `v0.54.5`.
  - **What it offers:**
    - It covers every endpoint ghinvite uses: org invitations, memberships, teams, collaborators, repo invitations, installations, users, and the OAuth web and device flows.
    - With `default-features = false` and `jwt-rust-crypto` it compiles for `wasm32-unknown-unknown`. On Workers it needs a custom `tower::Service` over `worker::Fetch` plus a `spawn_local` executor. That compiles; running it on a real Worker is UNVERIFIED.
  - **Hazards for a toolkit that wraps it:**
    - `Error::GitHub` carries GitHub's `message` (or the raw body) and every response header, and `Debug` prints them (`src/error.rs`, `src/lib.rs` L438-497).
    - The default `RetryConfig::Simple(3)` retries 5xx, 429 and transport errors immediately, for every method including POST (`src/service/middleware/retry.rs`).
    - Response bodies have no size limit.
    - Pagination does not check that next links stay on GitHub's host. Tokens are attached only for `api.github.com` (`src/lib.rs` L2527-2543).
    - Several types print secrets: `BasicAuth` `Debug` prints the password, `CachedToken` `Display` prints the token, and `models::InstallationToken` `Debug` prints the token.
    - Calling `.user_access_token()` on a client built with `.personal_token()` sends two `Authorization` headers.
    - About 25 releases in the last year, breaking minor versions every month or two, and in practice one active maintainer.
- **octoevents 0.5** (`sagikazarmark/octoevents`, already the base of gh-toolkit's `WebhookRouter`) covers:
  - signature verification with secret rotation (`Verifier::also`);
  - several apps behind one URL (`VerifierSource`);
  - a body limit and a content-type check;
  - redacted secrets and signatures;
  - a dispatcher;
  - octocrab models behind a feature;
  - wasm32.
- Using octocrab, or any client, happens in consumer code. A consumer only needs a token from the provider.
- GitHub Enterprise Server does not matter for now.

## Decision

1. **gh-toolkit2 does not wrap or depend on any GitHub API client.** It provides GitHub App authentication and installation tokens. Consumers choose their own client (octocrab, reqwest, anything else) and attach the token themselves.
2. **In scope now:**
   - `gh-toolkit2` (core): App JWT, installation tokens, permissions, IDs. Add a public response classifier that works on response parts (status, headers, optional message), so any client can reuse the rate-limit evidence rules and the reasons for a 403 (SSO, IP allow list, outside collaborators). Add compile-time tests that secret types are not `Serialize` or `Display`.
   - `gh-token-provider`: as it is. It MAY gain a client-agnostic retry-on-401 helper: run a closure; on a 401, call `replace` and run it once more.
   - Documentation and an example showing an octocrab client with a gh-toolkit2 token. They cover:
     - build the base client without auth and derive a client per token;
     - use `with_service` to avoid the default retries;
     - convert errors before logging or journaling.
3. **Later, when a consumer needs them:**
   - `gh-oauth`: user sign-in with PKCE and constant-time state.
   - `gh-redact`: a `scrub` filter for log layers.
4. **Webhooks use octoevents directly.** There is no `gh-webhook` crate.
5. **Deferred until a second consumer duplicates the code:**
   - an octocrab adapter: a hardened service stack, a `call` wrapper that maps errors, and `From<octocrab::Error>`;
   - a Workers transport for octocrab (a `tower::Service` on `worker::Fetch` with manual redirects and a body cap);
   - octoevents glue: a `VerifierSource` built from app config, and a handler adapter that provides an installation client.
6. **Not ported from gh-toolkit:**
   - the broker closure shape and `InstallationClient`;
   - typed endpoints, `Link` pagination, the ETag store, GraphQL, `Observations` and quota preflight;
   - `legacy_oauth`, the extended webhook decoders, `wasm_send`;
   - the extraction and public-API tooling.

   The following move to ghinvite when it migrates: the write governor (`WriteLedger`), the GitHub stub, the email-invitation and avatar helpers, the edge-case checks of the endpoint helpers, its octocrab and Workers transport setup, delivery dedupe, and confirming the event kind before destructive actions.
7. **GitHub Enterprise Server is out of scope** for now.

## Consequences

- **ADR 0003 decision 1 binds consumers' GitHub calls.** gh-toolkit2 keeps upstream text out of its own errors only. A consumer using octocrab MUST convert `octocrab::Error` before it reaches a log, a Restate journal or an octoevents `DispatchError`. The octoevents failed-delivery event renders the error's source chain. The main exposure is personal data, such as an invitee's email in a 422 message, and credentials reflected back by a proxy.
- **The 401 → one-retry acceptance check of ADR 0003** (amended decision 4) is tested in consumer code, or once in the helper if it is added.
- **Probot-like glue is not provided.** That is the pre-authenticated client per installation, and event handlers that receive it. gh-toolkit2 is "GitHub App auth and tokens" until a second app shows the glue is worth extracting.
- **On Workers a consumer may run two HTTP stacks:** reqwest's fetch backend for token creation and its own transport for API calls. reqwest's fetch backend follows redirects (documented in `gh-toolkit2` `lib.rs`); a consumer transport can refuse them.
- **The L2 spec's non-goal "wrapping `octocrab` is rejected"** (`specs/components/L2-github-app-toolkit.md` §2) no longer applies: the toolkit wraps no client. Most of the L2 spec describes gh-toolkit. Delta D-22 records the scope change for gh-toolkit2.
- **gh-toolkit stays in place** until ghinvite migrates. It is then deleted, not extracted (this departs from ADR 0005's "extract later" for L2).

## Amendment notes (2026-10-20, naming)

These don't change the decision.

- **Crate names:**
  - `gh-toolkit2` → `octoauth`
  - `gh-token-provider` → `octotoken`
  - `gh-redact` → `octoscrub`
  - `gh-oauth` → `octologin`
- **Workspace:** `implementation/octoauth`, moving to its own repository later.
- The client-agnostic retry-on-401 helper (decision 2) is built as `octotoken::with_token`.

See `plans/octoauth-plan.md` §1.1 and §7.

## Amendment notes (2026-10-20, ghinvite migration plan)

These refine decisions 5 and 6 for the first consumer. They don't change the decision; they apply it.

- **ghinvite keeps its own GitHub client.** Decision 6 assumed ghinvite would move to octocrab. ghinvite instead moves gh-toolkit's transport, REST layer and endpoints into its own crate `ghinvite-github` (`plans/ghinvite-toolkit-migration.md` PD-1). Its write fencing needs the transport's "not sent" vs "no answer" distinction, which octocrab cannot give, and its tests depend on the transport seam.
  - Consequence for decision 5: the deferred octocrab adapter and Workers transport have no consumer yet.
- **No pluggable sender for the toolkit's own requests.** The Workers redirect limitation is accepted (PD-2).
- **App-authenticated reads are toolkit scope.** octoauth gains `Installation::get()` and a secret `AppCredential` for app-level endpoints it does not wrap (task G2-06). Both are GitHub App authentication, like token creation.

## Amendment notes (2026-10-20, migration complete)

- ghinvite no longer uses gh-toolkit, and `implementation/gh-toolkit` is deleted. That closes the consequence "gh-toolkit stays in place until ghinvite migrates", ahead of SP-19/SP-19a, by the user's decision PD-7. Legacy OAuth is removed, and typed sign-in on `octologin` is the only sign-in path. Details are in `plans/ghinvite-toolkit-migration.md` §7.

## Amendment notes (2026-11-25, octocrab adapter and app-level retry)

These apply decision 5; they don't change the decision.

- **Trigger:** the toolkit's example GitHub App (`libs/octoauth/examples/installations`, a CLI listing the App's installations and their repositories, tested against octoemu) copied the octocrab error conversion of `crates/octotoken/examples/octocrab.rs` word for word. That is the duplication decision 5 waited for, though from an example, not a second product.
- **`octoauth-octocrab`** (new crate in the toolkit workspace) is the first part of decision 5's deferred octocrab adapter:
  - `builder(&App)`: octocrab's builder for the App's API base, with bounded connect and read times and no auth; `authenticated` derives a client per credential;
  - `Error`: `octocrab::Error` reduced to `ApiError` or a fixed `gh:octocrab:client`, without upstream text (the conversion this ADR's consequences require);
  - `is_unauthorized` and `unauthorized`: the 401 signals for `with_token` and `App::with_credential`.
- **A separate crate, not an octoauth feature,** so octocrab's breaking minor releases change only the adapter's version. octoauth still depends on no client (decision 1).
- **Still deferred:** the hardened service stack (body cap, refusing redirects in a custom service), the Workers transport for octocrab, and the octoevents glue. The adapter documents leaving octocrab's `retry` and `follow-redirect` features off; Cargo feature unification means it cannot enforce that. It is native-only.
- **`App::with_credential`** (octoauth) gives requests to app-level endpoints the crate does not wrap the same clock-skew correction as its own requests: one retry after a 401 that GitHub's `Date` explains. Before, a request built from `App::credential` failed for good on a skewed host clock unless the caller first made an octoauth call to re-learn the skew.
- **`WithTokenError`** (octotoken) is no longer `#[non_exhaustive]` and gains `map` and `into_error`; the new `WithCredentialError` has the same shape.

## Evidence

- `implementation/gh-toolkit` (inventory of modules and consumers, 2026-10-20 session)
- `implementation/octoauth/crates/octoauth/src/lib.rs`, `crates/octotoken/src/lib.rs` (paths after the rename; formerly `implementation/gh-toolkit2`)
- `decisions/0003-installation-tokens-never-journaled.md` (amendment of 2026-10-20)
- octocrab `v0.54.5`: `src/lib.rs`, `src/error.rs`, `src/service/middleware/retry.rs`, `src/service/middleware/auth_header.rs`, `src/page.rs`, `src/models/webhook_events/verification.rs`
- octoevents 0.5.0: `src/lib.rs`, `CONTEXT.md`, `docs/security.md` (event name, delivery ID and target headers are not signed)
- `specs/components/L2-github-app-toolkit.md` §2, §17 (D-21)
