# kekring formats, version 1

Status: **unreviewed construction; external review pending.** Pinned by vectors
(`tests/vectors/kekring-v1.json`, re-derived independently by `scripts/kekring-vectors.mjs`).
This document specifies the three **one-way contracts** of kekring. Each freezes when the first
production row is written with it: from then on every envelope written under it must decrypt
forever, so a change is a new version marker and a reader that accepts both, never a
reinterpretation. The review of the three is a pre-launch gate of the consuming application; its
outcome can still change any of them until that launch, after which a finding means a v2.

| Contract | Fixed by | Section |
|---|---|---|
| Descriptor | prefix `kekring1` | [Descriptor](#descriptor) |
| Wrapped-key blob | magic `krw1` | [Wrapped-key blob](#wrapped-key-blob) |
| Derivation labels and AAD | labels `kekring/v1/*`, AAD label `anykms/aad/v1\0` | [Derivation](#derivation), [AAD](#aad) |

kekring is a third-party anykms backend (design: anykms#35 as amended). It defines these formats;
the envelope around the blob is anykms envelope v1 (`anykms/docs/envelope-format.md`), and the AAD
is anykms canonical AAD v1 (`anykms/docs/aad-format.md`), neither of which kekring changes.

Notation: `‖` is concatenation, integers are big-endian, `u8 len ‖ name` is the name's byte length
in one byte followed by its bytes. **Wire** rules change bytes or what is representable and never
change within v1; **policy** rules bound accepted inputs and may be widened (never narrowed) by a
later release, readers first.

## Derivation

Each generation `v` of keyring `name` has a 32-byte root, provisioned out of band from a CSPRNG.
kekring derives, with HKDF-SHA256 (RFC 5869):

```
PRK = HKDF-Extract(salt = "kekring/v1/salt", IKM = root)
KEK = HKDF-Expand(PRK, info = "kekring/v1/kek" ‖ u8 len ‖ name ‖ u32 v, L = 32)
kid = HKDF-Expand(PRK, info = "kekring/v1/kid" ‖ u8 len ‖ name ‖ u32 v, L = 8)
```

| Item | Class | Value |
|---|---|---|
| Salt | wire | ASCII `kekring/v1/salt`, constant (RFC 5869 §3.1 allows it; the name in `info` separates keyrings) |
| KEK label | wire | ASCII `kekring/v1/kek` |
| kid label | wire | ASCII `kekring/v1/kid` |
| Name binding | wire | the name is part of every `info`: renaming is a new keyring (migrate by re-encrypting) |
| Version binding | wire | `u32` version in every `info`: one root never yields the same KEK for two versions |

The `kid` is a PRF output bound to name and version: a selector like the envelope's `kek_id`,
unauthenticated by design, revealing nothing about the root. It replaces the fork's "key check
value": its job is to select and to catch a wrong descriptor, not to authenticate.

## Descriptor

One secret string holds a keyring. Grammar (ABNF, RFC 5234; `DIGIT`, `ALPHA` as usual):

```
descriptor  = "kekring1" ";name=" name ";current=" version ";hwm=" version
              1*64( ";k" version "=" root "." kid ) [ %x0A ]
name        = ( lower / DIGIT ) *63( lower / DIGIT / "_" / "-" )
lower       = %x61-7A
version     = %x31-39 *9DIGIT            ; 1 ..= 4294967295 (u32), no leading zero
root        = 43base64url                ; 32 bytes, unpadded, canonical (zero trailing bits)
kid         = 16( DIGIT / %x61-66 )      ; the generation's kid, lowercase hex
base64url   = ALPHA / DIGIT / "-" / "_"
```

| Rule | Class | Detail | Error code |
|---|---|---|---|
| Length | policy | at most 8192 bytes (after the newline is removed), checked before splitting | `descriptor:length` |
| Charset | wire | printable ASCII without space (`%x21-7E`); a single trailing `\n` is removed first, so `\r\n`, two newlines and every other whitespace are refused | `descriptor:charset` |
| Prefix | wire | exactly `kekring1`: the descriptor format version | `descriptor:prefix` |
| Field order | wire | `name`, `current`, `hwm`, then generations; no other field, no repeat, no empty field | `descriptor:field_order` |
| Name | wire (charset), policy (64 bytes) | as `name` above: excludes `;`, `=`, `.`, whitespace, controls and upper case | `descriptor:name` |
| Versions | wire | canonical decimal `u32`, nonzero, in `current`, `hwm` and every `kN` | `descriptor:version` |
| Generation order | wire | strictly ascending, so no version repeats | `descriptor:generation_order` |
| Generation count | policy | 1 to 64 | `descriptor:generation_count` |
| Root | wire | 43 characters of the URL-safe alphabet decoding to 32 bytes, canonical | `descriptor:root` |
| kid | wire | 16 lowercase hex characters | `descriptor:kid` |
| kid match | wire | equals the kid derived from the root, name and version (constant-time) | `descriptor:kid_mismatch` |
| Nonzero root | policy | an all-zero root is refused | `descriptor:zero_root` |
| Distinct roots | policy | no two generations share root material | `descriptor:duplicate_root` |
| Current | wire | names a listed generation | `descriptor:current` |
| High-water mark | wire | at least every listed version (so at least `current`) | `descriptor:high_water_mark` |

The grammar is **canonical**: a keyring has exactly one descriptor text (plus the optional
newline), which is what `Keyring::to_descriptor` writes and what the descriptor fuzz target checks.

The **high-water mark** (`hwm`) is the highest version ever issued for this keyring. It survives
removal of generations, so a version is never reused and a reader can tell a removed generation
(at or below `hwm`, not listed) from one it has not been given yet (above `hwm`) as a fact, not a
guess. `validate_transition` keeps it monotonic across descriptors.

Every error is `Configuration` (operation `Configure`) with the code above in
`BackendDetail::code`. Messages name fields and versions, never root material, and carry no
source error. The same semantic rules (from "Nonzero root" down, plus name, count and order) apply
to `Keyring::builder`, with the same codes (a repeated version there is
`descriptor:duplicate_version`).

**Size.** The largest valid descriptor (64-byte name, 64 generations with ten-digit versions) is
4784 bytes, 4785 with the newline: within the 5 KiB per-variable limit Cloudflare documents for
Worker secrets at the time of writing (vector `largest`).

**No integrity.** Nothing authenticates a descriptor. Custody of the secret is the trust boundary:
whoever can write it can substitute keys, whoever can read it can decrypt everything it covers.
The kids catch accidents (a truncated root, a root pasted under the wrong version or keyring), not
attacks.

## Wrapped-key blob

What kekring writes into the envelope's key-wrapping slot:

| Offset | Size | Field | Class |
|---|---|---|---|
| 0 | 4 | magic `krw1` (`6b 72 77 31`): the blob format version | wire |
| 4 | 4 | `u32` version of the generation that wrapped | wire |
| 8 | 8 | that generation's kid | wire |
| 16 | 40 | AES-256-KW (RFC 3394, SP 800-38F KW, default IV `A6A6A6A6A6A6A6A6`) of the 32-byte DEK under the generation's KEK | wire |

Total exactly **56 bytes**. Wrapping takes exactly 32 bytes (the envelope's DEK); any other length
is `InvalidArgument` before any work. Wrapping is deterministic: the same DEK under the same
generation gives the same blob, so a no-op rewrap is byte-identical and completion scans are exact.

**Reader**, in this order; the prefix selects exactly one generation and a reader never
trial-decrypts across generations:

| Step | Condition | Kind | `BackendDetail::code` |
|---|---|---|---|
| 1 | length is not 56, magic is not `krw1`, or version is 0 | `DecryptFailed` | none |
| 2 | version above the descriptor's high-water mark | `DecryptFailed` | `VersionAboveHighWaterMark` |
| 3 | version at or below it but not listed (removed) | `KeyVersionUnusable` | `GenerationRemoved` |
| 4 | kid differs from the listed generation's (constant-time) | `DecryptFailed` | `KeyIdMismatch` |
| 5 | AES-KW integrity check fails | `DecryptFailed` | none |

The codes are stable identifiers of this backend's contract; they survive
`EnvelopeAead::decrypt`, which keeps the KEK error's detail and backend name. None of these errors
carries a retry hint: the descriptor is immutable for the process, so an in-process retry never
helps, and `Error::class` ranks the caller class first anyway. An application tolerating rollout
skew (a reader deployed with an older descriptor than a writer) branches on
`VersionAboveHighWaterMark`. Everything computed from the blob is caller-class: nobody who can
write a stored row can page an operator by editing its prefix. Errors name the resource
`kekring:<name>`.

`wrapped_key_version` (and the envelope's `ciphertext_version` / `needs_rewrap`) read step 1's
header only, without authenticating anything; a malformed blob there is `InvalidArgument`.

## AAD

Records are bound to their rows with **anykms canonical AAD v1**, byte for byte as specified by
`anykms/docs/aad-format.md` (label `anykms/aad/v1\0`, `u16` widths, sorted keys, `purpose` as a
field, caps 48/32/255/15 and 3072 in total). kekring implements it (`Aad::encode`) and checks it
against upstream's `tests/vectors/aad_v1.json`, copied unchanged; a rule violation is
`InvalidArgument` (operation `Construct`) with the code `aad:<rule>`, using upstream's rule names.

kekring's typed path adds, without changing any byte:

- A `Purpose` declares its name and field keys as constants; `Aad::builder::<P>()` checks the
  declaration at compile time (a violation is error E0080) and requires exactly the declared
  fields, each once (`aad:undeclared_field`, `aad:missing_field`).
- A purpose also declares the envelope padding for its records. Padding is not part of the AAD:
  the envelope records it in its own authenticated flag.

Through the envelope, the AAD enters only the local AES-GCM AAD; the KEK never sees it (AES-KW
takes no AAD). The envelope's `kek_id` is the keyring name.

## Fingerprint

Not stored with data, but used to compare deployments (logs, rollout checks, the consuming
application's short-lived progress tokens):

```
fp = SHA-256("kekring/v1/fp" ‖ u8 len ‖ name ‖ u32 current ‖ u32 hwm ‖ (u32 version ‖ kid)*)[0..8]
```

over the retained generations in ascending order, shown as 16 hex characters with the public
parts: `NAME current=N hwm=H versions=1,2 fp=…`. It never covers roots or KEKs.

## Provider URI

`kekring:<name>`, the name as in the descriptor grammar; nothing else (no authority, query or
fragment). A URI outside it is `Configuration` with `Operation::ParseUri`. A name the provider was
not built with opens without error and fails every operation with `NotFound`, as anykms requires.

## Test vectors

`tests/vectors/kekring-v1.json` is generated by `scripts/kekring-vectors.mjs` from this document
alone, with Node's OpenSSL bindings (`hkdfSync`, `createHash`, `id-aes256-wrap` checked against
RFC 3394 §4.6, `aes-256-gcm`); `node scripts/kekring-vectors.mjs --check` fails unless the
committed file is reproduced byte for byte. `tests/vectors.rs` checks the Rust implementation
against it:

- `derivation`: root to KEK and kid for two names, two versions, one root under two versions, a
  64-byte name and the largest version (the KEK is checked through `wraps`, since it is never
  exposed).
- `descriptors`: valid descriptors (single, added, promoted, retired, rolled back with `hwm` above
  every listed version, a gap, another name, a 64-byte name, the largest, a trailing newline) with
  their kids and fingerprints, each equal to its canonical re-serialization.
- `invalid_descriptors`: 49 negative vectors covering every rule in the descriptor table.
- `wraps`: deterministic blobs.
- `unwrap`: every reader rule over a keyring with a removed generation, with kind and code.
- `envelopes`: full anykms v1 envelopes (fixed DEK and nonce) with canonical AAD, unpadded and
  padded, and the failures that surface through the envelope: another AAD, another `kek_id`, a
  version above the high-water mark, a removed generation, another keyring's slot, a spliced slot.

The fuzz targets in `fuzz/` cover both parsers: `descriptor` (never panics; an accepted input is
its own canonical form) and `blob` (never panics; only the documented kinds and codes; an accepted
blob under the current generation is the one kekring would write).

## Security notes

- **Custody.** A leaked descriptor gives offline, silent and unrevocable decryption of every
  envelope under every generation it contains; a KMS compromise is online, audited and revocable.
  The unit of compromise is the descriptor. Rotation is hygiene, not recovery: after a leak,
  provision a new root out of band and **re-encrypt** (`reencrypt`: fresh DEK), since every rewrap
  keeps the exposed DEK.
- **No nonce.** AES-KW is deterministic; there is no per-wrap randomness to exhaust or to repeat
  after a snapshot or clone, which is why it replaced the fork's random-nonce AES-GCM wrap (whose
  2^32-invocations-per-key bound nothing in a fleet can count). Wrapping the same DEK twice under
  one generation reveals that it is the same DEK; DEKs are fresh random keys, so this happens only
  for a rewrap of the same envelope.
- **Integrity.** AES-KW's 64-bit integrity check value: a forged or altered wrapped key passes with
  probability about 2^-64 per attempt, and every attempt needs the application to run an unwrap.
- **Non-committing wrap.** AES-KW is not designed to commit to its key. That is acceptable here
  because a reader never trial-decrypts: the blob's prefix selects exactly one KEK, and the envelope
  then checks its key commitment over the unwrapped DEK before using it (anykms envelope v1,
  decryption step 4), so a slot spliced from another envelope fails.
- **Unauthenticated selectors.** The magic, version and kid are readable by anyone who can read a
  row. They reveal which generation wrapped a row and, through the kid, which keyring (rows under
  the same keyring and version share it). They do not reveal anything about the root. Anyone who
  can write a row can make it name any version or kid; that only selects which check fails.
  `needs_rewrap` and `ciphertext_version` read them and are not a basis for trust.
- **No authorship.** Anyone holding the descriptor can write a valid envelope for any row and AAD.
  The AAD binds a ciphertext to its row; it does not say who wrote it (AAD v1 security notes).
- **Name binding.** The keyring name is in every derivation input and is the envelope `kek_id`.
  Renaming is a new keyring; there is no alias.
- **No clock, no I/O.** Nothing in the crypto path reads time or performs I/O; the library parses
  strings it is given. Loading the secret is the application's job (`std-loaders` reads an
  environment variable or a private file on native targets).
- **Zeroization.** Roots, KEKs, unwrapped DEKs and the descriptor read by the loaders are held in
  `Zeroizing` buffers, and AES-KW key schedules zeroize on drop. On a JS host the descriptor
  arrives as a JS string (the Worker secret binding) that cannot be zeroized, and copies the runtime
  makes are out of reach.
- **Debug output.** `Debug` and the fingerprint show identifiers only. This is load-bearing:
  anykms's `EnvelopeAead` prints its KEK in its own `Debug`.

## Decisions

Points the anykms#35 amendment left open, decided here in the direction most consistent with it:

1. **Labels from the crate name.** `kekring/v1/{salt,kek,kid,fp}`, descriptor prefix `kekring1`,
   blob magic `krw1` ("kekring wrapped-key format 1"). They name the format, not anykms (no
   `anykms/` prefix), and stay as they are if the crate is renamed before publishing.
2. **Name charset** `[a-z0-9][a-z0-9_-]{0,63}`: the amendment excludes `;`, `=`, `.`, whitespace
   and controls; lower case only as well, so a name survives case-insensitive stores and URIs, and
   64 bytes as the bound "well under 255".
3. **Strict field order, ascending generations**: the descriptor is canonical (the fork accepted
   any order). `hwm` is mandatory in the text; `Keyring::builder` defaults it to the highest listed
   version.
4. **kid in the descriptor is verified at parse** and a mismatch is a `Configuration` error at
   startup, whereas a kid mismatch in a stored blob is `DecryptFailed` (`KeyIdMismatch`): the
   first is the operator's input, the second the caller's.
5. **Parser and AAD rule codes** (`descriptor:<rule>`, `aad:<rule>`) are diagnostic identifiers
   that the negative vectors pin; the three unwrap codes are the backend contract applications
   branch on.
6. **Removed generation** carries the code `GenerationRemoved` (the amendment's table), and a blob
   naming version 0 is malformed (`DecryptFailed`, no code), not removed.
7. **Selector reads of a malformed blob** (`wrapped_key_version`, `ciphertext_version`,
   `needs_rewrap`) are `InvalidArgument`, as anykms's reference backend does; only unwrapping maps
   everything to `DecryptFailed`.
8. **Padding belongs to the purpose** (`Purpose::PADDING`) and the envelope applies it per call, so
   there is no envelope-versus-purpose padding mismatch state (the fork had one, and an error for
   it).
9. **Every declared field is required.** No optional fields: one purpose has one AAD shape.
10. **The provider holds a fixed set** of keyrings from construction; `KekringProvider::new`
    refuses two with one name. There is no mutable provider (the fork's raw-gated one is gone).
11. **Conformance** runs in its own workspace (`conformance/`) because the suite pulls in
    anykms-recipes' aws-lc-rs RSA backend, which would otherwise enter the application's lockfile
    and test feature set. `RotateKeyWrap` (rotation under an open handle) and `KeyWrapRewrap` (no
    native `Rewrap`) are declared not applicable.
12. **Size bounds**: 64 generations (worst case 4785 bytes, inside 5 KiB) and an 8192-byte
    pre-parse length guard.
13. **`to_descriptor` exists** (the canonical writer) for operator tooling and tests, writing into
    a pre-sized zeroizing buffer; the request path never needs it.
14. **Fingerprint** covers the high-water mark and is not a wire contract: changing it would only
    invalidate in-flight progress tokens of the consuming application, not data.

## Future versions

A change to any layout or label is a new prefix, magic or label (`kekring2`, a new magic,
`kekring/v2/*`), read alongside v1, never a reinterpretation. Candidates the external review may
raise: binding the AAD into the envelope's commitment (an anykms envelope v2 matter), and an
integrity tag over the descriptor if custody ever stops being the trust boundary. If anykms adopts
a provisioned KEK upstream, it may accept this v1 as a legacy format and own a v2.
