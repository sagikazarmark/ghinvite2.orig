# 0010. Host Restate service endpoints on Workers with a carried SDK fork; keep a native fallback host

- **Status:** Proposed. This is a Gate-1 decision the user hasn't made yet. The recommendation below is the planning default until the user confirms or changes it.
- **Date:** 2026-10-15

## Context

- **Not officially supported.** Restate documents only the TypeScript SDK on Cloudflare Workers. Upstream's position is "ship it as a community library first", and the author did: `sagikazarmark/restate-worker` (`research/restate.md` Q1).
- **A patch is required.** Upstream `restate-sdk` 0.12.1 calls `std::time::{SystemTime, Instant}::now()` in `ctx.sleep`, delayed `send` and `ctx.run`, which abort on `wasm32-unknown-unknown`. The fix (PR restatedev/sdk-rust#128, open since 2026-09-03) has a lukewarm maintainer response and three unanswered pings. Upstreaming soon is unlikely (R-ALL-restate-fork).
- **Proven locally only.** The PoC runs on fork rev `e882e9e` (SDK 0.12.0) in `RequestResponse` mode, verified in workerd and Miniflare against real Restate 1.7.9: 7 round trips and ~35 KB max request for one admission. **Production on Restate Cloud is unverified** (PoC #61). SP-16 is "the biggest unverified platform claim" (R-ALL-restate-workers; `analysis/poc-ghinvite.md` §2, §8; `analysis/feasibility.md` §3).
- **Request/response costs.** Every await on something not yet journaled suspends, and each resume re-POSTs and replays the full journal. CPU per resume grows with journal length (R-ALL-rr-mode; `research/restate.md` Q1). Workers Paid limits are generous: 30 s–5 min CPU, 10k subrequests, 128 MB.
- **No fixed egress.** Workers have no fixed egress IP, so orgs with Enterprise IP allow lists block GitHub calls unless a fixed-IP proxy is used (R-ALL-org-policy).
- **The fallback already exists.** The PoC keeps native `*-server` builds of the same handlers (`ghinvite-workflows-server`) for dev and tests (`analysis/poc-ghinvite.md` §2).
- **Hosting doesn't decide feasibility.** By the agreed definition, hosting never decides a verdict (`PLAN.md` §7). This ADR is about cost, operations and preference (Workers-first).

## Decision (recommended)

1. **Workers-first.** Restate service endpoints run on Cloudflare Workers through `restate-worker`, using `ProtocolMode::RequestResponse`, against Restate Cloud.
2. **Carry the SDK fork.**
   - Keep `sagikazarmark/restate-sdk-rust` as the fork, consumed via `[patch.crates-io]` (as in the PoC).
   - Keep one branch per upstream release, starting with a rebase onto **0.12.1** (`PLAN.md` §9).
   - Fork CI runs the wasm32 checks from PR #128 and a smoke test that runs `ctx.run`, `ctx.sleep` and `send_after` under workerd.
   - Budget one rebase per SDK release.
   - Keep PR #128 open upstream and drop the fork when it merges.
   - SD-02 (pin the exact patch scope) runs with the first rebase.
3. **Native fallback host, always buildable.** Every Restate service crate keeps a native binary target (`*-server`) built in CI from the same handler code. The concrete fallback platform (a container host) is chosen when it's first needed. It is **not** chosen now.
4. **Switch criteria** (per service). Move a service's endpoint to the native host if SP-16 or production shows any of these:
   - CPU or subrequest limit errors that splitting handlers doesn't fix
   - signature or routing failures with Restate Cloud
   - App-JWT signing at or above 10 ms CPU per mint with both pure `rsa` and WebCrypto
   - a replay-cost trend that would breach limits at expected journal lengths

   The web Worker stays on Workers in all cases.
5. **Design rules on either host:**
   - Keep journals short per invocation: small exclusive VO handlers plus delayed sends, following PoC ADR 0009.
   - Use lazy state where reads are sparse.
   - Pass large values by claim-check (C9).
   - No secrets in the journal (ADR 0003).
   - Add an external-write fence for GitHub writes (R-ALL-github-write-fence).
6. **Fixed-egress proxy only on demand.** Add one only if orgs with IP allow lists become a target. Until then, document the limitation in onboarding.
7. **SP-16 runs early** in P1's build plan, before features that depend on deployed behaviour.

### Alternatives considered

| Option | For | Against |
|---|---|---|
| **A. Workers + carried fork + native fallback (recommended)** | Matches the stack preference. Proven locally by the PoC. One codebase for both hosts. The fallback is cheap to keep | Ongoing fork maintenance. Production unproven until SP-16. Replay cost in request/response mode |
| B. Native host only for Restate endpoints (container) | Official SDK, no fork, bidi streaming, no Workers CPU limits | A second platform to operate from day one. Gives up the Workers-first preference with no failure that forces it |
| C. TypeScript Restate services on Workers (officially supported) | Supported path. Journal codec available | Breaks "Rust everywhere". Rewrites the PoC's Rust handlers |
| D. Upstream SDK on Workers without the fork | No fork | Doesn't work: `ctx.run`, `sleep` and delayed sends abort on wasm32 (`research/restate.md` Q1) |
| E. Durable Objects instead of Restate on Workers | Native to Cloudflare | Gives up durable execution and journaling, which the PoC's correctness model is built on (PoC ADR 0003, 0004, 0009) |

## Consequences

- The fork is a standing maintenance item, owned in the restate-worker / SDK-fork repos (`PLAN.md` §9, "Work in existing repos").
- Heavy byte work (`restate-ffmpeg`, LiveKit egress, `restate-pyannote-audio`) stays on native hosts anyway (`analysis/feasibility.md` §2.3, §4). A native Restate host therefore exists once P6 or P7 starts, which lowers the cost of the fallback for P1 and P2.
- Each SDK upgrade costs a rebase plus a run of the fork CI.
- Deployments must keep old Worker versions alive while invocations are pinned to them (PoC `docs/deploy.md`, via `research/restate.md` Q1).
- **UNVERIFIED** until SP-16:
  - remote round trips and CPU per resume
  - whether each `ctx.run` suspends in request/response mode (SD-01)
  - the ~11-min abort for a long single `ctx.run` without journal entries in request/response mode (`research/restate.md` Q1)

## Evidence

- `research/restate.md` Q1 (support status, patch need, request/response suspension, limits), "Design implications → Platform"
- `analysis/poc-ghinvite.md` §2 (fork, protocol mode, measurements, deploy status), §6.2, §7 (platform row), §9 Q2, Q9
- `analysis/feasibility.md` §3 (platform rows), §2.1 required change 8, §6 Q6
- `analysis/risks.md`: R-ALL-restate-workers, R-ALL-restate-fork, R-ALL-rr-mode, R-ALL-org-policy, R-ALL-github-write-fence
- `analysis/spikes-pending.md`: SP-16, SD-01, SD-02, SD-09
