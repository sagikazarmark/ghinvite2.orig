# ghinvite Restate Json Payloads Design

## Goal

Replace `crates/restate-svc`'s custom Restate payload framing macro with Restate's idiomatic Rust payload model: `restate_sdk::serde::Json<T>` plus `schemars::JsonSchema`.

This keeps business behavior unchanged while letting the Restate SDK own JSON serialization, payload metadata, and service schema generation.

## Current State

`crates/restate-svc/src/restate_payload.rs` defines `impl_restate_json_payload!`, which manually implements `restate_sdk::serde::{Serialize, Deserialize, PayloadMetadata}` for local payload types. It serializes through `serde_json` and returns an empty schema object.

The macro is used by Restate-framed payload types in:

- `installation.rs`
- `share_link.rs`
- `github_invitation.rs`
- `invitation_request.rs`
- `reconcile.rs`

The workspace already enables the `schemars` feature on `restate-sdk`, but `restate-svc` does not currently depend directly on `schemars`.

## Chosen Approach

Fully remove `impl_restate_json_payload!` and convert all Restate-framed local payloads in `crates/restate-svc` to SDK-managed `Json<T>` wrappers.

This includes public service, object, and workflow handler signatures as well as internal Restate framing boundaries such as `ctx.run` return values and workflow promises. Pure business-logic functions continue to take and return plain Rust types.

## Detailed Design

Add a direct `schemars` dependency for `restate-svc`. Prefer a workspace dependency so the version is explicit and shared consistently with the Restate SDK's schema feature. Enable `schemars`'s `chrono04` feature because Restate payloads include `chrono::DateTime<Utc>` fields.

Add `schemars` support to `domain` only for types embedded in Restate payloads. The schema must match the existing JSON wire format:

- ULID newtypes (`ShareLinkId`, `RequestId`, `GithubInvitationId`) serialize as 26-character strings, so their schema should be a string, not an object containing `Ulid` internals.
- Enum types such as `Permission`, `AccountType`, and `RequestState` should preserve their existing serde rename rules.
- `ShareLinkRepo` can derive schema directly once its fields do.
- `SelectedRepos` uses custom serde: either the string `"all"` or an array of repo IDs. Its schema must model that untagged union rather than pretending it is an enum object.

For each type currently passed to `impl_restate_json_payload!`, derive `schemars::JsonSchema` alongside existing `serde::{Serialize, Deserialize}` derives.

Change Restate trait signatures from bare payload types to `Json<T>` where Restate frames input or output payloads. Examples:

```rust
async fn onboard(input: Json<OnboardInput>) -> Result<(), TerminalError>;

async fn create(input: Json<CreateLinkInput>) -> Result<Json<CreateLinkOutput>, TerminalError>;
```

At implementation edges, unwrap immediately and keep the existing pure logic unchanged:

```rust
let Json(input) = input;
```

For methods returning payloads, wrap the existing output before returning:

```rust
Ok(Json(output))
```

For workflow internals that Restate frames, use `Json<T>` at the framing boundary and unwrap before plain Rust matching or comparisons. This applies to durable `ctx.run` outputs and `ctx.promise::<Json<Decision>>(...)` payloads.

Remove the old helper module and imports:

- Delete `crates/restate-svc/src/restate_payload.rs`.
- Remove `pub(crate) mod restate_payload;` from `lib.rs`.
- Remove all `use crate::impl_restate_json_payload;` imports.
- Remove all `impl_restate_json_payload!(...)` calls.
- Update comments that refer to the old macro or custom Restate framing traits.

## Error Handling

Error behavior does not change.

`HandlerError`, terminal/transient classification, storage writes, GitHub calls, audit emission, and retry behavior remain untouched. Any compile-time schema gaps should be fixed by deriving `JsonSchema` on owned local or domain types rather than reintroducing custom payload serialization.

## Testing

Run:

```bash
cargo check -p restate-svc
cargo test -p restate-svc
```

`cargo check` validates SDK macro signatures, `Json<T>` usage, and `JsonSchema` derive coverage. `cargo test` verifies that the pure handler behavior remains unchanged.

If domain types embedded in Restate payloads lack schema support, add schema derives or manual schema annotations in the owning `domain` crate with the narrowest required changes. Do not change their serde wire format while adding schema support.

## Non-Goals

This refactor does not change Restate service names, method names, endpoint registration, request JSON shapes, storage schemas, GitHub API behavior, audit behavior, or web routes.

It does not introduce compatibility shims for the deleted macro because there are no external consumers of the macro and the conversion is internal to `restate-svc`.
