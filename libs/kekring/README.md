# anykms-keyring (stopgap, `libs/kekring`)

A provisioned local key-encryption key for anykms envelopes, lifted from the
monorepo fork of anykms (`implementation/anykms/crates/anykms-keyring` plus
`anykms-recipes/src/context.rs`, which lives here as the `context` module)
and rebuilt on the published `anykms-core`/`anykms-recipes` 0.2 crates.

**Status.** This is the pre-redesign crate, kept so the application keeps
building unchanged. Its wire formats (`akr1` descriptor and wrapped-DEK layout,
`anykms-keyring/v1/*` HKDF labels, `anykms-ctx/v1` context AAD) and its error
mapping are superseded by the decisions on
[sagikazarmark/anykms#33](https://github.com/sagikazarmark/anykms/issues/33)
(AAD encoding spec) and
[#35](https://github.com/sagikazarmark/anykms/issues/35) (AES-256-KW, `kid`,
high-water mark, `DecryptFailed` + stable codes, concrete wrapper,
`validate_transition`). Nothing in this crate is frozen; the application is
prelaunch and will be switched to the redesigned crate before its first
production write. Dropped relative to the fork: the `raw-kek` feature and
`KeyringProvider` (they needed `KeySemantics` additions that upstream declined,
anykms#34), the `akr` CLI, the worker/wasm examples and the trybuild UI tests.

The original guide follows.

---

# Provisioned local keyrings

Decision (BK-00, 2026-10-16): use separate `anykms-keyring` and `anykms-keyring-cli`
crates, AES-256-GCM wrapping, and a descriptor per product trust unit. The ephemeral
`anykms-local` backend keeps its existing test-only meaning.

Application dependencies disable default features. `anykms` features `keyring`,
`context`, and optionally `global` expose `anykms::keyring` and `anykms::recipes`.
No application enables `raw-kek`. Use `scripts/check-keyring-wasm.sh` to check the
production dependency graph and absence of raw access.

```rust
use anykms_keyring::{ContextEnvelopeSync, EncryptionContext, Keyring, purposes::Session};
use anykms_recipes::envelope::Padding;
# fn example(descriptor: &str) -> anykms_core::Result<()> {
let ring = Keyring::parse(descriptor)?;
let envelope = ring.context_envelope(Padding::NONE)?;
let context = EncryptionContext::try_from(&Session { session_id: "storage-key" })?;
let ciphertext = envelope.encrypt_sync(b"session record", &context)?;
let plaintext = envelope.decrypt_sync(&ciphertext, &context)?;
assert_eq!(&*plaintext, b"session record");
# Ok(()) }
```

The async methods (`encrypt`, `decrypt`, `rewrap_current`, `rewrap_verified`) need
no extension import. `ContextEnvelopeSync` provides the synchronous methods only
for the sealed `ReadyKek` implementations. Rust forbids defining inherent methods
on a type from another crate, so this keyring-owned extension preserves the
recipes-to-keyring dependency direction. Every sync call polls once, returns
`Backend` on `Pending`, and never blocks or spins. No keyring crypto path reads a
clock. `ContextEnvelope::needs_rewrap` uses nonsecret name/current metadata supplied
by `Keyring::context_envelope`; it does not authenticate ciphertext. Local rewrap
checks the DEK commitment. `KeyringKek` deliberately does not implement `Rewrap`.

## Provisioning and loading

Install `akr` with `cargo install --path crates/anykms-keyring-cli`. Descriptors are
secret strings in the form `akr1;name=NAME;current=N;kN=BASE64URL.KCV`. `akr new NAME`
generates a root using OS entropy. Pipe it to a secret manager or `wrangler secret
put ANYKMS_KEYRING`. Never commit real descriptors or put them in command arguments.
`add`, `promote N`, and `remove N` read stdin and write a validated descriptor;
`show` reads stdin and prints only versions, KCVs and the fingerprint. `diff OLD NEW`
validates a transition from private files. Use `add --min-version N` to retain the
highest-ever-issued generation when removed generations are no longer in the
input. Changed key bytes, reused versions, renames, removal of the old current,
and backwards promotion are refused by `diff` to the extent the old descriptor
contains the history.

The parser accepts exactly one trailing newline and otherwise rejects whitespace,
unknown or duplicate fields, noncanonical numbers, invalid names, noncanonical
base64url, incorrect KCVs, all-zero roots and repeated root material. Error strings
contain no input key bytes or decoder source. Native `from_env` and `from_file`
require `std-loaders`; `from_file` checks the opened file's Unix permissions and
refuses group/other access. `from_file_with_permissions(path, true)` explicitly
allows it. These methods are not part of the crypto path.

Workers read `env.secret("ANYKMS_KEYRING")?.to_string()`, then call `Keyring::parse`.
For Secrets Store, await the binding read in the application and pass the returned
string to the same parser. The library performs no I/O. `examples/worker/src/lib.rs`
is a compilable per-isolate `OnceLock<Arc<Keyring>>` example: only successful loads
are installed, failures return HTTP 500 and log one fixed error per isolate, the
next request retries, and successful installation logs the fingerprint once.
The same loader implementation is exercised by `tests/worker_loader.rs`: a missing
or malformed binding cannot poison the cell, a corrected value succeeds on a later
call, and successful loads do not reread the binding. The temporary native descriptor
is zeroizing. Actual workerd additionally proves generic HTTP 500, once-only error
logging and no descriptor/root in bounded logs; the successful smoke runs in a new
isolate, so it does not claim a live secret-propagation or hot-reload experiment.
It also installs `global` before dispatch. The unauthenticated `/keyring` route in
this local smoke fixture must be placed behind the consuming application's admin
authentication in a deployment.

A running isolate never reloads a descriptor. Every change requires explicit
redeployment of every Worker in the trust unit. Do not infer rollout from secret
propagation. The rollout gate is one new Worker version at 100%, 20 consecutive
matching diagnostic responses per Worker, and ten minutes of logs without an old
fingerprint. After promotion, code rollback means deploying the old code with the
current descriptor; do not restore an older version's bindings.

## Registered purposes

| Purpose | Required fields | Padding | Recover fields from |
|---|---|---|---|
| `user-token` | `app_client_id` string, `user_id` u64 | 64-byte blocks | namespace and subject columns |
| `installation-token-cache` | `app_id` u64, `installation_id` u64, `scope` string, `expires_at` u64 | 64-byte blocks | cache key and plaintext expiry column |
| `restate-sealed` | `type` string | none | stable compile-time type identifier |
| `session` | `session_id` string | none | storage key |
| `pending-signin` | `session_id` string | none | trusted session identifier before hashing the pending-row key |
| `bulk-email` | `email_ref` string, `batch_id` string, `row` u64, `account_id` u64, `installation_id` u64 | 256-byte blocks | authenticated vault row identity and batch manifest, never mailbox text |
| `bulk-email-review` | `email_ref` string, `batch_id` string, `row` u64, `account_id` u64, `installation_id` u64 | 256-byte blocks | authenticated review escrow identity and batch manifest, never mailbox text |
| `attempt-continuation` | `scope` string, `binding` string | none | plaintext continuation columns |

Numeric values are canonical unsigned decimals; strings are exact UTF-8 bytes.
Unknown, duplicate and missing fields are errors. Values must always be recoverable
before decrypting. The canonical encoding is `anykms-ctx/v1\0`, field count, and
sorted length-prefixed key/value pairs. Context values are redacted from Debug.
Applications can declare additional purposes with `purpose!`; declarations are
validated at context construction. A context cannot omit its purpose. Padding is
checked before encryption; decrypt accepts the authenticated payload's padding.

## Rotation and compromise

Run `akr diff` before each deployment. Add a new readable generation, deploy and
complete the rollout gate; promote it, deploy and complete the gate, recording
promotion time. Rewrap D1 rows using `rewrap_current` (commitment checked) or
`rewrap_verified` (also checks payload and context). Update only if the stored
ciphertext still equals the one read; concurrent writes win. Re-scan until zero
rows need rewrap and all malformed rows have been handled. KV session removal waits
at least the maximum old session TTL after promotion plus the application's grace
period, and every write must set that TTL. Journal removal waits until every
pre-promotion secret-touching invocation completes plus journal retention.
`Sealed<T>` belongs only in invocation journals: never state, promises or awakeables.
Then remove old readable generations and deploy again. Keep archived keys under
access control for backups according to the product's retention policy.

A leaked descriptor compromises every generation present in it. Provision a new
root out of band, decrypt and **fully re-encrypt with fresh DEKs**, rotate underlying
protected credentials, cancel affected in-flight invocations, and remove all leaked
generations. Rewrapping alone preserves the payload and DEK and does not repair a
compromise. Old snapshots/Time Travel copies remain a residual exposure.

Random 96-bit GCM nonces limit each generation to 2^32 wraps; rotate well before
that count. Root bytes and derived keys use `Zeroizing`; JS secret strings and
copies made by the runtime cannot be reliably zeroized. Envelope v1, keyring wrap,
KCV/fingerprint and context formats are unreviewed cryptographic constructions.
External review remains the C2 production gate. P1's recoverable, short-lived data
is not gated on that review.

## Errors and conformance

`UnknownKeyVersion` means a version newer than this descriptor, includes a 30-second
retry hint, and must not invalidate a grant/session. `KeyVersionUnusable` means a
removed version and is permanent. `Configuration` includes KCV mismatch and requires
retry plus an operator alert. `DecryptFailed` includes wrong context, tampering and
swapped slots. Logs for version/configuration errors include the fingerprint, never
plaintext, key bytes or context values.

The raw-gated mutable provider is test/migration tooling. Its `ProviderKey` resolves
one immutable snapshot per operation so existing registry handles see `replace`;
normal `KeyringKek` handles remain immutable. Missing keys fail at operation time
with a named `NotFound`, matching the existing conformance contract. Provider URI
grammar errors remain `InvalidArgument/Open`, explicitly declared by the fixture.
Key semantics declare exact 32-byte wrapping and KCV checking. Conformance retains
default behavior for all other backends, but uses these declarations to select a
valid hygiene marker length and expect `Configuration` for cross-key KCV mismatches.
`DisableVersion` is n/a because suite cases disable the primary, which cannot be
removed. Readable-generation removal is covered by the rotation tests.

Run native tests, independent vectors, wasm and workerd as described in
`examples/worker/README.md`. Platform SD-07 deployment/rollback/Secrets Store
experiments and deployed CPU measurements require real account access; local
workerd success is not evidence of those platform behaviors. No external release
or tag is implied by these local source changes.
