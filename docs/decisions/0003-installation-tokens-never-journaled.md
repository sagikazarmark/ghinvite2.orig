# 0003. Installation tokens are never journaled; C1 is a library used inside `ctx.run`

- **Status:** Accepted (D3, decided by the user in conversation; `PLAN.md` §9)
- **Date:** 2026-10-15

## Context

- A `ctx.run` result is journaled and replayed, never recomputed. A token returned from a step is frozen: a retry hours later, or a resume after a 7-day approval wait, replays the same expired value (`research/restate.md` Q3).
- A Restate "token service" that returns tokens would put them in call results and in both the caller's and the callee's journals. Rust has no journal codec (`research/restate.md` Q2).
- The PoC never journals installation tokens. It mints them inside the same `ctx.run` closure that uses them, journals only outcomes, and redacts all upstream text from errors (`analysis/poc-ghinvite.md` §6.8; `tests/credential_disclosure.rs` in the PoC).
- No dedicated mint rate limit is documented. Secondary limits (~180 POST/min per endpoint) are far above need (R-C1-token-limits; `research/github-app-tokens.md` Q1).
- Since 2026-04-27, installation tokens are stateless JWT-format `ghs_…` strings of about 520 characters (`research/github-app-tokens.md` Q1).
- F3 research had sketched C1 as a Restate Virtual Object broker (`research/github-app-tokens.md`, "Design implications → C1" item 1). This ADR rejects that shape.

## Decision

1. **Installation tokens never enter a Restate journal, state, call parameter, call result, run name, header or error.**
2. **C1 is a library, not a service.** Shape: `broker.with_token(installation_id, scope_profile, |token| async { … })`, called **inside** the `ctx.run` that makes the GitHub calls. The closure returns only a business outcome.
3. **Caching:**
   - an isolate-memory cache, refreshed when less than 60 s remains (as in the PoC `token_cache.rs`), keyed by (app, installation, scope profile)
   - an **optional** shared D1 or KV second-level cache, used only if measured mint volume calls for it. Entries are encrypted at rest with the anykms envelope under `purpose=installation-token-cache; app; installation_id; scope` (ADR 0001). `expires_at` is stored in plaintext next to the ciphertext.
4. **Re-mint on 401, once.** A GitHub call that returns 401 with a broker token invalidates the cache entry and retries once with a fresh token. A second 401 is terminal. Use `expires_at` from the mint response, never "now + 1 h".
5. **Scoped tokens.** Tokens are minted with a small set of named scope profiles per workflow kind (for example, P1 org invites: `members:write`). Tokens are not per repo.
6. **Size.** Storage columns, state and redaction patterns allow tokens of 600+ characters and match `ghs_`, `ghu_` and `ghr_` prefixes.
7. **No scheduled renewal.** Tokens are minted on demand.
8. **`Sealed<T>`** (ADR 0001 cipher) is only for the rare secret that genuinely has to cross a journal boundary. It is always sealed **inside** `ctx.run`; sealing outside gives error 570, journal mismatch (`spikes/restate-sealed-journal`).

### Acceptance checks (testable)

- A marker-scan test, modelled on `spikes/restate-sealed-journal`, runs a P1 delivery against a mock GitHub. It finds no `ghs_` marker in any Restate introspection table or admin view, including after a forced retry.
- The PoC's credential-disclosure test is lifted: no error value or log line contains token text or upstream response bodies.
- A unit test confirms that 401 → one re-mint → success, and that 401 → re-mint → 401 is terminal.

## Consequences

- C3 shrinks to `Sealed<T>` plus hygiene rules: no secrets in state keys, headers, run names or errors, and short journal retention on secret-touching services (`research/restate.md` Q2, "Design implications").
- **Mint volume.** Workers isolates are short-lived and many, so the effective mint rate is about one token per (installation, profile, isolate), not one per hour. The PoC builds its client per `fetch`, so its cache lasts one request (`analysis/poc-ghinvite.md` §6.8 inference). L2 must hold the cache at isolate scope (a module-level static). The shared cache is added only if measurements call for it.
- The JWT used for minting is cached inside the broker (sign once, reuse for about 8 min, `iat = now-60`). JWT signing CPU on deployed Workers is still unverified (SP-16).
- Revoking a stateless installation token isn't proven (SP-23). Until it is, an invalidated cache entry doesn't imply that GitHub has revoked the token.
- C1 lives in L2, the GitHub App toolkit, lifted from `ghinvite-github` (ADR 0005).


## Amendment notes (2026-10-16, cross-document consistency pass)

These record refinements made by later specs. They don't change the decision.

- **Shared-cache context (decision 3):** the registered purpose is `purpose=installation-token-cache; app_id; installation_id; scope; expires_at` (keyring §6.5 registry v1; L2 BR-14). `app` became `app_id` (numeric App ID), `expires_at` is bound as well as stored in plaintext, and `scope` holds L2's `cache_scope` = `<profile name>@<permission fingerprint>` (L2 BR-06).
- **Closure shape (decision 2):** the target API passes the closure an owned `InstallationRest` (token behind `Arc`, never the raw token) and keys the call by `TokenKey { app: AppSlot, installation, profile }` (`specs/components/L2-github-app-toolkit.md` §6.3, deltas D-17). The obligations of decisions 1–2 are unchanged.
- **Isolate cache key (decision 3):** `(AppId, InstallationId, cache_scope)` (L2 BR-09).

## Amendment (2026-10-20, gh-toolkit2 token provider)

`implementation/gh-toolkit2` (the rewrite of the L2 toolkit; renamed to `implementation/octoauth` with crates `octoauth` and `octotoken` by ADR 0014) changes how decisions 2–4 are realised. Decision 1 (tokens never enter a journal, state, call parameter or result, run name, header or error) is unchanged and still binds every consumer.

- **Decision 2 (library inside `ctx.run`) → consumer-facing provider.** Consumers obtain tokens from an `InstallationTokenProvider` (`token(installation, scope)`, `replace(rejected)`) and attach them to their own requests (`InstallationToken::expose_secret`). The closure shape (`with_token`) is not provided; a helper may be added later. Obligation for consumers: call the provider inside the same `ctx.run` that uses the token, and never return or journal the token. Consumers call `token()` per request rather than holding a token.
- **Decision 3 (caching)** is unchanged in substance: the first provider (`AppTokenProvider`) caches in process memory per (installation, scope); the optional shared cache becomes a pluggable, encrypted `TokenStore` (later), also because several services (not only isolates) must share tokens.
- **Decision 4 (re-mint on 401, once) moves to the consumer.** After a 401 the consumer calls `provider.replace(&rejected)` once and retries; `replace` returns a newer token or mints one, and concurrent replacers converge on one replacement. A 403 is never answered with `replace`. The acceptance check "401 → one re-mint → success; 401 → re-mint → 401 terminal" applies to consumer code (or the future helper).
- **Type properties:** `InstallationToken` becomes `Clone`, sharing one zeroized secret through an internal `Arc` (no copy of the secret); it remains not `Serialize`, not `Display`, with redacted `Debug`.
- **Restate-backed minting** (the rejected "token service") stays rejected in the form that returns tokens. If added, a Restate service may coordinate minting but must write the token to the sealed store and return only an acknowledgement.
- **Workers:** concurrent mints are combined only natively; on Workers a request never waits on another request's mint (the L2 spec's BR-11 rule), so duplicates per isolate are possible and harmless.

See `specs/components/L2-github-app-toolkit.md` §17 delta D-21. ADR 0014 (gh-toolkit2 wraps no GitHub client) makes decision 1 the consumer's obligation for its own API calls and errors as well.

## Evidence

- `PLAN.md` §9 D3
- `research/restate.md` Q2, Q3, "Design implications → C1, C3"
- `research/github-app-tokens.md` Q1 (lifetimes, ~520-char tokens), "Design implications → C1" (items 2, 4–6 adopted; item 1, the VO broker, rejected)
- `analysis/poc-ghinvite.md` §6.8 (mint inside `ctx.run`, redaction), §7 (C1, C3 rows)
- `spikes/restate-sealed-journal/README.md` (seal inside `ctx.run`; marker scan method)
- `analysis/risks.md`: R-C1-token-limits, R-C3-journal-secrets
- `analysis/spikes-pending.md`: SP-16, SP-23
