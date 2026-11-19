# Provenance

Where the code in this repository came from and what it pins outside itself.
The repository is standalone: every dependency is on crates.io, in this tree,
or pinned to a specific commit of a public fork.

## History

Up to tag `poc-89a67fa` (2026-09-25) this repository held the ghinvite
proof of concept. The P1 implementation (milestones M0–M2) and the migration
of its GitHub access onto the `octo*` toolkit were developed in a private
planning monorepo (`sagikazarmark/specs.orig`, `implementation/ghinvite`,
through commit `7576fe4`, 2026-10-21) and copied here in one commit without
that history. The planning documents the code was built from are under
`docs/decisions/` and `docs/specs/`; this codebase's own ADRs stay in
`docs/adr/` (the two numbering schemes are unrelated, hence two directories).

## In-tree libraries

| Path | Origin | Notes |
|---|---|---|
| `libs/octoauth` | `specs.orig/implementation/octoauth` @ `7576fe4` (originally `gh-toolkit2`) | The GitHub App toolkit: `octoauth`, `octotoken`, `octologin`, `octoscrub`. Own Cargo workspace, excluded from the root workspace, consumed by path. Scope per `docs/decisions/0014-gh-toolkit2-scope-client-agnostic.md`. Unpublished. |
| `libs/kekring` | `specs.orig/implementation/anykms/crates/anykms-keyring` + `anykms-recipes/src/context.rs` @ `7576fe4`, redesigned 2026-11-15 | Package `kekring`: a third-party anykms backend on crates.io `anykms-core`/`anykms-recipes` 0.4, redesigned per [anykms#35](https://github.com/sagikazarmark/anykms/issues/35) as amended (AES-256-KW, `kid`, high-water mark, stable unwrap codes, `validate_transition`) with anykms canonical AAD v1 ([anykms#33](https://github.com/sagikazarmark/anykms/issues/33)). Formats: `libs/kekring/docs/kekring-format.md` (external review pending). Nothing of the fork's wire formats (`akr1`, `anykms-keyring/v1/*`, `anykms-ctx/v1`) remains; also gone: `raw-kek`, `global`, the sealed sync traits, the `purpose!` macro, the `akr` CLI (replaced by the `descriptor` example), trybuild UI tests. |

## Pinned forks and version-sensitive dependencies

| Dependency | Pin | Why |
|---|---|---|
| `restate-sdk` 0.13 | `[patch.crates-io]` → `github.com/restatedev/sdk-rust`, `rev = "8a39311"` | Upstream commit of the `wasm32-unknown-unknown` fix ([restatedev/sdk-rust#141](https://github.com/restatedev/sdk-rust/pull/141), 2026-11-10: `web-time` clocks, no tokio drain timeout, getrandom backends). Not in a release yet; the pinned commit is 0.13.0 plus that fix, before the breaking `TerminalErrorExt` change (#132). Drop the patch when a release containing #141 ships. The earlier fork (`sagikazarmark/restate-sdk-rust`, branch `wasm`) is no longer used; its two extra local patches (`wasm-channels`, `verification-tests`) belonged to rotcred. |
| `restate-e2e-harness` 0.4 | crates.io | Requires `restate-sdk ^0.13`, hence the SDK bump. `restate-ext`'s `ContextClockExt` (with the never-published `now_named_with`) is inlined in `crates/ghinvite-workflows/src/clock.rs`; `restate-config` is unused. |
| Restate server 1.8.1 | `compose.yaml`, `.github/workflows/ci.yml`, `docs/native-fallback.md`, `scripts/verify-native-d1-cutover.sh` (tag + image digest; extracted `restate-server` sha256 recorded) | SDK 0.13 (`restate-sdk-shared-core` 8.2) negotiates service protocol v5–v8; 1.8.x speaks v7 by default. Known open issue: the workerd `test:admission` suite fails on 1.8.x because VQueues (unconditional since 1.8.0) reorder a fixture-induced past-due timer chain ahead of an ingress call; a test-fixture artefact, see `docs/local-testing.md`. |
| `anykms-core`, `anykms-recipes` 0.4 | crates.io | Upstream of the vendored fork the monorepo carried. `anykms-recipes` without default features (`envelope` only: the default RSA backend does not build for wasm32). The application uses `Ciphertext` and `Error`/`ErrorKind` directly; everything else goes through `libs/kekring`. |
| `octoevents` 0.5 | crates.io | Webhook verification and event types. |
| github/rest-api-description | commit `c9c95fe34ced05fe076001f5c55704742bc8e7f8` (2026-11-17), `descriptions-next/api.github.com/api.github.com.2022-11-28.json`, OpenAPI 3.1.0, sha256 `42f012de68d7c4641cf8e26c04c2cb37f0aee83803716aadd1bcfa217ba33aad` | The octoemu fidelity suite's schema check. Only the operations octoemu implements and the component schemas they reach are checked in (`libs/octoauth/crates/octoemu-conformance/schemas/rest-api-subset.json`, written by `schemas/extract.py`, which records the pin in the file); the 14 MB description is not. |
| `jsonschema` 0.61.2 | crates.io, `default-features = false` (no remote or file reference resolution) | Validates the emulator's responses against the subset above (`octoemu-conformance` only, in the toolkit workspace). |
| `miniflare` 4.20260730.0, `esbuild` 0.28.2 (`tests/worker`) | npm, exact; `overrides`: `undici` 7.30.2, `sharp` 0.35.5 | The workerd test harness. Latest stable miniflare 4. Its exact undici/sharp pins are vulnerable, so the overrides lift them to the patched releases in the same major (audit 2026-11-19, `docs/HANDOFF.md` §3.6). miniflare 5 changes the constructor options (a `workers` array is required). Drop the overrides when moving to miniflare 5. |
| `worker` 0.8.1, `wasm-bindgen-cli` 0.2.120, `worker-build` 0.8.1 | crates.io / tooling | Must match exactly (see `tests/worker/README` and the CI workflow). |

## Not carried

- `rotcred` (the rotating-credential library, L1): a P2 concern; ghinvite stores
  no user tokens and `crates/ghinvite-github/tests/dependency_boundary.rs`
  forbids it in the lockfile. Its Node test helper `runtime-output.mjs` was
  copied into `tests/native-d1-bridge/`.
- `restate-worker`, `workers-rs-worker`: not used by this workspace.
- The monorepo's `plans/implementation-status.md` ledger: stale relative to the
  code it described; not imported.
