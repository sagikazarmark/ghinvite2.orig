# Component spec: anykms production local keyring and typed encryption context

> **Superseded (2026-11-15).** The design was revised on
> [anykms#35](https://github.com/sagikazarmark/anykms/issues/35) (incubated outside anykms as
> `libs/kekring`) and [anykms#33](https://github.com/sagikazarmark/anykms/issues/33) (canonical AAD
> v1). The formats this spec defines (`akr1` descriptor, KCV, AES-256-GCM `akr1` wrapped key,
> `anykms-keyring/v1/*` labels, `anykms-ctx/v1` context bytes) and its error table
> (`UnknownKeyVersion` with a retry hint, KCV mismatch as `Configuration`) are **not** what the
> code implements. The current contract is `libs/kekring/docs/kekring-format.md`; operator steps
> are in `docs/deploy.md` ("Session keyring"). This file is kept as planning history.

- **Status:** draft, revision 2, 2026-10-15. This is a Phase 4 component spec (`PLAN.md` §4). Revision 2 addresses the review objections listed in §13.
- **Implements:** ADR 0001 (`decisions/0001-anykms-envelope-over-cryptbox.md`, `PLAN.md` §9 D1). It also covers the keyring parts of ADR 0003 and ADR 0004.
- **Lives in:** the `sagikazarmark/anykms` repo (`PLAN.md` §9, "Work in existing repos"). It is a **prerequisite for P1** (planning sequence step 1).
- **Risks:** R-C2-storage, R-C3-journal-secrets, R-C4-cf-kms, R-WASM-crypto. Related: R-C2-refresh, R-C1-token-limits.
- **Spikes:** SD-06 (workerd run), SD-07 (Secrets Store and `secret_key` bindings, extended in T11 with deploy and rollback questions), SP-16 (deployed Workers, including CPU measurement). SD-08 is out of scope.
- **Source state read:** anykms `main` at `32e1971` (2026-10-13). Files: `docs/envelope-format.md`, `docs/guarantees.md`, `docs/guide.md`, `crates/anykms-core/src/{traits,error,semantics,types}.rs`, `crates/anykms-recipes/src/envelope.rs`, `crates/anykms-recipes/src/rewrap.rs`, `crates/anykms-recipes/tests/envelope_rotation.rs`, `crates/anykms-local/src/lib.rs`, `crates/anykms-local/tests/conformance.rs`, and `crates/anykms-conformance/src/{lib,fixture}.rs`.
- **Depends on / depended on by:** `specs/projects/P1-github-invite.md` §11.3 (PD-G) and `specs/components/L1-rotating-credential.md` (ST-10 to ST-14) cite this spec (P1 PD-G was called SD-G in P1 revision 1). Later consumers: `specs/components/L2-github-app-toolkit.md` (BR-06, BR-14) and `specs/projects/P2-dependaboard.md` (§9.1, §13.1–§13.2). Edits those specs needed are listed in §12.1 (applied 2026-10-16).

Requirement keywords: **MUST**, **MUST NOT**, **SHOULD** and **MAY** have their RFC 2119 meanings. Requirements carry IDs (`KR-nn` for the keyring, `CX-nn` for the context) so that tests and later specs can cite them.

---

## 1. Purpose

Workers code needs at-rest encryption with these properties:

- it runs on `wasm32-unknown-unknown` inside workerd;
- its keys come from Cloudflare-held secrets;
- keys rotate without re-encrypting payloads;
- every ciphertext is bound to the record it belongs to.

ADR 0001 chose the anykms envelope v1 recipe for the cipher. Two pieces are missing from anykms today:

1. **A production local KEK backend.** `anykms-local` is "**For tests and development only.** Keys live in process memory with no persistence". It needs the runtime opt-in `allow_ephemeral_local_keys`, and it has no API for importing key material (`crates/anykms-local/src/lib.rs`; `research/secrets-and-keys.md` §2). The anykms HTTP KMS backends don't build for wasm32 (`spikes/wasm-crypto/README.md`). Today there is no way to run the envelope on Workers with a persistent key.
2. **A mandatory, typed AAD convention.** `Aead::encrypt(&self, plaintext, aad: &[u8])` accepts any bytes, including empty ones (`crates/anykms-core/src/traits.rs`). ADR 0001 §3 requires that every encryption carry a typed *encryption context*, and that an empty one is "an error, or unrepresentable in the API".

This spec defines both pieces: the **keyring** (`KR-*`) and the **encryption context** (`CX-*`).

### 1.1 Consumers

| Consumer | What it encrypts | Purpose (§6.5 registry) | Lifetime of the data | Storage | Evidence |
|---|---|---|---|---|---|
| **L1 encrypting store decorator** (C2) | GitHub App user access and refresh token pair | `user-token` | Up to 6 months. Rewritten on every refresh | D1 rows | ADR 0002 item 2; `PLAN.md` §8 (its "cryptbox" sentence is superseded by ADR 0001); L1 ST-10..ST-14; R-C2-storage |
| **L2 installation-token second-level cache** (C1), optional | Installation token (about 520 chars) | `installation-token-cache` | ≤ 1 h | D1 or KV | ADR 0003 item 3 (the optional D1/KV cache); R-C1-token-limits |
| **`Sealed<T>` in restate-toolkit** (C3), only if needed | A secret that has to cross a Restate journal boundary | `restate-sealed` (OQ-7) | Until the invocation completes, plus journal retention (KR-19) | Restate journal only (KR-19) | ADR 0003 item 8; `spikes/restate-sealed-journal`; R-C3-journal-secrets |
| **P1 web session** | Session record (no tokens; ADR 0007) | `session` | 30 days (PoC) | KV (`tower-sessions`) | P1 §11.3, decided as PD-G (formerly SD-G) |
| **P1 attempt continuations** | PoC "attempt continuation" payload | `attempt-continuation` | Until the attempt completes | D1 (`attempt_continuations`) | P1 §11.3 and data model table (PD-G) |

P1 itself stores no long-lived user token (ADR 0007 item 2). It needs the keyring for its first build because of sessions and continuations; P1 does not build the optional C1 shared cache in M0–M2 (P1 §10.2). P2 needs it for L1 (`user-token`) and for its own sessions (`session`, P2 §9.1), under two keyrings (KR-12 note).

## 2. Scope and non-goals

**In scope**

- A KEK backend in the anykms workspace. It holds versioned 32-byte root keys loaded from configuration. Its KEK implements `KeyWrap` and `Describe`, and deliberately **not** `Rewrap` (§6.3, KR-21).
- Parsers for key material, and thin loaders: a Worker secret string, a Secrets Store value that the app has already read, a native environment variable, and a native file.
- A process-global install point for consumers that can't receive the keyring as an argument (`Sealed<T>`; KR-15).
- A native provisioning CLI (`akr`, §5.5).
- The `EncryptionContext` type, its canonical encoding, typed purpose declarations, and a `ContextEnvelope` front end that accepts only a context, never raw AAD, and is the only route from a `Keyring` to the envelope in application builds (CX-04).
- A synchronous call path for consumers that can't `.await`, such as `Sealed<T>` inside serde traits.
- A rotation, rewrap and removal procedure with testable gates (§7), and a compromise procedure.
- Conformance, vectors, and a wasm test plan.

**Non-goals**

- **Generating keys at runtime.** The library never generates root keys. Keys are provisioned outside the app (KR-02, §5.5). Test fixtures may generate keys natively (T5).
- **A KMS service or remote KEK.** P4 is parked (ADR 0004). A remote KEK later is a KEK swap (§7.4). It needs a fetch transport, which is not part of this spec.
- **`secret_key` bindings (WebCrypto `CryptoKey`) in v1.** They don't give raw bytes, and calls through them are asynchronous JS promises. See §5.4 and OQ-3.
- **Replay, rollback and freshness protection.** The envelope makes no such claim (`docs/envelope-format.md`; `research/secrets-and-keys.md` §2).
- **Blind indexes or searchable encryption.** Use cryptbox for those (ADR 0001 item 4).
- **Changing envelope v1.** The format is frozen. AAD commitment belongs in a v2 (§8.3, OQ-10).
- **Distributing keys between Workers.** Every Worker that reads gets the same material through configuration (§5).

## 3. Background: what anykms already provides (reused as is)

- **Envelope v1** (`docs/envelope-format.md`):
  - Layout: `"AKE" ‖ 0x01 ‖ flags ‖ commit[32] ‖ nonce[12] ‖ kek_id_len ‖ kek_id ‖ wrapped_dek_len ‖ wrapped_dek ‖ AES-256-GCM ct ‖ tag`.
  - Each message gets a fresh 32-byte DEK. `HKDF-SHA256(DEK, info="anykms/envelope/v1")` produces `enc_key ‖ commit`.
  - `gcm_aad = header ‖ u64(len(aad)) ‖ user_aad`.
  - `wrapped_dek = KEK.wrap_key(DEK)` with empty KEK AAD.
  - `kek_id` and `wrapped_dek` sit outside the GCM AAD, which is what makes rewrapping possible.
- **`EnvelopeAead<K>`** (one KEK, one `kek_id`) and **`EnvelopeKeyring<K>`** (several KEKs routed by `kek_id`, with one primary). Rewrap methods and their checks (`crates/anykms-recipes/src/envelope.rs`, module docs "Rewrapping", lines 46–51, and lines 776–877):
  - `rewrap_local`, `rewrap_to`: unwrap locally and **check the key commitment** (`move_to` → `unwrap_committed_dek`), so a replaced wrapped-DEK slot fails with `DecryptFailed`.
  - `rewrap_verified(ct, aad)`: as `rewrap_local`, and also opens the payload under `aad`.
  - `EnvelopeAead::rewrap`: the server-side path through the KEK's `Rewrap` trait. Documented as **"No key-commitment check"**.
  - `EnvelopeAead::rewrap_current` (needs `K: KeyWrap + Rewrap`): calls `rewrap` first and falls back to `rewrap_local` **only** on `Unsupported`.
  - There is **no `needs_rewrap` library function.** It appears only as a rustdoc example written for `EnvelopeKeyring` (`envelope.rs` lines 68–87) and as a test helper (`crates/anykms-recipes/tests/envelope_rotation.rs:37`). This spec defines its own (KR-17).
- **Traits** (`crates/anykms-core/src/traits.rs`):
  - `KeyWrap { wrap_key, unwrap_key, wrapped_key_version }`
  - `Rewrap { rewrap(ct, aad) }`
  - `Describe`
  - All are `#[trait_variant::make(Send)]` with `Send + Sync` bounds. `KeyWrap` is forwarded for `&T`, `Arc<T>` and `Box<T>`.
- **Error kinds** (`crates/anykms-core/src/error.rs`): `DecryptFailed`, `InvalidArgument`, `KeyStateInvalid`, `NotFound`, `Unavailable`, `Configuration`, `Backend`, and, for versions:
  - `KeyVersionUnusable`: "below the minimum decryption version, or disabled or destroyed". A caller error; "the usual fix is to rewrap".
  - `UnknownKeyVersion`: "names a key version this key does not have". A caller error that "may carry a retry hint when the version may not have replicated yet".
  - Envelope decryption passes through every KEK error kind except `DecryptFailed`/`InvalidArgument`/`VerificationFailed`, which become `DecryptFailed` (`envelope.rs` `map_unwrap_error`, line 569; `docs/envelope-format.md`, "Errors").
- **Ciphertext text form:** `Ciphertext::to_base64`/`from_base64`, with optional serde (`docs/guide.md`, "Ciphertexts as text").
- **Padding:** `Padding::NONE`, `Padding::block(n)` and `Padding::length(n)` are `const` and `PartialEq` (`envelope.rs` `Padding`).
- **wasm status** (`docs/guarantees.md`):
  - Core, recipes and local are runtime-agnostic.
  - `Instant::now`/`SystemTime::now` panic on wasm32.
  - Local keysets read `SystemTime` when keys are created or rotated.
  - getrandom needs the app's `wasm_js` feature.
  - Locally, `anykms-recipes` with `envelope` and `anykms-local` built for wasm32 (`spikes/wasm-crypto/README.md`). An `EnvelopeAead` round-trip with a wrong-AAD rejection ran under Node 22 V8 (`research/secrets-and-keys.md` S2, 121 KB wasm).

## 4. Crate layout

| Item | Placement (proposed; OQ-1) | Depends on |
|---|---|---|
| Keyring, parsers, loaders, `KeyringKek`, `Keyring::context_envelope` | New crate **`anykms-keyring`**, kept separate from `anykms-local`, which stays test-only and keeps its opt-in and "tests only" stance | `anykms-core`, `anykms-recipes` (`envelope`, `context`), `aes-gcm`, `hkdf`, `sha2`, `zeroize`, `subtle`, `getrandom` (workspace versions) |
| `EncryptionContext`, `Purpose`, `purpose!`, `ContextEnvelope` | **`anykms-recipes`**, feature `context`. It is backend-agnostic and has to survive a KEK swap | `anykms-core`, `envelope` feature |
| Process-global install point (KR-15) | `anykms-keyring`, feature `global` | std `OnceLock` |
| **Raw KEK access** (`Keyring::raw_kek`) and the provider (`keyring://<name>`) | `anykms-keyring`, feature **`raw-kek`**, **off by default** (CX-04). Used only by the conformance tests (T5) and by the KEK-swap migration job (§7.4) | `anykms-core` registry |
| Native loaders (`from_env`, `from_file`) | `anykms-keyring`, feature `std-loaders`, off by default | std |
| Provisioning CLI `akr` (§5.5) | New binary crate **`anykms-keyring-cli`** (native only, never a dependency of an app) | `anykms-keyring` (`std-loaders`), `getrandom` |
| Facade re-export | `anykms` feature `keyring`, reached as `anykms::keyring`. The facade MUST NOT enable `raw-kek` | |

**KR-01.** With `default-features = false` and features `keyring`/`context`/`global`, `cargo tree --target wasm32-unknown-unknown` for `anykms-keyring` and `anykms-recipes` MUST NOT contain `tokio`, `reqwest`, `rustls`, `ring`, `aws-lc-sys` or `aws-lc-rs`. CI checks this (T8).

## 5. Key material: format and loading

### 5.1 Model

- A **keyring** has a `name` and a set of **generations**. Each generation is a triple `(version: u32 ≥ 1, root: [u8; 32], kcv: [u8; 8])`, where `kcv` is the key check value of §6.1.
- Exactly one generation is **current**. Every other generation present in the material is **readable**.
- A generation that isn't present, with a version **at or below** the highest present version, has been **removed**. Removal is the only revocation mechanism (KR-07).
- A version **above** the highest present version is **unknown** to this keyring. That happens on an isolate that runs an older descriptor (KR-07).
- The `name` is the envelope's `kek_id`. It is a stable logical name, never a URI, as the recipe advises (`envelope.rs`, `EnvelopeAead::new`).

**KR-16 (operator invariants).** The parser can't see history, so these are operator rules. `akr diff` (§5.5) enforces them between an old and a new descriptor:

- **Versions are monotonic and never reused.** A new generation gets a version greater than every version this keyring has ever had, including removed ones. Reusing a removed version with new bytes would turn `KeyVersionUnusable` into `Configuration` (KCV mismatch) for old envelopes, and would hide the removal.
- **Key bytes for an existing version never change.** A changed key for an existing version is caught at unwrap time by the KCV (KR-07 step 4), but only after the deploy.
- **`name` is immutable.** It is both the envelope's `kek_id` and an HKDF input (§6.1), so a rename makes every envelope unreadable. A rename is a KEK swap (§7.4).
- **The current generation is never removed,** and `current` never moves to a lower version.

### 5.2 Descriptor format v1 (one string)

The keyring is one ASCII string. It fits in a single Worker secret (5 KB limit; `research/secrets-and-keys.md` §4) or a Secrets Store value (≤ 65,536 bytes). Because it's one value, switching the current generation is atomic for each binding.

```
akr1;name=<name>;current=<v>;k<v>=<key>.<kcv>[;k<v>=<key>.<kcv>]*
  <key> = base64url, no padding, exactly 32 bytes (43 chars)
  <kcv> = lowercase hex, exactly 8 bytes (16 chars), = kcv_v of §6.1
```

Example: `akr1;name=p2-user-tokens;current=2;k1=q83v….3f9a0c1b2d4e5f60;k2=Xy9a….9c0d11e2a3b4c5d6`

Parser requirements (**KR-02**). The parser rejects with `Configuration`, before any key is used:

- The prefix must be exactly `akr1`. Fields are separated by `;`. Whitespace is not allowed, except one trailing `\n`, which is tolerated because `wrangler secret put` reads from stdin.
- `name` matches `^[a-z0-9][a-z0-9._-]{0,63}$`.
- `current` is a decimal `u32 ≥ 1` with no leading zeros.
- `k<v>` entries: decimal versions with no leading zeros, unique, and between 1 and 64 entries, in any order. Each value is `<key>.<kcv>`. `<key>` is **unpadded base64url** that decodes to **exactly 32 bytes**. `<kcv>` is exactly 16 lowercase hex characters.
- **KCV check:** the parser recomputes `kcv_v` from the decoded key, the `name` and `v` (§6.1). A mismatch is rejected (message: `k<v>: key check value does not match`). This catches a mistyped or truncated key, a key pasted under the wrong version, and a key copied from a keyring with another name.
- `current` must name a present `k<v>`.
- Unknown or duplicate fields are rejected.
- An all-zero key, or two generations with identical key bytes, are rejected (a sign of a provisioning mistake).
- **Error hygiene:** every descriptor error is a **fixed string** that names the field and, where relevant, the version (for example `k3: key is not 32 bytes of unpadded base64url`). It MUST NOT include any input bytes, offsets into a key value, or decoder messages, and it MUST NOT chain the decoder error as `source` (`base64` 0.23 `DecodeError::InvalidByte(offset, byte)` carries a key character). T2 asserts this.
- Decoded key bytes are held in `Zeroizing<[u8; 32]>`. **UNVERIFIED/limitation:** the JS string that holds the secret, and its copy into wasm memory, can't be zeroized. This is defense in depth only, as `docs/guarantees.md` already says about zeroization.

A 64-generation descriptor is about 4.3 KB (64 × 65 bytes per `k<v>` entry plus the header), which is under the 5 KB Worker-secret limit.

Descriptors are produced and changed with the `akr` CLI (§5.5), not assembled by hand.

### 5.3 Loading

The keyring crate **does no I/O and has no async on the key path** (**KR-03**). Loaders only turn a string into a `Keyring`:

| Environment | How the string is obtained | Notes |
|---|---|---|
| Workers, Worker secret | `env.secret("ANYKMS_KEYRING")?.to_string()` (the pattern the PoC uses for `GHINVITE_SESSION_SECRET`; `research/secrets-and-keys.md` §0) | Proven pattern in workers-rs |
| Workers, Secrets Store | `await env.X.get()` in the app, then `Keyring::parse(&value)` | Reading is asynchronous ([ss-w] in `research/secrets-and-keys.md` §4). A typed workers-rs accessor is **UNVERIFIED** (SD-07). Read through `JsValue` if none exists. Local dev can't read production values |
| Native, environment variable | `Keyring::from_env("ANYKMS_KEYRING")` (`std-loaders`) | For `*-server` builds and tests |
| Native, file | `Keyring::from_file(path)` (`std-loaders`) | On unix it SHOULD refuse a file readable by group or others, overridable by an explicit flag |
| Programmatic | `Keyring::builder(name).generation(v, bytes)?.current(v)?.build()?` | Tests and custom sources. The builder computes the KCVs itself |

**KR-04 (per-isolate load, fail closed, redeploy to change).**

- On Workers, the app loads the keyring **once per isolate** into a `std::sync::OnceLock<Arc<Keyring>>` (a module-level static), and shares the `Arc`. Parsing derives every per-generation KEK and KCV up front (§6.1), so no derivation happens on the request path.
- Load sequence at the top of `fetch`: if the `OnceLock` is set, use it. Otherwise read the string (awaiting Secrets Store if used), parse, and `set` it. Two concurrent first requests in one isolate may both parse. The loser of `set` drops its copy, which is harmless because both parsed the same value. No async-once primitive is needed.
- **Failure is fail-closed and not cached.** If reading or parsing fails, the request returns HTTP 500, the `OnceLock` stays unset, and the next request retries the load (parsing is cheap). The error is logged at most once per isolate, with the fixed KR-02 message only and never key bytes.
- **A running isolate never sees a new value.** This holds even if Secrets Store propagates changes live, because of the `OnceLock`. Therefore **every descriptor change MUST be followed by a deploy of every Worker in the trust unit**. That includes Secrets Store changes, where the value is shared but the isolates aren't. This spec doesn't specify a TTL-based reload. Rotation step completion is defined by the observable fingerprint check in KR-18, not by assumed propagation.
- Whether `wrangler secret put` creates and deploys a new Worker version at once, and whether `wrangler versions secret put` creates a version without deploying it, is **UNVERIFIED**. It is folded into SD-07/T11. Until T11 answers it, operators deploy explicitly after every change and confirm with KR-18.

**KR-15 (process-global install for `Sealed<T>`).** `Sealed<T>` needs a process-global keyring (ADR 0001 Consequences; `research/restate.md` "Design implications → C3"). Feature `global` provides:

```rust
// crate anykms-keyring, feature "global"
pub mod global {
    /// Sets the process-global keyring. Idempotent for an equal fingerprint (KR-18);
    /// a different fingerprint returns Configuration ("a different keyring is already
    /// installed") and leaves the installed one in place.
    pub fn install(keyring: Arc<Keyring>) -> anykms_core::Result<()>;
    /// The installed keyring, or Unavailable ("keyring not installed"), never a panic.
    pub fn get() -> anykms_core::Result<Arc<Keyring>>;
}
```

- The Worker's `fetch` entry MUST call `install` (after KR-04's load) **before** dispatching to the Restate endpoint or to any code that may deserialize a `Sealed<T>`.
- `Sealed<T>` (de)serialization with no keyring installed returns a serde error built from the `Unavailable` error. restate-toolkit MUST map it to a **retryable (non-terminal)** Restate error, never a panic and never `TerminalError`. How the Restate Rust SDK classifies a serde failure in a handler input or a journal entry is **UNVERIFIED**. The restate-toolkit `Sealed<T>` spec must confirm it with a test (T13).
- A descriptor parse failure follows KR-04: HTTP 500, nothing installed, retried on the next request.
- Tests: `install` can't be undone (the cell is a `OnceLock`). A test that needs a different keyring passes a `ContextEnvelope` directly, or runs in its own test binary. Library code other than `Sealed<T>` MUST take the keyring or `ContextEnvelope` as an argument and MUST NOT call `global::get`.

### 5.4 `secret_key` bindings (not in v1)

A `secret_key` binding exposes a WebCrypto `CryptoKey` and never raw bytes (`research/secrets-and-keys.md` §4 [sk]). Whether that key is non-extractable is UNVERIFIED (SD-07). Supporting it would mean:

- a different KEK implementation that calls WebCrypto `wrapKey`/`unwrapKey` (AES-KW or AES-GCM), whose operations are JS promises, so the futures are `!Send`. That needs `SendWrapper`/`SendFuture` or the non-`Send` trait variant that anykms's `docs/review-plan.md` 4.10 defers (`research/secrets-and-keys.md` §2);
- giving up the synchronous path (§6.6), which `Sealed<T>` needs.

So v1 supports **Worker secrets and Secrets Store values only**, which ADR 0001 item 2 accepts. ADR 0001 also says the spec "must cover both binding types". This section covers `secret_key` by excluding it, with reasons. Revisit if SD-07 shows a non-extractable `CryptoKey` and app-side key isolation becomes a requirement (OQ-3; the same condition that would revive P4, ADR 0004).

### 5.5 Provisioning CLI `akr`

A small native binary (`anykms-keyring-cli`). It reads a descriptor from stdin and writes one to stdout, so it composes with `wrangler secret put` and a password manager without writing keys to disk. Commands:

| Command | Effect | Refuses |
|---|---|---|
| `akr new <name>` | New descriptor, `k1` freshly generated (OS RNG), `current=1` | An invalid name |
| `akr add` | Adds `k<max+1>` (fresh) as readable. `max` is the highest version ever used, taken from `--min-version` if versions were removed (KR-16) | — |
| `akr promote <v>` | Sets `current=v` | `v` absent; `v` < current |
| `akr remove <v>` | Deletes `k<v>` | `v` = current |
| `akr show` | Prints `name`, `current`, versions, each KCV, and the fingerprint (KR-18). **Never key bytes** | — |
| `akr diff <old-file> <new-file>` | Checks a transition. Exit 0 if allowed | Changed bytes or KCV for an existing version; a renamed keyring; removal of the old `current`; `current` moving backwards; a new version ≤ a version present in `old` |

- Every command validates its input and output with `Keyring::parse` (KR-02).
- `akr diff` MUST be run on every descriptor change before it is deployed (§7.2 step 0). It can't detect reuse of a version that was removed before `old`. The `--min-version` record is the operator's responsibility, which is why KR-16 states it.
- T2 covers the CLI with round-trip tests.

## 6. Design

### 6.1 Per-generation KEK and key check value

**KR-05.** The root bytes are never used directly as an AES key. For each generation:

```
prk_v  = HKDF-Extract(salt = "anykms-keyring/v1/salt", ikm = root_v)
kek_v  = HKDF-Expand(prk_v, info = "anykms-keyring/v1/kek" ‖ u8(len(name)) ‖ name ‖ u32be(v), L = 32)
kcv_v  = HKDF-Expand(prk_v, info = "anykms-keyring/v1/kcv" ‖ u8(len(name)) ‖ name ‖ u32be(v), L = 8)
```

Why:

- Key separation by name and version. If the same root is accidentally reused under two names, the two keyrings still don't share a KEK.
- `kcv_v` identifies the key bytes without revealing them (a one-way HKDF output under a different `info`). It lets a wrong key be told apart from tampering (KR-07 step 4), and it makes the fingerprint (KR-18) change whenever any key changes.
- Room for future derived keys (for example a MAC key) under a different `info` without breaking the "KEK dedicated to envelopes" rule (`docs/envelope-format.md`, "Dedicated KEK").

### 6.2 Wrapped-DEK format (the bytes in the envelope's `wrapped_dek` slot)

```
wrapped = "akr1"[4] ‖ u32be(v)[4] ‖ kcv_v[8] ‖ nonce[12] ‖ AES-256-GCM(kek_v, nonce, DEK[32], aad = W) ‖ tag[16]      (76 bytes)
W       = "anykms-keyring/v1/wrap" ‖ u8(len(name)) ‖ name ‖ u32be(v) ‖ kcv_v
```

- **KR-06.** `wrap_key` accepts exactly 32-byte keys (the envelope only wraps DEKs) and returns `InvalidArgument` otherwise. It always wraps under the **current** generation with a fresh random 96-bit nonce from `getrandom`.
- **KR-07.** `unwrap_key` checks in this order, and each step returns before any later one:
  1. **Framing.** Length exactly 76 and magic `akr1`. Otherwise `InvalidArgument` ("malformed wrapped key"), which the envelope maps to `DecryptFailed` (`map_unwrap_error`). No AES call is made.
  2. **Version 0** → `InvalidArgument` (mapped to `DecryptFailed`).
  3. **Version not present**, before any AES call, and **never trying another generation** (no fallback; ADR 0001 item 2):
     - **(a)** `v` > the highest present version → **`UnknownKeyVersion`**, with a retry hint of 30 s (`Error::with_retry_hint`), and the message "wrapped key names version v, newer than this keyring's highest version m". The likely cause is an isolate running an older descriptor (§7.2). It is transient.
     - **(b)** `v` ≤ the highest present version → **`KeyVersionUnusable`**, with the message "wrapped key names removed version v". Permanent: the generation was removed (KR-16 makes versions monotonic, so an absent lower version is a removed one).
  4. **KCV.** If the slot's `kcv` ≠ `kcv_v` of the present generation, return **`Configuration`** ("key bytes for version v differ from the ones that wrapped this DEK"), before GCM. The envelope passes `Configuration` through unchanged.
  5. **GCM** open under `kek_v` with AAD `W`. Failure → `DecryptFailed`. The result must be exactly 32 bytes, held in `Zeroizing`.
- **KR-08.** `wrapped_key_version` returns `Some(KeyVersion::new(v.to_string()))`, parsed without crypto (framing failures → `InvalidArgument`). It is **not authenticated** by itself, consistent with the recipe's note on `ciphertext_version`. The version and KCV *are* authenticated at unwrap time, because `W` binds them.
- The internal AAD `W` is **not** the envelope's KEK AAD. The `KeyWrap` call still takes no AAD, so the envelope's "empty KEK AAD" rule (`docs/envelope-format.md` step 6) holds unchanged. `W` only ties the wrapped bytes to `(name, v, kcv_v)`.
- **Nonce budget:** NIST SP 800-38D caps random 96-bit nonces at 2^32 invocations per key. One wrap happens per envelope written. **KR-09:** the keyring documents this limit, and operators rotate the current generation well before it. For C2 (a few refreshes per user per day), this limit is unreachable. AES-KW (RFC 3394/5649, deterministic, no nonce) is the alternative (OQ-2).

**How consumers classify version errors.** Consumer specs MUST apply this table:

| Kind | Meaning | Restate handler | L1 / C2 | P1 session | C1 cache |
|---|---|---|---|---|---|
| `UnknownKeyVersion` | Lagging isolate (KR-07 3a), or a relabeled version | **Retryable** (non-terminal error; Restate retries with backoff) | Retry the request later. **MUST NOT** mark the grant lost or trigger re-auth | Treat as "try again" (HTTP 503), not as sign-out | Cache miss: mint a fresh token |
| `KeyVersionUnusable` | Generation removed (KR-07 3b) | **Terminal** | `StoreError::Corrupt` → lost grant → re-auth (L1 ST-13) | Session invalid → sign in again | Cache miss |
| `Configuration` | Wrong key bytes for a version (KR-07 4), bad descriptor | **Retryable**, plus an operator alert | Retry later and alert. **MUST NOT** trigger re-auth (avoids mass re-auth after a provisioning mistake) | HTTP 500 and alert | Cache miss and alert |
| `DecryptFailed` | Tampering, wrong context, swapped slot, malformed slot | **Terminal** | `Corrupt` → re-auth (L1 ST-13) | Session invalid | Cache miss |

A relabeled version can make `UnknownKeyVersion` permanent for one row. Retries are bounded by the consumer's retry policy, and the consumer escalates to terminal after it (Restate retry policy; L1 retry budget).

### 6.3 Types and trait implementations

```rust
// crate anykms-keyring

/// Immutable, cheaply clonable (Arc inside). Send + Sync. No interior I/O.
/// Implements NO anykms trait itself: it is not a KeyWrap (CX-04).
#[derive(Clone)]
pub struct Keyring { /* name, current: u32, gens: BTreeMap<u32, Gen { kek: Zeroizing<[u8;32]>, kcv: [u8;8] }> */ }

impl Keyring {
    pub fn parse(descriptor: &str) -> anykms_core::Result<Self>;               // KR-02
    pub fn builder(name: &str) -> KeyringBuilder;
    #[cfg(feature = "std-loaders")] pub fn from_env(var: &str) -> anykms_core::Result<Self>;
    #[cfg(feature = "std-loaders")] pub fn from_file(path: &std::path::Path) -> anykms_core::Result<Self>;

    pub fn name(&self) -> &str;                       // == envelope kek_id
    pub fn current_version(&self) -> u32;
    pub fn versions(&self) -> impl ExactSizeIterator<Item = u32> + '_;
    pub fn fingerprint(&self) -> Fingerprint;         // KR-18; never key bytes

    /// The supported application entry point (CX-04). `padding` must equal the
    /// `Purpose::PADDING` of every context this envelope encrypts (CX-06).
    pub fn context_envelope(&self, padding: anykms_recipes::Padding)
        -> anykms_core::Result<anykms_recipes::ContextEnvelope<anykms_recipes::EnvelopeAead<KeyringKek>>>;

    /// Raw KEK, for conformance (T5) and the KEK-swap migration job (§7.4) only.
    #[cfg(feature = "raw-kek")] pub fn raw_kek(&self) -> KeyringKek;
}

/// The KEK. Public so that types can name it; it has no public constructor
/// except `Keyring::raw_kek` (feature `raw-kek`), and no accessor in this crate or in
/// `ContextEnvelope` returns one (CX-04).
#[derive(Clone)]
pub struct KeyringKek { /* private: Keyring */ }

impl anykms_core::KeyWrap for KeyringKek { /* KR-06..08; every async body is synchronous */ }
impl anykms_core::Describe for KeyringKek { /* see "Describe and KeySemantics" below */ }

// Debug for Keyring and KeyringKek prints name, current, versions and fingerprint only,
// never key bytes (KR-10).

// Deliberately NOT implemented:
// - anykms_core::Aead, DataKeys, Mac: the KEK is dedicated to envelopes by construction
//   (docs/envelope-format.md "Dedicated KEK").
// - anykms_core::Rewrap (KR-21).

/// Marker: every future returned by this type's KeyWrap completes on its first poll.
/// Sealed; implemented only for KeyringKek (and &KeyringKek, Arc<KeyringKek>). Enables §6.6.
pub trait ReadyKek: anykms_core::KeyWrap + private::Sealed {}
```

**KR-21 (no server-side rewrap).** `KeyringKek` MUST NOT implement `anykms_core::Rewrap`. The DEK is in process memory on every path for a local key, so a "server-side" rewrap adds no isolation. It would also make `EnvelopeAead::rewrap_current` take the `EnvelopeAead::rewrap` path, which has **no key-commitment check** (`envelope.rs` lines 855–877), and so launder a swapped wrapped-DEK slot into a fresh wrap. Every rewrap of a keyring envelope goes through `rewrap_local`/`rewrap_verified`/`rewrap_to`, which check the commitment. `ContextEnvelope::rewrap_current` is defined on that basis (§6.5).

**No `Send`-future problem.** The trait bodies never await anything (no JS promise, no timer), so the futures `trait_variant::make(Send)` requires are trivially `Send`. No `SendWrapper` or `unsafe impl Send` is needed. This is unlike the PoC's `WasmSend` shims for KV/D1 (`analysis/poc-ghinvite.md` §6.3). **KR-11:** the crate is `#![forbid(unsafe_code)]`, like `anykms-local`.

**KR-18 (fingerprint).** `Keyring::fingerprint()` identifies a descriptor without revealing keys:

```
fp = hex(SHA-256("anykms-keyring/v1/fp" ‖ u8(len(name)) ‖ name ‖ u32be(current)
                 ‖ for each present v, ascending: u32be(v) ‖ kcv_v)[0..8])
Display: "<name> current=<v> versions=<v1,v2,…> fp=<16 hex>"
```

- It changes when the name, `current`, the set of versions or any key bytes change. It is independent of the order of fields in the descriptor.
- Every consuming Worker MUST log the fingerprint once per isolate when it loads the keyring (KR-04), and MUST include it in every log line for a keyring error (`UnknownKeyVersion`, `KeyVersionUnusable`, `Configuration`).
- Every consuming Worker MUST expose it on a diagnostic route. The route path and authentication are chosen by the consumer spec. It SHOULD be behind the app's admin authentication: the fingerprint isn't secret, but it reveals the keyring name and versions.

**Describe and `KeySemantics`** (read by conformance, T5):

- `KeyInfo::new("anykms-keyring", Capabilities::KEY_WRAP)`, with `primary_version = Some(current)`, and `versions` = every present version, each enabled. No key-level state (`KeyInfo::state` is `None`). No timestamps (no `SystemTime`).
- `KeySemantics::new(VersionModel::Floating, VersionShape::Numeric)` with `primary_reported = true`, `versions_listed = true`, `ciphertext_reveals_version = true`, `state_scope = StateScope::VersionOnly`, `data_keys = None`. Every other field keeps its default.

**Provider** (feature `raw-kek` only). `KeyringProvider` registers the `keyring://` scheme and serves `key_wrap` and `describe`:

- State: `RwLock<BTreeMap<String, Keyring>>`. `KeyringProvider::new([keyring, …])`, plus `insert(keyring)` and `replace(keyring)`. Conformance uses those to create and rotate keys (T5). The provider never parses a URI as a key source.
- URI grammar: `keyring://<name>`, with `<name>` per KR-02. A path, query, fragment or version suffix → `InvalidArgument`. A well-formed name not in the map → `NotFound`.

### 6.4 Using it with the envelope

- **Normal case:** `keyring.context_envelope(padding)`, which wraps an `EnvelopeAead<KeyringKek>` with `kek_id = name`. Generations are internal to the KEK, as in transit. Rotation moves an envelope to the current generation with `ContextEnvelope::rewrap_current`, which is a commitment-checked local rewrap (KR-21, §7.2). A later KEK swap uses `EnvelopeKeyring` (§7.4).
- **KR-12.** One keyring name per *trust unit*: the Workers of one product that must read each other's data. For P1 that is web + Restate endpoint, as in the PoC (`analysis/poc-ghinvite.md` §2). Purposes are separated by the context (§6.5), not by separate keyrings. A separate keyring is warranted only when rotation cadence or the set of readers differs. OQ-9 covers whether P1 and P2 share one. **Note (P2):** P2 uses two keyrings, `P2_SESSION_KEYRING` (`session`, read by `p2-web` only) and `P2_USER_TOKEN_KEYRING` (`user-token`, read by `p2-web`'s `stage()` path and `p2-restate`), because their reader sets differ (P2 §13.1, PD-13). The registry is unchanged.

### 6.5 Typed encryption context (the AAD convention)

**Model.** A context is a non-empty set of `(key, value)` fields. `purpose` is always one of them. Each purpose declares its keys, each with a kind (string or decimal) and a presence (required or optional), and its padding.

**Field rules (CX-01)**

- Keys match `^[a-z][a-z0-9_]{0,31}$`. `purpose` is reserved.
- Values are UTF-8 strings of 1–255 bytes, compared as exact bytes with no normalization.
- A key declared `Decimal` MUST hold a canonical decimal `u64`: `^(0|[1-9][0-9]{0,19})$` and ≤ `u64::MAX`. `ContextBuilder::str` with a non-canonical value for a `Decimal` key (for example `"007"`, `"+7"`, `" 7"`) is rejected. The GitHub numeric user ID is used, never the login, because logins can be renamed.
- At most 16 fields, `purpose` included. Duplicate keys are rejected.

**Purpose rules (CX-02)**

- A purpose name matches `^[a-z][a-z0-9-]{0,47}$`.
- A context is valid only if it contains **every** required key of its purpose and **no key** outside the declared keys (strict). A value whose kind doesn't match the declaration (`u64` given for a `Str` key, or a non-canonical string for a `Decimal` key) is invalid.
- The declaration itself is validated on first use (key regex, no duplicate, no `purpose` key, `NAME` regex). An invalid declaration fails every `build()` with `InvalidArgument`.
- Violations return `InvalidArgument` before any cryptographic work.

**Canonical encoding (CX-03).** This is the user AAD handed to the envelope:

```
aad = "anykms-ctx/v1" ‖ 0x00
    ‖ u8(n_fields)
    ‖ for each field in ascending byte order of key:
          u8(len(key)) ‖ key ‖ u16be(len(value)) ‖ value
```

- Every component is length-prefixed and the field order is fixed, so the encoding is **injective**: distinct contexts never produce equal bytes. The leading domain tag versions the convention separately from the envelope.
- `n_fields ≥ 1` always, so **the AAD is never empty**.
- The kind of a field is not encoded: `Decimal` values are canonical strings, so the bytes are the same however the value was supplied.

**Empty and raw AAD are unrepresentable (CX-04).** In a build without `raw-kek`, application code has no way to encrypt with keyring material under raw AAD:

- `EncryptionContext` has no public constructor that skips `purpose`. `Default` is not implemented.
- `Keyring` implements no anykms trait. The KEK type, `KeyringKek`, has no public constructor without `raw-kek`, so application code can't build an `EnvelopeAead<KeyringKek>` or a `KeyWrapKey` from it. `KeyWrap`'s forwarding impls for `&T`, `Arc<T>` and `Box<T>` don't help without an instance.
- The `keyring://` provider exists only with `raw-kek`, so the registry can't hand out a keyring `KeyWrapKey` either.
- `ContextEnvelope` has **no method that takes raw AAD bytes**, does **not** implement `anykms_core::Aead`, and has **no accessor** that returns the inner envelope, its KEK or any `Aead` implementation (there is no `inner()`).
- `ContextEnvelope<A>` is bounded by a **sealed** trait `EnvelopeBackend`, implemented only for `EnvelopeAead<K: KeyWrap>` and `EnvelopeKeyring<K: KeyWrap + Clone>`. A plain `AeadKey` or any other `Aead` can't be wrapped.
- **CI check:** each application workspace (P1, L1 consumers, restate-toolkit) runs `cargo tree -e features -i anykms-keyring` and fails if `raw-kek` is enabled for a non-dev, non-test target (T8).
- **Residual, stated explicitly:** (a) code compiled with `raw-kek` (conformance tests and the §7.4 migration job) can still encrypt with raw AAD; (b) `ContextEnvelope::new` is public, so code that holds some *other* KEK (not keyring material) can wrap it. Neither path reaches keyring material from an application build. T1b checks the compile-time part.
- On decrypt, a context that differs from the one used at encryption in any key or value fails with `DecryptFailed` (GCM tag), which is indistinguishable from tampering (`docs/envelope-format.md`, "Errors").

**Values must be recoverable (CX-05).** Every field value of a purpose, optional ones included, MUST be derivable at decrypt time from plaintext columns of the same row, the storage key, or the request. Each consumer spec names that source per key (the registry column below). A context field that only exists inside the plaintext, or only at write time, is forbidden. Under the strict rules it would make the row undecryptable.

**Padding is part of the purpose (CX-06).** Each purpose declares `PADDING`. `ContextEnvelope` carries one padding, fixed at construction. `encrypt` returns `InvalidArgument` ("purpose padding does not match this envelope") when `ctx.padding() != self.padding()`. A consumer that encrypts purposes with different paddings holds one `ContextEnvelope` per padding. `decrypt` accepts any padding, because the envelope's `padded` flag describes it.

```rust
// crate anykms-recipes, feature "context"

#[derive(Clone, Copy, Debug, PartialEq, Eq)] pub enum FieldKind { Str, Decimal }
#[derive(Clone, Copy, Debug, PartialEq, Eq)] pub enum Presence { Required, Optional }
#[derive(Clone, Copy, Debug)] pub struct FieldSpec { pub key: &'static str, pub kind: FieldKind, pub presence: Presence }

/// Declares a purpose. Usually generated by `purpose!`. Hand implementations are allowed:
/// `build()` enforces CX-01..CX-03 against the declaration at runtime, so the macro is a
/// convenience, not a safety boundary.
pub trait Purpose {
    const NAME: &'static str;
    const FIELDS: &'static [FieldSpec];
    const PADDING: Padding;
}

pub struct EncryptionContext { /* sorted fields incl. purpose; padding; canonical bytes cached */ }

impl EncryptionContext {
    /// Starts a context for purpose P. `build()` enforces CX-01/CX-02.
    pub fn builder<P: Purpose>() -> ContextBuilder<P>;
    pub fn purpose(&self) -> &str;
    pub fn get(&self, key: &str) -> Option<&str>;
    pub fn padding(&self) -> Padding;                 // P::PADDING
    pub fn as_aad(&self) -> &[u8];                    // CX-03 bytes, never empty
}

impl<P: Purpose> ContextBuilder<P> {
    pub fn str(self, key: &'static str, value: impl AsRef<str>) -> Self;
    pub fn u64(self, key: &'static str, value: u64) -> Self;   // Decimal keys only; canonical decimal
    pub fn build(self) -> anykms_core::Result<EncryptionContext>;
}

/// Sealed: implemented only for EnvelopeAead<K: KeyWrap> and EnvelopeKeyring<K: KeyWrap + Clone>.
pub trait EnvelopeBackend: private::Sealed + Send + Sync { /* crate-private methods */ }

/// The only application-facing API (CX-04). No `inner()`, no `Aead` impl.
pub struct ContextEnvelope<A: EnvelopeBackend> { /* inner: A, padding: Padding */ }

impl<A: EnvelopeBackend> ContextEnvelope<A> {
    /// `inner`'s padding must equal `padding`, else InvalidArgument.
    pub fn new(inner: A, padding: Padding) -> anykms_core::Result<Self>;
    pub fn padding(&self) -> Padding;
    pub async fn encrypt(&self, pt: &[u8], ctx: &EncryptionContext) -> Result<Ciphertext>;            // CX-06
    pub async fn decrypt(&self, ct: &Ciphertext, ctx: &EncryptionContext) -> Result<Zeroizing<Vec<u8>>>;
    /// Commitment-checked local rewrap to the current KEK version: EnvelopeAead::rewrap_local,
    /// or EnvelopeKeyring::rewrap_local. Never the server-side `rewrap` (KR-21).
    pub async fn rewrap_current(&self, ct: &Ciphertext) -> Result<Ciphertext>;
    /// As rewrap_current, and the payload must open under `ctx` (EnvelopeAead/EnvelopeKeyring::rewrap_verified).
    pub async fn rewrap_verified(&self, ct: &Ciphertext, ctx: &EncryptionContext) -> Result<Ciphertext>;
    /// Unauthenticated version of the wrapped DEK (Aead::ciphertext_version of the inner envelope).
    pub fn ciphertext_version(&self, ct: &Ciphertext) -> Result<Option<KeyVersion>>;
}

/// Keyring-specific operations (concrete impl block).
impl ContextEnvelope<EnvelopeAead<KeyringKek>> {
    pub fn current_version(&self) -> u32;
    pub fn needs_rewrap(&self, ct: &Ciphertext) -> Result<bool>;      // KR-17
}
```

**KR-17 (`needs_rewrap`).** `ContextEnvelope<EnvelopeAead<KeyringKek>>::needs_rewrap(ct)`:

1. `Envelope::parse(ct)`. A malformed envelope → `DecryptFailed`.
2. `kek_id` ≠ the keyring name → `Err(DecryptFailed)` ("kek_id mismatch"). The row belongs to another keyring, and the completion scan reports it as a bad row.
3. Read the wrapped slot's version without crypto (KR-08). Framing failure → `DecryptFailed`.
4. Return `Ok(version != current_version())`.

It does no cryptography and doesn't authenticate the version. That is fine for selecting rows and for counting them in the completion scan. Removal is still safe, because a mislabeled row fails on decrypt anyway. Tested in T7.

**`purpose!` macro.** It generates a struct with one public field per key (same visibility as the struct), an `impl Purpose`, and `impl TryFrom<&Struct<'_>> for EncryptionContext` with `Error = anykms_core::Error` (`InvalidArgument` on any CX-01/CX-02 violation, for example an empty or over-length string). There is no infallible `From`. Grammar (EBNF; terminals in quotes; `IDENT`, `STRING_LIT`, `LIFETIME`, `VIS` are Rust tokens):

```
purpose_decl = [ VIS ] "struct" IDENT [ "<" LIFETIME ">" ] "=" STRING_LIT
               [ "padding" "=" padding_expr ]
               "{" field { "," field } [ "," ] "}" ;
field        = IDENT ":" field_type ;                  (* IDENT is the context key *)
field_type   = base_type | "Option" "<" base_type ">" ;   (* Option<_> = Presence::Optional *)
base_type    = "String" | "&" LIFETIME "str"           (* FieldKind::Str *)
             | "u64" ;                                 (* FieldKind::Decimal *)
padding_expr = "none" | "block" "(" INT_LIT ")" ;      (* default: none *)
```

```rust
anykms_recipes::purpose! {
    pub struct UserTokenCtx<'a> = "user-token" padding = block(64) {
        app_client_id: &'a str,
        user_id: u64,
    }
}
// let ctx = EncryptionContext::try_from(&UserTokenCtx { app_client_id: "Iv23li…", user_id: 583231 })?;
```

The macro emits a `const` assertion where a check is possible at compile time (field count ≤ 15). Key and name regexes are checked by `build()` at runtime and by T4.

**Purpose registry (v1).** The registry lives in this spec until the anykms docs take it over. Each consumer spec cites it, and a new purpose is added here first. v1 registers **no optional keys**: every key is required, so every value is reconstructed at decrypt time (CX-05).

| Purpose | Keys (kind) | Padding | Source of each value at decrypt time (CX-05) | Owner |
|---|---|---|---|---|
| `user-token` | `app_client_id` (Str: GitHub App client ID), `user_id` (Decimal: GitHub numeric user ID) | `block(64)` | `app_client_id`: L1 store namespace column; `user_id`: subject column (L1 ST-10) | L1 (ADR 0002) |
| `installation-token-cache` | `app_id` (Decimal: GitHub App ID), `installation_id` (Decimal), `scope` (Str: scope profile name), `expires_at` (Decimal: Unix seconds) | `block(64)` | Cache key columns, and the plaintext `expires_at` column (ADR 0003 item 3). The `scope` value is L2's `cache_scope` = `<profile name>@<permission fingerprint>` (L2 BR-06, BR-14); still a `Str` | L2/C1 (ADR 0003) |
| `restate-sealed` | `type` (Str: stable type name of `T`) | none | Compile-time constant of `Sealed<T>` | restate-toolkit (OQ-7) |
| `session` | `session_id` (Str) | none | The KV key (session ID) | P1 §11.3 (PD-G); also P2 §9.1 |
| `bulk-email-review` | `email_ref` (Str), `batch_id` (Str), `row` (Decimal), `account_id` (Decimal), `installation_id` (Decimal) | `block(256)` | Immutable plaintext columns in `email_review`, matched to the currently authorized exact selected command; fixed one-hour review escrow | P1 M2 ADR0010, ID-61; additive registration 2026-10-17 |
| `bulk-email` | `email_ref` (Str), `batch_id` (Str), `row` (Decimal), `account_id` (Decimal), `installation_id` (Decimal) | `block(256)` | Immutable plaintext columns in `email_vault`, independently matched to the authorized batch/row delivery command; `row` uses `row_number` in SQL | P1 M2 §6.6/§11.4, ID-61; additive registration 2026-10-17 |
| `pending-signin` | `session_id` (Str) | none | Trusted canonical session ID recovered from the request session; the SQL lookup key is a domain-separated SHA-256 hash, never a substitute context value | P1 §6.3/§11.3; additive registration 2026-10-16 |
| `attempt-continuation` | `scope` (Str), `binding` (Str) | none | Plaintext columns `attempt_continuations.scope` and `attempt_continuations.binding` (P1 §11.3) | P1 §11.3 (PD-G) |

- **Two App identifiers, two key names.** A user token is bound to the App's **client ID**, because the OAuth refresh and grant APIs are keyed by it (`DELETE /applications/{client_id}/grant`; `PLAN.md` §8). The installation-token cache is bound to the numeric **App ID**, which signs the App JWT. Distinct key names make a mix-up a build-time `InvalidArgument`, not a runtime `DecryptFailed`. ADR 0001's example `app=<app id>` is refined accordingly.
- **`session` binds only `session_id`.** Session expiry lives inside the encrypted record in P1's design, so it isn't available at decrypt time (CX-05). Binding `app` would add nothing, because the keyring is already per trust unit (KR-12).
- **Candidate, not registered:** `link-secret`, which depends on P1 OQ-1 (keep an anykms-sealed copy of the link URL). It gets registered here only if P1 OQ-1 decides for the sealed copy.
- **Dropped in revision 2:** the optional keys `grant` (`user-token`), `repos` (`installation-token-cache`) and `service` (`restate-sealed`), and the `web-session` purpose (replaced by `session`). None of these was used by an implementation.

Why these bindings: row binding moved from cryptbox records to AAD (ADR 0001 Consequences). A ciphertext copied to another row, user, app, installation, session or purpose fails to decrypt. This replaces cryptbox `InRecord<…>` binding (`research/secrets-and-keys.md` §1).

### 6.6 Synchronous path (wasm-friendly, for serde)

The Restate SDK serde traits are synchronous and take no context (`research/restate.md` Q2; ADR 0001 Consequences), so `Sealed<T>` can't `.await`.

**KR-13.** `ContextEnvelope<EnvelopeAead<K>>` where `K: ReadyKek` provides these methods through the sealed `anykms_keyring::ContextEnvelopeSync` extension trait. Consumers import that trait. It is defined in the keyring crate and implemented only for local ready-KEK envelopes, avoiding an orphan inherent implementation or a recipes → keyring dependency cycle:

```rust
pub fn encrypt_sync(&self, pt: &[u8], ctx: &EncryptionContext) -> Result<Ciphertext>;
pub fn decrypt_sync(&self, ct: &Ciphertext, ctx: &EncryptionContext) -> Result<Zeroizing<Vec<u8>>>;
pub fn rewrap_current_sync(&self, ct: &Ciphertext) -> Result<Ciphertext>;
```

- Implementation: poll the async future **once** with `std::task::Waker::noop()` (stable since Rust 1.85; MSRV is 1.91). If it returns `Pending`, that is a bug, and it maps to `Backend` ("KEK future did not complete synchronously"). It never blocks or spins.
- No executor dependency (no `futures-executor`, no tokio).
- `ReadyKek` is sealed, so a future remote KEK can't be plugged into the sync path by accident. This enforces ADR 0004's note that a remote KEK needs DEK caching or an async layer.
- `Sealed<T>` obtains its envelope from `global::get()` (KR-15).

### 6.7 wasm32 / Workers constraints (KR-14)

- Target `wasm32-unknown-unknown`, built with `default-features = false`. The **application** enables `getrandom = { version = "0.4", features = ["wasm_js"] }`. `research/secrets-and-keys.md` §1 states that no `--cfg getrandom_backend` is needed with getrandom 0.4. **UNVERIFIED** by the spike: `spikes/wasm-crypto/README.md` built with `RUSTFLAGS='--cfg getrandom_backend="wasm_js"'` set, so it doesn't prove the claim. T8 builds with `RUSTFLAGS` unset to settle it in CI.
- No `std::time` calls anywhere on the keyring or context paths. `Describe` reports no timestamps. No `Retry`, `CachedVerifier` or `dek_cache` (they need `Instant`; `docs/guarantees.md`).
- No tokio, reqwest, rustls, ring or aws-lc (KR-01).
- Pure-Rust primitives only: `aes-gcm` 0.11, `hkdf` 0.13, `sha2` 0.11 (workspace `Cargo.toml`). In wasm, AES runs in software (no AES-NI). RustCrypto's software AES is the fixsliced, constant-time implementation (MEDIUM: from RustCrypto docs, not re-checked here).
- Per-operation cost for a token-sized payload: one HKDF, one GCM over the DEK (wrap) and one GCM over the payload. Microseconds natively (criterion benchmarks, T10). **UNVERIFIED** in workerd and on deployed Workers. It can't be measured from inside the handler: `std::time::Instant` panics on wasm32 (`docs/guarantees.md`), and Workers advance `Date.now()`/`performance.now()` only on I/O, so in-handler timing of synchronous crypto reads 0 (documented Workers behaviour; UNVERIFIED for local workerd). Measurement plan: the SP-16 deployed run executes N = 10,000 iterations per request and reads CPU time from the platform (Workers Logs or Tail `cpuTime`), divided by N.
- Size: the envelope recipe wasm was about 121 KB with a toy KEK (`research/secrets-and-keys.md` S2). The keyring adds HKDF/SHA-256, which the recipe already pulls in. Expect a small delta (UNVERIFIED).

## 7. Rotation, rewrap and removal

### 7.1 States per generation

| State | In descriptor | Wraps new DEKs | Unwraps |
|---|---|---|---|
| current | yes, `current=v` | yes | yes |
| readable | yes | no | yes |
| removed (`v` ≤ highest present) | no | no | no, `KeyVersionUnusable`, permanent (KR-07 3b) |
| unknown (`v` > highest present; seen only on a lagging isolate) | no | no | no, `UnknownKeyVersion` with a 30 s retry hint, transient (KR-07 3a) |

### 7.2 Scheduled rotation (reader-first, as anykms v2 rollout prescribes for formats)

Each step changes the descriptor and **deploys every Worker in the trust unit** (KR-04). With Secrets Store, one shared value serves every bound Worker, but each Worker is still redeployed.

**KR-18a (rollout check).** A descriptor change is **rolled out** when, for every Worker in the trust unit:

1. the active deployment is a single Worker version at 100% that was created after the change, and no gradual deployment is in progress;
2. the diagnostic route (KR-18) returns the new fingerprint on 20 consecutive requests;
3. for 10 minutes after (1) and (2) hold, the logs show no keyring load or keyring error line carrying an older fingerprint.

The 10-minute window is a spec decision. How long old isolates keep serving after a deploy is UNVERIFIED (T11). Step 1's wait guards **availability** only: thanks to KR-07 3a, an isolate that misses the change returns a retryable `UnknownKeyVersion`, never data loss.

**KR-20 (no rollback across a Promote).** After Promote (step 2) has started, no Worker in the trust unit may be rolled back or gradually deployed to a version whose descriptor lacks the current generation. Whether a Workers version rollback restores that version's old secret bindings is UNVERIFIED (T11), so operators assume it does. If a code rollback is needed, deploy the old code as a new version with the current descriptor. Violating KR-20 makes envelopes under the newest generation return `UnknownKeyVersion` (retryable) on the rolled-back isolates until the rollback is reverted.

Steps:

0. **Check.** Every new descriptor passes `akr diff <old> <new>` (§5.5) before it is deployed.
1. **Add.** `akr add`: the new root `k(N+1)` is *readable*, `current` is still `N`. Deploy. Done when KR-18a holds.
2. **Promote.** `akr promote N+1`. Deploy. Done when KR-18a holds. Record **T_p**, the time KR-18a held. From T_p on, every new envelope names version `N+1` in the wrapped slot.
3. **Rewrap sweep**, per consumer, as the table below says. For D1 rows:
   - For each row where `ctx_env.needs_rewrap(ct)` (KR-17) is true, compute `ctx_env.rewrap_current(ct)`. That is a local, commitment-checked rewrap (KR-21): a row whose slot was swapped fails with `DecryptFailed` and is reported, not rewritten. Use `rewrap_verified(ct, ctx)` when the job should also catch rows copied between contexts or with corrupted payloads.
   - Write the result with a **conditional update on the old ciphertext** (`UPDATE … WHERE id=? AND ct=?`), so a concurrent writer is never overwritten. For L1 this goes through its store's `rewrap_row` (L1 ST-14).
   - A row where `needs_rewrap` returns `Err` is counted as a bad row, logged by ID (never its contents), and left alone.
   - C2 rows also migrate naturally: every refresh writes a new envelope under the current generation. Idle users get the L1 keep-alive (about every 30 days; `PLAN.md` §8).
4. **Completion gate.** Every row of the table below must report done.
5. **Remove.** `akr remove N`. Deploy. Done when KR-18a holds. Any envelope still on `N` now fails with `KeyVersionUnusable`, which is the intended, observable signal.

The rewrap never touches payloads (`docs/envelope-format.md`, key-wrapping slot). The cost per row is one unwrap and one wrap.

**Per-consumer rotation (completion gate for step 4):**

| Data | Consumers | Sweep | Done when |
|---|---|---|---|
| D1 rows | L1 user tokens; C1 cache if on D1; P1 `attempt_continuations` | CAS sweep (step 3) | A full scan finds 0 rows with `needs_rewrap = true` **and** 0 bad rows (or every bad row has been triaged and deleted) |
| KV entries with a TTL | P1 sessions (30 days); C1 cache if on KV (≤ 1 h) | None. KV has no conditional update, so a sweep could overwrite a concurrent write | `now ≥ T_p + max TTL + 1 h`. Requires that every write sets a KV `expiration_ttl` no longer than the purpose's maximum lifetime. A consumer that can't guarantee that MUST use D1 instead |
| Restate journals holding `Sealed<T>` | restate-toolkit | None. Journals are immutable | KR-19 |
| Restate K/V state, workflow state, promise or awakeable payloads | — | — | Not applicable: `Sealed<T>` is forbidden there (KR-19) |
| D1 Time Travel, backups | All D1 consumers | None | Not gated. After removal those copies become unreadable, which is the intent |

**KR-19 (Restate-held envelopes).**

- `Sealed<T>` MUST NOT be stored in Restate Virtual Object state, workflow state, workflow promises or awakeable payloads in v1. Those outlive the journal and can't be swept (`research/restate.md` Q4: "Workflow state and promises are dropped after workflow retention"). restate-toolkit enforces this by design (no `Sealed<T>` setter on state; documented rule) and by its marker-scan test (T12). A `Sealed<T>` therefore lives only in journal entries (`ctx.run` results and call parameters).
- Journal retention starts when an invocation **completes**. A suspended or long-running invocation, such as P1's multi-day approval waits, keeps its journal and replays it on resume (`research/restate.md` Q3). So generation `N` MUST NOT be removed until both hold:
  1. no invocation of a secret-touching service (one that creates or reads `Sealed<T>`; the restate-toolkit spec lists them per deployment) that was created before T_p is still not completed. Checked through the Restate introspection SQL, for example `SELECT id FROM sys_invocation WHERE target_service_name IN (…) AND status != 'completed' AND created_at < <T_p>` returning 0 rows. `sys_invocation` exists (`spikes/restate-sealed-journal/README.md`); its column names are **UNVERIFIED**;
  2. `now ≥ T_p +` the journal retention of those services (ADR 0003 Consequences: short retention on secret-touching services).
- If (1) can't be met because an old invocation waits indefinitely, the operator either waits or cancels that invocation. Removing `N` early makes its next replay fail with `KeyVersionUnusable`, which is terminal (§6.2 table).

### 7.3 Suspected compromise (descriptor granularity)

**Scope.** Every generation lives in the same descriptor string and is deployed to the same Workers (§5.2, §8.1). Any realistic leak (the secret value, a deployer or CI credential, a compromised Worker; §8.2) therefore exposes **every generation present at the time of the leak**. The procedure treats all of them as compromised. Rewrap is not enough: it re-wraps the **same DEK**, and anyone who copied an envelope and holds a leaked root can still unwrap its DEK from the copy.

1. **Close the leak path first.** If the leak came through a deployer, CI or account credential, rotate that credential before delivering new key material through the same channel.
2. **New root, out of band.** Generate `k(N+1)` with `akr add` on an operator machine outside the leak path, and deliver it directly to the secret binding(s). Promote it at once (`akr promote N+1`). The old generations stay readable for now, because step 3 must decrypt with them. Deploy. Skipping the KR-18a wait is acceptable here: the cost is transient, retryable `UnknownKeyVersion` errors on lagging isolates.
3. **Re-encrypt everything under fresh DEKs:** for every envelope under this keyring, `decrypt` then `encrypt` (**not** a rewrap), written with the same conditional update as §7.2 step 3. KV data isn't re-encrypted. It is invalidated (step 4).
4. **Rotate the protected secrets themselves**, because old envelopes survive in copies:
   - C2: force a refresh for every grant (the old refresh token is spent), or revoke the grant and require re-auth (`DELETE /applications/{client_id}/grant`; `PLAN.md` §8).
   - C1: flush the shared cache. Tokens expire within 1 h. Whether a stateless installation token can be revoked is unproven (SP-23).
   - Sessions: global invalidation (P1 §11.3, "Global Session Invalidation").
   - Attempt continuations: invalidate per P1's rules.
   - Restate: cancel every in-flight invocation of a secret-touching service that may hold a `Sealed<T>` under an old generation, and re-run the business operation if needed (KR-19).
5. **Remove every old generation.** The descriptor now holds only `k(N+1)`. Deploy and confirm with KR-18a.
6. **Residual exposure, recorded in the incident:** D1 Time Travel and backups keep old envelopes, which remain decryptable with the leaked roots. Step 4 makes their contents dead credentials. That is why step 4 is mandatory and not optional. The length of the D1 Time Travel window for the plan in use is UNVERIFIED.

Tested by T7b.

### 7.4 Later KEK swap to a real KMS (ADR 0004)

- A migration job built with `raw-kek` builds `EnvelopeKeyring::new("kms-kek", remote).with_kek("<keyring name>", keyring.raw_kek())`, with the remote KEK as primary, and wraps it in a `ContextEnvelope`.
- Then `ContextEnvelope::rewrap_current` (which calls `EnvelopeKeyring::rewrap_local`, local move with the commitment checked; KR-21) and a completion scan. The scan counts envelopes whose `kek_id` is not the primary. The version-aware `needs_rewrap` (KR-17) applies only to the keyring backend, and a KMS-side version check is specified with the swap.
- Payloads are never re-encrypted.
- Prerequisites, all outside this spec: a fetch-based anykms transport, a `Send` story, and DEK caching or an async layer for `Sealed<T>`. Until then `ReadyKek` stays sealed to the local keyring (KR-13).

## 8. Security and threat model

### 8.1 Trust domain

The keyring sits in the **same trust domain as a Worker secret**. Trusted: Cloudflare (runtime, secret storage, Secrets Store) and every account member who can deploy a bound Worker or edit or bind the secret. Such a person can read the root keys (`research/secrets-and-keys.md` §3 threat table, §5 threat model; R-C4-cf-kms). The user accepted this model (ADR 0001 Context; ADR 0004). All generations share one descriptor, so the unit of compromise is the descriptor, not a generation (§7.3).

### 8.2 What it protects and what it doesn't

| Threat | Protected? | Mechanism |
|---|---|---|
| A D1/KV dump, Time Travel export or backup; a read of Restate state or journal (operator, introspection, admin API) | **Yes** | Ciphertext only; the key never reaches the storage or Restate |
| Tampering with a stored ciphertext | **Yes** | GCM tag; commitment check before GCM |
| Copying a ciphertext to another row, user, app, installation, session or purpose | **Yes** | Required context fields (CX-02); a mismatch is `DecryptFailed` |
| An envelope that decrypts under two different keys ("invisible salamanders") | **Yes** | Key commitment (`docs/envelope-format.md` "Key commitment") |
| Swapping the wrapped-DEK slot (for example with another DEK wrapped under the same keyring) | **Yes**, on decrypt **and** in the rotation sweep | Commitment mismatch → `DecryptFailed`. The sweep uses only commitment-checked rewraps (KR-21), so a swapped slot is reported, never laundered into a fresh wrap |
| Relabeling `kek_id`, the version or the KCV in the wrapped slot | Detected | `kek_id` is an unauthenticated hint and a mismatch is `DecryptFailed`. Version and KCV are bound by `W` (§6.2). A relabeled version yields `UnknownKeyVersion`/`KeyVersionUnusable`, and a relabeled KCV yields `Configuration`. Either way the row is denied, so the attacker gains a DoS on that row, which they had already. Operators tell a single-row `Configuration` (tampering suspect) from an all-rows one (provisioning mistake) by its spread |
| Wrong key bytes provisioned for an existing version | **Detected** | KCV in the descriptor (KR-02) and in the slot (KR-07 step 4) → `Configuration`, never mistaken for tampering, never mass re-auth (§6.2 table) |
| Rollback or replay of an older ciphertext for the **same** context, by someone with DB write | **No** | No freshness. For C2, a rolled-back refresh token is already spent, so the result is `bad_refresh_token` → re-auth (DoS, not a leak; R-C2-storage). For the C1 cache, `expires_at` is bound (registry), so an old ciphertext can't be paired with a fresh `expires_at`. Only a whole old (ciphertext, `expires_at`) pair can be restored, and the broker sees it as expired and re-mints |
| Raw-AAD use of keyring material by application code | **Yes** in builds without `raw-kek` | CX-04, T1b, T8. Residual: code built with `raw-kek` |
| A compromised app Worker | **No** | It holds the keys. A decrypt oracle *and* key exfiltration of **every** generation (§7.3) |
| A CF account admin or deployer | **No** | Same trust domain (§8.1) |
| Payload length | Partly | Per-purpose padding (CX-06, registry): `block(64)` for tokens |
| Use after removal | **Yes** | KR-07: `KeyVersionUnusable`, no fallback |

### 8.3 Known envelope properties (the consumer specs MUST repeat these; ADR 0001 Consequences)

- **The AAD is not committed** against a malicious creator. Whoever knows a DEK can craft one ciphertext valid under two AADs (`docs/envelope-format.md` Security notes). Here the only creators are holders of the KEK, which is the same trust domain as readers, so this is accepted. A v2 could put `SHA-256(user_aad)` into the HKDF `info` (OQ-10).
- **`kek_id` is an unauthenticated routing hint.**
- **KEKs must be dedicated to envelopes.** This is enforced by construction: `KeyringKek` implements neither `Aead` nor `DataKeys` (§6.3).
- **No authorship and no replay protection.**
- **The KEK nonce budget** is 2^32 wraps per generation (KR-09).

### 8.4 Key commitment

Envelope v1 is key-committing for the DEK (`commit = HKDF(DEK)[32..64]`, compared in constant time before GCM). Every rewrap path this spec allows keeps that check (KR-21). The keyring adds commitment-like binding for the KEK version and key identity through the wrap AAD `W`. This is one of D1's stated reasons (`PLAN.md` §9).

## 9. Format status and why it's acceptable here

- Envelope v1 is **frozen** and every future release decrypts it. Its **external review is pending**, so anykms says it "is not yet recommended for long-lived data" (`docs/envelope-format.md` status; `docs/guarantees.md`). The keyring wrap format (§6.2), the descriptor (§5.2), the KCV and fingerprint derivations (§6.1, KR-18) and the context encoding (CX-03) are new and **unreviewed** too. They need test vectors and must enter the same review.
- **Why it's acceptable for the consumers above:** each of them holds data that is **recoverable** or short-lived:
  - C1 cache entries live ≤ 1 h and can be re-minted (ADR 0003).
  - `Sealed<T>` values live only in journals (KR-19). They are recoverable by re-running the business operation, not by decrypting. An in-flight invocation that loses its generation fails terminally, which KR-19 prevents in normal rotation.
  - Sessions can be invalidated and the user signs in again.
  - Attempt continuations can be invalidated and the attempt restarted (P1 rules).
  - C2 tokens are replaced on every refresh, and losing them already has a designed outcome: lost grant → re-auth (`PLAN.md` §8, R-C2-refresh).
  - If the review finds a flaw, v1 still decrypts, new writes move to the new version, and C2 data turns over through refresh and keep-alive within about 30 days, without a dedicated migration.
- **Gate (unchanged from ADR 0001):** the anykms envelope review replaces cryptbox review #12 as **the gating item before C2 production**. Building and running P1 (no long-lived user tokens) isn't gated. Whether recoverability is reason enough to relax the C2 gate is the user's decision (OQ-6).
- The FIPS story is "algorithm-friendly", not validated. The envelope payload encryption runs in RustCrypto in-process, and anykms has no `fips` feature (`docs/guarantees.md` "FIPS scope").

## 10. Errors (summary)

| Situation | Kind | When |
|---|---|---|
| Bad descriptor (format, length, duplicates, current missing, KCV mismatch) | `Configuration`, fixed message naming field and version only, no `source` | `parse`/loaders/builder (KR-02) |
| `global::install` with a different fingerprint | `Configuration` | KR-15 |
| `global::get` before `install` | `Unavailable` (retryable) | KR-15 |
| Context missing a required key, unknown key, wrong kind, non-canonical decimal, bad value, invalid declaration | `InvalidArgument` | `ContextBuilder::build`, `TryFrom` (CX-01/02) |
| Context padding ≠ envelope padding | `InvalidArgument` | `ContextEnvelope::encrypt` (CX-06) |
| `wrap_key` input not 32 bytes | `InvalidArgument` | Before any crypto (KR-06) |
| Envelope `kek_id` ≠ keyring name | `DecryptFailed` ("kek_id mismatch", naming both ids) | Envelope recipe, before KEK use; `needs_rewrap` (KR-17) |
| Malformed wrapped slot (magic, length ≠ 76, version 0) | `InvalidArgument` from the KEK → `DecryptFailed` from the envelope | KR-07 steps 1–2, before any AES call |
| Wrapped slot names a version above the highest present | `UnknownKeyVersion` + 30 s retry hint (passed through) | KR-07 3a; transient |
| Wrapped slot names a removed version | `KeyVersionUnusable` (passed through) | KR-07 3b; permanent |
| Wrapped slot's KCV ≠ the present generation's KCV | `Configuration` (passed through) | KR-07 step 4, before GCM |
| Wrong context, tampering, wrong key, swapped slot | `DecryptFailed` (one message) | Envelope recipe; also every rewrap (KR-21) |
| Sync path got `Pending` | `Backend` | KR-13; unreachable for `KeyringKek` |

Errors and `Debug` output never contain key bytes, KCV-derived secrets, plaintext or context *values*. Context keys, the purpose, versions, KCVs and the fingerprint may appear. Consumers keep secrets out of Restate error messages (R-C3-journal-secrets; ADR 0003 item 8). Consumer classification of these kinds: §6.2 table.

## 11. Test strategy

| # | Test | Where | Pass criterion |
|---|---|---|---|
| T1 | **ADR 0001 acceptance checks.** Empty context rejected (compile-time: T1b; runtime: a declaration with no fields still yields `n_fields ≥ 1`). Changing `user_id` fails with `DecryptFailed`. Written under `k1`, decrypts after `k2` becomes current. After `rewrap_current`: `ctx_env.ciphertext_version(ct) == Some("2")` **and** `Envelope::parse(ct).kek_id() == name`, and it decrypts after `k1` is removed. Mapping note: ADR 0001's "it names `k2`" refers to the generation version in the wrapped slot. The envelope `kek_id` stays the keyring name (§5.1) | `anykms-keyring/tests` | All pass |
| T1b | **Raw AAD unreachable** (trybuild compile-fail, built without `raw-kek`): (a) `ContextEnvelope::encrypt(pt, b"")` doesn't compile; (b) no `inner()`, and `ContextEnvelope` is not `Aead`; (c) `KeyringKek` can't be constructed outside the crate; (d) `EnvelopeAead::new(name, keyring)` doesn't compile (`Keyring: !KeyWrap`); (e) `KeyringProvider` doesn't exist; (f) `ContextEnvelope<AeadKey>` doesn't compile (sealed `EnvelopeBackend`) | `anykms-keyring/tests/ui` | Every case fails to compile with the expected error |
| T2 | **Descriptor parser and CLI.** Positive and negative cases for every KR-02 rule, including a KCV mismatch, a key under the wrong version, and a key from another name. A fuzz target (`cargo fuzz`, as anykms already does for local framing). `akr` round trips: `new → add → promote → remove → show`, each output re-parsed. `akr diff` refuses each KR-16 violation. **Leak check:** for every malformed input, no error's `Display`, `Debug` or `source()` chain contains any 8-character substring of any `<key>` value in the input | unit + `fuzz/` + CLI tests | No panics; every malformed input is `Configuration`; leak check passes |
| T3 | **Vectors.** (a) KEK, KCV and fingerprint derivation for fixed roots; wrapped-DEK vectors. (b) Full envelope vectors with keyring KEKs. Positive cases, and negative cases with expected kinds: unknown newer version → `UnknownKeyVersion`; removed version → `KeyVersionUnusable`; KCV mismatch → `Configuration`; slot replaced by another DEK wrapped under the same keyring → `DecryptFailed` on decrypt **and** on `rewrap_current`; relabeled version; malformed slot (bad magic, length 75/77) → `DecryptFailed`; wrong context → `DecryptFailed`. (c) Context encoding golden bytes, including ambiguity pairs | JSON in `tests/vectors/`, like `envelope_v1.json`. Schema per case: `{ "id", "root_hex"[], "name", "version", "current", "kcv_hex", "nonce_hex", "dek_hex", "wrapped_hex", "context": {key: value}, "padding", "aad_hex", "plaintext_hex", "envelope_b64", "expected_error": null or kind }` | Reproduced independently (Python `cryptography` or Node), as was done for cryptbox's vectors (`research/secrets-and-keys.md` §1) |
| T4 | **Context properties.** The encoding is injective and insensitive to builder call order (proptest). Strictness (CX-02). Through `purpose!` + `TryFrom` and through the builder: `user_id = "007"` → `InvalidArgument`; an empty value → `InvalidArgument`; a 256-byte value → `InvalidArgument`; `u64` on a `Str` key → `InvalidArgument`; an `Option<_>` key absent → valid, present → encoded. Every registry purpose builds | unit | No collisions in 10^6 cases; order-independent bytes; every listed case gives the stated result |
| T5 | **`anykms-conformance`**, run natively with `harness = false`, built with `raw-kek`, with the fixture of §11.1 | `anykms-keyring/tests/conformance.rs` | Pass with `ANYKMS_FAIL_ON_SKIP=1`; the `conformance::declarations` trial passes (every `Feature` declared) |
| T6 | Sync path: `encrypt_sync`/`decrypt_sync`/`rewrap_current_sync` never return `Pending`, and give results equal to async | unit | Pass |
| T7 | **Rotation scenario end to end:** add → promote → sweep with conditional writes and a concurrent writer → completion gate → remove. Assertions: `needs_rewrap` is false for every row before promote, true for old rows after promote, false after the sweep, and `Err(DecryptFailed)` for a foreign `kek_id`. A swapped-slot row makes `rewrap_current` fail with `DecryptFailed` and is not rewritten. A lagging keyring (without `N+1`) reading an `N+1` envelope gets `UnknownKeyVersion` with a retry hint. After removal, an old envelope gives `KeyVersionUnusable`. A KV-style consumer is gated on the TTL rule | integration (native, SQLite) | Pass; no lost concurrent write; scan count 0 |
| T7b | **Compromise scenario (§7.3):** two readable generations leak. After the procedure, the descriptor holds only `N+1`; every stored envelope's DEK differs from before (the `wrapped_dek` unwraps to a new DEK, the payload changed); a copy of any pre-incident envelope fails with `KeyVersionUnusable` | integration | Pass |
| T8 | **wasm build:** consumer crate with `getrandom/wasm_js`, `cargo build --target wasm32-unknown-unknown` run with **`RUSTFLAGS` unset** (`env -u RUSTFLAGS`), plus the KR-01 dependency check. App-side check in each application workspace: `cargo tree -e features -i anykms-keyring` shows no `raw-kek` outside dev/test targets | CI (anykms and each app repo) | Builds; banned crates absent; `raw-kek` absent |
| T9 | **wasm run under V8:** `wasm-bindgen-test` under Node running T1 and T6. Any `std::time` call panics here, which proves KR-14 | CI | Pass |
| T10 | **wasm run in workerd = SD-06, extended to the keyring:** `wrangler dev --local`, a workers-rs `fetch` handler that loads the descriptor from a Worker secret per KR-04, installs it (KR-15), round-trips a context-bound envelope, rejects a wrong context, and serves the fingerprint route. Timing: native criterion benchmarks only. Deployed CPU per operation comes from SP-16 (§6.7) | `spikes/` (SD-06), then `examples/worker` in anykms | HTTP 200, `ok=true wrong_ctx_fails=true`, fingerprint matches `akr show`. No CPU figure is claimed from the local run |
| T11 | **SD-07, key loading and deploy semantics** (each item UNVERIFIED until answered): (1) the workers-rs API that reads a Secrets Store value; (2) whether a Secrets Store value change reaches running isolates (KR-04 makes it irrelevant for correctness, but it should be recorded); (3) whether `wrangler secret put` creates **and deploys** a new version; (4) whether `wrangler versions secret put` creates one without deploying; (5) whether a version rollback restores that version's secret bindings (KR-20); (6) whether gradual deployments run versions with different secrets side by side; (7) how long old isolates serve after a deploy (KR-18a window) | spike | Each question answered with the command run and the observed fingerprint; §5.3, KR-18a and KR-20 updated |
| T12 | **`Sealed<T>` marker scan** re-run with the anykms envelope (ADR 0001 UNVERIFIED item; `spikes/restate-sealed-journal` method), only when `Sealed<T>` is built. Also asserts that no `state`/`sys_promise` row holds a `Sealed<T>` (KR-19) | restate-toolkit | Marker absent from all introspection tables; opens after replay |
| T13 | **Global install (KR-15) and load failure (KR-04):** deserializing a `Sealed<T>` before `install` returns a serde error built from `Unavailable` (no panic), and restate-toolkit maps it to a retryable error; `install` is idempotent for an equal fingerprint and returns `Configuration` for a different one; a malformed descriptor makes the request return 500, logs the fixed message once, and a corrected value loads on the next request without an isolate restart (native simulation) | unit + restate-toolkit | Pass |
| T14 | **Padding (CX-06):** for `block(64)` purposes, the envelope payload length (ciphertext minus tag) is a multiple of 64 for plaintext lengths 0..=600; a context whose purpose padding differs from the envelope's is rejected with `InvalidArgument` | unit | Pass |
| T15 | **Fingerprint (KR-18):** stable across descriptor field order; changes when any key byte, the name, `current` or the version set changes; its `Display` contains no 8-character substring of any key | unit | Pass |

### 11.1 Conformance fixture (T5; closes OQ-5)

- `create_key(TestKeyKind::KeyWrap, Fresh)`: the fixture generates a 32-byte root natively, builds a keyring `t-<n>` with `k1` current, `insert`s it into the `KeyringProvider`, and returns `keyring://t-<n>`. `Shared` may return a pooled keyring. Other `TestKeyKind`s are never requested, because their features are declared n/a.
- `rotate(key)`: `replace` with the keyring plus a fresh `k<max+1>` promoted to current. It returns the same `TestKey` (`VersionModel::Floating`).
- `disable_version(key, v)`: `replace` with `k<v>` removed. It refuses `v = current` with `Unsupported`.
- `missing_key_uri(KeyWrap)`: `keyring://missing-0` (well-formed, never inserted → `NotFound`).
- `provider()`: the `KeyringProvider`. `typed()`: a `TypedHandles` that overrides `key_wrap` to open `KeyringKek` handles with the §6.3 `KeySemantics`.

`FixtureCaps` (every variant of `anykms-conformance` `Feature` at `32e1971`, `crates/anykms-conformance/src/fixture.rs` lines 30–150):

| Feature | Declaration | Reason |
|---|---|---|
| `KeyWrap` | supports | Native key-wrapping keys through `Provider::key_wrap` |
| `Describe` | supports | §6.3 |
| `RotateKeyWrap` | supports | `rotate` above |
| `DisableVersion` | supports, **or** n/a (see note) | Removal of a readable generation → `KeyVersionUnusable` for inputs naming it, matching the feature's contract |
| `KeyWrapRewrap` | n/a | No server-side rewrap by design (KR-21) |
| `RewrapWithAad` | n/a | No `Rewrap` impl |
| `Aead`, `Rewrap`, `DataKeys`, `WrappedDataKeys`, `Mac`, `Signing(*)` | n/a | The KEK is dedicated to envelopes (§8.3) |
| `Rotate`, `RotateMac`, `RotateSigning` | n/a | No AEAD, MAC or signing keys |
| `Disable`, `PreflightState`, `DescribeVersionState` | n/a | No key-level state. A keyring is present or absent (`NotFound`) |
| `MinDecryptionVersion` | n/a | Removal is the only revocation mechanism (KR-07) |
| `Extension`, `BackendInfo` | n/a | No backend extension calls and no backend metadata |
| `KeyWrapPublicKeys` | n/a | Symmetric only |
| `Throttle`, `RequestCount`, `UnauthorizedProvider`, `ServerErrorMessages` | n/a | No server, no requests, no credentials |
| `OversizePlaintext`, `EmptyPlaintext`, `EmptyMessage`, `MacInputLimit`, `MessageInputLimit`, `UsedPrimary` | n/a | AEAD/MAC/signing behaviours the KEK doesn't have |
| `NativeCiphertext` | n/a | No non-anykms client exists |

- **`DisableVersion` note (pre-implementation check, owner: implementer):** read the `anykms-conformance` cases gated on `DisableVersion`. If any of them disables the **primary** version, declare `not_applicable(DisableVersion, "the current generation cannot be removed; removal of a readable generation is covered by T7")`. Otherwise declare `supports`.
- `Feature` is `#[non_exhaustive]`. A variant added later fails the `declarations` trial (`crates/anykms-conformance/src/lib.rs` lines 35–41) until this table is updated, which is the intended behaviour.

## 12. Open questions

| ID | Question | Type | Owner | Blocks v1 build? | Default if nobody decides |
|---|---|---|---|---|---|
| OQ-1 | Crate placement and names: a new `anykms-keyring` (with `anykms-keyring-cli`) plus a `context` feature in `anykms-recipes`, or extend `anykms-local` (which would change its documented test-only stance)? L1 (`specs/components/L1-rotating-credential.md`, crate table and `SealedStore`, ST-12) already pins `anykms-keyring`, `ReadyKek` and `ContextEnvelope<EnvelopeAead<K>>`, so changing those names means editing L1. The question of hiding the raw envelope is decided (CX-04, `raw-kek`) | decision | user / anykms maintainer | **Yes** | New crates as in §4 |
| OQ-2 | Wrap algorithm: AES-256-GCM with a random nonce (no new dependency, 2^32 budget) or AES-KW RFC 5649 (deterministic, WebCrypto-compatible, so it eases a later `secret_key` KEK; RustCrypto `aes-kw` compatibility with `aes` 0.9/`cipher` 0.5 is UNVERIFIED). Changes the wire format (§6.2) | design | user / anykms maintainer | **Yes** | AES-256-GCM |
| OQ-3 | Support `secret_key` bindings (non-extractable `CryptoKey`) at all? It needs async WebCrypto, so `!Send` futures, and loses the sync path. Depends on SD-07 showing `extractable=false` | decision, after SD-07 | user | No | No (v1) |
| OQ-4 | Secrets Store and Worker-secret deploy semantics. Folded into T11 items 1–7. KR-04 already requires a redeploy after every change, so the answers tune the KR-18a window, not correctness | research/spike (SD-07) | implementer | No | Redeploy-only; 10-minute window |
| OQ-5 | ~~Conformance shape~~ **Closed** by §11.1 (full `FixtureCaps`, provider with `insert`/`replace`, `KeySemantics` pinned). One pre-implementation check remains (the `DisableVersion` note) | — | — | — | — |
| OQ-6 | Does the envelope review stay the hard gate before C2 production (ADR 0001), or does C2's recoverability (§9) let it ship earlier, with a dated re-check? | decision | user | No (blocks C2 production only) | Stays a gate |
| OQ-7 | What context does `Sealed<T>` bind to? The serde traits carry no invocation or row data, so only type-level fields are available. Is `type` alone enough, given ADR 0003 makes `Sealed<T>` rare? | design | restate-toolkit spec | No | `purpose=restate-sealed; type` |
| OQ-8 | ~~Migrate P1 sessions and continuations?~~ **Closed** by P1 PD-G, formerly SD-G (`specs/projects/P1-github-invite.md` §11.3, §16.2): both move to the envelope. Registered as `session` and `attempt-continuation` (§6.5) | — | — | — | — |
| OQ-9 | One keyring per product trust unit (P1, P2 separate), or one shared across products? Sharing widens blast radius (§7.3 is per descriptor); separate means more secrets to rotate | decision | user | No | One per product |
| OQ-10 | Ask anykms for an AAD-committing v2 (`info = "anykms/envelope/v2" ‖ SHA-256(user_aad)`) before C2 production, or accept v1's non-committed AAD under the single trust domain? | decision | anykms maintainer | No | Accept v1; note for v2 |
| OQ-11 | ~~Padding policy~~ **Closed:** padding is declared per purpose (CX-06, registry) | — | — | — | — |
| OQ-12 | Workers CPU cost per operation and the wasm size delta | measure (SP-16, §6.7 method) | implementer | No | Assume negligible until measured |

### 12.1 Follow-up edits in other specs

**Status: applied 2026-10-16** (consistency pass; `plans/README.md`). P1 §11.3 and Appendix A.4, L1 §8.3 (ST-11, ST-13, construction note) and ADR 0003 (amendment note) now carry these edits. ST-10's `app_client_id` was already in L1 revision 2.


- **P1** (`specs/projects/P1-github-invite.md` §11.3): cite the `akr1` descriptor (§5.2) instead of "base64 key list with IDs"; keep `purpose=session; session_id` and `purpose=attempt-continuation; scope; binding` as registered here, and name the plaintext columns that `scope` and `binding` come from (CX-05); set a KV `expiration_ttl` on every session write (§7.2 table); use the §6.2 error classification ("a missing KEK ID" is `KeyVersionUnusable` for removed versions, but `UnknownKeyVersion` is a retry, not a sign-out); add the KR-18 diagnostic route.
- **L1** (`specs/components/L1-rotating-credential.md`): ST-10 key `app` → `app_client_id`; build `SealedStore`'s envelope with `keyring.context_envelope(Padding::block(64))` (`K = KeyringKek`); ST-11 cites the registry instead of OQ-11; ST-13 classifies `UnknownKeyVersion` and `Configuration` as retryable, not `Corrupt` (§6.2 table).
- **ADR 0003 / L2:** the `installation-token-cache` keys are `app_id; installation_id; scope; expires_at`.

## 13. Review log (revision 2)

| # | Severity | Objection (one line) | Resolution |
|---|---|---|---|
| 1 | blocker | `impl Rewrap for Keyring` sends `rewrap_current` down the server-side path with no commitment check, laundering swapped slots | **Accepted.** KR-21: no `Rewrap` impl. `ContextEnvelope::rewrap_current` = `rewrap_local`. §7.2, §7.4 and §8.2 reworded. T3/T7 negative cases; `KeyWrapRewrap` n/a (§11.1) |
| 2 | major | Context isn't mandatory: `Keyring: KeyWrap`, the provider, `inner()` and `ContextEnvelope<any Aead>` all reach raw AAD | **Accepted.** `Keyring` implements no trait; opaque `KeyringKek`; `raw_kek` and the provider behind `raw-kek` (off by default); `inner()` removed; sealed `EnvelopeBackend`; app-side CI check (T8); compile-fail tests (T1b); residual stated (CX-04). Mechanism adapted: an opaque KEK type instead of a feature-gated trait impl, because `EnvelopeAead<K>` needs `K: KeyWrap` |
| 3 | major | Retyped or regenerated key bytes for an existing version are indistinguishable from tampering | **Accepted.** Per-generation KCV (§6.1) in the descriptor (KR-02) and in the wrapped slot (76 bytes, bound in `W`); mismatch → `Configuration` before GCM (KR-07 step 4); `akr diff` (§5.5); vectors (T3) |
| 4 | major | Compromise procedure assumes one generation leaks alone | **Accepted.** §7.3 rewritten at descriptor granularity: all present generations compromised, out-of-band root, full re-encrypt, rotate protected secrets, cancel in-flight invocations, remove all old generations, Time Travel/backup residual. T7b |
| 5 | major | "Wait out journal retention" ignores in-flight invocations and state-held `Sealed<T>` | **Accepted.** KR-19: no removal while pre-T_p secret-touching invocations are active, plus retention; `Sealed<T>` forbidden in K/V, workflow state, promises and awakeables; compromise consequence stated; §9 corrected |
| 6 | major | "No isolate runs the old descriptor" is unobservable; deploy, rollback and gradual-deploy semantics are unaddressed | **Accepted.** KR-04: redeploy after every change (no TTL reload); KR-18 fingerprint, logs and route; KR-18a rollout check; KR-20 no rollback across Promote; T11 questions (UNVERIFIED) |
| 7 | major | `needs_rewrap` isn't an anykms API | **Accepted.** KR-17 `ContextEnvelope::needs_rewrap`; §3 corrected; T7 assertions |
| 8 | major | Process-global keyring init is unspecified | **Accepted.** KR-15 `global::install`/`get` (`OnceLock`), install before Restate dispatch, `Unavailable` → retryable serde error, fail-closed parse; T13 |
| 9 | major | `KeyVersionUnusable` is misused for versions an isolate hasn't seen yet | **Accepted.** KR-07 split: above highest → `UnknownKeyVersion` + retry hint; at or below → `KeyVersionUnusable`; §6.2 consumer classification; §7.1, §10, T3, T7 updated |
| 10 | minor | `purpose!` `From` can't be infallible; `app: str` is invalid | **Accepted.** `TryFrom`; example uses `&'a str`/`u64`; T4 cases |
| 11 | minor | Optional keys may not be recoverable; `app` means two identifiers | **Accepted.** CX-05; no optional keys in v1; `app_client_id` vs `app_id`, with the rationale in the registry |
| 12 | minor | getrandom "no `--cfg` needed" cites a spike that set the cfg | **Accepted.** Cites research §1 only, marked UNVERIFIED; T8 builds with `RUSTFLAGS` unset |
| 13 | minor | In-Worker per-operation CPU timing is impossible | **Accepted.** §6.7 method (N iterations, platform `cpuTime` in SP-16); T10 reports pass/fail plus native criterion |
| 14 | minor | Malformed-slot error kind unspecified; base64 errors could echo key bytes | **Accepted.** KR-07 step 1 (`InvalidArgument` → `DecryptFailed`, before AES); KR-02 fixed messages, no `source`; T2 leak check |
| 15 | minor | `expires_at` of the C1 cache isn't bound | **Accepted.** Required key of `installation-token-cache`; §8.2 rollback row updated |
| 16 | major | Registry is out of sync with P1 SD-G (`session`, `attempt-continuation`, `link-secret`, key list) | **Accepted.** OQ-8 closed; `session` (`session_id`) and `attempt-continuation` (`scope`, `binding`) registered; `web-session` dropped; `link-secret` recorded as a candidate; P1 edits listed in §12.1 (P1 isn't edited by this revision) |
| 17 | major | `inner()` leaks raw AAD; generic `rewrap_verified` isn't buildable; no supported constructor | **Accepted.** Forwarding methods on a sealed `EnvelopeBackend`; `Keyring::context_envelope(padding)` as the supported constructor (on the keyring side, because `anykms-recipes` can't depend on `anykms-keyring`); T1b trybuild |
| 18 | major | `Purpose` has no value types; macro grammar undefined; conversion must be fallible | **Accepted.** `FieldSpec {kind, presence}` and `Decimal` canonical check; EBNF grammar with `Option<_>`; `TryFrom`; T4 cases. Hand impls documented as allowed (safety is in `build()`), instead of sealing, because a downstream macro expansion can't implement a truly sealed trait |
| 19 | major | Rotation isn't executable for KV consumers; step 1 is untestable | **Accepted.** Per-consumer rotation table (D1 CAS sweep; KV TTL gate; journal KR-19); fingerprint (KR-18) and rollout check (KR-18a); propagation marked UNVERIFIED (T11) |
| 20 | major | T5 can't pass: features undeclared, fixture undefined, `KeySemantics`/`KeyInfo` unspecified | **Accepted.** §11.1 full `FixtureCaps`, mutable provider with `insert`/`replace`, URI grammar, `KeySemantics` and `KeyInfo` pinned; OQ-5 closed, with one pre-implementation check (`DisableVersion`) |
| 21 | major | `needs_rewrap` doesn't exist (duplicate of 7) | **Accepted.** Same resolution as #7 (KR-17) |
| 22 | minor | Operator invariants (no version reuse, immutable name) and a provisioning tool are missing | **Accepted.** KR-16; `akr` CLI (§5.5) with `diff`; T2 round trips |
| 23 | minor | Per-purpose padding has no API | **Accepted.** CX-06: `Purpose::PADDING`, checked against the envelope's padding; one envelope per padding; T14; OQ-11 closed |
| 24 | minor | Load-time failure behaviour and the async-once primitive are undefined | **Accepted.** KR-04: `OnceLock` set only on success, 500 and retry on the next request, log once; T13 |
| 25 | minor | T1 observable for "names `k2`" and the T3 vector schema are undefined | **Accepted.** T1 asserts `ciphertext_version == "2"` and `kek_id == name`, with a mapping note; T3 schema defined |
| 26 | minor | OQs lack owner and blocking status | **Accepted.** Owner and Blocks columns; OQ-1 and OQ-2 blocking; OQ-5, OQ-8 and OQ-11 closed; L1 name pinning noted under OQ-1 |

No objection was rejected. Two were accepted with an adapted mechanism (#2: an opaque KEK type; #18: documented hand impls instead of sealing), each for the reason given in its row.
