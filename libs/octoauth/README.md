# octoauth — GitHub App toolkit

A small, client-agnostic set of crates for building GitHub Apps in Rust,
natively and on `wasm32-unknown-unknown` (Cloudflare Workers). Its own Cargo
workspace inside the ghinvite repository; consumed by path and intended to be
extracted to its own repository once a second product uses it.

| Crate | Purpose |
|---|---|
| `octoauth` | App JWT (RS256, cached, clock-skew corrected), installation access tokens, `Installation::get`, `App::with_credential` (app-level requests with the same skew correction), a public `ApiError::classify` for GitHub REST responses |
| `octotoken` | `InstallationTokenProvider` trait, the in-memory `AppTokenProvider` cache, `with_token` (one 401 → one replacement → one retry), the object-safe `DynInstallationTokenProvider` and the isolate-scope `ProviderRegistry` |
| `octoauth-octocrab` | octocrab glue: a client builder for the App's API, `octocrab::Error` converted to `ApiError` without upstream text, the 401 signals for `with_token` and `App::with_credential`. Separate from octoauth so octocrab's breaking releases stay out of octoauth's versions |
| `octologin` | Sign-in web flow: authorize URL with state + PKCE, code exchange, `GET /user`, verified emails, token revocation. Never persists a token |
| `octoscrub` | Pattern-based scrubbing of tokens, Basic/Bearer credentials and JWTs from log text |

Design and scope: `../../docs/decisions/0014-gh-toolkit2-scope-client-agnostic.md`
(client-agnostic: no owned REST client, no transport seam, webhooks via
`octoevents`), target design `../../docs/specs/L2-github-app-toolkit.md`, build
plan `../../docs/specs/octoauth-plan.md`. Examples of attaching a token to
`octocrab` and `reqwest` are in `crates/octotoken/examples/`.

`examples/installations` is a complete, minimal GitHub App (a CLI that lists
the App's installations and their repositories), tested against a
local GitHub stand-in (wiremock). Its README lists the friction it found in
the toolkit.

The GitHub emulator, `octoemu`, is not part of the toolkit: it is ghinvite's
test infrastructure, in its own workspace at `../octoemu`. Its tests run the
toolkit's clients against it (`octoauth`, `octotoken`, `octologin`). The
toolkit's own tests use wiremock.

Optional features:

| Feature | Crate | Effect |
|---|---|---|
| `middleware` | `octoauth` | Requests go through `reqwest_middleware` (retries, `reqwest-tracing`) |
| `tracing` | `octoauth`, `octotoken` | `tracing` events for token creation, replacement and failure, clock-skew correction and credential signing; fields are IDs, error codes and seconds, never a secret (octotoken's also enables octoauth's) |
| `send-wrapper` | `octotoken` | `DynInstallationTokenProvider` futures are `Send` on wasm32 too (`send_wrapper::SendWrapper`), for hosts such as Restate's `ctx.run` |

Stable error codes (`code()`) are listed in `error-codes.txt`; they are
append-only, and `crates/octoauth/tests/error_codes.rs` fails when the sources
and the list differ. Licensed under MIT OR Apache-2.0 (`LICENSE-MIT`,
`LICENSE-APACHE`); each crate has a `CHANGELOG.md`.

Checks (also run by CI and `dagger check ghinvite:toolkit`):

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-features --locked
cargo check --locked -p octoauth -p octotoken -p octologin -p octoscrub --target wasm32-unknown-unknown
cargo check --locked -p octoauth -p octotoken -p octologin -p octoscrub --all-features --target wasm32-unknown-unknown
```

`octoauth-octocrab` and `examples/installations` are native-only and left
out of the wasm32 checks; on wasm32 they compile to empty crates, so a
`--workspace` wasm32 check still passes. (octocrab on Workers needs its own
transport, which is unverified; ADR 0014.)

Hardening before a second consumer (HANDOFF §3.5, tier (a)), done:

- [x] Object-safe `DynInstallationTokenProvider` with an opt-in `send-wrapper`
  feature, and an isolate-scope `ProviderRegistry` keyed by App ID (ghinvite's
  hand-written adapter and cache in `crates/ghinvite-github/src/apps.rs` are
  gone).
- [x] Optional `tracing` events on `App` and `AppTokenProvider`.
- [x] Licenses, per-crate changelogs, committed `error-codes.txt`.
- [x] `InstallationTokenProvider::invalidate` and `invalidate_installation`:
  drop a rejected token, or every token of an installation, without creating
  one (ghinvite's fenced writes and permission recheck, P1 spec §9.4/§10.2).

Known gaps (tier (b), deferred; tracked in the ghinvite issue tracker): no
acting-grant exchange or `rotcred` `Refresher` adapter, no
`check_token`/`delete_grant`, no user-installation listing.
