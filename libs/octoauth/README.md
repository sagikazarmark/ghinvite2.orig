# octoauth — GitHub App toolkit

A small, client-agnostic set of crates for building GitHub Apps in Rust,
natively and on `wasm32-unknown-unknown` (Cloudflare Workers). Its own Cargo
workspace inside the ghinvite repository; consumed by path and intended to be
extracted to its own repository once a second product uses it.

| Crate | Purpose |
|---|---|
| `octoauth` | App JWT (RS256, cached, clock-skew corrected), installation access tokens, `Installation::get`, a public `ApiError::classify` for GitHub REST responses |
| `octotoken` | `InstallationTokenProvider` trait, the in-memory `AppTokenProvider` cache, and `with_token` (one 401 → one replacement → one retry) |
| `octologin` | Sign-in web flow: authorize URL with state + PKCE, code exchange, `GET /user`, verified emails, token revocation. Never persists a token |
| `octoscrub` | Pattern-based scrubbing of tokens, Basic/Bearer credentials and JWTs from log text |

Design and scope: `../../docs/decisions/0014-gh-toolkit2-scope-client-agnostic.md`
(client-agnostic: no owned REST client, no transport seam, webhooks via
`octoevents`), target design `../../docs/specs/L2-github-app-toolkit.md`, build
plan `../../docs/specs/octoauth-plan.md`. Examples of attaching a token to
`octocrab` and `reqwest` are in `crates/octotoken/examples/`.

Checks (also run by CI and `dagger check ghinvite:toolkit`):

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo check --locked -p octoauth -p octotoken -p octologin -p octoscrub --target wasm32-unknown-unknown
```

Known gaps before a second consumer (tracked in the ghinvite issue tracker):
no object-safe provider or isolate-scope registry (ghinvite carries its own in
`crates/ghinvite-github/src/apps.rs`), no `tracing` or event hooks, no
`invalidate_installation`, no acting-grant exchange or `rotcred` `Refresher`
adapter, no `check_token`/`delete_grant`, no user-installation listing.
