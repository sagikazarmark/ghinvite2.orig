# 0001. anykms envelope with a production local keyring, not cryptbox, for C2/C3

- **Status:** Accepted (D1, decided by the user in conversation; `PLAN.md` §9)
- **Date:** 2026-10-15
- **Supersedes:** the cryptbox recommendation in `research/secrets-and-keys.md` §3 ("Recommendation", items 1–2)

## Context

Two components need an at-rest cipher that runs on Cloudflare Workers (wasm32):

- **C2 / L1** stores GitHub App user access and refresh tokens. Refresh tokens live up to 6 months and roll forward on every refresh (`research/github-app-tokens.md` Q3).
- **C3** covers the rare secret that has to cross a Restate journal boundary (`Sealed<T>`). Rust has no journal codec (`research/restate.md` Q2).

The candidates are the user's own crates:

- **cryptbox:** record and field encryption with seal IDs, keyrings, blind indexes and context binding through `InRecord<…>`. It doesn't accept free-form AAD. The PoC set it aside for sessions for that reason (`analysis/poc-ghinvite.md` §4, PoC ADR 0002). F3 research recommended a cryptbox `Record` for C2 (`research/secrets-and-keys.md` §3(b)).
- **anykms:** a frozen envelope v1 (AES-256-GCM, a fresh 32-byte DEK per message, HKDF-SHA256 key commitment, DEK wrapped by a KEK through `KeyWrap`), plus `EnvelopeKeyring` and `rewrap_*` operations. It accepts free-form AAD. Its `local` backend is **tests-only**: ephemeral, no key import, runtime opt-in. Its HTTP KMS backends **don't build for wasm32** (`research/secrets-and-keys.md` §2; `spikes/wasm-crypto`).

Both crates' core crypto builds and runs on wasm32 (`spikes/wasm-crypto`: `anykms-recipes` with `envelope` and `anykms-local` PASS; cryptbox PASS). Neither crate is audited. Under the agreed threat model, Cloudflare and the account admin are trusted, so key custody is the same for every option that keeps the key in a Worker secret or Secrets Store (`research/secrets-and-keys.md` §3 threat table; R-C2-storage, R-C4-cf-kms).

## Decision

1. **Cipher.** C2 (L1's encrypting store decorator) and C3 (`Sealed<T>`) use the **anykms envelope v1 recipe** (`EnvelopeAead` / `EnvelopeKeyring`).
2. **New anykms backend: a production local keyring.** It is built in the anykms repo and is a prerequisite for P1 (`PLAN.md` §9, "Work in existing repos"). Required behaviour:
   - KEKs are **versioned**, each with a stable key ID. Key material is **loaded** from a Worker secret or a Secrets Store binding (one or more 32-byte keys plus their IDs). It is never generated at runtime.
   - Exactly one KEK is *current* and is used for wrapping. Every configured KEK can unwrap.
   - **Rotation:** adding a new current KEK needs no re-encryption. `rewrap` moves an envelope's DEK to the current KEK without touching the payload.
   - Unwrapping under a KEK ID that isn't configured fails with a typed error. It never falls back to another key.
   - It builds for `wasm32-unknown-unknown` with `default-features = false` and has no tokio, `Instant` or `SystemTime` dependency.
   - It passes `anykms-conformance`.
   - It doesn't need the `allow_ephemeral_local_keys` opt-in, because the keys aren't ephemeral.
3. **Mandatory typed AAD convention.** Every encryption takes a typed *encryption context*, never raw bytes:
   - The context is a set of key/value pairs with a canonical, unambiguous byte encoding: sorted keys and length-prefixed fields.
   - `purpose` is always required. Each purpose declares its other required keys. Examples: `purpose=user-token; app=<app id>; user_id=<GitHub numeric id>` and `purpose=installation-token-cache; app=<app id>; installation_id=<id>; scope=<profile>`.
   - **An empty context is rejected** (an error, or unrepresentable in the API). Decrypting under a context that differs in any key or value fails.
4. **cryptbox stays available** for cases that genuinely need blind indexes or record and seal-ID machinery. It isn't used for C2 or C3.

### Acceptance checks (testable)

- Encrypting with an empty context is rejected. Decrypting with a context that changes one value (for example `user_id`) fails.
- An envelope written under KEK `k1` still decrypts after `k2` becomes current. After `rewrap`, it names `k2` and decrypts with `k1` removed.
- The keyring crate and a consumer crate build for wasm32 with `getrandom/wasm_js` enabled in the consumer, matching the `spikes/wasm-crypto` setup.
- The backend passes `anykms-conformance`, run natively.

## Consequences

- **Reasons for the choice (user's):** it's simpler to use than cryptbox's record and seal-ID machinery; it's NIST/FIPS-friendly (AES-256-GCM, HKDF-SHA256); it's key-committing; and moving to a real KMS later is a **KEK swap**. Only the wrapped DEKs are rewrapped and payloads aren't re-encrypted (ADR 0004).
- **Row binding moves from cryptbox records to AAD.** The protection a cryptbox `Record` gave against copying fields between rows (`research/secrets-and-keys.md` §3(b)) now comes from the encryption context: `user_id` and `app` are required for `purpose=user-token`. Rollback by an attacker with DB write access is still possible. It only causes a denial of service, because a stale single-use refresh token is already dead (R-C2-storage).
- **Audit gating changes.** Neither crate is audited. anykms says envelope v1 "is not yet recommended for long-lived data" until its external review is done (`research/secrets-and-keys.md` §2). C2 refresh tokens are long-lived data, so the **anykms envelope review replaces cryptbox review #12 as the gating item before production** for C2. Owner: the anykms repo.
- **Known envelope properties to document in the C2/C3 specs** (`research/secrets-and-keys.md` §2):
  - The AAD isn't committed against a malicious creator.
  - `kek_id` is an unauthenticated hint.
  - KEKs must be dedicated to envelopes.
  - There is no replay protection.
- **`Sealed<T>` fits the sync SDK traits.** The Restate SDK serde traits are synchronous and take no context (`research/restate.md` Q2), so `Sealed<T>` needs a process-global keyring. A local keyring has no async unwrap, so it fits. A future remote-KMS KEK wouldn't fit inside those traits without DEK caching or an async layer around them. That is recorded as an ADR 0004 follow-up.
- **UNVERIFIED:** anykms `Ciphertext` serde (base64 in human-readable formats) hasn't been spiked as a Restate journal value. `spikes/restate-sealed-journal` used cryptbox. Re-run the spike's marker scan with the anykms envelope when `Sealed<T>` is built (only if `Sealed<T>` is needed at all; see ADR 0003).
- **Not decided here:** whether the PoC's XChaCha20-Poly1305 session encryption (PoC ADR 0002) migrates to the envelope. It isn't C2 or C3 data.
- **Reading note:** `PLAN.md` §8 says L1's encrypting store decorator "seals the pair with cryptbox". That sentence predates D1. Under this ADR the decorator uses the anykms envelope.
- Key loading from Secrets Store and `secret_key` bindings is still unspiked (SD-07). The keyring spec must cover both binding types.


## Amendment notes (2026-10-16, cross-document consistency pass)

These record refinements made by later specs. They don't change the decision.

- **Context keys.** The example contexts in decision 3 are refined by the keyring purpose registry v1 (`specs/components/anykms-local-keyring.md` §6.5): `purpose=user-token; app_client_id; user_id` (the GitHub App **client ID**, because grant APIs are keyed by it) and `purpose=installation-token-cache; app_id; installation_id; scope; expires_at` (the numeric **App ID**). Distinct key names make a mix-up an `InvalidArgument` at build time.
- **Session encryption is now decided:** P1 sessions and attempt continuations move to the envelope (P1 spec PD-G, §11.3; purposes `session` and `attempt-continuation`). The "Not decided here" bullet above is closed.
- **Reading note resolved:** `PLAN.md` §8 now says the decorator uses the anykms envelope.
- **Binding types:** the keyring spec covers Worker secrets and Secrets Store values; `secret_key` (non-extractable `CryptoKey`) bindings are out of v1 (keyring §5.4, OQ-3, pending SD-07).

## Evidence

- `PLAN.md` §9 D1; §9 "Work in existing repos" (anykms local keyring and AAD convention, prerequisite for P1)
- `research/secrets-and-keys.md` §2 (anykms envelope v1, `local` tests-only, `Send` traits, wasm), §3 (comparison and the superseded recommendation)
- `spikes/wasm-crypto/README.md` (wasm32 build results)
- `spikes/restate-sealed-journal/README.md` (seal only inside `ctx.run`; cryptbox-based)
- `analysis/poc-ghinvite.md` §4 (PoC ADR 0002: cryptbox rejected for lack of arbitrary AAD)
- `analysis/risks.md`: R-C2-storage, R-C3-journal-secrets, R-C4-cf-kms, R-WASM-crypto
- `analysis/spikes-pending.md`: SD-06, SD-07
