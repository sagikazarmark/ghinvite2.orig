# Handoff: ghinvite consolidation and what comes next

Written 2026-11-11 for the next agent; updated 2026-11-15 (§3.1–§3.5 closed, see the session log in §1). Everything referenced is in this
repository unless a path starts with `specs.orig/` (the private planning
monorepo at `/home/laborant/ghinvite/specs.orig`, read-only context) or is a
GitHub issue.

## 1. Where things stand

Repository `/home/laborant/ghinvite/ghinvite2.orig`. **`consolidate` is merged
into `main` (fast-forward) and pushed**; the branch is kept locally and equals
`main`. The consolidation is three commits on top of the PoC (`89a67fa`, tagged
`poc-89a67fa`):

| Commit | What |
|---|---|
| `1fc3ec3` | The P1 implementation (M0–M2 + gh-toolkit→octoauth migration) copied from `specs.orig/implementation/ghinvite@7576fe4` without history; `libs/octoauth`, `libs/kekring`; deps re-pointed (restate-sdk 0.13 fork, anykms 0.2 crates.io, harness 0.4, `ContextClockExt` inlined); Restate server 1.8.1 pins; rotcred test-helper escapes removed |
| `c47589f` | CI (`ci.yml`: ported `native-d1-bridge`/`native-d1-cutover`, new `toolkit` job, kekring wasm check), Dagger `ghinvite:toolkit`, `docs/decisions`, `docs/specs`, `PROVENANCE.md`, README/doc path fixes |
| `642489b` | restate-sdk from upstream `restatedev/sdk-rust@8a39311` (#141 merged, unreleased); KEK crate renamed `kekring`; root cause of the workerd failure documented |

Follow-ups on 2026-11-14/15, all on `main`:

| Commit | What |
|---|---|
| `1766d43` | workerd `test:admission` fixture fix (§3.2); compose-mode `authoritative_admission` limitation documented |
| `57111f9` | regenerated `styles.built.css` (CI CSS freshness; Tailwind auto-detects `isolate`/`row-0` in Rust sources) |
| `866ab98` | octoauth `wasm_dependencies` test: `cargo metadata --locked` instead of `--offline` (cold CI cache lacks wasm32-only packages) |
| `926dd2a` | `link-pause.mjs` 21-way concurrent admission burst: 90 s client timeout (serialized at ~0.8 s per admit; hosted runners exceed 25 s) |

`specs.orig`: `implementation/{ghinvite,octoauth}` and `.github/workflows/p1.yml` removed, pointer in `implementation/SOURCES.md` and `SESSION.md`. That is commit `45edec2`, pushed 2026-11-15. The gitignored `implementation/ghinvite/.devenv` toolchain profile (on the PATH below) stays on disk and now shows as untracked there.

GitHub reports **30 Dependabot vulnerabilities** on `main` (13 high, 11 moderate, 6 low). The `gh` token cannot read the alerts API (403), so they are untriaged. Two Dependabot PRs were already open before the push.

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
| workerd `npm run test:admission` | pass (26 steps, fixed 2026-11-14, see §3.2) |

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
  - User chose: the KEK crate lives in **`libs/kekring`** of this repo for now; crate name **`kekring`** (placeholder, can change). Since §3.4 the wire labels derive from it (`kekring/v1/*`, `kekring1`, `krw1`); nothing of the fork's formats remains.
- **rotcred** (L1 rotating credentials, P2 only): stays in `specs.orig`; ~85–90 % of its spec implemented and its conformance gates re-run green locally; not needed by ghinvite (`crates/ghinvite-github/tests/dependency_boundary.rs` forbids it in the lockfile).
- **restate-sdk.** `restatedev/sdk-rust#128` was declined, then the fix landed as **#141** (`8a39311`, 2026-11-10). We pin that commit via `[patch.crates-io]`; later `main` has a breaking `TerminalErrorExt` change (#132). The user's fork is no longer referenced.
- **restate-toolkit.** `ContextClockExt` (incl. the never-published `now_named_with`) is inlined in `crates/ghinvite-workflows/src/clock.rs`; `restate-e2e-harness` 0.4 from crates.io; `restate-ext`/`restate-config` dropped.
- **Restate server 1.8.1** everywhere (compose, CI, scripts, docs) by tag + digest `sha256:fea6b4a8…b2c7`. SDK 0.13 (`shared-core` 8.2) negotiates protocol v5–v8; 1.8.x speaks v7.
- **History:** code copied without the monorepo's history (user decision).

## 3. Open items, in priority order

### 3.1 Land the branch — done (2026-11-15)
Merged and pushed. The first `ci.yml` runs (`1766d43`, `57111f9`) failed on three never-before-green steps, each fixed (table in §1): CSS freshness, the octoauth offline-metadata test, and the `PAUSE_ONLY` burst timeout. `native-d1-bridge` and `native-d1-cutover` passed on their first run from this layout. CI on `926dd2a` (run 43658076272): **all 12 jobs green**.

Note: the `Worker session and admission runtime` job stops at its first failing step. The steps after `PAUSE_ONLY` (`DELIVERY_CONTROL_ONLY`, `CANCELLATION_ONLY`, `BUDGET_CAP_ONLY`, `test:admission`, `test:journey`, `test:delivery`, …) had never run on a hosted runner before `926dd2a`; all pass locally. The serialized ~0.75 s per admission on one link (identical on Restate 1.7.9 and 1.8.1) was diagnosed on 2026-11-15 (branch `test-infra`). It is not a delay: Restate re-invokes within ~1.5 ms. Each admission is ~45 request-response rounds on its critical path (lazy state reads, run acknowledgements and calls each suspend under `ProtocolMode::RequestResponse`). Each round costs ~14-17 ms in the fixture, ~11.5 ms of it the Worker rebuilding the endpoint (all services' discovery metadata) on every request in debug Wasm. The round count is production behaviour, so per-link throughput in production is ~1/(45 x per-round RTT+CPU). Recommended fixes, not implemented because both need care: build the endpoint once per isolate, and batch independent state reads or merge runs. The burst keeps its 90 s timeout. Evidence and numbers are in `docs/local-testing.md`. Revisit before load-sensitive product gates.

Still open: push `specs.orig` `45edec2` when the user agrees.

### 3.2 Workerd `test:admission` fixture — done (2026-11-14)
The step "exact deadline equality, overdue readmission without refund, request-owned terminal decision" now anchors its timeline at the real clock (option (a)), so `tick_expire`'s delayed self-sends stay in the future and lazy expiry does the work. The mechanism (pinned `Date.now()` + SDK `send_after` = past timers; VQueues starve the queued `decide`) is in `docs/local-testing.md`. Any new fixture step that schedules request expiry under `x-test-clock` must do the same. The optional `tick_expire` non-positive-delay guard was not added (unreachable in production).

**Resolved (2026-11-15, branch `test-infra`):** `authoritative_admission` cannot share a Restate server, so `scripts/test-restate.sh authoritative_admission` now refuses with the native command and the script's default target is `request_owner`. Each test deploys its own endpoint (own faults, own in-memory storage) under the same service names: on one server, new invocations go to whichever deployment registered last (two concurrent tests reproduce the `200`-for-`503` deterministically), and retained object state (already onboarded `AccountInstallation/repo:100` emits no installation projection) leaves a later test's storage without parents (`requester_message` fails on the second run against one server even alone; `--test-threads=1` over all ten: 9/10). The suite now uses `Harness::start_owned()` (`ReusePolicy::Never`, one spawned server per test); natively 10/10 in 184 s on 1.8.1. Details in `docs/local-testing.md`.

### 3.3 anykms upstream — done upstream
All of it merged before this was picked up: #37 (AAD v1 byte spec, `docs/aad-format.md`, vectors in `crates/anykms-recipes/tests/vectors/aad_v1.json`, `PLAN.md` §29 triggers), #42 (KeyWrap unwrap conformance case), #43 (custody non-goal, `PLAN.md` §30), #44 (backend-author guide, third-party backends). #33 and #38 are closed; #35 (tracking, third-party KEK) stays open. **anykms 0.3.0** (2026-11-12) ships them; ghinvite uses 0.3 since §3.4, and kekring passes the new `keywrap` conformance case. **anykms 0.4.0** (2026-11-15, the security-review fixes of anykms#46) replaced it: no API change for ghinvite or kekring; the envelope now passes `KeyVersionUnusable` and `Unauthenticated` from the unwrap through unchanged, which ghinvite already handles as its own kind. AAD v1 vectors unchanged; conformance 23 passed, 489 n/a.

### 3.4 kekring redesign + ghinvite switch — done (2026-11-15, PR #125, merged)
anykms 0.3.0 from crates.io; `libs/kekring` rebuilt per the #35 amendment; ghinvite switched in one go. Wire formats (spec and decisions: `libs/kekring/docs/kekring-format.md`):

- **Labels** `kekring/v1/{salt,kek,kid,fp}` (HKDF-SHA256, info `label ‖ u8 len ‖ name ‖ u32be version`).
- **Descriptor** `kekring1;name=NAME;current=N;hwm=H;kV=ROOT43.KID16[;…]`, fixed order, generations ascending (canonical), name `[a-z0-9][a-z0-9_-]{0,63}`, 1–64 generations (worst case 4785 bytes), one trailing `\n`, kids checked at parse.
- **Blob** `krw1 ‖ u32be version ‖ kid[8] ‖ AES-256-KW(DEK)` = 56 bytes; unwrap failures `DecryptFailed` (+`VersionAboveHighWaterMark` / `KeyIdMismatch`), `KeyVersionUnusable` (+`GenerationRemoved`) only below the HWM, no retry hints.
- **AAD**: anykms canonical AAD v1, checked against upstream's `aad_v1.json`; purposes are const-checked `impl Purpose` (`compile_fail,E0080` doctests).

API: `Keyring` (+builder, `to_descriptor`, `fingerprint`), `KeyringEnvelope` (`encrypt`/`decrypt`/`rewrap_current`=`rewrap_local`/`rewrap_verified`/`reencrypt`/`needs_rewrap` + `*_sync`), `Kek` (`KeyWrap + Describe + Semantics`, no `Rewrap`), `KekringProvider` (`kekring:<name>`), `validate_transition`, `Code::of`. The `descriptor` example replaces the `akr` CLI. Vectors: `scripts/kekring-vectors.mjs --check` (Node/OpenSSL, byte-exact). anykms 0.3 conformance (`libs/kekring/conformance`, own workspace because the suite pulls in aws-lc-rs): 23 passed incl. `keywrap::authentication_failures`, 455 declared n/a, `ANYKMS_FAIL_ON_SKIP=1`. Fuzz targets `libs/kekring/fuzz` (`descriptor`, `blob`): build on stable; no nightly/cargo-fuzz here, so only an uninstrumented smoke run (~41M/~28M execs, no failure). New CI job `kekring`.

ghinvite: purposes in `ghinvite_core::purpose`; rollout skew via `kekring::Code::of(&e) == Some(VersionAboveHighWaterMark)`; a wrong root is now `KeyIdMismatch` (caller-class: no alert, record unreadable but untouched) instead of a 500 + alert; fixture descriptors and the four `ring()` helpers re-derived; `scripts/check-keyring-features.sh` (raw-kek guard) removed; docs (`deploy.md` session keyring, README, ADR 0002, verification records, specs banners) updated.

**Gates on the branch (this machine):** fmt and clippy (`-D warnings`) clean; `cargo test --workspace --locked` **1331 passed, 0 failed, 9 ignored** (run with `CARGO_PROFILE_DEV_DEBUG=0` because full debuginfo test binaries no longer fit the disk); `cargo test -p kekring --all-features` 29 passed; wasm32 checks (4 crates + `kekring --features js`) pass; workerd `test:startup`, `smoke`, `pending-signin`, `email-vault`, `email-review`, `signin-cutover`, `domain-signin`, `operational-signals`, `decision-queue`, `request-history`, `app-routes`, `test:admission` (26), `test:org-webhooks`, `test:email-delivery` pass. Not run: `test:production` (needs `worker-build`).

**Open follow-ups:** the external review of the three one-way formats (descriptor, blob, labels/AAD) is still outstanding and gates launch; real fuzzing with cargo-fuzz on nightly; crate/type naming is still a placeholder (labels would not change); push the branch and let CI run the new `kekring` job.

### 3.5 Toolkit hardening (tier (a), `libs/octoauth`) — done (2026-11-15, PR #126)
- **octotoken**: `DynInstallationTokenProvider` (boxed `TokenFuture`, blanket-implemented for every provider; `dyn` values implement `InstallationTokenProvider` again, so `with_token` takes them) with marker `DynSendSync`; opt-in `send-wrapper` feature (futures `Send` on wasm32 via `SendWrapper`); `ProviderRegistry` (`const fn new`, `provider(&App) -> AppTokenProvider`, keyed by App ID, first `App` kept).
- **tracing** feature on octoauth (`installation_token_created`/`_failed`, `clock_skew_corrected`, `app_credential_signed`) and octotoken (`token_created`, `token_replaced`, `token_failed`; enables octoauth's): IDs, token IDs (digests), codes and seconds only; tests capture every event and check for secrets.
- `LICENSE-MIT`/`LICENSE-APACHE` (anykms's texts; root copies, symlinked per crate), a `CHANGELOG.md` per crate; `error-codes.txt` (35 codes) diffed against the sources' `"gh:…"` literals by `crates/octoauth/tests/error_codes.rs` (also checks ER-01's grammar).
- **ghinvite**: `TokenSource` is a re-export of `octotoken::DynInstallationTokenProvider` (adapter deleted); on wasm32 one `ProviderRegistry` per slot replaces the `thread_local` cache (same `(slot, App ID)` key). `dependency_boundary.rs` forbids `rotcred-*` only in the `ghinvite-*` and workspace manifests (key, `package =` rename, dotted table; unit-tested predicate), `gh-toolkit` still in the whole lockfile.
- CI `toolkit` job (and `dagger check ghinvite:toolkit`): clippy and tests with `--all-features`, plus an `--all-features` wasm32 check.

**Gates (this machine):** toolkit fmt, clippy `--all-features -D warnings` clean, `cargo test --workspace --all-features --locked` **156 passed** (151 with default features; was 143), wasm32 checks with and without `--all-features` pass; root fmt and clippy clean, `cargo test --workspace --locked` **1333 passed, 0 failed, 9 ignored** (+2 boundary tests), wasm32 checks of `ghinvite-web-worker` and `ghinvite-workflows-worker` pass. ghinvite does not enable the `tracing` feature yet.

### 3.6 Dependency pins to revisit
- `restate-sdk`: when a release containing #141 ships (0.13.1+ or 0.14), drop the `[patch.crates-io]`; watch for #132's breaking `TerminalErrorExt` change when crossing that boundary.
- `restate-e2e-harness` 0.4 is fine; `now_named_with` could still be upstreamed to `sagikazarmark/restate-toolkit` to delete the inlined copy (optional).
- Checked 2026-11-15: restate-sdk's latest release is still 0.13.0, and upstream `main` is only #132 (breaking) and #142 past the pinned `8a39311`. Nothing to do yet.
- **Dependabot:** GitHub reports 30 vulnerabilities on `main` (13 high) and 204 on `specs.orig`. They are untriaged because the `gh` token cannot read the alerts API (403).

### 3.6a Known flakes and follow-ups found while landing
- **#127:** the `request-history` Playwright fixture server sometimes starts returning 500s mid-run in CI. Seen once (PR #125's first run). It passed on rerun and 6/6 locally. Step 1: give the browser fixture servers a tracing subscriber.
- `bridge_startup` never-listen check: a TCP self-connect on the reserved ephemeral port made `connect()` succeed with no listener (once, on PR #126). Fixed by ignoring self-connections.
- Restate SDK debug-build panic (integer underflow, `endpoint/mod.rs:256`) on a Restate-signed request to a path with fewer than three segments. It poisons that Worker instance. Release builds are unaffected. Worth reporting upstream.
- Throughput: about 45 Restate↔Worker round trips per admission on one link, and the Worker rebuilds the whole endpoint on every request. Fixes: build the endpoint once per instance, and use fewer rounds (needs deployment versioning). See §3.1.
- **`repository_protocol` write-401 regression (fixed 2026-11-16):** three tests failed on main (`unauthorized_write_releases_its_generation_and_remints_once`: the remint saw fence `(1,0)`, not the released `(1,1)`; `two_unauthorized_writes_…`: 3 write mints before the recheck, not 2; `write_mint_422_…`: 2 refresh mints, not 1). Not #126: they fail identically at 1fc3ec3, where the file was added, so the cause came with the gh-toolkit → octoauth migration imported there. `GithubApp::write_once` `replace`d the token on a 401 (a mint before the fence release, and one even after the last 401) and `refresh` was `replace(token())` (two mints when nothing was cached) per scope; P1 spec §9.4/§10.2 says `call` invalidates, the caller releases then `ensure`s, and the recheck is `invalidate_installation` + one `ensure_fresh`. Fix: octotoken `invalidate`/`invalidate_installation` (creates nothing); `write_once` invalidates, `refresh` invalidates the installation and creates one token, the recheck refreshes the write scope only. The suite was never in CI; it and the other integration suites CI skipped (`installation_authority`, `invite_budget`, `journal_steps`, `link_capability`, `request_control`, `settlement_evidence`, `sweep_schedule`) now run in the Restate gate (timeout 30 → 40 min).

### 3.7 Then the product gates
**Blocked on an operator (checked 2026-11-15):** this machine has no Cloudflare, Restate Cloud or GitHub App credentials, and these gates are live deployments. Per the planning ledger (`specs.orig/plans/implementation-status.md`, stale and deliberately not imported), every live/deployed gate is unverified: M0/M1 acceptance against a real GitHub App installation, remote D1, Restate Cloud, the operator checklist in `docs/deploy.md`. The P1 spec and build plan in `docs/specs/` define them.

### 3.8 GitHub emulator (planned 2026-11-15)
Plan: `docs/specs/github-emulator.md` (`octoemu`, tasks GE-01…GE-10): a stateful GitHub App emulator modelled on aws/aws-emulator, kept honest by one fidelity suite run against both it and real GitHub, replacing `stub.rs` and the inline fakes in `tests/worker/*.mjs`. Plan accepted 2026-11-15 (§7): `octoemu` at `libs/octoauth/crates/octoemu` (lifts the §4 deferral for the emulator only), vercel-labs/emulate as reference only, ghinvite's routes first. GE-01 (crate skeleton, #131), GE-02 (App auth: JWTs, stateless installation tokens, suspension, per-route permission checks, #132) and GE-03 (repo surface: collaborators, permission, invitations with expiry and the daily limit, users, avatars, admin `act` accept/decline, a `member` event outbox; ghinvite-github's endpoint tests run against it, #133) and GE-04 (webhook delivery with GitHub's headers and HMAC signatures, recorded attempts, the deliveries API with redelivery, `installation` / `installation_repositories` events from admin actions; ghinvite-web's webhook intake test is fed by it, #134) and GE-05 (OAuth web flow: authorize page with auto-consent, PKCE, codes, `ghu_`/`ghr_` tokens and refresh, `/user`, `/user/emails`, `/applications` revoke, the invitee's `/user/repository_invitations` routes, admin-minted user tokens and PATs; octologin's and ghinvite-web's sign-in run against it, #135) and GE-06 (org surface: memberships, organization invitations by user or email with teams, 7-day expiry into the failed list, the 50/500 budget, seats, teams and team memberships with repository grants, `PATCH /user/memberships/orgs` accept, `organization` / `membership` events; the JS org drivers' GitHub-side scenarios run over ghinvite-github's org client, #136) and GE-07 (fault rules: respond, delay, hold, apply-then-lose, close, reset, matched by method, path pattern, credential, body and nth/times, marked in the call log; the temporary stub-compatibility shim under `/__octoemu/compat/` with the `compat` seed; `ghinvite_e2e::Harness::start` serves the emulator, and every Restate workflow suite that used the stub runs on it, #137) done; next GE-08/GE-09 (GE-09 also moves the suites that keep their own scenario fakes, the JS drivers and the native D1 bridge off the stub, then deletes `stub.rs` and the shim). Open: live-target credentials (needed from GE-08).

## 4. Deferred by decision (do not start without asking)
- **P2 Dependaboard prerequisites** (tier (b) toolkit gaps): GitHub `rotcred_core::Refresher` + `user_token_context` next to `octologin` (the deleted 303-line reference is at `specs.orig` git `a7c4d86^:implementation/gh-toolkit/crates/gh-toolkit/src/signin/rotating.rs`), acting-grant exchange with `Acting` typestate, `check_token`/`delete_grant`, `list_user_installations`, GraphQL, extraction of ghinvite-github's generic transport/REST/pagination/credential/error/mock plumbing into the toolkit, rotcred's two Node lockfile fixes and hosted CI.
- Durable Object executor for rotcred; `anykms-keyring` graduation upstream (after external review of the formats).
- Clerk, P6 Klassroom, everything else in `specs.orig/PLAN.md` §9.

## 5. Quick reference

```bash
export PATH="$HOME/.cargo/bin:/home/laborant/ghinvite/specs.orig/implementation/ghinvite/.devenv/profile/bin:$PATH"
cargo fmt --all --check && cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked                       # 1333 / 0 / 9 (since §3.5)
for p in ghinvite-web-worker ghinvite-workflows-worker ghinvite-ui ghinvite-island; do cargo check --locked -p $p --target wasm32-unknown-unknown; done
cargo check --locked -p kekring --features js --target wasm32-unknown-unknown
node libs/kekring/scripts/kekring-vectors.mjs --check
ANYKMS_FAIL_ON_SKIP=1 cargo test --locked --manifest-path libs/kekring/conformance/Cargo.toml --target-dir target
cargo check --locked --manifest-path libs/kekring/fuzz/Cargo.toml --target-dir target
(cd libs/octoauth && cargo test --workspace --all-features --locked && cargo clippy --workspace --all-targets --all-features --locked -- -D warnings)
RESTATE_SERVER_BIN=/tmp/opencode/restate-server-1.8.1 cargo test --locked -p ghinvite-workflows --features integration --test authoritative_admission -- --test-threads=2
bash scripts/test-restate.sh retained_delivery        # Docker
(cd tests/worker && npm ci && npm run test:startup && npm run test:admission)
```

Issue tracker: GitHub Issues on `sagikazarmark/ghinvite2.orig` (`AGENTS.md`, `docs/agents/`). Bisect logs and server binaries from this session: `/tmp/opencode/` (ephemeral).
