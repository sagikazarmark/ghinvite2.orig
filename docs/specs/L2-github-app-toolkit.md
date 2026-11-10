# Component spec: L2, the GitHub App toolkit

> **Superseded (2026-10-20).** The toolkit this spec describes, `implementation/gh-toolkit`, has been deleted (delta D-23). L2 is now:
> - the client-agnostic `octoauth`, `octotoken`, `octologin` and `octoscrub` crates (ADR 0014, `plans/octoauth-plan.md`);
> - ghinvite's own GitHub client, `ghinvite-github` (`plans/ghinvite-toolkit-migration.md`).
>
> This document is kept as the record of the original design.

- **Status:** draft, revision 2, 2026-10-15 (reviewer objections addressed; see the Review log at the end). Planning output for `PLAN.md` §9 planning-sequence step 3 ("L2 planning: extract from P1, check against P2's needs, switch P1 over"), written ahead of that step so P1's toolkit crate is built toward a known target.
- **What this document is** (ADR 0005): the **target design** of L2 plus the **extraction plan**. P1 is built first, with the toolkit in its own domain-free crate `gh-toolkit` inside the P1 workspace (`specs/projects/P1-github-invite.md` §10, cited as **P1 spec**; `plans/P1-build-plan.md` §6, cited as **P1 plan**). L2 is finalized and extracted after P1 M2, checked against P2. This spec therefore (a) defines the target API, (b) uses P1's names and lists every delta (§17), and (c) checks the API against P2's needs (§16).
- **Implements:** C1 (installation token broker) and C5 (GitHub App scaffolding) from `PLAN.md` §2 and §9 ("L2 GitHub App toolkit"); ADR 0003 (installation tokens never journaled; broker is a library), ADR 0005 (build first, extract later), ADR 0007 (sign-in app separate from the acting grant; **Proposed**), ADR 0008 (several apps per product; **Proposed**). It provides the GitHub user-token `Refresher` adapter for L1 (ADR 0002; `specs/components/L1-rotating-credential.md` §8.5, cited as **L1 spec**) and the optional sealed shared token cache that uses the anykms purpose registry (ADR 0001; `specs/components/anykms-local-keyring.md` §6.5, cited as **keyring spec**).
- **Risks:** R-C1-token-limits, R-C3-journal-secrets, R-C5-token-cap, R-WASM-crypto, R-ALL-org-policy, R-ALL-gh-terms, R-ALL-github-write-fence, R-ALL-restate-workers, R-P1-two-apps, R-P1-ratelimit, R-P1-uninstall, R-P2-discovery, R-P2-fanout-limits, R-P2-check-status, R-P2-merge, R-P2-rebase-as-user, R-C2-refresh (`analysis/risks.md`).
- **Spikes:** SP-14, SP-15, SP-16, SP-23 (`analysis/spikes-pending.md`); SP-19a, which is defined in P1 spec §16.1 (and listed in `analysis/spikes-pending.md` since 2026-10-16); P2: SP-03, SP-04, SP-05, SP-09, SP-10, SD-09 (`analysis/spikes-pending.md`). Proposed desk spikes `SD-L2-a`…`SD-L2-e` are defined in §18.2; they are **not yet** in `analysis/spikes-pending.md`.
- **Primary evidence:** the PoC `sagikazarmark/ghinvite2.orig` at `89a67fa`, crate `crates/ghinvite-github` (cited as **PoC `file`**, paths relative to that crate's `src/` unless they start with `crates/`); `analysis/poc-ghinvite.md` §6.3, §6.8, §7; `research/github-app-tokens.md` Q1–Q7 and "Design implications"; `research/dependaboard-prs.md` Q1–Q6; `research/github-invitations.md` §1–§10; `research/link-security-and-gh-terms.md` §2.3; `research/secrets-and-keys.md` §2; `analysis/feasibility.md` §2.2; `ideas/P2-dependaboard.md`; `ideas/P3-repo-manager.md`; `octoevents` 0.3.0 sources (`src/signature.rs`, `src/meta.rs`).

**Conventions.** Requirement keywords **MUST**, **MUST NOT**, **SHOULD**, **MAY** have their RFC 2119 meanings. Requirement IDs: `GA-nn` (app auth), `BR-nn` (broker), `TR-nn` (transport), `RL-nn` (rate limits and governor), `PG-nn` (pagination), `WH-nn` (webhooks), `OA-nn` (OAuth), `ER-nn` (errors), `OB-nn` (observability), `TS-nn` (tests), `EX-nn` (extraction). Acceptance checks are `AC-L2-nn`. `R1`–`R10` are the toolkit boundary rules of P1 plan §6. IDs are never reused. **UNVERIFIED** marks a claim no primary source or spike backs yet. **[Spec decision]** marks a choice this spec makes without an ADR; each is listed in §18.3 so the user can overrule it. Code blocks are Rust-like: **names are normative where they repeat a P1 name; signatures, trait bounds and enum shapes are normative; bodies and comments are indicative.** Every signature in this spec MUST compile on stable Rust at the MSRV (1.91, [Spec decision, SPD-6]); TS-02 holds the compile tests that pin the non-obvious ones.

---

## 1. Purpose and consumers

L2 is the one place where GitHub-App mechanics live: who we are (App JWT), what we may do (installation tokens with scope profiles), how we talk (transport, rate limits, pagination, redaction), what GitHub tells us (webhooks), and who the user is (OAuth). Products contain policy only.

| Consumer | Uses from L2 | Apps | Hosting | Evidence |
|---|---|---|---|---|
| **P1 GitHub Invite** (priority 1; first and shaping consumer) | App JWT for two acting apps; broker with four read/write scope profiles; REST endpoints for collaborators, invitations, org memberships, teams; exhaustive pagination; governor primitives for the Invite Budget; webhooks for `installation*`, `organization`, `membership`; sign-in OAuth with `/user`, `/user/emails`, token revoke | sign-in, repo, org (ADR 0007, 0008) | Workers: `p1-web` (read-only broker, webhooks, OAuth) and `p1-restate` (Restate endpoint) (P1 spec §20.1) | P1 spec §7, §10; P1 plan B0-02…B0-07, B1-01 |
| **P2 Dependaboard** (priority 2) | Everything P1 uses, plus: user-token REST calls (approve, merge, `@dependabot rebase` comment); GraphQL (search, `statusCheckRollup`, `mergeable`, merge-queue and auto-merge mutations); `merge-async` submit and poll; PR/check webhooks; per-installation **and** per-user write governor (80/min, 500/h); acting-grant OAuth and the L1 `Refresher` adapter; `/user/installations` for SSO reach | sign-in, viewer (read), merger (write + user grant) (`research/dependaboard-prs.md` "Design implications" 6; `analysis/feasibility.md` §2.2 change 5) | Workers (Dioxus SPA on Static Assets + Worker API + Restate endpoint) | §16 |
| **P3 Repo manager** (later; MVP is a CLI, `ideas/P3-repo-manager.md` P3-F06) | Native build; installation tokens or a user token supplied by the operator (e.g. from `gh auth token`); repo settings and rulesets endpoints, which include `PATCH /repos/{o}/{r}` | one app with Administration write | Native CLI first | `ideas/P3-repo-manager.md`; R-P3-api-coverage |

## 2. Scope and non-goals

**In scope (L2 v1 = the extracted crate).**
1. App identity: multi-app registry, RS256 App JWT with several keys and skew handling (§5).
2. Installation token broker per ADR 0003: library inside `ctx.run`, isolate cache, optional sealed shared cache, scope profiles, single-flight mint (native) and duplicate-tolerant mint (wasm32), one re-mint on 401 (§6).
3. Transport seam (native and wasm32), no automatic redirects, rate-limit classification, quota observation, governor primitives, redaction, opt-in conditional requests (§7, §8).
4. REST: typed endpoints with permission and retry metadata, a templated escape hatch, exhaustive `Link` pagination. GraphQL: transport, error and cost handling, exhaustive cursor pagination (§9, §10).
5. Webhooks: multi-app verification over `octoevents`, dedupe keys, typed payloads that validate their event kind (§11).
6. OAuth: web flow with state and PKCE for sign-in apps and acting grants, user API, token/grant revocation and check, the L1 `Refresher` adapter and `user_token_context` (§12).
7. Observability events with a fixed field contract (§13a).
8. A test kit: `MockTransport`, the GitHub stub server and its recorded contract, credential-disclosure and compile tests (§14).

**Non-goals.**
- **No Restate dependency** and no Restate types in the API (P1 plan §6 R2; ADR 0003 decision 2). L2 is used *inside* `ctx.run`; journaling rules are stated as contracts and enforced by type properties (no `Serialize` on secrets).
- **No storage.** No D1, KV, SQL or Durable Object code in the core crate (R2). Caches, permission snapshots, dedupe receipts and governor ledgers are traits or plain serde values the app persists. Workers-specific adapters live in a separate crate (§4).
- **No token vault.** Long-lived user tokens belong to L1 (ADR 0002). L2 never stores, refreshes on its own initiative, or caches user tokens (L1 OQ-10 stays with P2).
- **No product policy** (P1 plan §6 R7): scope-profile *constants*, role-precedence policy, availability state machines, Invite Budget buckets, error → outcome mapping, session handling, domain matching, merge sequencing all stay in apps.
- **No bulk-operations engine** (C8, deferred by ADR 0005 item 5).
- **No onboarding UI, no authorization model (C6), no audit (C7).**
- **No full GitHub API coverage.** Typed endpoints exist only for what a consumer uses; everything else goes through the generic call (§9.3). Wrapping `octocrab` is rejected: it is a native-first client and its wasm32 support is UNVERIFIED (`research/` has no evidence either way), and it would bypass the redaction and token rules. (Superseded for gh-toolkit2 by ADR 0014: the toolkit wraps no client; consumers choose one; see D-22.)
- **No in-library sleeping or retry loops on rate limits.** Waiting is the caller's job (Restate durable sleep or a native timer) (§8, RL-03).
- **No device flow, no OAuth-App (non-GitHub-App) flows, no GHES base-URL matrix** in v1. The base URL is configurable, so GHES is possible but untested (§18.3 OQ-7).

## 3. Design principles (normative)

- **GK-P1. Secrets never leave by value.** `InstallationToken`, `UserAccessToken`, `EphemeralUserToken`, private keys, client secrets and webhook secrets implement neither `Serialize` nor `Display` nor `Clone`; `Debug` prints `Type(..)`. There is no public accessor that returns the raw string. The only ways a credential reaches the wire are (a) a `Credential` writing the `authorization` header inside an L2 call, and (b) the crate-private `SecretBody` builder (§9.2, TR-19), which is the only place a token is serialized into a request body (P1 plan §6 R5; ADR 0003 decision 1).
- **GK-P2. No upstream text in errors.** Error values carry status, enumerated codes, numeric IDs and sizes only; never a response body, a GitHub `message`, a URL (from a `Link` or `Location` header or elsewhere), or a `reqwest` error string (PoC `redact.rs` module docs; ADR 0003 acceptance checks).
- **GK-P3. A partial observation is an error.** Pagination either walks to the end or fails (PoC `pagination.rs`; P1 spec §7.7). The same rule applies to GraphQL cursors, to search results above GitHub's 1,000-result cap (§10), and to listings GitHub marks as SSO-partial (PG-07).
- **GK-P4. Multi-app from the first line.** Every JWT, token, webhook and OAuth call is keyed by an app handle (P1 plan §6 R9).
- **GK-P5. Same API on native and wasm32.** Target differences are confined to the transport, the clock, the `Send` wrapper (TR-03) and the in-flight mint coalescing (BR-11, which is specified per target).
- **GK-P6. Classify, don't decide.** L2 reports what GitHub said (`RateLimited{retry_after}`, `PermissionsNotGranted`, `Ambiguous`); apps decide what it means for their workflow.

## 4. Crate layout and feature flags

L2 lives in its **own repository** after extraction (EX-03). Working names follow P1: the core crate stays **`gh-toolkit`**; the final name is OQ-1.

| Crate | Contents | May depend on | Targets |
|---|---|---|---|
| **`gh-toolkit`** (core) | Everything in §5–§13a except the Workers adapters | `serde`, `serde_json`, `thiserror`, `rsa`, `sha2`, `hmac`, `base64`, `url`, `percent-encoding`, `zeroize`, `futures-util` (native single-flight, `BoxFuture`), `http` (webhook `HeaderMap`), `bytes` (webhook body), `getrandom`, `parking_lot` or `std::sync`, `tracing`; optional `reqwest`, `octoevents`, `rotcred-core`, `anykms-recipes`, `axum` (test stub only) | native, wasm32 |
| **`gh-toolkit-worker`** (new at extraction) | `WorkerFetchTransport` (`worker::Fetch`, `redirect: manual`), `KvTokenStore` and `D1TokenStore` for the shared cache, a WebCrypto `JwtSigner` (only if SP-16 requires it, GA-06) | `gh-toolkit`, `worker` | wasm32 |

**TR-01.** `cargo tree -p gh-toolkit -e normal,build` (default features and every production feature in the table below) MUST NOT contain `restate-sdk`, `restate-*`, `worker`, `sqlx`, `dioxus`, `axum`, `tower-sessions`, `tokio` (wasm32 target), or any P1/P2 crate. This is P1 plan §6 R1–R2 carried into L2's own CI (EX-04). The native-only `test-stub` feature is an explicit test-harness exception for `axum`; production consumer trees MUST exclude it. `http` and `bytes` are allowed (they are what `octoevents` and `reqwest` already pull).

| Feature | Default | Adds | Who enables it |
|---|---|---|---|
| `reqwest` | on | `ReqwestTransport` (native and wasm32; on wasm32 reqwest dispatches to `fetch`, PoC `transport.rs`) | everyone |
| `webhooks` | on | §11 (`octoevents` 0.3, `sagikazarmark/octoevents`) | P1, P2 |
| `graphql` | off | §10.2 | P2 |
| `oauth` | on | §12.1–§12.3 | P1, P2 |
| `rotating-credential` | off | §12.4: `GithubUserTokenRefresher`, `user_token_context`, `credential_key` (depends on `rotcred-core`, `anykms-recipes` feature `context`) | P2 |
| `shared-cache` | off | §6.5 sealed second-level cache (depends on `anykms-recipes` feature `context`; storage via a trait) | only if BR-17 fires |
| `repo-admin` | off | Typed endpoints that can modify or delete a repository (`PATCH`/`DELETE /repos/{o}/{r}`, rulesets writes), and lifts the runtime deny-list of §9.3 (TR-18) | P3 only |
| `test-mock` | off | `MockTransport`, `FakeClock`, fixtures; every constructor it adds builds a `Broker` with an **isolated** cache and isolated `Observations` (BR-09, RL-02) | dev-dependencies of every consumer |
| `test-stub` | off | Stub GitHub server (axum, native only) and the recorded stub contract | consumer integration tests |

- The `repo-admin` gate replaces P1's "no variant at all" rule (P1 spec AC-SEC-3) at L2 level: L2 has the variants behind a feature P1 never enables, the generic call refuses repo-admin method/template pairs at runtime without it (TR-18), and P1's CI asserts `cargo tree -e features -i gh-toolkit` shows no `repo-admin` (delta D-12, §17).
- wasm32 randomness: `getrandom` with the `wasm_js` backend (and the 0.2 `js` feature while `rsa 0.9` pulls 0.2), configured as the PoC does (PoC `crates/ghinvite-github/Cargo.toml`; `spikes/wasm-crypto/README.md`; R-WASM-crypto).
- Every crate builds per crate, never `--workspace`, for wasm32 (as L1 W-01 and the PoC CI do).

### 4.1 Async traits and `Send` policy (both targets)

Restate's handler futures and anykms/L1 traits require `Send` (`research/secrets-and-keys.md` §2; L1 spec §5.2). L2 therefore keeps **`Send` bounds on both targets** and supplies one sanctioned wrapper for wasm32's `!Send` JS futures:

```rust
pub type BoxFut<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// gh_toolkit::wasm (cfg(target_arch = "wasm32") only). The PoC's WasmSendFut (PoC transport.rs
/// L276–298), exported. Sound because wasm32-unknown-unknown is single-threaded.
pub fn assert_send<F: Future>(f: F) -> SendFut<F>;   // SendFut<F>: Future<Output = F::Output> + Send
```

- **TR-03. `unsafe`.** `unsafe` appears only in `gh-toolkit/src/wasm_send.rs` (the `SendFut` impl). `gh-toolkit-worker` MUST NOT contain `unsafe`; it wraps `worker` futures with `gh_toolkit::wasm::assert_send`. CI greps both crates (P1 plan §6 R5).
- **TR-20. Object-safe traits.** Every trait taken as `Arc<dyn …>` is written by hand with `BoxFut` returns (`trait_variant::make(Send)` does not produce dyn-compatible traits; L1 spec §5.2 notes the same). Each such trait `XDyn` has a blanket `impl<T: X> XDyn for T`, so implementers write ordinary `async fn` against the non-dyn trait `X` (declared with `#[trait_variant::make(Send)]`). The pairs are: `HttpTransport`/`HttpTransportDyn`, `GrantedPermissions`/`GrantedPermissionsDyn`, `SharedTokenStore`/`SharedTokenCacheDyn` (via `SealedSharedCache`, §6.5), `EventSink` (sync, already object-safe), `Clock` (sync).

## 5. App identity and App JWT

### 5.1 App registry

```rust
/// Product-chosen handle, e.g. "signin", "repo", "org", "viewer", "merger". Used for routing,
/// logs and metrics; NOT a cache key (BR-09). [a-z0-9-]{1,32}.
pub struct AppSlot(&'static str);
pub struct AppId(u64);                 // numeric GitHub App ID: cache keys (BR-09), JWT iss fallback, cache AAD
pub struct ClientId(String);           // "Iv23…"; OAuth, grant APIs, L1 namespace, JWT iss (GA-03)
pub struct InstallationId(u64);        // also UserId(u64), RepoId(u64), OrgId(u64), TeamId(u64): newtypes (R8)

pub struct AppPurposes { pub sign_in: bool, pub acting: bool, pub installed: bool }

pub struct AppConfig {
    pub slot: AppSlot,
    pub app_id: Option<AppId>,                 // None for a sign-in-only app (never installed, ADR 0007)
    pub client_id: ClientId,
    pub client_secret: Option<ClientSecret>,   // web Worker only (P1 spec §20.1 least privilege)
    pub signing_keys: Option<SigningKeys>,     // None ⇒ this process cannot mint (e.g. a pure OAuth Worker)
    pub webhook_secrets: Option<WebhookSecrets>,
    pub user_agent: String,                    // required; GitHub rejects requests without one
    pub purposes: AppPurposes,                 // OA-05
}
pub struct AppRegistry { /* AppSlot → AppConfig; ≥ 1 entry */ }
impl AppRegistry {
    pub fn new(apps: Vec<AppConfig>) -> Result<Self, ConfigError>;
    pub fn get(&self, slot: &AppSlot) -> Option<&AppConfig>;
}
pub enum ConfigError { Missing { slot: AppSlot, field: &'static str }, Malformed { slot: AppSlot, field: &'static str, what: &'static str },
                       Duplicate { field: &'static str }, Inconsistent { slot: AppSlot, rule: &'static str } }
```

- **GA-01.** `AppRegistry::new` validates everything and fails closed with a `ConfigError` that names the field and slot, never the value (P1 plan B0-03, B0-22 "error names the variable, never the value"). Rules: duplicate slots, duplicate `app_id`s and duplicate `client_id`s are rejected; `signing_keys: Some` requires `app_id: Some` and `purposes.installed`; `purposes.installed` requires `app_id: Some`; `purposes.acting` requires `client_secret: Some` in any process that constructs `OAuthConfig<Acting>` (checked at OA-05 construction, not here, because `p1-restate` legitimately has no client secret); `user_agent` non-empty and printable ASCII; at least one purpose set.
- **GA-02.** Parsers for the P1 secret formats (P1 spec §20.1) live in L2: `SigningKeys::parse("kid1:<base64 PEM>;kid2:…")` and `WebhookSecrets::parse("s1;s2")`. Malformed input → `ConfigError::Malformed{field, what}` with no key bytes (P1 plan B0-03 test; TS-15).
- The PoC hard-codes `user-agent: ghinvite` (PoC `util.rs` L7–13). That is P1 vocabulary (R4) and moves to `AppConfig.user_agent` (delta D-08).

### 5.2 Key formats and multi-key rotation

- GitHub hands out PKCS#1 PEMs (`BEGIN RSA PRIVATE KEY`); an app can hold up to 25 keys, which never expire (`research/github-app-tokens.md` Q2).
- **GA-04.** `SigningKeys` accepts PKCS#1 and PKCS#8 PEM, as the PoC does (PoC `jwt.rs` `from_pem`), and normalizes to an in-memory `rsa::RsaPrivateKey`. At load it validates the modulus is 2048–4096 bits (GitHub keys are 2048; UNVERIFIED that GitHub never issues other sizes) and rejects others with `ConfigError::Malformed{what: "modulus"}`. The P1 deployment format is base64 PKCS#8 (P1 spec §20.1). PKCS#8 is **required** only for the WebCrypto signer (WebCrypto `importKey` takes `pkcs8`/`jwk`, not PKCS#1; `research/github-app-tokens.md` Q2, UNVERIFIED on Workers), and the loader converts in memory if that signer is selected.
- **GA-05. Rotation.** The first key signs; the others are held for rollback. Procedure (P1 spec §20.1): add a key in GitHub → prepend → deploy → delete the old key in GitHub → remove it from the list. **[Spec decision, SPD-1]** If a mint returns 401 after the GA-07 re-sign, and more keys are configured, the broker re-signs once with the next key; success emits `Event::SigningKeyFallback{app, kid}` (§13a; an operator alert: the first key is probably deleted in GitHub). At most one fallback per mint.

### 5.3 JWT signing and clock skew

```rust
pub trait JwtSigner: Send + Sync {
    /// Signature length = modulus length (256 bytes for the 2048-bit keys GitHub issues).
    fn sign_rs256(&self, signing_input: &[u8]) -> Result<Vec<u8>, AppAuthError>;
}
// default: RustCryptoSigner (pure `rsa` + `sha2`, PKCS#1 v1.5), as PoC jwt.rs

pub struct AppJwt(/* private; no Serialize/Display/Clone */);
pub struct AppJwtSource { /* registry, signer per slot, clock, &'static Observations (or isolated) */ }
impl AppJwtSource {
    /// Cached per AppId; re-signed when older than 8 min or after invalidate().
    pub fn jwt(&self, app: &AppSlot) -> Result<Arc<AppJwt>, AppAuthError>;
    pub fn invalidate(&self, app: &AppSlot);
}
```

- **GA-03. Claims.** `alg = RS256`; `iat = now' − 60`; `exp = now' + 540`; `iss` = the **client ID** (GitHub's recommendation) when configured, else the App ID. `now' = clock.now() + skew` where `skew` is read from `Observations::skew()` (GA-07). Source: `research/github-app-tokens.md` Q2. The PoC uses `iss = app_id` (PoC `jwt.rs`); either is accepted by GitHub, so the change is safe but is listed (delta D-09).
- **GA-06. Signer choice.** v1 uses the pure-Rust signer (PoC-proven natively and under workerd locally; `analysis/poc-ghinvite.md` §6.4). SP-16 measures its CPU on deployed Workers. If it exceeds 10 ms per signature, `gh-toolkit-worker` adds a WebCrypto `JwtSigner`; the JWT cache (one signature per app per ≤ 8 min per isolate) makes this unlikely to matter (ADR 0003 consequences). `cargo deny` will flag RUSTSEC-2023-0071 (Marvin timing side channel in `rsa`); it is accepted with a recorded rationale: signing only, inputs are our own claims, at most one signature per 8 min per isolate. Current status of the advisory is UNVERIFIED at extraction time and must be re-checked.
- **GA-07. Skew.** The `ResponseObserver` layer (RL-02) records `server_minus_local = Date − clock.now()` from every GitHub response with a `date` header into the `SkewEstimate` held by the same `Observations` value (isolate static by default) that `AppJwtSource` reads. If |skew| > 30 s, it is applied to JWT times, bounded to ±10 min, and `Event::ClockSkew{secs}` is emitted. A mint 401 triggers exactly one re-sign with the freshly observed skew before any key fallback (GA-05). Rationale, with GA-03's offsets: a local clock **fast** by more than 60 s puts `iat` in the future and `exp` more than 10 min ahead of GitHub's clock, both of which GitHub rejects; a local clock **slow** by more than 540 s (9 min) puts `exp` in the past. A slow clock between 0 and 9 min is harmless. TS-04 pins +61 s and −9.5 min.
- **GA-08. Time source.** All time comes from an injected `Clock` (`now() -> UnixTime`); the default is `SystemTime` natively and `js_sys::Date::now()` on wasm32. No `std::time::SystemTime` on wasm32 code paths (L1 W-02; keyring KR-14). `Duration` arithmetic is fine.

## 6. Installation token broker (C1)

### 6.1 Contract (ADR 0003, verbatim obligations)

- **BR-01.** An installation token MUST NOT enter a Restate journal, state, call parameter or result, run name, header or error (ADR 0003 decision 1). L2 enforces what a library can: `InstallationToken` and `InstallationRest` are not `Serialize`, not `Clone`, not `Display` (AC-L2-01, trybuild). Apps enforce the rest with the marker-scan test (P1 spec AC-JRN-1).
- **BR-02.** The broker is a **library** called **inside** the `ctx.run` that makes the GitHub calls; the closure returns a business outcome only (ADR 0003 decision 2).
- **BR-03.** No scheduled renewal; tokens are minted on demand (ADR 0003 decision 7).

### 6.2 Scope profiles

```rust
pub enum Permission { Administration, Members, Metadata, Contents, PullRequests, Issues, Checks,
                      Statuses, Workflows, Emails, /* … */ }          // #[non_exhaustive]; GitHub names
pub enum Access { Read, Write, Admin }

pub struct ScopeProfile { name: &'static str, permissions: &'static [(Permission, Access)], class: AccessClass }
pub enum AccessClass { ReadOnly, Write }
impl ScopeProfile {
    /// const fn; a compile-time panic if any entry is not Read ⇒ a write profile can't be declared read.
    pub const fn read(name: &'static str, permissions: &'static [(Permission, Access)]) -> ReadProfile;
    pub const fn write(name: &'static str, permissions: &'static [(Permission, Access)]) -> ScopeProfile;
    /// "<name>@<first 8 hex of SHA-256 over the canonical sorted permission list>"
    pub fn cache_scope(&self) -> String;
}
pub struct ReadProfile(ScopeProfile);
```

- **BR-04.** Profiles are **declared in app code** (P1 spec §10.1; P1 plan B0-05: `REPO_READ`/`REPO_INVITE`/`ORG_READ`/`ORG_INVITE` live in `ghinvite-workflows`). L2 provides the type, the canonical serialization of the mint request's `permissions` object, and endpoint permission metadata (§9.1) so an app can write the table test (P1 spec AC-BRK-3).
- **BR-05.** Tokens are scoped by permissions only, never per repository (ADR 0003 decision 5). Repository narrowing (`repositories`/`repository_ids`, up to 500; `research/github-app-tokens.md` Q1) is **not** offered in v1; it is additive later if a consumer needs it.
- **BR-06. `cache_scope`** is the value used for the cache key and the keyring context field `scope` (keyring §6.5 registry: `scope` is "Str: scope profile name"). Appending the permission fingerprint means a deploy that changes a profile's permissions under the same name cannot read a cached token minted for the old permission set. It stays a `Str`, and it is recoverable from the cache key at decrypt time (CX-05). Follow-up note for the keyring registry: §18.4.

### 6.3 API (names as P1 spec §10.2; shapes changed, deltas D-17, D-18)

```rust
pub struct TokenKey { pub app: AppSlot, pub installation: InstallationId, pub profile: ScopeProfile }
pub struct ReadKey  { pub app: AppSlot, pub installation: InstallationId, pub profile: ReadProfile }

/// The token. Never handed to callers directly; it lives behind Arc inside the cache and inside
/// InstallationRest. Zeroizing<String> + expires_at + generation. No Serialize/Clone/Display.
pub struct InstallationToken(/* private */);

/// Non-secret identity of one minted token: (expires_at, per-isolate mint counter). Copy + Eq.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct TokenGeneration { /* private */ }

/// What a broker closure receives. Owned, so the closure's future may borrow from it across awaits.
/// Holds Arc<InstallationToken> + Arc<RestEnv>. No Serialize/Clone/Display; Debug = "InstallationRest(..)".
pub struct InstallationRest { /* private */ }
impl InstallationRest {
    pub fn rest(&self) -> Rest<'_>;                // §9.2
    pub fn generation(&self) -> TokenGeneration;
}

#[trait_variant::make(Send)]
pub trait GrantedPermissions: Send + Sync {
    /// The installation's permissions as last observed (e.g. P1 `installations.permissions_json`)
    /// with a monotonically increasing snapshot version. None = unknown ⇒ skip the pre-check.
    async fn granted(&self, app: &AppSlot, installation: InstallationId) -> Option<GrantedSnapshot>;
}
pub struct GrantedSnapshot { pub permissions: PermissionSet, pub version: u64 }
pub trait GrantedPermissionsDyn: Send + Sync {         // TR-20; blanket impl for every GrantedPermissions
    fn granted<'a>(&'a self, app: &'a AppSlot, installation: InstallationId) -> BoxFut<'a, Option<GrantedSnapshot>>;
}

impl Broker {
    pub fn new(registry: Arc<AppRegistry>, transport: Arc<dyn HttpTransportDyn>, clock: Arc<dyn Clock>) -> Self;
    pub fn with_permissions(self, p: Arc<dyn GrantedPermissionsDyn>) -> Self;
    pub fn with_shared_cache(self, c: Arc<dyn SharedTokenCacheDyn>) -> Self;     // feature shared-cache
    pub fn with_cache(self, c: TokenCache) -> Self;                               // tests: TokenCache::isolated()
    pub fn with_events(self, sink: Arc<dyn EventSink>) -> Self;                   // §13a
    pub fn read_only(&self) -> ReadOnlyBroker;    // P1 `p1-web`; accepts ReadKey only (compile-time)

    /// Mint, or validate a cached token for `key` with ≥ refresh_margin left. Call BEFORE a fence.
    pub async fn ensure(&self, key: &TokenKey) -> Result<(), MintError>;
    /// Same, but ignores a negative-cache entry (BR-12). For recheck paths.
    pub async fn ensure_fresh(&self, key: &TokenKey) -> Result<(), MintError>;

    /// Run `f` exactly once. Uses the cached token; if none is within margin it mints first (no write
    /// has happened yet, so a mint error is a pre-write exit). No retry. For writes behind a fence.
    /// If `f` returns CallError::Api(ApiError::Unauthorized), `call` invalidates the generation `f`
    /// used (BR-07) and returns BrokerError::Call(CallError::Api(Unauthorized)).
    pub async fn call<T, E, F, Fut>(&self, key: &TokenKey, f: F) -> Result<T, BrokerError<E>>
        where F: FnOnce(InstallationRest) -> Fut + Send,
              Fut: Future<Output = Result<T, CallError<E>>> + Send, T: Send, E: Send;

    /// Reads: run `f`; on CallError::Api(Unauthorized) invalidate that generation, re-mint once, run
    /// `f` again; a second 401 is BrokerError::Unauthorized. Never used with a fence inside `f`.
    pub async fn with_token<T, E, F, Fut>(&self, key: &TokenKey, f: F) -> Result<T, BrokerError<E>>
        where F: FnMut(InstallationRest) -> Fut + Send,
              Fut: Future<Output = Result<T, CallError<E>>> + Send, T: Send, E: Send;

    /// Drop the positive entry for `key` (isolate and shared) only if it is still `seen`.
    pub async fn invalidate(&self, key: &TokenKey, seen: TokenGeneration);
    /// Drop the positive and negative entries for `key` unconditionally (isolate and shared).
    pub async fn invalidate_key(&self, key: &TokenKey);
    /// Drop every profile's positive and negative entries for one installation (isolate and shared).
    /// For installation.deleted / suspend / new_permissions_accepted handlers.
    pub async fn invalidate_installation(&self, app: &AppSlot, installation: InstallationId);
}
```

Usage that MUST compile (TS-02 compile test `closure_borrows_rest_across_await`):

```rust
let n = broker.with_token(&key, |h: InstallationRest| async move {
    let rest = h.rest();                                   // borrows h, which the future owns
    let a = rest.get_collaborator_permission(&repo, &user).await?;
    let b = rest.list_repo_invitations(&repo).await?;      // second await, same borrow
    Ok::<_, CallError<MyErr>>(a.rank() + b.len())
}).await?;
```

- **Why this shape [Spec decision, SPD-6].** A closure `FnOnce(&InstallationToken) -> Fut` with `Fut` a separate type parameter cannot return a future that borrows its argument (the future type is fixed outside the higher-ranked lifetime). `AsyncFnOnce` needs Rust ≥ 1.85 and still has rough edges around `Send` bounds on the returned future; an HRTB boxed form works but forces `Box::pin` on every caller. Passing an **owned** `InstallationRest` works on stable with plain `async move` and keeps the token behind `Arc`, so a concurrent `invalidate` never frees a token in use.
- `T` is a business outcome. `InstallationRest` is not `Serialize`, so returning it inside `T` cannot reach a journal; apps MUST NOT store it (§15 rule 1).
- **BR-07. Write path.** `ensure` → (app claims its fence) → `call`. On a 401 inside `call`, `call` has already invalidated the generation it used; the app releases its fence attempt (nothing was written), calls `ensure`, re-claims and calls once more; a second 401 is terminal. This is P1 spec §7.3 S4 / §9.4; it stays app-driven because the fence is app state. L2's obligations: `call` never retries, and every `invalidate*` is idempotent.
- **BR-08. Read path.** `with_token` implements ADR 0003 decision 4 for reads. A long paginated walk inside `f` that hits 401 restarts the walk once from the first page (pagination is all-or-nothing anyway, PG-01).
- **BR-19. Result mapping** (exact variants; TS-03 has one case per row):

| Situation | `ensure` | `call` | `with_token` |
|---|---|---|---|
| Mint fails (any §6.6 row) | `Err(MintError::…)` | `Err(BrokerError::Mint(…))`, `f` not run | `Err(BrokerError::Mint(…))` |
| `f` returns `Err(CallError::Api(Unauthorized))` | n/a | invalidate(generation); `Err(BrokerError::Call(CallError::Api(Unauthorized)))` | first time: invalidate, re-mint, rerun; second time: `Err(BrokerError::Unauthorized)` |
| Re-mint after the first 401 fails | n/a | n/a | `Err(BrokerError::Mint(…))` |
| `f` returns any other `Err(e)` (incl. a 429 → `CallError::Api(ApiError::RateLimited{..})`) | n/a | `Err(BrokerError::Call(e))` | `Err(BrokerError::Call(e))` |
| `f` returns `Ok(t)` | n/a | `Ok(t)` | `Ok(t)` |

### 6.4 Isolate cache, single-flight, margins

- **BR-09. Isolate scope and key.** The default `TokenCache` is a **process/isolate-wide static** keyed by `(AppId, InstallationId, cache_scope)` and holding `Arc<InstallationToken>`. `AppSlot` is not part of the key (it is a product-chosen string; two registries in one process may reuse slot names for different apps, e.g. native tests in parallel threads or a CLI switching profiles); it appears in logs and events only. A `TokenKey` is resolved to its `AppId` through the registry; a slot without `app_id` cannot mint (GA-01). Rationale for the static: the PoC builds its client per `fetch`, so its cache lived one request (`analysis/poc-ghinvite.md` §6.8 inference; ADR 0003 consequences); making the static the default means an app can't regress this by constructing a `Broker` per request. Tests use `with_cache(TokenCache::isolated())`, and every `test-mock` constructor does so by default (§4).
- **BR-10. Expiry.** A cached token is used only while `expires_at − now' ≥ refresh_margin` (default **60 s**, ADR 0003 decision 3; configurable 30–900 s). `expires_at` comes from the mint response, never "now + 1 h" (ADR 0003 decision 4). On Workers the clock does not advance during pure computation, only across I/O, so `now'` can read **early** and overstate the token's remaining life. The lag is bounded by the CPU time since the last I/O, which is far below the 60 s margin (Workers CPU limits are seconds; UNVERIFIED exact plan limit for the deployed tier, SP-16). The broker reads the clock immediately before handing the token to `f`; no further mitigation.
- **BR-11. In-flight mint coalescing, per target.**
  - **Native:** concurrent `ensure`s for the same key in one process share one mint through a `futures::future::Shared` per key, stored in the cache map. Its output type is `Result<Arc<InstallationToken>, MintError>`, which satisfies `Shared`'s `Output: Clone` bound (`MintError: Clone`, ER-06). An error is **not** cached as a token, and the in-flight slot is cleared when the shared future resolves, so the next caller mints again.
  - **wasm32 (Workers):** in-flight mint futures are **never shared across request contexts**. Each Worker request (a web request or a Restate invocation) has its own I/O context, and Workers documents that I/O objects cannot be used from another request's handler, so a future driven by request A cannot safely be awaited by request B (UNVERIFIED for reqwest-on-fetch specifically; SD-L2-b case 4). On wasm32 the isolate static holds **completed tokens only** (`Arc<InstallationToken>`, which is plain memory); the in-flight map lives in the `Broker` value, and apps construct a `Broker` per request context (cheap: it holds `Arc`s; documented in the crate docs and in §15 rule 6). Two concurrent requests that both miss the cache therefore mint twice; the second completed token replaces the first only if it expires later. At about one mint per (isolate, installation, profile) per hour this duplication is cheap.
  - Across isolates there is no coalescing on either target. Mint volume budget: RL-09.
- **BR-12. Negative cache [Spec decision, SPD-2].** A mint answered 404 (`InstallationGone`) or 422 (`PermissionsNotGranted`) is remembered for 60 s per key, so a burst of callers doesn't turn into a burst of mints. Each negative entry records the `GrantedSnapshot.version` seen when it was written (or none). While a negative entry exists and `GrantedPermissions` is configured, `ensure` reads `granted()`; a **newer** version voids the entry. The cache is **isolate-local**: `invalidate_key`/`invalidate_installation` clear only the calling isolate (and the shared store, if any). Consequence for P1: webhooks land on `p1-web` while Deliveries run on `p1-restate` (P1 spec §20.1), so a `p1-web` invalidation cannot clear `p1-restate`'s entries. Recheck paths therefore MUST use `ensure_fresh` (P1's `Installation.recheck_permissions` after it refreshes `permissions_json`, P1 spec §10.2), and the snapshot version voids stale entries for every other caller.
- **BR-13. Size.** No length assumption on tokens; storage and patterns allow 600+ characters (ADR 0003 decision 6; tokens are ~520 chars, `research/github-app-tokens.md` Q1).

### 6.5 Shared second-level cache (feature `shared-cache`, optional)

ADR 0003 decision 3: added only if measured mint volume calls for it. P1 does not build it in M0–M2 (P1 spec §10.2). Target design:

```rust
#[trait_variant::make(Send)]
pub trait SharedTokenStore: Send + Sync {        // storage only; gh-toolkit-worker has KV and D1 impls
    async fn get(&self, key: &str) -> Result<Option<(Vec<u8>, UnixTime)>, StoreError>;     // (sealed, expires_at)
    async fn put(&self, key: &str, sealed: &[u8], expires_at: UnixTime) -> Result<(), StoreError>;
    async fn delete_if(&self, key: &str, expires_at: UnixTime) -> Result<(), StoreError>;  // only that entry
}
/// The object-safe form the Broker takes (TR-20). Works on tokens, not bytes.
pub trait SharedTokenCacheDyn: Send + Sync {
    fn get<'a>(&'a self, k: &'a SharedKey) -> BoxFut<'a, Result<Option<InstallationToken>, SharedCacheError>>;
    fn put<'a>(&'a self, k: &'a SharedKey, t: &'a InstallationToken, now: UnixTime) -> BoxFut<'a, Result<(), SharedCacheError>>;
    fn delete_if<'a>(&'a self, k: &'a SharedKey, expires_at: UnixTime) -> BoxFut<'a, Result<(), SharedCacheError>>;
}
pub struct SharedKey { pub app: AppId, pub installation: InstallationId, pub scope: String /* cache_scope */ }
pub struct SealedSharedCache<S: SharedTokenStore, K: anykms_keyring::ReadyKek> { /* ContextEnvelope */ }
impl<S: SharedTokenStore, K: anykms_keyring::ReadyKek> SharedTokenCacheDyn for SealedSharedCache<S, K> { /* seal/open + S */ }
```

- **BR-14. Key and context.** Store key `itc/v1/<app_id>/<installation_id>/<cache_scope>`. Value = anykms envelope v1 of the token, under the registered purpose **`installation-token-cache`** with fields `app_id` (Decimal, the numeric App ID), `installation_id` (Decimal), `scope` (Str = `cache_scope`), `expires_at` (Decimal, Unix seconds); padding `block(64)` (keyring §6.5 registry v1). `expires_at` is also stored in plaintext beside the ciphertext (ADR 0003 decision 3). All values come from the key and the plaintext column at decrypt time (CX-05). Binding `expires_at` stops an old ciphertext being paired with a fresh expiry (keyring §8.2 table).
- **BR-15. Lookup order and consistency.** isolate → shared → mint. A shared hit is used only under BR-10's margin and is copied into the isolate cache. A mint writes both, except that the shared `put` is **skipped** when `expires_at − now < max(60 s, refresh_margin)` (Workers KV rejects `expirationTtl` below 60 s; UNVERIFIED for D1, where it is harmless). KV writes set `expiration_ttl = expires_at − now` (keyring §7.2 table). `invalidate` deletes the shared entry only if its `expires_at` equals the invalidated token's (`delete_if`). Costs, stated honestly:
  - **D1** (`DELETE … WHERE key = ? AND expires_at = ?`) is strongly consistent per database; the race costs at most one extra 401-and-re-mint per concurrent caller. **Preferred** whenever invalidation matters.
  - **KV** is eventually consistent (propagation up to about 60 s; Cloudflare docs, UNVERIFIED current figure). After an invalidation, other isolates may keep reading the old token for up to that delay; each pays one 401 and one re-mint and may `put` its own new token. Read-compare-delete can also delete a newer token another location wrote. Upper bound on wasted calls ≈ (KV propagation delay ÷ request inter-arrival) per isolate, i.e. up to one 401 + one mint per affected isolate per invalidation. SD-L2-a measures 401s after invalidation.
- **BR-16. Failure is a miss.** Any shared-cache error (store, `DecryptFailed`, unknown key version, wrong context) is treated as a miss and reported by `Event::SharedCacheError{kind}` (§13a); it never fails the call. A wrong keyring is therefore an alert plus a mint, not an outage.
- **BR-17. Trigger to build it** [Spec decision, SPD-3]: any of (a) `Event::Mint` counts show more than **15 mints/min per app sustained over 1 h** (900/h, below 50 % of the 2,000/h OAuth-token limit that RL-09 assumes mints count against); (b) the RL-09 counter emits `Event::OAuthBudget{level: Warn}` on any day; (c) SP-16 shows mint latency dominating deployed invocations. Otherwise it stays unbuilt. SD-L2-a and SD-L2-e decide whether to relax (a).

### 6.6 Mint errors

RL-01 classification runs **first** on every mint response; only a response that is not a rate limit reaches the status rows below.

| Mint response | `MintError` | Broker behaviour | Source |
|---|---|---|---|
| 201 | — | cache, return | `research/github-app-tokens.md` Q1 |
| rate-limit evidence (RL-01; incl. a secondary-limit 403 on the mint POST) | `RateLimited{scope, retry_after}` | none; caller waits | §8 |
| 401 | `AppAuth(JwtRejected)` after GA-07 re-sign and GA-05 fallback | re-sign once (skew), then next key once | Q1 ("401" on mint = bad JWT / skew), Q2 |
| 403 (not a rate limit) | `InstallationSuspended` if a follow-up `GET /app/installations/{id}` with the App JWT shows `suspended_at` set; otherwise `MintForbidden{reason}` (`reason`: `SsoRequired`, `IpAllowList`, `Other`, per ER-03) | one confirmation read; no retry; no negative cache | Q1 + "Design implications" item 1 ("403 means suspended **or not allowed**", `research/github-app-tokens.md` L189); P1 spec §10.2 |
| 404 | `InstallationGone` | negative cache 60 s (BR-12) | Q1; P1 spec §10.2 |
| 422 | `PermissionsNotGranted` | negative cache 60 s; never retried by L2 | Q1 ("cannot be granted permissions that the app was not granted"); P1 spec §10.2 |
| 5xx, transport error (sent or not), timeout | `Transient{kind}` (`Server{status}`, `NotSent`, `NoResponse`) | none; Restate retries the `ctx.run`. A mint is retry-safe (it creates no visible state), so `NoResponse` is transient here | P1 spec §10.2 |
| 3xx (TR-17) | `Unexpected{status}` | none | TR-17 |
| Pre-mint check: profile ⊄ `granted().permissions` | `PermissionsNotGranted` | **no mint call** | P1 plan B0-04; P1 spec AC-BRK-4 |

- If the confirmation `GET` itself fails, the result is `MintForbidden{reason: Other}` (never `InstallationSuspended` without evidence).
- **BR-18. Revocation after use** is not done. `DELETE /installation/token` is offered as `revoke_installation_token(&Rest)` (endpoint `RevokeInstallationToken`) for apps that want it, but the broker doesn't call it, and an `invalidate` does not imply GitHub revoked anything until SP-23 shows revocation works for stateless tokens (ADR 0003 consequences).

## 7. Transport

### 7.1 Seam

```rust
#[trait_variant::make(Send)]
pub trait HttpTransport: Send + Sync + 'static {
    async fn send(&self, request: Request) -> Result<Response, TransportError>;
}
pub trait HttpTransportDyn: Send + Sync + 'static {            // TR-20; blanket impl
    fn send(&self, request: Request) -> BoxFut<'_, Result<Response, TransportError>>;
}
pub enum Method { Get, Post, Put, Patch, Delete }            // PoC lacks Patch (delta D-03)
pub struct Request  { method, url, headers: BTreeMap<String, String>, body: Option<Body>, timeout: Duration }
pub enum Body { Json(Vec<u8>), Form(Vec<u8>), Secret(SecretBody) }   // SecretBody: §9.2, TR-19
pub struct Response { status: u16, headers: BTreeMap<String, String>, body: Vec<u8>, final_url_matches: bool }
pub enum TransportError { NotSent, SentNoResponse { timed_out: bool }, Oversize }   // §13 ER-04
```

- `Request`'s `Debug` prints method, path (scrubbed, TR-09) and header **names** only; it never prints header values or the body. `tracing` spans on requests use the same fields.
- **TR-02. Implementations.** `ReqwestTransport` (feature `reqwest`, native and wasm32; on wasm32 a per-request timeout because `ClientBuilder::timeout` is native-only, PoC `transport.rs` L247–353, `docs/network-deadlines.md`). `WorkerFetchTransport` in `gh-toolkit-worker`, built if SD-L2-b shows reqwest-on-fetch misbehaving under workerd (cancellation, header handling, subrequest accounting, cross-request use) **or** if TR-17 cannot be met by reqwest on wasm32. `MockTransport` (feature `test-mock`).
- **TR-04. Deadlines.** Default per-request timeout 30 s (PoC). Callers may lower it per request; the L1 refresher uses 15 s (RF-04).
- **TR-05. Headers set by L2 on every GitHub API call.** `accept: application/vnd.github+json`, `user-agent` from `AppConfig`, `x-github-api-version` (default `2022-11-28` as the PoC; overridden by `EndpointMeta.api_version`, because whether `merge-async` needs a newer version such as `2026-03-10` is UNVERIFIED, `research/dependaboard-prs.md` Q3), and `authorization` from the credential (§9.2). The OAuth token endpoint on `web_base` gets `accept: application/json` and `user-agent` only. Header names are lower-cased (PoC convention). TS-17 asserts the exact set per endpoint.
- **TR-06. Base URLs.** `api_base` (default `https://api.github.com`) and `web_base` (default `https://github.com`, for OAuth). Pagination refuses `Link` targets outside `api_base` (PG-01).
- **TR-07. Body limits.** Responses above 10 MiB are rejected as `TransportError::Oversize` → `ApiError::Decode{what: "oversize"}` before parsing [Spec decision, SPD-4]; error bodies are only inspected up to 64 KiB (PoC `redact.rs` `MAX_INSPECT`). TS-17 covers both.
- **TR-17. No automatic redirects.** Transports MUST NOT follow redirects. `ReqwestTransport` natively builds its client with `reqwest::redirect::Policy::none()` (the PoC uses the default policy, which follows up to 10 hops: PoC `transport.rs` L262 `Client::builder()` and the `is_redirect` branch at L324). `WorkerFetchTransport` sets `redirect: "manual"`. On wasm32, reqwest-on-fetch's ability to disable redirects is **UNVERIFIED** (SD-L2-b case 5). Until manual redirect behavior is proven, use `WorkerFetchTransport` with `redirect: manual` or fail closed before sending a credentialed request. If reqwest cannot satisfy manual redirect behavior, `WorkerFetchTransport` is mandatory regardless of whether workerd strips `authorization`: following a redirect can repeat a write as well as leak credentials. `Response::final_url_matches` remains defense in depth; a mismatch discards the response as `ApiError::Moved{status: 0}` and emits `Event::RedirectFollowed`, but detection after transmission does not satisfy this requirement. L2 maps every 3xx to `ApiError::Moved{status}` with no URL text (GK-P2). Writes never follow. A typed **read** MAY follow once, and only when the `Location` resolves to the same `api_base` origin (scheme, host, port); this is opt-in per call (`.follow_same_origin_redirect()`), used for renamed-repo reads. TS-01 and TS-10 include a 307 to a foreign host carrying a marker token: the marker MUST NOT reach the foreign host and MUST NOT appear in any output.

### 7.2 Redaction

- **TR-08.** No `TransportError`, `ApiError` or log line contains upstream text (GK-P2). The PoC's `redact.rs` is lifted unchanged in spirit: only GitHub's documented `errors[].code` sub-codes (`VALIDATION_SUB_CODES`), the OAuth error codes (`OAUTH_ERROR_CODES`), sizes and shapes survive (PoC `redact.rs` L1–120). Classifiers that need body text (rate-limit wording, "spammed", SSO) read the **raw** body before redaction and keep only an enum (PoC `transport.rs` `rate_limit`).
- **TR-09. Pattern backstop** for logs and panics (defence in depth, not the primary control): `ghs_[A-Za-z0-9._\-]{36,}`, `ghu_…`, `ghr_…`, `gho_…`, `ghp_…`, `github_pat_…`, `eyJ[A-Za-z0-9_\-]{10,}\.[A-Za-z0-9_\-]+\.[A-Za-z0-9_\-]*` (JWTs, which includes App JWTs and stateless installation tokens' payloads), `Bearer\s+\S+`, `Basic\s+\S+`, and the generic OAuth parameters `(?i)([?&](code|state|code_verifier|client_secret|refresh_token|access_token)=)[^&\s]+` → `$1<redacted>` plus the same names as JSON keys `(?i)("(code_verifier|client_secret|refresh_token|access_token)"\s*:\s*")[^"]+` → `$1<redacted>` (ADR 0003 decision 6; P1 plan B0-06). Only product-specific patterns (P1's `/i/` Link Secret segment, P1 spec §11.4) stay in apps. Exposed as `redact::scrub(&str) -> Cow<str>` for apps' log layers.
- **TR-10.** `reqwest` errors render URLs; only the classification survives (PoC `transport.rs` `transport_failure`).

### 7.3 Conditional requests (opt-in)

- **TR-11.** `Request::if_none_match(etag)` and `Response::is_not_modified()` are available on GETs. An optional `EtagStore` trait (app-provided; isolate memory or KV) lets typed list calls reuse ETags per (credential class, URL). Default **off**.
- Why opt-in: GitHub documents that a `304` doesn't count against the primary limit when the request was authorized, but whether ETags stay valid across rotating installation tokens (responses may `Vary` on `Authorization`) is **UNVERIFIED** → SD-L2-c. P2's mergeability polling and installation-repo listing are the expected users; P1's Reconciler isn't (its lists must be complete and are small).

## 8. Rate limits and the write governor

### 8.1 Classification and observation (every response)

- **RL-01.** Every response is classified once (PoC `Response::rate_limit`, kept as is): a 403/429 is a rate limit if it carries `retry-after`, is a 429, names a rate limit or "abuse detection" in `message`, or has `x-ratelimit-remaining: 0` with no other stated reason; otherwise a 403 is a permission refusal. The scope (`Primary`/`Secondary`) and the wait (`retry-after`, else `x-ratelimit-reset` − `date` if the quota is exhausted, both measured against GitHub's `date` header) follow PoC rules. Without any wait hint, `retry_after = None`; GitHub's documented guidance in that case is "wait at least one minute" (`research/dependaboard-prs.md` Q6). L2 itself never chooses a governor closure time: `RateLimited::suggested_wait()` returns `retry_after` or 60 s when it is `None`, and apps pass whatever they choose as `until` to `record_limit` (RL-06). P1's 24 h closure on 422 "spammed" (P1 spec §7.5) is such an app choice.
- **RL-02. Observations.** A `ResponseObserver` layer sits between `Rest` and `HttpTransportDyn` in every L2 call. It owns no state itself; it writes into an `Observations` value (`Observations::global()`, an isolate static, by default; `Observations::isolated()` for tests) that holds (a) a `QuotaObservation` per (`QuotaBucket`, `x-ratelimit-resource`): `limit`, `remaining`, `used`, `reset`; (b) the `SkewEstimate` (GA-07); (c) the RL-09 OAuth-token counter. The bucket comes from the credential (`Credential::bucket()`, §9.2): `Installation{app: AppId, id}` (stamped by the broker at mint), `User{client: ClientId, user: UserId}` (stamped at construction), `App(AppId)` (JWT calls), `Client(ClientId)` (Basic calls), or `Unattributed` (a user token whose user ID is not known yet, e.g. a fresh sign-in token before `/user`). Exposed read-only for metrics and for a **pre-flight fail-fast**: a call whose credential's bucket is known exhausted until *t* returns `RateLimited{retry_after: t − now}` without sending (saves a request; disabled per call with `.no_preflight()`; never applied to `Unattributed`). TS-05 asserts pre-flight blocks only the exhausted bucket.
- **RL-03.** L2 **never sleeps** (§2). On Workers a sleep inside `ctx.run` burns wall time and can outlive the invocation; Restate's durable `ctx.sleep` is the right tool, which is app code (L1 EX-R5 is the same pattern).
- **RL-04. "Spammed".** A 422 whose raw body names the anti-spam refusal is `ApiError::Spammed` (P1 spec §7.5: "422 spammed closes the bucket"; text from SP-08 is recorded in the stub contract). The match text is UNVERIFIED until SP-08 records it.
- **RL-05. GraphQL** cost: `rateLimit { cost remaining resetAt }` is requested by L2's GraphQL helper when the caller opts in and feeds RL-02 for resource `graphql`; GraphQL rate-limit errors (`type: RATE_LIMITED` in `errors`, HTTP 200 or 403) are `RateLimited` (`research/dependaboard-prs.md` Q1; exact error shape UNVERIFIED, recorded by SP-05).
- **RL-09. OAuth-token budget [Spec decision, SPD-7].** GitHub documents "No more than 2,000 OAuth access token requests per hour are allowed for GitHub Apps and OAuth apps"; whether installation-token mints count toward it is **UNVERIFIED** (`research/github-app-tokens.md` L16, L37; L1 RC-14 cites the same limit). Until SD-L2-e resolves it, L2 **assumes they count**. `Observations` keeps an isolate-local sliding 1 h counter per `AppId`/`ClientId` pair (one GitHub App) that increments on every installation-token mint, OAuth code exchange (sign-in and acting) and L1 refresh made through L2. At 1,000/h (50 %) it emits `Event::OAuthBudget{app, level: Warn, count}` once per hour, at 1,600/h (80 %) `level: High`. It is a signal, not a limiter, and it sees one isolate only, so the per-app total is the sum across isolates in metrics. P2's merger app draws mints, acting-grant exchanges and refreshes from the same budget. Follow-up for R-C1-token-limits: §18.4.

### 8.2 Published caps (constants)

```rust
pub mod caps {   // research/github-app-tokens.md Q1; research/dependaboard-prs.md Q1, Q6;
                 // research/github-invitations.md §1, §2, §9; research/link-security-and-gh-terms.md §2.3
    pub const CONTENT_WRITES_PER_MIN: u32 = 80;
    pub const CONTENT_WRITES_PER_HOUR: u32 = 500;
    pub const MIN_WRITE_SPACING_SECS: u32 = 1;
    pub const REST_POINTS_PER_MIN: u32 = 900;        // GET 1, mutating 5
    pub const GRAPHQL_POINTS_PER_MIN: u32 = 2_000;
    pub const MAX_CONCURRENT: u32 = 100;
    pub const SEARCH_PER_MIN: u32 = 30;              // GraphQL search counting here too: UNVERIFIED (assume yes)
    pub const SEARCH_MAX_RESULTS: u32 = 1_000;
    pub const ORG_INVITES_PER_DAY_NEW: u32 = 50;     // org < 1 month old and free
    pub const ORG_INVITES_PER_DAY_ESTABLISHED: u32 = 500;
    pub const REPO_INVITES_PER_DAY: u32 = 50;        // per repo; org members not counted
    pub const OAUTH_TOKEN_REQUESTS_PER_HOUR: u32 = 2_000;  // per app; used by RL-09 and BR-17
}
```

### 8.3 Governor primitives (P1 plan B1-01)

```rust
/// Pure, serde, journal-safe (no secrets). Apps hold one or more as Restate VO state. All methods
/// take `now`; none reads a clock (replay-safe).
#[derive(Clone, Serialize, Deserialize)]
pub enum RefundError { UnknownSlot, InvalidSubset }
pub struct WriteLedger { /* buckets: BTreeMap<BucketId, BucketState>, holds, request_keys */ }
#[derive(Clone, Serialize, Deserialize)]
pub struct BucketSpec { pub id: BucketId, pub windows: Vec<(u32 /*cap*/, u32 /*window secs*/)>, pub min_spacing_secs: u32 }
pub struct BucketId(String);                 // app-chosen, e.g. "org:123", "repo:456", "user:789"
pub struct ReservationTag(String);           // app-chosen attribution, e.g. "link:<id>"; never a secret
pub enum Decision { Granted { slot: SlotId }, RetryAt(UnixTime), UnknownBucket(BucketId) }
pub enum HoldDecision { Held { hold: HoldId, expires_at: UnixTime }, RetryAt(UnixTime), UnknownBucket(BucketId) }

impl WriteLedger {
    pub fn new() -> Self;
    /// Insert or replace a bucket's spec. A cap change takes effect on the next reserve/hold; existing
    /// timestamps are kept (raising 50 → 500 frees capacity at once; lowering may yield RetryAt).
    pub fn upsert_bucket(&mut self, spec: BucketSpec);
    pub fn remove_bucket(&mut self, id: &BucketId);

    /// Single-ledger, all-or-nothing reservation across `buckets`. With `request_key`, a repeated
    /// call with the same key returns the same Decision (idempotent; key kept 24 h, then forgotten).
    pub fn reserve(&mut self, buckets: &[BucketId], tag: Option<&ReservationTag>, request_key: Option<&str>, now: UnixTime) -> Decision;
    pub fn release(&mut self, slot: SlotId);                                       // write didn't happen
    pub fn refund_buckets(&mut self, slot: SlotId, buckets: &[BucketId]) -> Result<(), RefundError>;

    /// Two-phase form for buckets held in DIFFERENT ledgers (different VOs). A hold counts against
    /// caps and spacing until committed, released, or `expires_at` (now + ttl, ttl ≤ 300 s).
    pub fn hold(&mut self, buckets: &[BucketId], tag: Option<&ReservationTag>, request_key: Option<&str>, ttl_secs: u32, now: UnixTime) -> HoldDecision;
    pub fn commit(&mut self, hold: HoldId, now: UnixTime) -> Result<SlotId, LedgerError>;   // Expired if past expires_at
    pub fn release_hold(&mut self, hold: HoldId);

    /// Close a bucket until `until` (always the caller's choice; RL-01).
    pub fn record_limit(&mut self, bucket: &BucketId, until: UnixTime);
    /// Granted + held slots carrying `tag` in `bucket` within its longest window.
    pub fn count_tagged(&self, bucket: &BucketId, tag: &ReservationTag, now: UnixTime) -> u32;
    pub fn snapshot(&self, now: UnixTime) -> LedgerSnapshot;                       // for UI and ETA
}
pub struct SlidingWindow { /* timestamps, bounded by the largest cap; O(cap) memory */ }
pub fn content_write_buckets(installation: BucketId, user: Option<BucketId>) -> Vec<BucketSpec>; // 80/min, 500/h, ≥ 1 s
```

- **RL-05a. Partial refunds.** `refund_buckets` validates the entire nonempty subset against the slot’s original or already-refunded bucket memberships before changing any charge. A foreign bucket refuses atomically (`InvalidSubset`); an unknown/expired slot returns `UnknownSlot`. Repeated subset members/refunds are idempotent while the slot remains retained. Unselected bucket charges, timestamps, spacing and original reserve/commit replay decisions stay unchanged. This primitive carries no upstream-outcome policy: the app must verify which charges may be refunded. P1’s successful PUT204 refunds only its repository invitation bucket and permanently fences later all-bucket release at the app slot boundary.

- **RL-06. Multi-bucket reservation.** `reserve` is atomic **within one `WriteLedger`**. Buckets in different VOs use the two-phase protocol below; L2 provides the primitives, apps run the protocol from their Restate handler:
  1. `hold` in the **user** ledger (VO keyed by user), then `hold` in the **installation** ledger (VO keyed by installation). Fixed order user → installation. Holds never block other callers (a full bucket answers `RetryAt`), so there is no deadlock; the order only bounds wasted holds.
  2. If the second `hold` returns `RetryAt(t)`, `release_hold` the first and return `Throttled{until: t}`.
  3. Both held → `commit` both (before the write's fence), then write. `release(slot)` on either side if the write didn't happen.
  4. A crash between steps is retried by Restate from its journal; if it is not (e.g. the invocation is killed), the orphaned hold expires after `ttl` and frees its capacity. `ttl` default 60 s.
  P2's user-token merge does this across its per-user and per-installation ledgers, because whether the 500/h content limit is per user or per installation for user-to-server calls is UNVERIFIED (`research/dependaboard-prs.md` Q6) [Spec decision, SPD-5]. An app MAY instead keep all buckets in one ledger in one VO per app and use `reserve`; that serializes every write of the app through one VO (throughput bound: one VO handler call per write, UNVERIFIED Restate-on-Workers VO latency, SP-16). The P2 spec chooses.
- **RL-07. App policy on top.** Invite caps, "confirmed established org" (`upsert_bucket` with 500 after admin confirmation) and the 24 h invite windows are app policy built from `BucketSpec` (P1 spec §7.5, PD-I, PD-L). P1's per-link share: the app calls `count_tagged(bucket, link_tag, now)` before `reserve` and refuses when the share would be exceeded; admin-issued requests reserve with no tag (P1 spec AC-GOV-1 maps to TS-05 case "tagged share").
- **RL-08.** Endpoint metadata (§9.1) marks each endpoint `content_creating`, so apps can assert in tests that every content-creating call passes the ledger. Which GraphQL mutations count as content-creating is UNVERIFIED; L2 marks all mutations as content-creating (conservative).
- **RL-10. Bounded state.** `reserve`, `hold` and `snapshot` drop timestamps older than each bucket's longest window, expired holds, and request keys older than 24 h. Buckets with no timestamps and no closure for longer than their longest window are removed only by `remove_bucket` (specs are app configuration); apps with unbounded bucket families (per repo, per link) MUST remove idle buckets or key VOs by bucket family. A ledger's serialized size is O(Σ caps + holds + request keys).

## 9. REST client

### 9.1 Typed endpoints with metadata

```rust
#[non_exhaustive]
pub enum Endpoint { CollaboratorPermission, ListCollaborators, PutCollaborator, ListRepoInvitations,
    UpdateRepoInvitation, DeleteRepoInvitation, RemoveCollaborator, GetOrgMembership, CreateOrgInvitation,
    ListOrgInvitations, ListFailedOrgInvitations, CancelOrgInvitation, GetTeamMembership, PutTeamMembership,
    RemoveOrgMember, RemoveTeamMembership, ListOrgTeams /* TR-13 UNVERIFIED permission */, GetUserByLogin,
    GetUserById, GetOrg, GetInstallation, ListInstallationRepos, ListInstallationRequests,
    CreateInstallationToken, RevokeInstallationToken,
    GetAuthenticatedUser, ListUserEmails, ListUserInstallations, ListUserInstallationRepos,  // user token
    RevokeUserToken, DeleteGrant, CheckToken,                                               // ClientBasic
    /* P2, additive (P2 spec §18): */ MergeAsync, GetMergeAsync, CreateIssueComment, CreateReview, GetPull,
    ListPullReviews, ListIssueComments, ListCommentReactions, GetRepoCustomPropertyValues, GraphQl, /* … */ }
pub struct EndpointMeta { pub method: Method, pub path: PathTemplate, pub credential: &'static [CredentialKind],
    pub permission: Option<(Permission, Access)>, pub content_creating: bool, pub paginated: bool,
    pub retry_safe: bool, pub api_version: Option<&'static str> }
impl Endpoint { pub const fn meta(self) -> EndpointMeta; pub fn all() -> &'static [Endpoint]; }

/// Non-API endpoint (web_base): POST /login/oauth/access_token. Not an Endpoint variant (no api_base,
/// no API headers); has its own stub-contract entries (code exchange, refresh, error-in-200).
pub struct OAuthTokenEndpoint;
```

- **TR-12.** The P1 endpoint set is exactly P1 spec §7.2 (P1 plan B0-05 for repo/installation/user/token rows, B1-01 for org/team rows), with typed results `RepoRole` (with `rank()`, `pull`/`push` → `read`/`write`, `RepoRole::Custom(String)` unrankable), `OrgRole`, `TeamRole`, `MembershipState` (P1 spec §10.1). Results of writes distinguish status codes the caller needs (e.g. collaborator PUT `201 Created{invitation_id}` vs `204 NoContent`, PoC `installation.rs` `CollaboratorAddition`).
- **TR-13.** Permissions in `EndpointMeta` are the documented requirement; entries the research marks UNVERIFIED (e.g. `ListCollaborators?affiliation=direct`, `ListOrgTeams` = `GET /orgs/{org}/teams`, P1 spec §7.1) carry `// UNVERIFIED` in source and a test-name suffix `_unverified` so they are easy to find when a spike lands.
- **TR-14.** `PATCH /repos/{o}/{r}`, `DELETE /repos/{o}/{r}` and ruleset writes exist as `Endpoint` variants only under `repo-admin` (§4); the generic call refuses them at runtime without it (TR-18). CI additionally greps for those path literals outside redaction tests (P1 spec AC-SEC-3 moved into L2); the grep is a backstop, TR-18 is the control.
- **TR-22. P2 endpoint notes (P2 spec §18, additive).** `CollaboratorPermission` also accepts an installation credential for P2's Visible Set check (P2 §6.6; P2 calls it "GetCollaboratorPermission"; required permission UNVERIFIED, SP-05 addition). `CreateIssueComment` and `CreateReview` accept a user credential; `GetMergeAsync` accepts both installation and user credentials (P2 §7.4). `MergeAsync` maps 200/202/400/409/422 per P2 §7.4, carries the uuid from a 409 body as a typed field, and uses `api_version = 2026-03-10` (TR-05); a 422 "spammed" maps to `ApiError::Spammed` when the body allows telling it apart (shape recorded by SP-09).
- **TR-21. `retry_safe`.** True when re-sending the request after an unknown outcome cannot create a second effect: every GET, `CreateInstallationToken`, `CheckToken`, GraphQL **queries**, and idempotent PUTs whose repeat is a no-op (`PutCollaborator` repeats return 204, PoC `installation.rs`; `PutTeamMembership`). DELETEs of a specific resource are retry-safe (a repeat is 404). False for POSTs that create (`CreateOrgInvitation`, `CreateIssueComment`, `CreateReview`), asynchronous merge submission (`MergeAsync`, HTTP **PUT** per P2 §7.4 and the GitHub merge-async contract), GraphQL **mutations**, and `UpdateRepoInvitation`. For `Endpoint::GraphQl` the value comes from `GraphQlRequest.kind` (§10.2). Note `retry_safe` is about duplicate effects only; apps behind a fence still read back (R-ALL-github-write-fence). TS-11 asserts a value with a one-line reason for every variant.

### 9.2 Credentials and `Rest`

```rust
pub trait Credential: sealed::Sealed + Send + Sync {
    fn kind(&self) -> CredentialKind;
    fn bucket(&self) -> QuotaBucket;
    /* writes the authorization header; crate-private */
}
pub enum CredentialKind { AppJwt, Installation, User, ClientBasic }
pub enum QuotaBucket { Installation { app: AppId, id: InstallationId }, User { client: ClientId, user: UserId },
                       App(AppId), Client(ClientId), Unattributed }
// implemented by: AppJwt, InstallationToken (bucket stamped by the broker at mint), UserAccessToken,
// EphemeralUserToken (kind User), ClientCredentials (Basic client_id:secret)
impl UserAccessToken {
    pub fn from_secret(s: &rotcred_core::Secret, client: ClientId, user: UserId) -> Self;  // feature rotating-credential
    pub fn from_operator(s: Zeroizing<String>, client: Option<ClientId>, user: Option<UserId>) -> Self; // P3 `gh auth token`
}

pub struct RestEnv { /* Arc<dyn HttpTransportDyn>, api_base, user_agent, &'static Observations (or Arc to isolated), Arc<dyn EventSink> */ }
impl RestEnv { pub fn for_slot(registry: &AppRegistry, slot: &AppSlot, transport: Arc<dyn HttpTransportDyn>) -> Result<Self, ConfigError>; }
pub struct Rest<'a> { env: &'a RestEnv, cred: &'a dyn Credential }
impl<'a> Rest<'a> { pub fn new(env: &'a RestEnv, cred: &'a dyn Credential) -> Self; }

/// crate-private. The only code that serializes a user token or client secret into a request body.
pub(crate) struct SecretBody { /* Zeroizing<Vec<u8>>; Debug = "SecretBody(..)"; not Serialize */ }
```

- **TR-15.** Typed endpoint functions take `&Rest<'_>` and refuse (with `ApiError::Forbidden{reason: WrongCredential}`, not sent) a credential whose kind is not in the endpoint's `credential` list. This is the change that lets P2 call the same `create_issue_comment`/`merge_async` with a user token or an installation token (§16, P2 row "user-token calls"). P1's `InstallationClient` facade (PoC name) stays as sugar with the **same closure argument as the broker**: `client.with_token(key, |h: InstallationRest| async move { h.rest().… })`.
- **TR-16.** User tokens are never refreshed by L2. A 401 on a user-token call returns `ApiError::Unauthorized`; the caller follows L1 RC-12 (refresh through the executor, `access_fresh`, at most three attempts). L2 provides no helper that hides that loop, because the loop crosses `ctx.run` boundaries (L1 §7.2 "Caller flow").
- **TR-19. Secret bodies.** `revoke_token`, `delete_grant`, `check_token` (JSON `{"access_token": …}`), the OAuth code exchange (form `client_secret`, `code`, `code_verifier`) and the L1 refresh (form `client_secret`, `refresh_token`) build their bodies only through `SecretBody`. `SecretBody` is excluded from `Request`'s `Debug` and from `tracing` fields, is zeroized on drop, and `MockTransport` records it only behind an explicit test accessor. TS-01 places markers in each of these bodies and asserts none appears in logs, errors or `Debug`.

### 9.3 Generic call (escape hatch)

```rust
pub struct PathTemplate { /* &'static str with "{}" placeholders, e.g. "/repos/{}/{}/topics" */ }
impl PathTemplate {
    pub const fn new(t: &'static str) -> Self;            // must start with '/', no '?', no '..'
    pub fn seg(self, s: &str) -> Self;                    // fills the next "{}", percent-encoding every byte
                                                          // outside unreserved (RFC 3986), incl. '/', '.', '%'
}
impl Rest<'_> {
    pub async fn call<Req: Serialize, Resp: DeserializeOwned>(&self, method: Method, path: PathTemplate,
        query: &[(&str, &str)], body: Option<&Req>, retry_safe: bool) -> Result<ApiResponse<Resp>, ApiError>;
}
```

- Same headers, classification, observation and redaction as typed calls. A template with unfilled placeholders, or a segment that is empty, `.` or `..`, is refused before sending (`ApiError::Decode{what: "path"}`, never sent). This replaces the "pre-joined path" form, which had no rule for caller-supplied logins or slugs.
- **TR-18. Runtime repo-admin deny-list.** Without feature `repo-admin`, `call` refuses, without sending, any (method, template) pair matching the deny-list `PATCH /repos/{}/{}`, `DELETE /repos/{}/{}`, `POST|PUT|PATCH|DELETE /repos/{}/{}/rulesets…`, `POST|PUT|PATCH|DELETE /orgs/{}/rulesets…`, `PUT|DELETE /repos/{}/{}/branches/{}/protection…`, with `ApiError::Forbidden{reason: Disabled}`. Matching is on the template string, so dynamic segments cannot dodge it. TS-11 covers each entry.
- Endpoints used through the escape hatch by two consumers are promoted to `Endpoint` variants.

## 10. Pagination and GraphQL

### 10.1 REST `Link` pagination

- **PG-01.** `walk_link_pages` (PoC `pagination.rs`) is the only REST pagination: it walks to the last page or errors; a revisited page, more than `MAX_PAGES` (1,000) pages, an unparseable `Link`, or a target outside `api_base` is an error, never a shorter list. `per_page = 100` by default.
- **PG-02.** It becomes public as `rest.list_all::<T>(endpoint, params) -> Result<Vec<T>, ApiError>` and a streaming `rest.for_each_page(…, |page| …)` for large lists (Workers memory). Both keep the all-or-nothing guarantee: the streaming form returns `Err` after the callback has seen earlier pages, and documents that callers must not treat partial callbacks as complete.
- **PG-07. SSO partial results.** For SAML-SSO orgs GitHub can answer a list call with 2xx and `X-GitHub-SSO: partial-results; organizations=<id>,<id>` (documented for classic PATs in GitHub's "Authenticating to the REST API", Enterprise Cloud; behaviour for GitHub App user tokens UNVERIFIED, SP-15). If any page of a walk (REST or GraphQL) carries `partial-results`, the walk returns `ApiError::Partial{reason: PartialReason::SsoRequired, org_ids: Vec<OrgId>}` (numeric IDs only, parsed from the header, capped at 100). A caller that opts in with `.allow_partial()` gets `Ok(Listing{items, partial: Some(PartialInfo{reason, org_ids})})` instead, so the omission is explicit. `research/github-app-tokens.md` L136–151 (missing SSO session ⇒ "missing repos"). TS-06 and TS-10 include a stub page with the header.

### 10.2 GraphQL (feature `graphql`, for P2)

```rust
pub enum GraphQlKind { Query, Mutation }
pub struct GraphQlRequest<'q, V: Serialize> { pub query: &'q str, pub kind: GraphQlKind, pub variables: V, pub with_rate_limit: bool }
pub struct GraphQlResponse<D> { pub data: Option<D>, pub errors: Vec<GraphQlErrorCode>, pub cost: Option<QueryCost> }
impl Rest<'_> {
    pub async fn graphql<V: Serialize, D: DeserializeOwned>(&self, req: GraphQlRequest<'_, V>)
        -> Result<GraphQlResponse<D>, ApiError>;
    /// Exhaustive cursor walk over a connection selected by `page(&D) -> &PageInfo`. Queries only.
    pub async fn graphql_walk<V, D, T>(&self, …) -> Result<Vec<T>, ApiError>;
}
```

- **PG-03.** `POST {api_base}/graphql`; HTTP 200 with `errors` is not success. Error `type`s are reduced to an enum (`NotFound`, `Forbidden`, `RateLimited`, `Unknown(code-bounded)`); `message` and `path` are dropped (GK-P2). Partial `data` with `errors` is returned only if the caller sets `allow_partial`; the default is `Err` (GK-P3). `kind` sets `retry_safe` (TR-21) and `content_creating` (RL-08: mutations). L2 does not parse the document to check `kind`; a test helper `assert_kind_matches(query)` (first keyword) is offered for apps' tests.
- **PG-04.** `graphql_walk` uses `pageInfo { hasNextPage endCursor }` with `first` 1–100; same bounds and cycle detection as PG-01.
- **PG-05. Search.** A search walk whose `issueCount` exceeds 1,000 returns `ApiError::SearchTruncated{count}`: the caller must split the query (by `repo:` or `created:`), because GitHub returns at most 1,000 results (`research/dependaboard-prs.md` Q1). Installation-token search scoping is UNVERIFIED (SP-05); L2 offers `retain_installation_repos(results, &repo_ids)` as the post-filter the research recommends.
- **PG-06. Query documents live in apps.** P2's `statusCheckRollup`/`mergeable`/`mergeStateStatus`/search documents, merge-queue enqueue and auto-merge mutations, and their response types stay in P2 until a second consumer needs them (`PLAN.md` §7 F4 rule of thumb). L2 owns transport, errors, cost and paging only. `MergeableState::Unknown` handling (retry with durable sleep) is app policy (`research/dependaboard-prs.md` Q2).

## 11. Webhooks (feature `webhooks`)

The PoC verifies webhooks in the web crate with `octoevents` 0.3 (PoC `crates/ghinvite-web/src/routes/webhook.rs` L1–40), the author's own crate (`sagikazarmark/octoevents`). It already provides HMAC-SHA256 verification over the exact body, multiple secrets for rotation (`Verifier::also`, every secret evaluated so timing reveals nothing), the `X-GitHub-Delivery` ID, `EventMeta` (kind, action, installation, repository, organization, sender) and a wasm-safe receiver (`octoevents-0.3.0/src/signature.rs` L382–447, `src/meta.rs` L54–90). L2 builds on it; it does not reimplement HMAC.

```rust
pub struct WebhookRouter { /* AppSlot → octoevents::Verifier built from WebhookSecrets (GA-02) */ }
impl WebhookRouter {
    pub fn new(registry: &AppRegistry) -> Result<Self, ConfigError>;
    /// Verify for the app named by the route (`/webhooks/{app}`, P1 spec §11.5); unknown slot ⇒ UnknownApp.
    pub fn verify(&self, slot: &AppSlot, headers: &http::HeaderMap, body: bytes::Bytes) -> Result<VerifiedDelivery, WebhookError>;
}
pub struct VerifiedDelivery { pub slot: AppSlot, pub envelope: octoevents::Envelope, pub hints: UnauthenticatedHints, pub dedupe: DedupeKeys }
/// From headers, NOT covered by the signature: use for routing and logs only, never for authorization.
pub struct UnauthenticatedHints { pub delivery_id: String, pub claimed_kind: octoevents::EventKind, pub hook_id: Option<u64> }
pub struct DedupeKeys { pub delivery_id: String, pub payload_sha256: [u8; 32] }
pub mod events { /* typed, #[non_exhaustive] payload structs; `T::decode(&VerifiedDelivery) -> Result<T, WebhookError>` */ }
```

- **WH-01.** Verification uses the slot's secrets only; a delivery verified by another app's secret is rejected (P1 spec §7.7: "with the org app's secret"). Failures emit `Event::WebhookRejected{app, reason}` (§13a; per-slot metric, P1 spec §14.2) and the handler returns 401 without a body.
- **WH-02. Dedupe.** L2 computes both keys; storage of receipts is app code. `payload_sha256` defends against a captured payload replayed under a new delivery ID (the signature covers the body only; octoevents `Verifier::verify` docs, `src/signature.rs` L414–420). But legitimate repeats can be byte-identical (e.g. `membership` `added` for the same team, user and sender after a remove and re-add), so a receipt keyed on `payload_sha256` MUST be time-bounded: L2 recommends a **7-day dedupe window** [Spec decision, SPD-8] and documents that, within the window, an identical legitimate repeat is dropped. Apps whose correctness depends on repeats (P1 M2 access expiry and removal) MUST rely on a reconciling read (P1 Reconciler, P1 spec §7.7) rather than on every event arriving. `delivery_id` identifies GitHub redeliveries; whether redeliveries keep their GUID is UNVERIFIED (L1 OQ-14). Follow-up for P1 spec §7.7: §18.4.
- **WH-03. No replay window.** GitHub signs no timestamp, so L2 offers none. Apps make handlers idempotent and, where an event's effect is state-changing, confirm it with an API read (P1's absence-needs-evidence rule; L1 RC-16 for revocations).
- **WH-04. Typed payloads with kind validation.** `X-GitHub-Event`, `X-GitHub-Delivery` and the hook target headers are **unauthenticated** (octoevents `signature.rs` docs: the signature "does not cover the delivery ID, event name, or target headers"; `meta.rs` L57 `kind` "parsed from `X-GitHub-Event`"). Each typed decoder (decoded fields only, unknown fields ignored, IDs as newtypes) MUST validate the payload's **discriminating fields** for the claimed kind and return `WebhookError::Decode{what: "kind_mismatch"}` on a mismatch: (a) `action`, when the event has actions, must be in the decoder's enumerated set for that kind; (b) the kind's required top-level object must be present (`installation` for `installation`, `repositories_added`/`repositories_removed` for `installation_repositories`, `pull_request` for `pull_request`, `check_suite`/`check_run` likewise, `membership`-specific `team` + `member` + `scope` for `membership`, `ref` + `before` + `after` for `push`, `sha` + `state` + `context` for `status`); (c) objects that belong to a different kind's discriminator must be absent where they are mutually exclusive (e.g. `pull_request` absent in `check_run`). Kinds covered: `installation` (`created`, `deleted`, `suspend`, `unsuspend`, `new_permissions_accepted`, with `sender` and `permissions`), `installation_repositories`, `github_app_authorization` (`revoked`), `organization` (`member_added`, `member_invited`, `member_removed`), `membership` (P1); `pull_request` (`opened`, `synchronize`, `closed`, `reopened`, `edited`, `labeled`, `unlabeled`, `ready_for_review`, `converted_to_draft`), `pull_request_review` (`submitted`, `dismissed`; discriminator `review` + `pull_request`), `check_suite` and `check_run` (at least `completed`), `status`, `push`, `repository` (`archived`, `unarchived`, `renamed`, `transferred`, `edited`, `deleted`, `privatized`, `publicized`; discriminator `repository` without `pull_request`), and `installation_repositories` (`added`, `removed`, with `sender`) (P2, additive; `research/dependaboard-prs.md` "Design implications" 2; P2 spec §6.3, §18). **Not** `issue_comment` (P2 PD-12). Any action outside a kind's enumerated set is `kind_mismatch`. The table of discriminators per kind is in the crate and in TS-07; exact payload matching for P1's acceptance events is UNVERIFIED (SP-18). Authorization decisions use authenticated payload data only.
- **WH-05.** Body limit 2 MiB by default (the PoC's), configurable; larger bodies → `WebhookError::TooLarge` before HMAC. GitHub's payload cap is higher, so a P2 `push` with many commits may need a larger limit (UNVERIFIED sizes; measure). P2 configures 8 MiB (P2 spec §6.3). TS-17 covers the limit.

## 12. OAuth: sign-in apps, acting grants, and the L1 adapter (feature `oauth`)

### 12.1 Web flow with state and PKCE

```rust
pub struct SignIn;  pub struct Acting;          // purpose markers (ADR 0007)
pub trait Purpose: sealed::Sealed { const NAME: &'static str; fn declared(p: &AppPurposes) -> bool; }

pub struct OAuthConfig<P: Purpose = SignIn> { /* slot, client_id, client_secret, redirect_uri, web_base */ }
impl<P: Purpose> OAuthConfig<P> {
    /// Fails with OAuthConfigError::PurposeNotDeclared{slot, purpose} if the slot's AppConfig.purposes
    /// lacks P, MissingClientSecret{slot} if client_secret is None, or BadRedirectUri.
    pub fn from_registry(registry: &AppRegistry, slot: &AppSlot, redirect_uri: Url) -> Result<Self, OAuthConfigError>;
    /// state: 32 CSPRNG bytes, base64url; PKCE: 32-byte verifier, S256 challenge.
    pub fn authorize(&self, opts: AuthorizeOptions) -> Result<(AuthorizeUrl, PendingAuthorization), OAuthError>;
}
pub struct PendingAuthorization { pub state: String, pub pkce_verifier: Zeroizing<String> }  // app stores it server-side
pub fn verify_state(expected: &str, received: &str) -> bool;                                  // constant time
impl OAuthConfig<SignIn> {
    pub async fn exchange(&self, transport: &dyn HttpTransportDyn, code: &str, pkce_verifier: &str) -> Result<EphemeralUserToken, OAuthError>;
}
impl OAuthConfig<Acting> {
    /// Requires an expiring token with a refresh token; a non-expiring grant is OAuthError::NonExpiringGrant.
    pub async fn exchange_grant(&self, transport: &dyn HttpTransportDyn, code: &str, pkce_verifier: &str, now: UnixTime)
        -> Result<GrantTokens, OAuthError>;      // feature rotating-credential: GrantTokens → rotcred_core::TokenPair
}
/// Credential (kind User, bucket Client(client_id) until with_user is called). No refresh accessor,
/// no conversion into UserAccessToken or TokenPair. No Serialize/Clone/Display; zeroized on drop.
pub struct EphemeralUserToken(/* private */);
impl EphemeralUserToken { pub fn with_user(self, user: UserId) -> Self; /* re-stamps the bucket */ }
```

- **OA-01. State.** 32 bytes from `getrandom`, base64url; compared in constant time; single use; stored server-side (P1: in the session, PoC `oauth.rs` L32–71; P1 spec §11.3). The state MUST NOT carry application data (P1 spec §11.1: no Link Secret in `state`).
- **OA-02. PKCE.** `code_challenge_method=S256` is always sent; GitHub supports S256 only (`research/github-app-tokens.md` Q3). New relative to the PoC (delta D-10).
- **OA-03. No `scope`.** The PoC sends `scope=read:user read:org` (PoC `oauth.rs` L57); GitHub App user tokens carry the app's permissions, not scopes, so L2 omits it. That GitHub ignores `scope` for GitHub Apps is UNVERIFIED but low-risk; SP-19a observes it.
- **OA-04. Errors in the body.** The token endpoint answers some errors with HTTP 200 and an `error` field; L2 parses the JSON `error` whatever the status and reduces it to a bounded code from `OAUTH_ERROR_CODES` (PoC `oauth.rs` L74–125; `research/github-app-tokens.md` Q3). `error_description` is dropped.
- **OA-05. Purpose split (ADR 0007, Proposed).** A sign-in exchange returns an `EphemeralUserToken` with no refresh accessor and no conversion into an L1 `TokenPair`. An acting exchange returns `GrantTokens` that convert only into `TokenPair` (for L1 `stage()`, RC-08). `OAuthConfig::<P>::from_registry` refuses a slot whose `AppConfig.purposes` does not declare `P` (startup failure, TS-08). An app therefore can't wire its sign-in route to the acting app's grant path without changing types and configuration. Mitigates R-C5-token-cap whatever SP-14 shows.
- **OA-06. Ephemeral tokens.** `EphemeralUserToken` implements `Credential` (kind `User`) and is used directly with `Rest::new(&env, &token)` for `get_user`, `get_user_emails` and then revoked with `revoke_token` (P1 plan B0-12). Its `Drop` zeroizes; it has no `Serialize`.

### 12.2 User API

All take a `Rest` whose credential kind is `User` (`UserAccessToken` or `EphemeralUserToken`); any other kind → `ApiError::Forbidden{reason: WrongCredential}`, not sent (TR-15).

```rust
pub async fn get_user(rest: &Rest<'_>) -> Result<GhUser /* {id: UserId, login: String, …} */, ApiError>;          // GetAuthenticatedUser
pub async fn get_user_emails(rest: &Rest<'_>) -> Result<Vec<VerifiedEmail /* {email, primary, verified} */>, ApiError>; // ListUserEmails
pub async fn list_user_installations(rest: &Rest<'_>) -> Result<Vec<UserInstallation>, ApiError>;               // ListUserInstallations
pub async fn list_user_installation_repos(rest: &Rest<'_>, id: InstallationId) -> Result<Vec<RepoRef>, ApiError>; // ListUserInstallationRepos
```

- `get_user_emails` needs the "Email addresses: read" account permission (`research/github-invitations.md` §6, SP-19/SP-19a). `list_user_installations`/`list_user_installation_repos` serve P2's SSO reach check (`research/github-app-tokens.md` Q6, "Design implications → C2" item 8); P1 deletes its PoC copy (P1 spec §18.2), so L2 re-adds them (delta D-11). Both are exhaustive (PG-01) and SSO-partial-aware (PG-07).

### 12.3 App-level token and grant management (Basic auth, client ID/secret)

All take `&Rest` with a `ClientCredentials` credential (kind `ClientBasic`) and send the user token only through `SecretBody` (TR-19).

- `revoke_token(rest, token: &dyn UserTokenRef)` = `DELETE /applications/{client_id}/token` (`RevokeUserToken`; P1 sign-in callback; L1 §8.5 row 2 best-effort revoke).
- `delete_grant(rest, token: &dyn UserTokenRef)` = `DELETE /applications/{client_id}/grant` (`DeleteGrant`; P2 disconnect; L1 §10).
- `check_token(rest, token: &dyn UserTokenRef) -> TokenCheck { Valid{user_id, expires_at}, Invalid }` = `POST /applications/{client_id}/token` (`CheckToken`; "checks a token without burning failed-login limits", `research/github-app-tokens.md` Q3). Whether it distinguishes "revoked" from "expired access token" is UNVERIFIED (L1 OQ-14, owner: L2 implementer agent).
- `UserTokenRef` is a sealed trait implemented by `UserAccessToken` and `EphemeralUserToken`, with a crate-private method that writes the token into a `SecretBody`.
- `scoped_user_token(…)` = `POST /applications/{client_id}/token/scoped` is **not** in v1 (independence from later refreshes is UNVERIFIED, `research/github-app-tokens.md` C2 item 9).

### 12.4 L1 adapter (feature `rotating-credential`)

```rust
/// Implements L1's trait exactly (L1 spec §5.2):
///   #[trait_variant::make(Send)]
///   pub trait Refresher: Send + Sync {
///       async fn refresh(&self, refresh_token: &RefreshSecret, now: UnixTime) -> Result<TokenPair, RefreshError>;
///   }
pub struct GithubUserTokenRefresher { client: ClientCredentials, transport: Arc<dyn HttpTransportDyn>, timeout: Duration /* T_r, 15 s */ }
impl rotcred_core::Refresher for GithubUserTokenRefresher { /* below */ }

/// L1 ContextFn (L1 spec §8.3): fn(&CredentialKey) -> Result<EncryptionContext, StoreConfigError>.
/// Builds keyring purpose `user-token` { app_client_id: Str = namespace, user_id: Decimal = subject }.
pub fn user_token_context(key: &rotcred_core::CredentialKey) -> Result<anykms_recipes::EncryptionContext, rotcred_core::StoreConfigError>;
/// namespace = client ID, subject = canonical decimal numeric user ID (never the login).
pub fn credential_key(client_id: &ClientId, user: UserId) -> rotcred_core::CredentialKey;
```

- **OA-07. Request.** `POST {web_base}/login/oauth/access_token`, `Accept: application/json`, form body (`SecretBody`, TR-19) `client_id`, `client_secret`, `grant_type=refresh_token`, `refresh_token` (L1 spec §8.5; as `spikes/restate-refresh-rotation/src/bin/service.rs`). Counts toward RL-09.
- **OA-08. Classification table** is L1 spec §8.5's table, implemented verbatim: 2xx with `access_token` + `refresh_token` + both `expires_in`s → `Ok(TokenPair)` with `expires_at = now + expires_in` (RF-03); 2xx without `refresh_token` → `LostGrant(ProviderChangedGrantType)` after a best-effort `revoke_token` of the returned access token bounded by *T_r* and an operator event; `bad_refresh_token` (any status) → `LostGrant(BadRefreshToken)`; `incorrect_client_credentials` → `Config(InvalidClientCredentials)`; `unsupported_grant_type` → `Config(UnsupportedGrant)`; `unverified_user_email` → `LostGrant(UserActionRequired)` (UNVERIFIED that it occurs, L1 OQ-9); any other code → `Config(Other(code))`; 429 (+`Retry-After` → `retry_after_secs`), 5xx, network error, timeout, unparseable body, 2xx without `access_token` → `Retryable` (RF-01, RF-02).
- **OA-09.** The adapter constructs `TokenPair`/`RefreshSecret` only through `rotcred_core::backend` (RC-09); exact constructor names are UNVERIFIED until L1 is built. It reads the incoming `RefreshSecret` the same way.
- **OA-10.** The adapter MUST pass `rotcred-conformance::refresher_contract` against L2's recorded GitHub token-endpoint fixture (RF-05). Its errors and `Debug` contain no token or body text (RF-05 asserts it; also TS-01 here).
- **OA-11. Timeout** *T_r* via the transport's per-request timeout (host timer on wasm32, RF-04).
- **OA-12. Disabled expiry.** An acting app with "Expire user authorization tokens" off would return non-rotating tokens; `exchange_grant` refuses them (OA-05) and the refresher maps the same case at refresh time per OA-08 row 2.

## 13. Error taxonomy

```rust
#[derive(Clone)] #[non_exhaustive] pub enum ApiError {
    Unauthorized,                                     // 401
    Forbidden { reason: ForbiddenReason },            // 403 that is not a rate limit (RL-01); also local refusals (not sent)
    NotFound,                                         // 404 (also "no access", GitHub conceals)
    Conflict,                                         // 409 (e.g. merge sha mismatch)
    NotAllowed,                                       // 405 (e.g. PR not mergeable)
    Gone,                                             // 410
    Moved { status: u16 },                            // 3xx, never followed (TR-17); 0 = redirect detected after the fact
    Validation { codes: Vec<ValidationCode>, message_len: usize },   // 422, codes bounded (PoC redact)
    Spammed,                                          // 422 anti-spam (RL-04)
    RateLimited { scope: RateLimitScope, retry_after: Option<Duration>, resource: Option<Resource> },
    SearchTruncated { count: u32 },                   // PG-05
    Partial { reason: PartialReason, org_ids: Vec<OrgId> },          // PG-07
    Server { status: u16 },                           // 5xx
    Ambiguous,                                        // request sent, no response (timeout/reset): outcome unknown
    NotSent,                                          // transport failed before sending
    Decode { what: &'static str },                    // shape/oversize/path; never quotes the body
    Status { status: u16 },                           // any other status
}
#[derive(Clone)] #[non_exhaustive] pub enum ForbiddenReason { Permission, SsoRequired, IpAllowList, WrongCredential, Disabled, Other }
#[derive(Clone)] #[non_exhaustive] pub enum PartialReason { SsoRequired }
#[derive(Clone)] #[non_exhaustive] pub enum MintError { AppAuth(AppAuthError), InstallationGone, InstallationSuspended,
    MintForbidden { reason: ForbiddenReason }, PermissionsNotGranted,
    RateLimited { scope: RateLimitScope, retry_after: Option<Duration> },
    Transient { kind: TransientKind /* Server{status} | NotSent | NoResponse */ }, Unexpected { status: u16 }, Decode }
pub enum CallError<E = std::convert::Infallible> { Api(ApiError), App(E) }      // impl<E> From<ApiError> for CallError<E>
#[non_exhaustive] pub enum BrokerError<E = std::convert::Infallible> { Mint(MintError), Call(CallError<E>), Unauthorized }
#[non_exhaustive] pub enum OAuthError { Provider { code: String /* bounded */ }, StateMismatch, NonExpiringGrant, Api(ApiError) }
#[non_exhaustive] pub enum OAuthConfigError { PurposeNotDeclared { slot: AppSlot, purpose: &'static str }, MissingClientSecret { slot: AppSlot }, BadRedirectUri }
#[non_exhaustive] pub enum WebhookError { UnknownApp, BadSignature, MissingHeader, TooLarge, Decode { what: &'static str } }
```

- `BrokerError` no longer duplicates `ApiError` variants: every mint outcome is under `Mint`, every closure outcome under `Call`, and `Unauthorized` means only "second 401 inside `with_token`" (BR-19). `BrokerError<E>: Clone` when `E: Clone`.
- **ER-01. Stable codes.** Every error has `fn code(&self) -> &'static str` in the grammar `gh:<class>(:<detail>)*` where each segment matches `[a-z0-9_]+` (e.g. `gh:api:rate_limited:secondary`, `gh:mint:permissions_not_granted`, `gh:call:app` for `CallError::App`, whose own code is the app's business). These codes are what apps may journal or put in Restate terminal errors; they contain no secret and no upstream text. Codes are append-only (as L1 CM-04): the full list is committed as `gh-toolkit/error-codes.txt`, and CI fails if a line is removed or changed (TS-18).
- **ER-02. Retryability** is defined per variant, not guessed by apps. `ApiError::is_transient_for(&self, meta: &EndpointMeta) -> bool` (and `is_transient_for_retry_safe(bool)` for the generic call):

| Variant | Transient? |
|---|---|
| `Server{..}`, `NotSent` | true |
| `RateLimited{..}` | true, **after** waiting `suggested_wait()` (RL-01); callers MUST NOT retry earlier |
| `Ambiguous` | `meta.retry_safe` (TR-21). False for non-retry-safe writes: the outcome is unknown and must go through the app's fence and read-back (R-ALL-github-write-fence; P1 spec §7.3 S5 `OutcomeUnknown`) |
| `Moved`, `Unauthorized`, `Forbidden`, `NotFound`, `Conflict`, `NotAllowed`, `Gone`, `Validation`, `Spammed`, `SearchTruncated`, `Partial`, `Decode`, `Status` | false |

  `MintError::is_transient()`: true for `Transient{..}` and for `RateLimited` (after the wait); false otherwise. `BrokerError::is_transient()` delegates to `Mint` or, for `Call(Api(e))`, needs the endpoint and is therefore not offered (apps call `e.is_transient_for(..)`); `Unauthorized` is false.
- **ER-03. `ForbiddenReason::SsoRequired`** is set when a 403 carries the `X-GitHub-SSO` header (`research/github-app-tokens.md` SP-15 "Record … `X-GitHub-SSO`"); a 2xx with `X-GitHub-SSO: partial-results` is PG-07, not ER-03. Exact header semantics UNVERIFIED until SP-15. `IpAllowList` is detected from the documented 403 wording once observed (UNVERIFIED text, P1 spec §7.4). `WrongCredential` and `Disabled` are local refusals (never sent). A 403 is never a revocation signal (L1 RC-12).
- **ER-04. Ambiguity mapping.** `TransportError::SentNoResponse` → `Ambiguous`; `NotSent` → `NotSent`; `Oversize` → `Decode{what: "oversize"}`. With reqwest the boundary is: connect errors are `NotSent` natively; on wasm32 fetch exposes no such distinction, so every wasm32 transport failure is `Ambiguous` (conservative; PoC `transport_failure` comments).
- **ER-05.** The PoC's `Error::Status{status, body: String}` keeps a redacted summary string (PoC `error.rs`); L2 keeps only `codes` and `message_len` in typed fields (delta D-06). `Display` for every error prints the code and status only.
- **ER-06. Clone.** `ApiError`, `MintError`, `AppAuthError` and every enum they contain derive `Clone` (BR-11 requires it of `MintError`). None holds a secret, so `Clone` is safe.

## 13a. Observability

```rust
#[non_exhaustive] #[derive(Clone, Debug)]
pub enum Event {
    Mint { app: AppSlot, app_id: AppId, outcome: &'static str /* ER-01 code or "ok" */, latency_ms: u32, coalesced: bool },
    CacheHit { level: CacheLevel /* Isolate | Shared */ },
    SigningKeyFallback { app: AppSlot, kid: String },
    ClockSkew { secs: i64 },
    SharedCacheError { kind: &'static str },
    WebhookRejected { app: AppSlot, reason: &'static str },
    RateLimited { bucket: QuotaBucketLabel, scope: RateLimitScope },
    OAuthBudget { app: AppSlot, level: BudgetLevel /* Warn | High */, count: u32 },
    RedirectFollowed { status: u16 },
}
pub trait EventSink: Send + Sync { fn emit(&self, e: &Event); }
pub struct TracingSink;   // default
```

- **OB-01.** The default `TracingSink` emits one `tracing` event per `Event` at target `gh_toolkit`, level `info` (`warn` for `SigningKeyFallback`, `ClockSkew`, `SharedCacheError`, `OAuthBudget`, `RedirectFollowed`), with field `event` = the snake-case variant name and one field per variant field under the same name. Field names are part of the public API (append-only, like ER-01).
- **OB-02.** No `Event` carries a secret, a URL, a login, or upstream text; `QuotaBucketLabel` is the bucket kind plus numeric IDs. TS-01 captures every event.
- **OB-03.** BR-17's trigger, RL-09's budget, SD-L2-a and P1 spec §14.2's operational signals are computed from these events.

## 14. Testing

| ID | Test | Where | Source |
|---|---|---|---|
| **TS-01** | Credential disclosure: drive every endpoint, mint, OAuth exchange, refresh, revoke/check/grant call, and webhook path through `MockTransport` with marker secrets (`ghs_MARKER…` 640 chars, `ghu_`, `ghr_`, a client secret, a PEM, a webhook secret, an OAuth `code`, a `state`, a PKCE verifier); markers also inside every `SecretBody`; a 307 to a foreign host (TR-17); capture `tracing` output, every `Event`, and every `Display`/`Debug` of requests, results and errors; any marker fails | `gh-toolkit/tests/credential_disclosure.rs` | PoC `tests/credential_disclosure.rs`; P1 AC-SEC-4 |
| **TS-02** | Compile tests (trybuild): `InstallationToken`, `InstallationRest`, `UserAccessToken`, `EphemeralUserToken`, `AppJwt`, `ClientSecret` are not `Serialize`/`Clone`/`Display`; `ReadOnlyBroker::ensure` rejects a write `TokenKey`; `ScopeProfile::read` with a Write entry fails const evaluation. **Must-compile** cases: `closure_borrows_rest_across_await` (§6.3), the same with `call`, and with the `InstallationClient` facade; `KvTokenStore` plugs into `Broker::with_shared_cache` on wasm32 (built in `gh-toolkit-worker`) | `tests/ui/`, `tests/compile_pass/` | ADR 0003 decision 1; P1 AC-BRK-5; P1 plan B0-04 |
| **TS-03** | Broker: one case per BR-19 row; 401 → re-mint → success; 401 → re-mint → 401 terminal (AC-BRK-1); boundary `expires_at` triggers re-mint (AC-BRK-2); 422 → `PermissionsNotGranted`, pre-check fails with no mint (AC-BRK-4); native single-flight: 20 concurrent `ensure` → 1 mint; **20 concurrent `with_token` with an `invalidate_key` in between never use a freed token and each completes or gets a defined error**; `invalidate` with a stale generation keeps the newer token; `invalidate_installation` clears every profile, positive and negative; negative cache voided by a newer `GrantedSnapshot.version` and bypassed by `ensure_fresh`; mint 401 → skew re-sign → key fallback order; mint 403 + `suspended_at` → `InstallationSuspended`; mint 403 without it → `MintForbidden`; mint 403 secondary limit → `RateLimited` (RL-01 first); every §6.6 row (AC-L2-02); cache keyed by `AppId`: two registries reusing slot names don't share tokens | unit, `MockTransport` + `FakeClock` | ADR 0003 acceptance; P1 spec §10.2 |
| **TS-04** | JWT: verify with `jsonwebtoken` (dev-dep only, as PoC); PKCS#1 and PKCS#8 inputs; non-2048-bit key rejected; claims at skew 0, +45 s, −45 s, +61 s, −9.5 min, +5 min; skew bounded to ±10 min; cache reuse < 8 min | unit | PoC `jwt.rs` tests; GA-03…GA-07 |
| **TS-05** | Rate-limit classification table (PoC `transport.rs`/`error.rs` cases) plus property tests over header/body combinations; pre-flight blocks only the exhausted bucket (RL-02); RL-09 counter thresholds; `WriteLedger`: window arithmetic, single-ledger atomicity, `upsert_bucket` cap raise/lower, idempotent `request_key`, `count_tagged` share (AC-GOV-1 shape), `record_limit` (AC-GOV-2 shape), spacing, GC (RL-10), and two-ledger hold/commit including **a hold that expires after a simulated crash between the two holds** | unit, `proptest` | P1 plan B1-01; P1 spec §7.5 |
| **TS-06** | Pagination: cycles, off-base `Link`, page bound, partial failure ⇒ `Err`; `X-GitHub-SSO: partial-results` ⇒ `Partial` / `allow_partial` listing; GraphQL cursor walk; `SearchTruncated` | unit | PoC `pagination.rs` |
| **TS-07** | Webhooks: signature vectors (good, bad, rotated secret, other app's secret), dedupe keys stable across header changes, typed decode of recorded payloads, **kind mismatch** (a signed `membership` body sent with `X-GitHub-Event: organization`, and every discriminator row of WH-04) ⇒ `Decode{kind_mismatch}` | unit | octoevents tests; P1 AC-ACC-5 |
| **TS-08** | OAuth: authorize URL contains S256 challenge and no `scope`; state mismatch; error-in-200 bodies; sign-in vs acting type split; `from_registry` refuses an undeclared purpose and a missing client secret; revoke/grant/check request shapes (Basic auth, `SecretBody`) | unit + stub | PoC `tests/oauth_url.rs` |
| **TS-09** | L1 adapter: `rotcred-conformance::refresher_contract` over the recorded fixture; RF-03 arithmetic; RF-04 timeout | `--features rotating-credential` | L1 RF-05 |
| **TS-10** | Stub contract: the axum stub server (PoC `stub.rs`, feature `test-stub`) serves recorded request/response pairs (status, headers incl. rate-limit and `X-GitHub-SSO` headers, body shape) for every `Endpoint` variant and for `OAuthTokenEndpoint`; each pair names its source (docs URL or spike ID, e.g. SP-01 no-seat text, SP-08 "spammed" body, SP-18 payloads); includes a 307 to a foreign host | `gh-toolkit/stub-contract/*.json` | P1 plan §5.3 |
| **TS-11** | Endpoint metadata table test: every `Endpoint` has a permission (or `None` with a reason), allowed credential kinds, `content_creating` for every mutating REST call and all GraphQL mutations, `retry_safe` with a reason (TR-21); `repo-admin` variants absent without the feature; TR-18 deny-list refuses each entry via the generic call without the feature and passes it with the feature; `PathTemplate` encodes `/`, `.`, `%`, `..` segments | unit | P1 AC-BRK-3 (toolkit half), AC-SEC-3 |
| **TS-12** | wasm32: `cargo build --target wasm32-unknown-unknown -p gh-toolkit` for default and each production feature; `wasm-bindgen-test` (node runner) for JWT signing, redaction, pagination, ledger; a workerd smoke (mint + one GET against the stub through `wrangler dev --local`) lives in consumers (P1 `tests/worker`) because it needs a Worker; the runner choice for in-crate wasm tests is UNVERIFIED (SD-L2-d) | CI | PoC `wasm_smoke.rs`; P1 plan §5.1 T5 |
| **TS-13** | Live (manual, gated, never in default CI): against the GitHub test org L and the staging apps, never production apps (P1 plan §5.4): mint, one read per profile, one invite-free write, webhook redelivery | `--features live-tests` | SP-16, SP-23 hooks |
| **TS-14** | Boundary: `cargo tree` deny-list (TR-01), no P1/P2 vocabulary in `cargo public-api` output (R4), public-API snapshot committed, `#![deny(missing_docs)]` (R10), `unsafe` only in `gh-toolkit/src/wasm_send.rs` and none in `gh-toolkit-worker` (TR-03) | CI | P1 plan §6 R1–R5, R10 |
| **TS-15** | Config parsing and startup validation: every GA-01 rule; `SigningKeys::parse`/`WebhookSecrets::parse` malformed inputs ⇒ error naming field and slot, no key bytes in `Display`/`Debug`; `OAuthConfig::from_registry` refusals (OA-05); `RestEnv::for_slot` with unknown slot | unit | P1 plan B0-03; GA-01, GA-02 |
| **TS-16** | Shared cache (`--features shared-cache`, in-memory `SharedTokenStore`, test keyring): seal/open round trip; wrong context field ⇒ miss + `Event::SharedCacheError`; `delete_if` keeps a newer entry; store error ⇒ miss, call succeeds; put skipped under 60 s remaining | unit | BR-14…BR-16 |
| **TS-17** | Headers and limits: exact header set per endpoint incl. per-endpoint `x-github-api-version` (TR-05); OAuth endpoint headers; 10 MiB response cap and 64 KiB error-inspection cap (TR-07); webhook default 2 MiB and configured 8 MiB boundaries (WH-05); no redirect followed (TR-17) | unit | TR-05, TR-07, TR-17, WH-05 |
| **TS-18** | Error-code snapshot: `error-codes.txt` generated from every variant; grammar check (ER-01); CI diff fails on any removed or changed line | CI | ER-01 |

Toolkit tests use only toolkit fixtures; no test imports a consumer crate (P1 plan §6 R6).

**Acceptance checks for L2 v0.1 (extraction release).**
- AC-L2-01: TS-01, TS-02 green with no allow-list.
- AC-L2-02: TS-03 covers every row of §6.6 and of BR-19.
- AC-L2-03: TS-12 builds green for every feature combination in §4 except `test-stub` on wasm32.
- AC-L2-04: P1 builds and passes its full CI on the extracted crate with no source change beyond imports (EX-07).
- AC-L2-05: the §16 table has no open "breaking" row.
- AC-L2-06: every requirement ID of the form `GA-nn`, `BR-nn`, `TR-nn`, `RL-nn`, `PG-nn`, `WH-nn`, `OA-nn`, `ER-nn`, `OB-nn` in this spec appears in at least one test function name or doc-test comment (`// covers: BR-11`), checked by `scripts/req-coverage.sh` (greps this spec for IDs and the crate for `covers:` tags) in CI. Exempt IDs are listed in the script with a reason (e.g. RL-03 is a non-behaviour, checked by a grep for `sleep`).

## 15. Restate usage contract (documentation, not code)

L2 has no Restate dependency, so these rules are documented in the crate docs and enforced by consumers' tests (P1 AC-JRN-1; L1 CF-08):
1. Mint and use inside one `ctx.run`; return a business enum. Never return, log, `ctx.set`, or put in a run name anything holding a credential, including `InstallationRest` (ADR 0003).
2. For writes: `ensure` before the fence, `call` once, app-driven 401 handling (BR-07).
3. On `RateLimited{retry_after}`: return a journaled outcome from the run, then `ctx.sleep` outside it; record the limit in the app's `WriteLedger` VO with an `until` the app chooses (RL-01, RL-03).
4. Error codes (ER-01) are the only GitHub-derived text that may enter the journal.
5. User tokens: the L1 caller flow (L1 §7.2), with L2 calls inside each `ctx.run`.
6. On wasm32, construct the `Broker` per request context (per Restate invocation or web request); the token cache is still isolate-wide (BR-11).
7. Cross-VO write reservations use the RL-06 hold/commit protocol.

## 16. P2 API check (ADR 0005 consequences; P1 plan §10 "list of P2 needs")

Rating: **fits** (no change), **additive** (new items, no break), **breaking** (P1 code must change).

| P2 need | Evidence | L2 answer | Rating |
|---|---|---|---|
| Viewer and merger apps plus a sign-in app | `research/dependaboard-prs.md` "Design implications" 6; feasibility §2.2 change 5 | `AppRegistry` with N slots and declared purposes (GK-P4, OA-05); per-slot webhooks | fits |
| Reads with installation tokens (PRs, repos, rollups) | dep Q1–Q2; tok "Design implications → C1" 7 | Broker with read profiles, e.g. `PR_READ = [pull_requests:read, checks:read, statuses:read, metadata:read]` (app-declared) | fits |
| **User-token calls**: approve (`POST …/reviews`), merge (`merge-async`), `@dependabot rebase|recreate` comment | dep Q3–Q4; tok Q5 | `Credential` trait and `Rest<'_>` are implemented and P1 consumers migrated in place; typed review, merge and comment functions accept user credentials (TR-15); 401 → caller's RC-12 loop (TR-16) | fits (D-01 implemented before extraction) |
| GraphQL search, `statusCheckRollup`, `isRequired`, `mergeable`, `mergeStateStatus` | dep Q1–Q2 | `graphql` feature, cursor walk, `SearchTruncated`, installation post-filter (§10.2); documents stay in P2 (PG-06) | additive |
| Merge-queue enqueue and auto-merge GraphQL mutations | feasibility §2.2 change 1 | `GraphQlKind::Mutation` ⇒ `content_creating` (RL-08) and not retry-safe (TR-21); documents stay in P2 (PG-06) | additive |
| `merge-async` submit and poll (`GET …/merge-async/{uuid}`), sync merge 405/409 | dep Q3 | Endpoints `MergeAsync`, `GetMergeAsync` with per-endpoint API version (TR-05); `NotAllowed`, `Conflict` errors | additive |
| PR comments as the user | tok Q5; SP-03 | `CreateIssueComment` accepting a user credential; attribution and Dependabot acceptance are SP-03's question, not L2's | additive |
| Webhooks: `pull_request`, `pull_request_review`, `check_suite`, `check_run`, `status`, `push`, `repository`, `installation_repositories` (not `issue_comment`, P2 PD-12) | dep "Design implications" 2; P2 spec §6.3, §18 | WH-04 typed payloads with kind validation | additive |
| `github_app_authorization` revocation webhook | tok Q4; L1 §10 | WH-04; app calls L1 `revoke(RevokeIf::InstalledBefore)` | additive |
| Per-installation **and** per-user rate governor: 80 writes/min, 500/h, ≥ 1 s spacing; search 30/min | dep Q6; link-security §2.3 | Generic `WriteLedger` with hold/commit across VOs or one ledger per app (RL-06), `caps`, `content_creating` metadata (RL-08) is implemented; P1's Invite Budget wraps it with app-owned invite buckets | fits (D-05 implemented before extraction) |
| OAuth-token budget shared by mints, acting exchanges and refreshes | tok Q1 (2,000/h); L1 RC-14 | RL-09 counter and `Event::OAuthBudget`; SD-L2-e | additive |
| Acting-grant OAuth (connect once, re-auth only on loss) | ADR 0007 decision 4; L1 RC-08 | `OAuthConfig::<Acting>::from_registry` + `exchange_grant` → `TokenPair` → L1 `stage` (OA-05) | additive |
| L1 `Refresher` adapter and `user_token_context` | L1 §8.5, ST-10 | §12.4 | additive |
| Disconnect: `DELETE /applications/{client_id}/grant`; token check | tok Q3; L1 §10, OQ-14 | §12.3 | additive |
| SSO reach: `GET /user/installations`; SSO-partial listings | tok Q6; SP-15 | §12.2; PG-07; `ForbiddenReason::SsoRequired` (ER-03) | additive (P1 deletes its PoC copy, D-11) |
| Workflows permission for `github-actions` PRs | dep Q3; SP-10 | `Permission::Workflows`; profile choice is P2's | fits |
| Rulesets / branch protection reads (`isRequired` covers most) | dep Q2, "Design implications" 6 | GraphQL; optional `Administration: read` endpoint later (reads are not on the TR-18 deny-list) | additive |
| Long fan-out over many installations within Workers limits | dep Q6; R-P2-fanout-limits | `for_each_page` streaming (PG-02); no per-isolate concurrency cap needed below GitHub's 100 concurrent | fits |
| Conditional requests for polling | GitHub docs (UNVERIFIED for rotating tokens) | TR-11 opt-in | additive |
| Installation availability state machine (P1 `availability.rs`) | P1 spec §10.1; P1 plan §10 | Stays in apps for v1: P2's index is driven by `installation_repositories` events, a different machine (`InstallationSync`, P2 spec §10.1; confirmed by P2 spec §18). L2 provides the primitives (`GetInstallation`, exhaustive `ListInstallationRepos`, permission snapshot) | fits |

**P3 check (lighter).** Native CLI, no Restate: the broker works without Restate and the isolate cache is a process static keyed by `AppId` (fits; profile switching cannot mix apps). Operator-supplied user tokens: `UserAccessToken::from_operator` (fits). Repo settings and rulesets writes: `repo-admin` feature, which also lifts TR-18 (additive). Device flow for a CLI sign-in: not in v1 (OQ-6).

Implementation check, 2026-10-17: the two conditional breaking rows above now fit
the registered P1 interfaces. The additive P2 REST, GraphQL, webhook, Acting OAuth
and actual L1 refresher interfaces are implemented in tree. Evidence and remaining
extraction gates are recorded in
[`repository-protocol-verification.md`](../../implementation/ghinvite/docs/repository-protocol-verification.md).
This API fit check does not establish release, staging or full L1 executor
conformance.

## 17. Alignment with P1 (names and deltas)

P1 names that L2 keeps **unchanged in name and meaning**: crate `gh-toolkit`; `ScopeProfile`; `Permission`, `Access`; `InstallationToken` (now never handed to closures, D-17); `Broker` with `ensure`, `call`, `invalidate`, `with_token`, `read_only()`; `HttpTransport`, `ReqwestTransport`, `MockTransport`, `Request`, `Response`, `Method`; `RepoRole` (`rank()`, `Custom(String)`), `OrgRole`, `TeamRole`, `MembershipState`; `InstallationClient`; `OAuthConfig`, `AuthorizeUrl`; P1 profile constants `REPO_READ`, `REPO_INVITE`, `ORG_READ`, `ORG_INVITE` (in P1 code). Names kept with a **changed shape** are listed as deltas: `TokenKey` (D-17), the broker closure and `invalidate` signatures (D-17), `BrokerError`/`CallError` and P1 spec §10.1's error classes (D-18), `AppJwtSigner` (D-19).

| # | Delta (L2 target vs P1 spec/plan or PoC) | Affects | Cheapest time to do it | Kind |
|---|---|---|---|---|
| D-01 | Typed endpoints take `&Rest<'_>` over a sealed `Credential` trait instead of an installation token (TR-15) | P1 plan B0-05 | In B0-05 (same effort); later it is breaking | API shape |
| D-02 | `ScopeProfile::read`/`write` const constructors; `ReadOnlyBroker` accepts `ReadKey` only (implements P1's "refuses write profiles at compile time", P1 spec §20.1) | B0-04 | In B0-04 | type detail |
| D-03 | `Method::Patch` (PoC has Get/Post/Put/Delete; P1 needs `PATCH …/invitations/{id}`, P1 spec §7.2) | B0-05 | B0-05 | additive |
| D-04 | Isolate-wide static cache as the **default**, keyed by `AppId`, holding `Arc<InstallationToken>`; `with_cache` for tests (BR-09); `invalidate(key, generation)`, `invalidate_key`, `invalidate_installation`, `ensure_fresh`, negative entries voided by snapshot version (BR-12) | B0-04 | B0-04 | behaviour |
| D-05 | Governor primitive named `WriteLedger` with `upsert_bucket`, tagged and idempotent `reserve`, `hold`/`commit`, `release`, atomic subset `refund_buckets`, `record_limit`, `count_tagged`, `snapshot` (RL-06, RL-07, RL-10); P1's `InviteBudget` VO wraps it | B1-01, B1-02 | B1-01 | API shape |
| D-06 | Errors keep enumerated codes in typed fields, no summary string (ER-05); split `Ambiguous` vs `NotSent` (ER-04); name the enum `ApiError` | B0-06 | B0-06 | API shape |
| D-07 | `GhInstallationToken` (PoC `payloads.rs`) derives `Serialize` with `token: String`; L2 removes it (GK-P1) | B0-04 | B0-04 | safety |
| D-08 | `user-agent` from `AppConfig` instead of hard-coded `ghinvite` (PoC `util.rs`) | B0-03 | B0-03 (also an R4 vocabulary fix) | config |
| D-09 | JWT `iss` = client ID when configured (GA-03); skew estimation (GA-07); key fallback on mint 401 (GA-05) | B0-03 | B0-03 or at extraction | behaviour |
| D-10 | PKCE S256 and no `scope` in the authorize URL (OA-02, OA-03) | B0-07, B0-12 | B0-07 | behaviour |
| D-11 | `list_user_installations`, `list_user_installation_repos` kept in L2 although P1 deletes its use (P1 spec §18.2 row `oauth.rs`) | B0-12 | keep the functions in the crate, delete only P1's call sites | scope |
| D-12 | P1's "no `DELETE`/`PATCH /repos/{o}/{r}` variant" (AC-SEC-3) becomes "only under feature `repo-admin`, plus a runtime deny-list in the generic call (TR-18)"; P1 CI asserts the feature is off | B0-05 CI | at extraction | CI rule |
| D-13 | Webhook verification lives in `gh-toolkit` as `WebhookRouter` over `octoevents` (P1 plan B0-07 already puts it in the toolkit; the PoC has it in the web crate); typed decoders validate the event kind (WH-04) | B0-07 | B0-07 | placement |
| D-14 | `OAuthConfig<P = SignIn>` purpose marker built with `from_registry`; acting exchange separate (OA-05). Default parameter keeps P1 call sites close; P1 adds `purposes` to its config | B0-03, B0-07 | B0-03 | additive |
| D-15 | `gh-toolkit-worker` adapter crate (Worker fetch, KV/D1 token stores, WebCrypto signer) | none for P1 | at extraction, only if needed | layout |
| D-16 | `cache_scope = name@fingerprint` for cache keys and the keyring `scope` field (BR-06) | none in P1 (no shared cache) | at extraction | detail |
| D-17 | `TokenKey.app`: `AppId` (P1 spec §10.2) → `AppSlot` (product handle; resolved to `AppId` for cache keys, BR-09); new `ReadKey`; closures receive an owned `InstallationRest` instead of `&InstallationToken`; `call` returns `BrokerError<E>` and invalidates on 401 itself; `invalidate` takes a `TokenGeneration` | P1 spec §10.2; P1 plan B0-04, B0-05; P1 plan R9 | B0-04 | API shape |
| D-18 | Error shapes vs P1 spec §10.1: `SecondaryLimited` → `RateLimited{scope: Secondary, ..}`; `RateLimited{retry_after, reset}` → `reset` dropped (folded into `retry_after`, RL-01); `Validation{code, message_kind}` → `Validation{codes, message_len}`; `Timeout/Ambiguous` → `Ambiguous` + `NotSent`; `Transient` → `Server{status}` (API) and `MintError::Transient{kind}` (mint); new `Moved`, `Partial`, `Decode`, `Status`; `BrokerError{InstallationGone, InstallationSuspended, PermissionsNotGranted}` → `MintError::…` under `BrokerError::Mint`, plus `MintForbidden`; `CallError` generic over the app error | P1 spec §10.1, §10.2, §7.3 S5; P1 plan B0-04, B0-06 | B0-06 | API shape |
| D-19 | `AppJwtSigner` (P1 name) → `JwtSigner` trait (signature `Vec<u8>`) + `AppJwtSource` (cache, skew) | P1 plan B0-03 | B0-03 | rename |
| D-20 | `retry_safe` in `EndpointMeta`; `is_transient_for(&EndpointMeta)` replaces a method-based rule (TR-21, ER-02) | P1 plan B0-05, B0-06 | B0-05 | API shape |
| D-21 | gh-toolkit2 rewrite (`implementation/gh-toolkit2`, 2026-10; renamed `implementation/octoauth`, crates `octoauth` and `octotoken`, ADR 0014): the broker (§6.3) is replaced by a consumer-facing `InstallationTokenProvider` trait (`token(installation, &TokenScope)`, `replace(&rejected)`) in crate `gh-token-provider` (now `octotoken`), first implementation `AppTokenProvider` (in-memory cache). Departures: `InstallationToken` is `Clone` (internal `Arc`, one zeroized secret; still not `Serialize`/`Display`) and handed to consumers, who attach it via `expose_secret()` (BR-01 / AC-L2-01, §6.3); re-mint on 401 is the consumer's `replace` call, not the broker's (ADR 0003 decision 4, amended); futures are `Send` natively but not on wasm32 (§4.1, TR-20); concurrent mints are combined natively only, never across Worker requests (consistent with BR-11). | ADR 0003 (amended 2026-10-20) | gh-toolkit2 | API shape |
| D-22 | gh-toolkit2 scope (ADR 0014): GitHub App auth and installation tokens only, with no GitHub API client. Consumers choose their client (octocrab or other) and attach tokens themselves. Dropped from L2 v1 scope (§2 items 3–8): transport seam, governor, quota observation, conditional requests, typed REST endpoints, `Link` and GraphQL pagination, GraphQL, the `MockTransport` and stub. Webhooks (§11) use octoevents directly, with no toolkit wrapper. OAuth (§12) and redaction become later crates (`gh-oauth`, `gh-redact`; named `octologin`, `octoscrub` by the ADR 0014 amendment, as `gh-toolkit2` and `gh-token-provider` became `octoauth` and `octotoken`). Added: a public response classifier over status, headers and message, so any client can reuse rate-limit and 403 classification. GK-P2 (no upstream text) binds the toolkit's own calls; for API calls it is the consumer's obligation (ADR 0003 decision 1). | ADR 0014 | gh-toolkit2 | scope |
| D-23 | gh-toolkit removed (2026-10-20). ghinvite uses `octoauth`, `octotoken`, `octologin` and `octoscrub`, plus its own `ghinvite-github` (transport, REST layer, endpoints and stub, moved from gh-toolkit). Legacy OAuth is removed; typed sign-in is the only sign-in path. See `plans/ghinvite-toolkit-migration.md` | ADR 0014 | ghinvite | removal |

D-01, D-02, D-04, D-05, D-06, D-07, D-10, D-17, D-18, D-19 and D-20 are cheapest inside the P1 tasks named; this spec recommends the P1 build plan adopt them there (follow-up edit, §18.4). None changes P1 behaviour that P1's acceptance checks pin, except that P1's recheck path must call `ensure_fresh` (BR-12), which P1 spec §10.2's "never loops on 422" already implies.

## 18. Extraction plan, versioning, open questions

### 18.1 Extraction steps (run after P1 M2; `PLAN.md` §9 step 3)

**Local sequencing override, 2026-10-17:** the user explicitly authorized the
canonical extraction and P1 switch now. Per implementation decision ID-69, the
actual local workspace is `implementation/gh-toolkit`, with P1 consuming its
version-constrained sibling crates. The production-readiness observations,
tag/publication, history evidence and deployed verification below retain their
separate acceptance requirements; a local source move does not satisfy them.

- **EX-01. Gate.** Every item of P1 plan §10 "Ready for L2 extraction" holds, including the P2 checklist of §16 with each row rated.
- **EX-02. Break in place first.** Any row still rated "breaking" is fixed **inside the P1 workspace** (one PR per delta, P1 CI green) before the move, so the move itself carries no API change.
- **EX-03. Move.** `git filter-repo --path crates/gh-toolkit` into the new repository (name: OQ-1), preserving history. Add `README.md`, `CHANGELOG.md`, LICENSE, `deny.toml` (with the GA-06 advisory rationale), the stub contract, `error-codes.txt` and the public-API snapshot.
- **EX-04. CI in the L2 repo.** Required jobs: native tests; wasm32 build per feature set (TS-12); boundary deny-list (TR-01) and vocabulary grep (R4) for **every** consumer's vocabulary (`Link`, `Delivery`, `Invitation Link`, `Dependaboard`, `Merge Job`, …); public-API snapshot diff; credential disclosure and trybuild; `cargo deny`; `repo-admin` grep and TR-18 test (TS-11); error-code diff (TS-18); requirement coverage (AC-L2-06); rustdoc `missing_docs`.
- **EX-05. Publish `0.1.0`** as a git tag (crates.io optional; OQ-2). Semver from then on: while `0.x`, a minor bump may break; every break gets a CHANGELOG "Migration" entry. Public enums are `#[non_exhaustive]` (R8), so adding errors, endpoints, events or permissions is not breaking.
- **EX-06. Switch P1.** One PR in the P1 repo: replace the path dependency with `gh-toolkit = { git = "…", tag = "v0.1.0" }` (or version), delete `crates/gh-toolkit`, replace `scripts/check-toolkit-boundary.sh` with a check that no in-tree `gh-toolkit` reappears and that `repo-admin` is off (D-12). Expected diff: `Cargo.toml` and imports only (P1 plan §10 last item).
- **EX-07. Verify.** All P1 CI gates green (P1 plan F-7, §5.2), AC-JRN-1 marker scan and AC-SEC-4 re-run, then a staging deploy (P1 plan B0-22 environment) with one AC-LIVE-1 style delivery.
- **EX-08. P2 builds on L2 `0.2`**: `graphql`, `rotating-credential`, P2 endpoints and webhook payloads land as additive releases driven by the P2 spec. The L1 adapter is built at this point (ADR 0005 item 4), after L1 v0.1 exists.
- **EX-09. Rollback.** If EX-07 fails, revert the EX-06 PR; the in-tree crate is still in P1's history and nothing else changed.

### 18.2 Pending spikes and proposed desk spikes

Owner roles: **user** (the author), **L2 implementer** (the agent building `gh-toolkit` in P1 B0 tasks or at extraction), **P1 operator** (whoever runs P1 production metrics), **P2 planner** (the agent writing the P2 spec).

| Spike | Question | Effect on L2 | Owner / due |
|---|---|---|---|
| **SP-14** | Ten-token cap for GitHub App user tokens | None on the API: OA-05 separates sign-in from the acting grant either way (ADR 0007). A "cap applies" result makes OA-05 load-bearing and adds a doc warning to `OAuthConfig<Acting>` | per `analysis/spikes-pending.md` |
| **SP-15** | SSO: refreshed tokens keep org access? missing session hides the org? **Added check:** does a user-token list call return 2xx with `X-GitHub-SSO: partial-results`? | Confirms ER-03 and PG-07 and whether `list_user_installations` is the right reach check. No refresher change (L1 OQ-3) | per `analysis/spikes-pending.md`; added check due before EX-08 |
| **SP-16** | Deployed Restate-on-Workers; App JWT CPU < 10 ms (pure `rsa` vs WebCrypto); VO latency | GA-06: > 10 ms → WebCrypto signer in `gh-toolkit-worker`. Also yields deployed mint latency for BR-17 and VO latency for RL-06's single-ledger option | per `analysis/spikes-pending.md` |
| **SP-19a** (defined in P1 spec §16.1; listed in `analysis/spikes-pending.md`) | Sign-in app with no repo/org permissions can complete the web flow and read `/user`, `/user/emails` | Validates the `SignIn` purpose (OA-05); records whether `scope` is ignored (OA-03) | P1 (gates P1 M0 step 7) |
| **SP-23** | Revoking a stateless installation token | BR-18: PASS → document `revoke_installation_token` as effective; FAIL → document that `invalidate` is cache-only and keep 1 h tokens | per `analysis/spikes-pending.md` |
| **SP-03** | `@dependabot rebase` with a user-to-server token | P2 shape only; L2 offers the comment endpoint either way | P2 planner |
| **SP-05** | Installation-scoped search returns only that installation's repos | PG-05 post-filter stays mandatory on FAIL, optional on PASS; records GraphQL rate-limit error shape (RL-05) | P2 planner |
| **SP-09**, **SP-10** | Merge under rulesets with user-token approval; Workflows permission | Endpoint metadata (`MergeAsync` permission rows) and P2 profiles | P2 planner |
| **SP-04** | Sequential merge convergence | None on L2 (P2 policy) | P2 planner |
| **SD-09** | Restate-on-Workers polling `merge-async` with 429/`retry-after` injection under the governor | Exercises RL-01…RL-06 and TR-05 end to end; uses L2's stub contract | P2 planner |
| *SD-L2-a (proposed)* | Mints per hour per app and per (app, installation) with the isolate cache on deployed Workers; 401s after an invalidation with a KV shared cache, if built | Decides BR-17. **PASS (shared cache stays unbuilt)** if p95 mints/min per app < 15 and the RL-09 counter never reaches `Warn`, over 7 days of P1 production; **FAIL** otherwise | P1 operator; due 7 days after P1 M0 production deploy, and again before EX-01 |
| *SD-L2-b (proposed)* | reqwest-on-fetch under workerd. Cases: (1) per-request timeout cancels the subrequest; (2) header case preserved; (3) dropped future aborts the fetch; (4) two concurrent invocations in one isolate, the first cancelled mid-mint, the second minting independently: no "Cannot perform I/O on behalf of a different request", no hang; (5) redirects: can reqwest set manual mode on wasm32, and does workerd strip `authorization` on a cross-origin redirect | Decides whether `WorkerFetchTransport` is needed (TR-02, TR-17) and confirms BR-11's wasm32 rule. **PASS** if all five cases behave as TR-02/TR-17/BR-11 require with `ReqwestTransport`; **FAIL** on any case ⇒ `WorkerFetchTransport` mandatory on wasm32 | L2 implementer; due at P1 B0-05 (transport) |
| *SD-L2-c (proposed)* | ETags across rotating installation tokens; do 304s spare the primary quota for installation tokens? | Decides whether TR-11 is used by P2 polling. **PASS** if a conditional GET with an ETag from token A, sent with a freshly minted token B for the same installation, returns 304 and leaves `x-ratelimit-remaining` unchanged; **FAIL** otherwise (TR-11 stays off for installation tokens) | P2 planner; due at P2 spec |
| *SD-L2-d (proposed)* | `wasm-bindgen-test` runner for in-crate wasm tests (node vs workerd) | TS-12 tooling. **PASS** if the node runner executes JWT signing, redaction, pagination and ledger tests green in CI; **FAIL** ⇒ those tests run in the consumer workerd smoke | L2 implementer; due at EX-04 |
| *SD-L2-e (proposed)* | Do installation-token mints count toward GitHub's 2,000/h "OAuth access token requests" limit? Method: inspect rate-limit headers on mint responses; on a staging app, mint 2,050 tokens across 10 installations within one hour and record whether any mint is refused with an OAuth-token limit | RL-09 and BR-17. **PASS (mints don't count)** if all 2,050 succeed and no limit header refers to it ⇒ RL-09 counts only exchanges and refreshes, BR-17 (a) may rise to 60/min; **FAIL** ⇒ keep RL-09 and BR-17 as written and record it in R-C1-token-limits | L2 implementer (needs user approval to run on a staging app); due before P2 spec |

### 18.3 Open questions and spec decisions

| ID | Question | Owner | Due / blocks | Default if nobody decides |
|---|---|---|---|---|
| OQ-1 | Final repo and crate names (`gh-toolkit` is P1's working name) | user | EX-03 | Repo `sagikazarmark/gh-toolkit`, crates `gh-toolkit`, `gh-toolkit-worker` |
| OQ-2 | Publish to crates.io or git tags only | user | EX-05 | Git tags until P2 ships |
| OQ-3 | Keep `octoevents` as a dependency or fold the needed parts into L2 | user (author owns both) | EX-03 | Keep the dependency; L2 adds only multi-app routing, kind validation and typed payloads |
| OQ-4 | Should GraphQL query documents P2 writes move into L2 when P3 or a later product needs them? | L2 implementer | none (revisit at P3 spec) | Promote on the second consumer (PG-06) |
| OQ-5 | Should `availability.rs` (P1) become an L2 module? | P2 planner | **Answered** by P2 spec §18: no (P2 uses `InstallationSync`) | No for v1 (§16 last row) |
| OQ-6 | Device flow for CLI products (P3) | user | P3 spec | Not in v1; P3 uses `gh auth token` via `from_operator` |
| OQ-7 | GHES support (separate base URLs, API version differences) | user | none | Configurable base URL, untested, unsupported |
| OQ-8 | Restate helper crate (`gh-toolkit-restate`: error-code → `TerminalError` mapping, durable-sleep-on-`RateLimited` helper, RL-06 hold/commit helper) | L2 implementer | EX-08 | No; apps write the code (§15) |

Spec decisions open to the user: **SPD-1** key fallback on mint 401 (GA-05); **SPD-2** 60 s negative cache for mint 404/422 (BR-12); **SPD-3** shared-cache build trigger at 15 mints/min per app (BR-17); **SPD-4** 10 MiB response cap (TR-07); **SPD-5** reserve in both user and installation buckets for user-token writes, via hold/commit (RL-06); **SPD-6** MSRV 1.91 and the owned `InstallationRest` closure argument instead of `AsyncFnOnce` (§6.3); **SPD-7** assume mints count toward the 2,000/h OAuth-token limit until SD-L2-e (RL-09); **SPD-8** 7-day recommended dedupe window for `payload_sha256` receipts (WH-02).

### 18.4 Follow-up edits in other documents

**Status: applied 2026-10-16** (consistency pass; `plans/README.md`), except the conditional `analysis/spikes-pending.md` item "add SD-L2-a…SD-L2-e if the user accepts them", which waits for the user (`plans/README.md` decisions list).


- `plans/P1-build-plan.md`: fold D-01, D-02, D-04, D-05, D-06, D-07, D-08, D-10, D-17, D-18, D-19, D-20 into B0-03…B0-07 and B1-01/B1-02 acceptance criteria; add D-12's feature assertion and TR-18 to the §5.2 "No repo DELETE/PATCH" gate at extraction; P1 plan R9 ("key everything by `AppId`") becomes "key caches by `AppId`, address apps by `AppSlot`".
- `specs/projects/P1-github-invite.md` §10.1: replace the error-class list with the D-18 shapes; §10.2: replace the broker sketch with §6.3's signatures (D-17), the `OnceCell`/`RefCell` cache note with BR-09/BR-11, and the mint 403 row with §6.6's `MintForbidden`/`InstallationSuspended` split; `Installation.recheck_permissions` uses `ensure_fresh` (BR-12); §7.5: `upsert_bucket` for the 50 → 500 change and `count_tagged` for the per-link share (RL-07); §7.7: dedupe receipts on `payload_sha256` are time-bounded (WH-02), repeats rely on the Reconciler; §11.4: OAuth parameter redaction now comes from the toolkit (TR-09), only `/i/` stays in P1.
- `specs/components/anykms-local-keyring.md` §6.5: note that the `installation-token-cache` `scope` value is `name@fingerprint` (BR-06); the key set and kinds do not change.
- `specs/components/L1-rotating-credential.md` §14: OQ-9, OQ-14 and OQ-17 now point to L2 §12.3–§12.4 as their owner's home; RC-14 notes that RL-09 counts refreshes against the same 2,000/h budget.
- `analysis/risks.md` R-C1-token-limits: record that installation mints may count toward the 2,000/h OAuth-token limit (UNVERIFIED, SD-L2-e) and that RL-09/BR-17 govern it.
- `analysis/spikes-pending.md`: add SD-L2-a…SD-L2-e if the user accepts them; add the PG-07 check to SP-15; optionally add SP-19a (today only in P1 spec §16.1).

---

## Review log

Revision 2 addresses the adversarial review of revision 1. All 30 objections were accepted (none rejected); several duplicates were resolved by one change. Severity as given by the reviewer.

| # | Severity | Objection (one line) | Resolution |
|---|---|---|---|
| 1 | major | `FnOnce(&InstallationToken) -> Fut` cannot return a future borrowing the token; `CallError` closed | Accepted. Closures take an owned `InstallationRest` (Arc-held token) (§6.3, SPD-6); `CallError<E>` generic; must-compile case in TS-02 |
| 2 | major | `Shared` mint futures across Workers request contexts break I/O isolation | Accepted. BR-11 split per target: wasm32 shares completed tokens only, in-flight map per `Broker` (per request); SD-L2-b case 4; §15 rule 6 |
| 3 | major | No redirect policy; tokens may follow 3xx to foreign hosts, writes re-sent | Accepted. TR-17 (no redirects, `Policy::none()`, `redirect: manual`, `ApiError::Moved`, same-origin opt-in for reads); SD-L2-b case 5; TS-01/TS-10/TS-17 cases |
| 4 | major | 2,000/h OAuth-token limit may include mints; BR-17 trigger too high; constant unused | Accepted. RL-09 counter (mints, exchanges, refreshes; 50 %/80 % events), BR-17 lowered to 15/min, SD-L2-e, R-C1 follow-up (§18.4) |
| 5 | major | Atomic multi-bucket reserve impossible across two VOs | Accepted. `hold`/`commit`/`release_hold` with expiring holds and user → installation order (RL-06), single-ledger alternative with throughput note; TS-05 crash case |
| 6 | major | SSO `partial-results` 2xx listings returned as complete | Accepted. PG-07 `ApiError::Partial{SsoRequired, org_ids}` with `allow_partial`; ER-03 clarified; TS-06/TS-10; SP-15 added check |
| 7 | major | `repo-admin` gate cosmetic via generic call; path segments unencoded | Accepted. `PathTemplate` with encoded segments; TR-18 runtime deny-list → `Forbidden{Disabled}`; TS-11 |
| 8 | major | Event-kind header unauthenticated; unbounded `payload_sha256` dedupe drops legitimate repeats | Accepted. WH-04 discriminator validation → `Decode{kind_mismatch}`; `UnauthenticatedHints`; WH-02 7-day window (SPD-8) and Reconciler reliance; TS-07 |
| 9 | minor | GA-07 and BR-10 time-direction errors | Accepted. Both rationales corrected; clock read right before handing out the token; TS-04 +61 s / −9.5 min |
| 10 | minor | Negative cache invalidated from `p1-web` cannot reach `p1-restate` isolates | Accepted. `ensure_fresh`, isolate-local documented, snapshot version voids entries (BR-12) |
| 11 | minor | All non-rate-limit mint 403s mapped to suspension; RL-01 order unstated | Accepted. `MintForbidden{reason}` unless `GET /app/installations/{id}` shows `suspended_at`; RL-01 first (§6.6); TS-03 rows |
| 12 | minor | Method-based `is_transient_for` wrong for GraphQL queries and idempotent PUTs | Accepted. `EndpointMeta.retry_safe` (TR-21), `GraphQlKind`; ER-02 table; TS-11 |
| 13 | minor | KV eventual consistency and 60 s TTL minimum understated | Accepted. BR-15 states the bound, skips puts under 60 s, prefers D1; SD-L2-a measures 401s |
| 14 | minor | User tokens sent in bodies outside the GK-P1 path | Accepted. Crate-private `SecretBody` (TR-19), GK-P1 reworded, TS-01 body markers |
| 15 | minor | Internal inconsistencies (signer names, TokenKey.app, error classes, purposes, `[u8; 256]`, deps) | Accepted. D-17/D-18/D-19, `AppConfig.purposes`, `Vec<u8>` + modulus check, `http`/`bytes` in §4 |
| 16 | blocker | `Shared` needs `Output: Clone`; token borrowed from mutable cache | Accepted. Cache holds `Arc<InstallationToken>`; `Shared` output `Result<Arc<_>, MintError>`; ER-06 `Clone`; TS-03 concurrent invalidate case |
| 17 | blocker | Closure signature does not compile; TR-15 facade contradicts it | Accepted (same fix as #1). Facade uses the same `InstallationRest` argument (TR-15); TS-02 compile tests for broker and facade |
| 18 | major | `invalidate(key)` can't tell which token got the 401; no per-installation clear | Accepted. `invalidate(key, TokenGeneration)`, `invalidate_key`, `invalidate_installation`; `call` self-invalidates on 401 (BR-07); D-04; TS-03 stale-generation case |
| 19 | major | `CallError` unbuildable; `BrokerError` duplicates `ApiError`; `is_transient` undefined | Accepted. `CallError<E>`, `BrokerError<E>{Mint, Call, Unauthorized}`, `MintError`; BR-19 mapping table; ER-02 per-variant table |
| 20 | major | Silent deltas vs P1 (`TokenKey.app`, error classes, `AppJwtSigner`) | Accepted. §17 "kept" sentence corrected; D-17, D-18, D-19 added; §18.4 lists P1 spec §10.1/§10.2 and plan B0-03/04/06 edits |
| 21 | major | Responses cannot be attributed to quota buckets; skew owner undefined | Accepted. `Credential::bucket()`, constructors carry client/user IDs, broker stamps installation; `ResponseObserver` + `Observations` own quota and skew, read by `AppJwtSource` (RL-02, GA-07); TS-05 |
| 22 | major | `WriteLedger` lacks bucket registration, cap changes, per-link attribution, idempotency, GC | Accepted. `new`, `upsert_bucket`, tagged + `request_key` reserve, `count_tagged`, `record_limit` until = caller choice, RL-10 GC; TS-05 AC-GOV-shaped cases |
| 23 | major | `AppConfig` lacks purposes; `OAuthConfig` duplicates secrets; ephemeral token not a credential; user API unsigned | Accepted. `AppPurposes`, `OAuthConfig::<P>::from_registry`, `EphemeralUserToken: Credential`, full §12.2 signatures, TS-08/TS-15 |
| 24 | major | `…Dyn` traits undefined; Send vs `!Send` worker types; unsafe outside core | Accepted. §4.1: hand-written `BoxFut` dyn traits with blanket impls (TR-20), `SharedTokenCacheDyn` implemented by `SealedSharedCache`; `Send` kept, sole wrapper `gh_toolkit::wasm::assert_send`, no `unsafe` in `gh-toolkit-worker` (TR-03); TS-02 wasm32 compile case |
| 25 | major | Many normative requirements untested | Accepted. TS-15…TS-18 added; AC-L2-06 requirement-coverage script |
| 26 | major | Events and metrics referenced but undefined | Accepted. §13a `Event`, `EventSink`, `TracingSink` field contract (OB-01…OB-03); referenced from BR-17, RL-09, SD-L2-a |
| 27 | minor | Endpoints L2 calls missing from `Endpoint`; P2 merge-queue mutations unmentioned | Accepted. Variants added (user, ClientBasic, `RevokeInstallationToken`, `ListOrgTeams`, `GraphQl`), `OAuthTokenEndpoint` non-API entry, §16 merge-queue row |
| 28 | minor | Cache keyed by product `AppSlot` string can mix apps | Accepted. Key is `AppId` (BR-09); GA-01 rejects duplicate app/client IDs and requires `app_id` with keys; `test-mock` defaults to isolated cache; TS-03 case |
| 29 | minor | TR-9 misses OAuth `code`/`state`/PKCE/client secret/refresh token | Accepted. TR-09 query and JSON patterns; TS-01 markers; only `/i/` stays in P1 |
| 30 | minor | OQ/SD owners are documents; SDs lack PASS/FAIL; SP-19a home unremarked | Accepted. Owner roles and due points for every OQ and SD; PASS/FAIL lines; SP-19a home noted in header and §18.2 |
