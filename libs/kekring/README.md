# kekring

A provisioned, versioned key-encryption key for [anykms](https://github.com/sagikazarmark/anykms)
envelopes: one secret descriptor holds every generation's root, the current generation wraps new
data keys with AES-256-KW, and records are bound to their rows with anykms canonical AAD v1. It
incubates here as a third-party anykms backend, outside anykms (custody is a non-goal there,
`anykms/docs/PLAN.md` §30); the design is
[sagikazarmark/anykms#35](https://github.com/sagikazarmark/anykms/issues/35) as amended.

**Status.** The three wire formats (descriptor, wrapped-key blob, AAD and derivation labels) are
specified in [`docs/kekring-format.md`](docs/kekring-format.md). They are one-way contracts that
freeze when the first production row is written, and they are an **unreviewed construction**:
external review is pending. The crate name is a placeholder; the labels and the descriptor prefix
derive from it and are what matter.

## Use

```rust
use kekring::{Aad, Keyring, Purpose};

struct Session<'a> { id: &'a str }
impl Purpose for Session<'_> {
    const NAME: &'static str = "session";
    const FIELDS: &'static [&'static str] = &["session_id"];
}
impl TryFrom<&Session<'_>> for Aad {
    type Error = anykms_core::Error;
    fn try_from(s: &Session<'_>) -> anykms_core::Result<Self> {
        Aad::builder::<Session>().str("session_id", s.id).build()
    }
}

# fn example() -> anykms_core::Result<()> {
# let descriptor = &*Keyring::builder("app").generation(1, [7; 32]).current(1).build()?.to_descriptor();
let ring = Keyring::parse(descriptor)?;
let envelope = ring.envelope();
let aad = Aad::try_from(&Session { id: "storage-key" })?;
let ciphertext = envelope.encrypt_sync(b"session record", &aad)?;
assert_eq!(&*envelope.decrypt_sync(&ciphertext, &aad)?, b"session record");
# Ok(()) }
# example().unwrap();
```

- **`Keyring`** is the parsed descriptor (or a `Keyring::builder`): immutable for its lifetime.
  `fingerprint()` shows name, current, high-water mark, versions and a digest over their key ids,
  never roots; so does `Debug`.
- **`KeyringEnvelope`** (`Keyring::envelope()`) is anykms envelope v1 over the keyring, bound to
  an `Aad`, padded as the purpose declares. Async and `*_sync` methods: `encrypt`, `decrypt`,
  `rewrap_current` (local rewrap with the key-commitment check), `rewrap_verified`, `reencrypt`
  (fresh data key: the only compromise-recovery tool), plus `needs_rewrap` and
  `ciphertext_version`, which read the unauthenticated selector. The sync methods poll once; the
  KEK is in process, so they complete on that poll, need no runtime and work on wasm32.
- **`Purpose`** is implemented by hand per record type, with `impl TryFrom<&Record> for Aad`. The
  declaration (name, field keys) is checked at compile time; every builder must supply exactly the
  declared fields.
- **`Kek`** (`Keyring::kek()`) is the anykms `KeyWrap + Describe + Semantics` handle, and
  **`KekringProvider`** opens keyrings as `kekring:<name>` through an anykms `Registry`. There is
  no `Rewrap`: anykms's server-side rewrap path skips the key-commitment check.
- **`validate_transition(old, new)`** says whether a descriptor may replace another in one
  deployment step (see Rotation).
- Feature `js` for wasm32 on a JS host; `std-loaders` for `Keyring::from_env` / `from_file`
  (which refuses a file readable by group or others on Unix).

## Errors

| Condition | Kind | `Code` (`detail().code()`) |
|---|---|---|
| wrapped key names a version above the high-water mark (a lagging reader) | `DecryptFailed` | `VersionAboveHighWaterMark` |
| version at or below the high-water mark, no longer listed | `KeyVersionUnusable` | `GenerationRemoved` |
| listed version, key id of other root material (wrong descriptor) | `DecryptFailed` | `KeyIdMismatch` |
| another AAD, tampering, wrong magic or length, failed integrity check | `DecryptFailed` | none |
| a descriptor or builder rule | `Configuration` | `descriptor:<rule>` |
| an AAD rule | `InvalidArgument` | `aad:<rule>` |

No error carries a retry hint: the descriptor is immutable for the process, so retrying in
process never helps. An application that must survive rollout skew branches on
`Code::of(&e) == Some(Code::VersionAboveHighWaterMark)`, never on the kind. Errors carry no
roots, data keys, plaintexts or AAD values.

## Descriptors and rotation

A descriptor is a secret string,
`kekring1;name=NAME;current=N;hwm=H;kV=ROOT.KID;...` (grammar in the format document). The
`descriptor` example is the operator tool: it reads descriptors from stdin or private files and
writes them to stdout, so they can be piped into a secret store without touching command lines:

```text
cargo run -q -p kekring --example descriptor -- new NAME         # k1 from the OS CSPRNG
cargo run -q -p kekring --example descriptor -- add < old        # generation hwm+1
cargo run -q -p kekring --example descriptor -- promote N < old
cargo run -q -p kekring --example descriptor -- remove N < old
cargo run -q -p kekring --example descriptor -- show < old       # fingerprint and key ids
cargo run -q -p kekring --example descriptor -- check OLD NEW    # validate_transition
```

Every writing command checks `validate_transition` against its input. Store descriptors in the
platform's secret store and never commit or log them. A running process never reloads it: every change is a redeploy of every
process in the trust unit.

Rotation, each step checked with `validate_transition(previous, next)` before it is deployed:

1. **Add** generation `n+1` (`hwm` becomes `n+1`), deploy everywhere.
2. **Promote** it (`current=n+1`), deploy everywhere.
3. **Rewrap** stored rows with `rewrap_current` (or `rewrap_verified`), writing back only if the
   row still holds the bytes that were read; re-scan with `needs_rewrap` until no row selects.
   Rewrapping is deterministic, so a repeated sweep is a no-op.
4. **Remove** the old generation once nothing reads it (rows rewrapped, TTL-bound copies expired),
   deploy. The high-water mark keeps its version retired forever.

`validate_transition` refuses a rename, a changed root for a retained version, a lowered
high-water mark or a reused version, removal of the old current, backwards promotion, and
promotion to a generation added in the same step.

**Compromise.** A leaked descriptor exposes every generation in it, offline and silently;
rotation is hygiene, not recovery. Provision a new root out of band, `reencrypt` every row (fresh
data keys), rotate the protected credentials, then remove every leaked generation. Rewrapping
alone keeps the exposed data keys.

## Checks

- `cargo test -p kekring --all-features`: builder, parser, reader and AAD rules, rotation,
  transition, provider, async/sync, the vectors below.
- `node scripts/kekring-vectors.mjs --check`: re-derives `tests/vectors/kekring-v1.json` with
  Node/OpenSSL (`id-aes256-wrap`, HKDF, SHA-256, AES-GCM), byte for byte, and its AAD encoder
  against upstream's `tests/vectors/aad_v1.json` (copied from anykms 0.3.0; unchanged in 0.4.0).
- `conformance/`: the anykms 0.4 conformance suite against `KekringProvider` (its own workspace,
  because the suite pulls in an RSA backend the application must not build).
- `fuzz/`: cargo-fuzz targets for the descriptor parser and the blob reader.
