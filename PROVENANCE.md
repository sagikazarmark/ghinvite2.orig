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
| `libs/kekring` | `specs.orig/implementation/anykms/crates/anykms-keyring` + `anykms-recipes/src/context.rs` @ `7576fe4` | Package `anykms-keyring`: provisioned local KEK with the typed encryption context folded in as its `context` module, rebuilt on crates.io `anykms-core`/`anykms-recipes` 0.2. **Stopgap**: formats and error mapping are superseded by [anykms#33](https://github.com/sagikazarmark/anykms/issues/33) and [anykms#35](https://github.com/sagikazarmark/anykms/issues/35); the application is prelaunch and will move to the redesigned crate before its first production write. Dropped from the fork: `raw-kek`/`KeyringProvider` (needed `KeySemantics` changes upstream declined in anykms#34), the `akr` CLI, examples, trybuild UI tests. |

## Pinned forks and version-sensitive dependencies

| Dependency | Pin | Why |
|---|---|---|
| `restate-sdk` 0.13 | `[patch.crates-io]` → `github.com/sagikazarmark/restate-sdk-rust`, branch `wasm` (`Cargo.lock` records the commit; `b20d2050` at the time of writing) | Carries [restatedev/sdk-rust#128](https://github.com/restatedev/sdk-rust/pull/128) (`web-time` clocks and no tokio drain timeout on `wasm32-unknown-unknown`), which upstream declined on 2026-11-09. Rebased onto each upstream release; `cargo update -p restate-sdk` moves the pin deliberately. The branch has been force-pushed before: prefer tagging a release on the fork and switching to `tag =`. Two further local patches that existed in the monorepo (`wasm-channels`, `verification-tests`) are not needed here and were dropped. |
| `restate-e2e-harness` 0.4 | crates.io | Requires `restate-sdk ^0.13`, hence the SDK bump. `restate-ext`'s `ContextClockExt` (with the never-published `now_named_with`) is inlined in `crates/ghinvite-workflows/src/clock.rs`; `restate-config` is unused. |
| Restate server 1.8.1 | `compose.yaml`, `.github/workflows/ci.yml`, `docs/native-fallback.md`, `scripts/verify-native-d1-cutover.sh` (tag + image digest; extracted `restate-server` sha256 recorded) | SDK 0.13 (`restate-sdk-shared-core` 8.2) negotiates service protocol v5–v8; 1.8.x speaks v7 by default. Known open issue: the workerd `test:admission` suite fails deterministically on 1.8.x and passes on 1.7.9 with either SDK (see `docs/local-testing.md`). |
| `anykms-core`, `anykms-recipes` 0.2 | crates.io | Upstream of the vendored fork the monorepo carried. Only `Ciphertext`, `Error`/`ErrorKind` and `envelope::Padding` are used directly; everything else goes through `libs/kekring`. |
| `octoevents` 0.5 | crates.io | Webhook verification and event types. |
| `worker` 0.8.1, `wasm-bindgen-cli` 0.2.120, `worker-build` 0.8.1 | crates.io / tooling | Must match exactly (see `tests/worker/README` and the CI workflow). |

## Not carried

- `rotcred` (the rotating-credential library, L1): a P2 concern; ghinvite stores
  no user tokens and `crates/ghinvite-github/tests/dependency_boundary.rs`
  forbids it in the lockfile. Its Node test helper `runtime-output.mjs` was
  copied into `tests/native-d1-bridge/`.
- `restate-worker`, `workers-rs-worker`: not used by this workspace.
- The monorepo's `plans/implementation-status.md` ledger: stale relative to the
  code it described; not imported.
