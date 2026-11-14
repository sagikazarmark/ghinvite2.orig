# Handoff: ghinvite consolidation and what comes next

Written 2026-11-11 for the next agent. Everything referenced is in this
repository unless a path starts with `specs.orig/` (the private planning
monorepo at `/home/laborant/ghinvite/specs.orig`, read-only context) or is a
GitHub issue.

## 1. Where things stand

Repository `/home/laborant/ghinvite/ghinvite2.orig`, branch **`consolidate`**
(not merged to `main`, not pushed), three commits on top of the PoC
(`89a67fa`, tagged `poc-89a67fa`):

| Commit | What |
|---|---|
| `1fc3ec3` | The P1 implementation (M0–M2 + gh-toolkit→octoauth migration) copied from `specs.orig/implementation/ghinvite@7576fe4` without history; `libs/octoauth`, `libs/kekring`; deps re-pointed (restate-sdk 0.13 fork, anykms 0.2 crates.io, harness 0.4, `ContextClockExt` inlined); Restate server 1.8.1 pins; rotcred test-helper escapes removed |
| `c47589f` | CI (`ci.yml`: ported `native-d1-bridge`/`native-d1-cutover`, new `toolkit` job, kekring wasm check), Dagger `ghinvite:toolkit`, `docs/decisions`, `docs/specs`, `PROVENANCE.md`, README/doc path fixes |
| `642489b` | restate-sdk from upstream `restatedev/sdk-rust@8a39311` (#141 merged, unreleased); KEK crate renamed `kekring`; root cause of the workerd failure documented |

Working tree clean. `specs.orig` untouched (only gitignored `node_modules`/`target` were created there and removed).

### Verified state of `consolidate` HEAD (this machine: rustc 1.92, 4 cores)

| Gate | Result |
|---|---|
| `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --locked -D warnings` | clean |
| `cargo test --workspace --locked` | **1319 passed, 0 failed, 9 ignored** (Playwright fixture servers). Pre-consolidation baseline on the same code: 1301 |
| wasm32 `cargo check`: `ghinvite-web-worker`, `ghinvite-workflows-worker`, `ghinvite-ui`, `ghinvite-island`, `kekring --features js` | pass |
| `libs/octoauth`: fmt / clippy / `cargo test` / wasm32 | clean, **143 passed**, pass |
| `cargo test -p ghinvite-workflows --features integration --test authoritative_admission` vs real `restate-server 1.8.1` (CI's step; harness 0.4) | 10/10 (≈190 s with `--test-threads=2`) |
| `scripts/test-restate.sh retained_delivery` (compose, 1.8.1) | pass |
| workerd `npm run test:startup` (62 cases) | pass |
| workerd `npm run test:admission` | **FAIL at one step — test-fixture artefact, understood, not fixed** (see §3.2) |

Not run: `test:production` (needs `worker-build`, which needs OpenSSL dev headers here), Playwright browser suites, the native-d1-bridge/cutover scripts, the other `test-restate.sh` targets, Dagger checks.

Toolchain notes for this machine: `export PATH="$HOME/.cargo/bin:/home/laborant/ghinvite/specs.orig/implementation/ghinvite/.devenv/profile/bin:$PATH"` gives cargo 1.92 plus a clang `cc` (there is no system C compiler); `wasm-bindgen-cli` 0.2.120 is installed; a full debug+test+wasm pass needs ~35 GB in `target/`; `restate-server` 1.8.1 and 1.7.9 binaries are at `/tmp/opencode/restate-server-1.8.1` / `-1.7.9` (sha256 of 1.8.1: `cb4c0890…9b0c`, recorded in `docs/native-fallback.md`).

## 2. Background: how we got here (decisions already made)

Read in this order if you need the reasoning: `PROVENANCE.md`, `docs/decisions/` (cross-cutting ADRs 0001, 0003, 0005–0010, 0014 from the planning repo), `docs/adr/` (this codebase's own ADRs), `docs/specs/` (P1 spec, L2 toolkit spec, keyring spec, build plans, PoC harvest, GitHub research).

- **Which copy was ahead.** The `specs.orig` copy was a functional superset of the PoC (177k vs 62k Rust LOC, 28 vs 1 migrations, 26 vs 13 Restate components); every file unique to the PoC had a traceable replacement. The "imported at 89a67fa" claim in `specs.orig/implementation/SOURCES.md` named the ancestor, not the content: ~3 weeks of P1 work only ever existed in that monorepo.
- **Toolkit verdict.** `libs/octoauth` is good enough for ghinvite; not enough for a second product (P2 Dependaboard) or standalone reuse. Gaps are listed in `libs/octoauth/README.md` (tier (a) hardening) and in §4.4 below (tier (b), P2-gated).
- **anykms.** Not vendored. ghinvite uses crates.io `anykms-core`/`anykms-recipes` 0.2. The fork's additions were triaged with the anykms maintainer's reviewers:
  - [anykms#33](https://github.com/sagikazarmark/anykms/issues/33) typed AAD: reduced to **a byte-encoding spec with no type**, bytes decided (`anykms/aad/v1\0`, u16/u16/u16 widths, caps 48/32/255/15 and total ≤ 3072, key `[a-z][a-z0-9_]*`, purpose `[a-z][a-z0-9-]*`, no `purpose` key override, byte-exact values). Triggers for a later `CanonicalAad` type recorded. **Landing sequence agreed; PR not yet written.**
  - [anykms#34](https://github.com/sagikazarmark/anykms/issues/34) `KeySemantics` key-material check: **closed**; the taxonomy argument won (KCV mismatch → `DecryptFailed` + `BackendDetail::code`, never `Configuration`). Replaced by [anykms#38](https://github.com/sagikazarmark/anykms/issues/38) (KeyWrap unwrap contract: `keywrap::authentication_failures` conformance case, 32-byte hygiene marker, `wrap_key` doc on unsupported lengths, `error.rs:47-50` clarification, `Semantics` in prelude). **PR not yet written.**
  - [anykms#35](https://github.com/sagikazarmark/anykms/issues/35) provisioned KEK: **incubate as a third-party backend outside anykms** (custody is a recorded non-goal for anykms for now); amended by comment with the redesign: AES-256-KW exact-32 (not GCM), `kid` not "KCV", high-water mark in the descriptor + full grammar, `DecryptFailed` + stable codes (`VersionAboveHighWaterMark`, `KeyIdMismatch`) with **no retry hint**, `KeyVersionUnusable` only for removed-below-HWM, Provider shipped unconditionally, drop `raw-kek`/`global`/sealed `ReadyKek`, concrete wrapper owning `rewrap_current = rewrap_local`, `needs_rewrap`, `reencrypt`, `*_sync`, `validate_transition(old, new)` as the library seam, format-named labels, pre-launch review of three ONE-WAY formats. [anykms#36](https://github.com/sagikazarmark/anykms/issues/36) (`akr` CLI): closed, stays downstream.
  - User chose: the KEK crate lives in **`libs/kekring`** of this repo for now; crate name **`kekring`** (placeholder, can change; wire labels are still the fork's `anykms-keyring/v1/*`, `akr1` — they change with the redesign anyway).
- **rotcred** (L1 rotating credentials, P2 only): stays in `specs.orig`; ~85–90 % of its spec implemented and its conformance gates re-run green locally; not needed by ghinvite (`crates/ghinvite-github/tests/dependency_boundary.rs` forbids it in the lockfile).
- **restate-sdk.** `restatedev/sdk-rust#128` was declined, then the fix landed as **#141** (`8a39311`, 2026-11-10). We pin that commit via `[patch.crates-io]`; later `main` has a breaking `TerminalErrorExt` change (#132). The user's fork is no longer referenced.
- **restate-toolkit.** `ContextClockExt` (incl. the never-published `now_named_with`) is inlined in `crates/ghinvite-workflows/src/clock.rs`; `restate-e2e-harness` 0.4 from crates.io; `restate-ext`/`restate-config` dropped.
- **Restate server 1.8.1** everywhere (compose, CI, scripts, docs) by tag + digest `sha256:fea6b4a8…b2c7`. SDK 0.13 (`shared-core` 8.2) negotiates protocol v5–v8; 1.8.x speaks v7.
- **History:** code copied without the monorepo's history (user decision).

## 3. Open items, in priority order

### 3.1 Land the branch
Merge `consolidate` → `main`, push, watch the first `ci.yml` run (13 jobs; `native-d1-cutover` and `native-d1-bridge` have never run from this layout). Then decide what to do with `specs.orig/implementation/{ghinvite,octoauth}` so there is one source of truth (delete + pointer in `SOURCES.md`, or mark superseded).

### 3.2 Fix the workerd `test:admission` fixture (root cause known)
`tests/worker/admission.mjs` step "exact deadline equality, overdue readmission without refund, request-owned terminal decision" (around line 531–559) times out on Restate ≥ 1.8.

Mechanism (verified by bisect, see `docs/local-testing.md`): the fixture pins the Worker's `Date.now()` to a January-2026 timeline via the `x-test-clock` header (`tests/worker/workflows-shim.mjs`), and the SDK computes `send_after(delay)` as `Date.now() + delay` (`sdk-rust/src/endpoint/context.rs:528`). So `InvitationRequest/tick_expire`'s "7 days later" self-send (`crates/ghinvite-workflows/src/request_owner.rs:1251-1266`) is already months in the past for the real server and fires immediately, forming a self-send chain on one object key. The legacy inbox (≤ 1.7, or 1.7.9 with v7 but without VQueues) interleaved the ingress `decide` FIFO so the chain ended after four links; **VQueues** (unconditional since 1.8.0, no off switch) schedule due timers ahead of inboxed calls, so `decide` starves until the 25 s client timeout. SDK version is irrelevant (fork and upstream behave identically). Production clocks are real, so the loop cannot form there.

Fix options: (a) anchor that step's fictional timeline near the real clock (e.g. `base = Date.now()` rounded; deadlines `base + 7d`; "advance to the deadline" sets `clock = deadline` which is still real-future, so timers stay inert and lazy expiry in `decide`/`request_status` does the work — the later step at line 565 already uses `Date.now() - 604800000 + 5000` for this reason); (b) make the shim not affect the SDK's absolute-time computation (hard: global `Date`). Prefer (a). Optionally also guard `tick_expire` against a non-positive computed delay (defence in depth; `expire_due` already makes it unreachable in production).

Also pre-existing: `scripts/test-restate.sh` runs `authoritative_admission`'s 10 tests fully parallel under a hard 150 s bound, too tight on 4 cores (CI uses `--test-threads=2`, ~190 s). Either raise the bound or pass `--test-threads=2`.

### 3.3 anykms upstream PRs (small, unblock the kekring redesign)
1. #33: `docs/aad-format.md` (envelope-style status header, wire-fixed vs policy-cap table, the settled bytes, versioning story, "no decoder; bytes rebuilt never parsed", reader-first note, rationale vs AWS ESDK), `tests/vectors/aad_v1.json` built by an independent test-only encoder, a short "Choosing AAD" guide section, PLAN.md entry with the triggers.
2. #38: conformance case + marker + two doc fixes + prelude export.
3. PLAN.md: custody-out-of-identity non-goal, "community backends" note, `with_default_providers` excludes third parties.
The reviewers asked to see the drafts; the user has ADMIN on `sagikazarmark/anykms` (gh is authenticated as that account).

### 3.4 kekring redesign + ghinvite switch (one change, call sites move once)
Apply the #35 amendment to `libs/kekring` (see §2 bullet). Keep `kekring` as the name unless the user picks another; derive labels from it (e.g. `kekring/v1/{kek,kid,fp}`, descriptor `kekring1;…`, blob magic for the AES-KW layout `magic ‖ u32be version ‖ kid[8] ‖ KW(DEK)` = 56 bytes). Implement `Aad` per the #33 spec bytes inside kekring until upstream ships a type. Write `docs/kekring-format.md` (three ONE-WAY contracts), JSON vectors re-derived by a Node+OpenSSL script (`id-aes256-wrap`), descriptor + blob fuzz targets, `validate_transition`, `compile_fail,E0080` doctests (no trybuild).

ghinvite side: move the 5 purposes (`Session`, `PendingSignIn`, `AttemptContinuation`, `BulkEmail`, `BulkEmailReview`) into `ghinvite-core` as hand-written `impl Purpose` + `impl TryFrom<&X> for Aad`; switch rollout-skew handling from `ErrorKind::UnknownKeyVersion` to `detail().code()`; re-derive the keyring bytes in the six `tests/worker/*.mjs` helpers (`signin-cutover`, `email-vault`, `email-review`, …) and `ghinvite-config`'s descriptor parsing; regenerate `libs/kekring/tests/vectors/*.json`; `anykms-recipes` stays `default-features = false, features = ["envelope"]` (+`js` via kekring on wasm32). Prelaunch: no data migration, local DB reset is fine.

### 3.5 Toolkit hardening (tier (a), `libs/octoauth`)
`DynInstallationTokenProvider` (boxed futures) + opt-in `send-wrapper` feature so consumers stop re-writing `crates/ghinvite-github/src/apps.rs:123-171`; an isolate-scope `ProviderRegistry` keyed by `AppId` replacing `apps.rs:236-266`; optional `tracing` feature / event hook on `App` and `AppTokenProvider`; LICENSE-MIT/APACHE + CHANGELOG per crate; consider a committed `error-codes.txt` with a CI diff; relax `dependency_boundary.rs` to forbid `rotcred-*` only among ghinvite crates' direct deps (needed once octologin gains the L1 adapter).

### 3.6 Dependency pins to revisit
- `restate-sdk`: when a release containing #141 ships (0.13.1+ or 0.14), drop the `[patch.crates-io]`; watch for #132's breaking `TerminalErrorExt` change when crossing that boundary.
- `restate-e2e-harness` 0.4 is fine; `now_named_with` could still be upstreamed to `sagikazarmark/restate-toolkit` to delete the inlined copy (optional).

### 3.7 Then the product gates
Per the planning ledger (`specs.orig/plans/implementation-status.md`, stale and deliberately not imported), every live/deployed gate is unverified: M0/M1 acceptance against a real GitHub App installation, remote D1, Restate Cloud, the operator checklist in `docs/deploy.md`. The P1 spec and build plan in `docs/specs/` define them.

## 4. Deferred by decision (do not start without asking)
- **P2 Dependaboard prerequisites** (tier (b) toolkit gaps): GitHub `rotcred_core::Refresher` + `user_token_context` next to `octologin` (the deleted 303-line reference is at `specs.orig` git `a7c4d86^:implementation/gh-toolkit/crates/gh-toolkit/src/signin/rotating.rs`), acting-grant exchange with `Acting` typestate, `check_token`/`delete_grant`, `list_user_installations`, `invalidate_installation`, GraphQL, extraction of ghinvite-github's generic transport/REST/pagination/credential/error/mock plumbing into the toolkit, rotcred's two Node lockfile fixes and hosted CI.
- Durable Object executor for rotcred; `anykms-keyring` graduation upstream (after external review of the formats).
- Clerk, P6 Klassroom, everything else in `specs.orig/PLAN.md` §9.

## 5. Quick reference

```bash
export PATH="$HOME/.cargo/bin:/home/laborant/ghinvite/specs.orig/implementation/ghinvite/.devenv/profile/bin:$PATH"
cargo fmt --all --check && cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked                       # 1319 / 0 / 9
for p in ghinvite-web-worker ghinvite-workflows-worker ghinvite-ui ghinvite-island; do cargo check --locked -p $p --target wasm32-unknown-unknown; done
cargo check --locked -p kekring --features js --target wasm32-unknown-unknown
(cd libs/octoauth && cargo test --workspace --locked && cargo clippy --workspace --all-targets --locked -- -D warnings)
RESTATE_SERVER_BIN=/tmp/opencode/restate-server-1.8.1 cargo test --locked -p ghinvite-workflows --features integration --test authoritative_admission -- --test-threads=2
bash scripts/test-restate.sh retained_delivery        # Docker
(cd tests/worker && npm ci && npm run test:startup && npm run test:admission)   # the latter fails until §3.2
```

Issue tracker: GitHub Issues on `sagikazarmark/ghinvite2.orig` (`AGENTS.md`, `docs/agents/`). Bisect logs and server binaries from this session: `/tmp/opencode/` (ephemeral).
