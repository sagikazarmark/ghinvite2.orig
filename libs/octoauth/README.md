# octoauth — GitHub App toolkit

A small, client-agnostic set of crates for building GitHub Apps in Rust,
natively and on `wasm32-unknown-unknown` (Cloudflare Workers). Its own Cargo
workspace inside the ghinvite repository; consumed by path and intended to be
extracted to its own repository once a second product uses it.

| Crate | Purpose |
|---|---|
| `octoauth` | App JWT (RS256, cached, clock-skew corrected), installation access tokens, `Installation::get`, a public `ApiError::classify` for GitHub REST responses |
| `octotoken` | `InstallationTokenProvider` trait, the in-memory `AppTokenProvider` cache, `with_token` (one 401 → one replacement → one retry), the object-safe `DynInstallationTokenProvider` and the isolate-scope `ProviderRegistry` |
| `octologin` | Sign-in web flow: authorize URL with state + PKCE, code exchange, `GET /user`, verified emails, token revocation. Never persists a token |
| `octoscrub` | Pattern-based scrubbing of tokens, Basic/Bearer credentials and JWTs from log text |
| `octoemu` | Test-only, native-only GitHub emulator (embedded or `octoemu` binary): seed, virtual clock, admin API, call log, fidelity manifest. Skeleton: every GitHub route answers 501 until GE-02+ (`../../docs/specs/github-emulator.md`, `crates/octoemu/README.md`) |

Design and scope: `../../docs/decisions/0014-gh-toolkit2-scope-client-agnostic.md`
(client-agnostic: no owned REST client, no transport seam, webhooks via
`octoevents`), target design `../../docs/specs/L2-github-app-toolkit.md`, build
plan `../../docs/specs/octoauth-plan.md`. Examples of attaching a token to
`octocrab` and `reqwest` are in `crates/octotoken/examples/`.

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

`octoemu` is native-only and left out of the wasm32 checks; on wasm32 it
compiles to an empty crate, so a `--workspace` wasm32 check still passes.

Hardening before a second consumer (HANDOFF §3.5, tier (a)), done:

- [x] Object-safe `DynInstallationTokenProvider` with an opt-in `send-wrapper`
  feature, and an isolate-scope `ProviderRegistry` keyed by App ID (ghinvite's
  hand-written adapter and cache in `crates/ghinvite-github/src/apps.rs` are
  gone).
- [x] Optional `tracing` events on `App` and `AppTokenProvider`.
- [x] Licenses, per-crate changelogs, committed `error-codes.txt`.

Known gaps (tier (b), deferred; tracked in the ghinvite issue tracker): no
`invalidate_installation`, no acting-grant exchange or `rotcred` `Refresher`
adapter, no `check_token`/`delete_grant`, no user-installation listing.
