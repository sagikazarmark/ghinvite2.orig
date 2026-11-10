# 0006. P1 codebase: extend the ghinvite PoC rather than restart

- **Status:** Proposed. This is a Gate-1 decision the user hasn't made yet. The recommendation below is the planning default until the user confirms or changes it.
- **Date:** 2026-10-15

## Context

- The PoC (`sagikazarmark/ghinvite2.orig`, 593 commits) is a production-grade, **repo-collaborator-only** slice of P1 (`analysis/poc-ghinvite.md` §1–§3):
  - Rust on two Workers, D1, KV and Restate Cloud
  - Dioxus SSR with one island
  - GitHub App OAuth (no Clerk)
  - an audit log
  - an "outcome unknown" model and a D1 attempt fence for GitHub writes
  - a shared D1/SQLx conformance suite and CI gates against real Restate and Miniflare
- About 70% of its effort went into correctness under failure. The architecture was reworked three times. ADR 0009 (link VO, request-owned lifecycle, per-repo `RepositoryDelivery`) is the current shape (PoC §1, §4).
- **Extensibility is resolved-feasible.** The ADR 0009 shape (manifest → per-item VO → receipts) fits org and team deliveries and bulk without a rewrite (R-P1-poc-extend; PoC §7 C8 row). Feasibility doesn't depend on this choice; the schedule does (`analysis/feasibility.md` §6 Q1; PoC §9 Q1).
- **What P1 needs that the PoC lacks** (PoC §3; `analysis/feasibility.md` §2.1 required changes):
  - org links, teams, per-repo permission levels and pause
  - bulk invite as a quota-aware queue
  - two apps (ADR 0008)
  - email-domain check
  - role precedence (the PoC PUTs blindly, a possible downgrade)
  - own expiry with a scheduled reconciler (#86)
  - org eligibility checks
  - link secrecy (a separate ≥128-bit secret; the link ULID is the public code today)
  - ~520-char tokens
  - production deploy (#61)
- **Known PoC liabilities:**
  - The single island cost about 1,000 LOC plus 2,400 LOC of tests (PoC ADR 0001).
  - About 84 duplicated bind lists per storage adapter (PoC ADR 0005).
  - Sessions hold a GitHub user token with no refresh, so admin checks break after 8 h if token expiry is on (PoC §6.9).
  - `docs/adr.bak/` duplicates exist (PoC §9 Q10).

## Decision (recommended)

**Extend the PoC in place as the P1 codebase.** Do these foundation steps before any new feature:

1. **Toolkit crate made domain-free** (ADR 0005): cut `ghinvite-github`'s dependency on `ghinvite-core` (`Permission`) and add the CI dependency check.
2. **Role-precedence fix** on the existing repo path: read the effective `role_name` before every PUT and skip if it's at or above the link's role (`research/github-invitations.md` §3). This fixes a live downgrade risk in the PoC. SP-07 confirms the details.
3. **Domain generalization** in `CONTEXT.md`: the Invitation Link targets an org (role, teams) and/or repos (per-repo permission). The existing vocabulary is kept: guardrails, use, revoke, admission, decision deadline, outcome unknown, blocked/throttled (PoC §5). New delivery kinds are added beside `RepositoryDelivery` under the ADR 0009 pattern.
4. **Platform hygiene:**
   - rebase the SDK fork onto 0.12.1 (ADR 0010)
   - allow 600+ char tokens in columns and redaction (ADR 0003)
   - schedule the reconciler (#86)
   - delete `docs/adr.bak/`
5. **Auth change per ADR 0007** (Proposed): the session no longer depends on a stored user token after sign-in.

**Restart trigger** (the condition under which this default is revisited): the P1 spec finds that org or team delivery, pause, or bulk can't be modelled as new delivery and owner objects under ADR 0009 without changing link admission (`admission.rs`) or the attempt-fence contract (PoC ADR 0004). If that happens, re-plan as a partial rewrite of the affected crates only.

### Alternatives considered

| Option | For | Against |
|---|---|---|
| **A. Extend in place (recommended)** | Keeps the tested correctness machinery (admission, fence, settlement, audit, conformance suites, CI gates), which is exactly what the org half and bulk need. History and ADRs stay traceable. R-P1-poc-extend is resolved-feasible | Carries the PoC's weight: one costly island, duplicated bind lists, three architecture revisions to read through. New contributors face a large codebase |
| B. Restart, using the PoC as reference | Clean naming for org-first links. Can pick a SPA console (PoC ADR 0001's named escape hatch) from the start | Rebuilds the ~70% correctness work and its tests. Re-learns the D1, Workers and Restate quirks (PoC §6). The schedule slips, for no feasibility gain |
| C. New repo, copying selected crates | Lighter start. Lifts `ghinvite-github`, storage and audit | Loses history. Partial copies drift from their tests. Mostly B's cost with A's baggage |

## Consequences

- P1 planning (`PLAN.md` §9 step 1) specs **changes to the PoC**, not a new system. The P1 spec cites PoC files and ADRs as its baseline, and the build plan starts with steps 1–5 above.
- The PoC's choices that differ from the notes need explicit user calls in the P1 spec:
  - revoke doesn't cancel pending invites (PoC ADR 0003 vs notes)
  - guardrails are immutable vs "editable until first request" (P1-Q09)
  - no resend (infeasible anyway; cancel + re-create)
- The P1 console stays SSR + islands unless the bulk-review UI hits ADR 0001's escape-hatch triggers. That call is made in the P1 spec.
- Production rollout (#61) becomes part of P1's build plan, and SP-16 (ADR 0010) runs early.

## Evidence

- `analysis/poc-ghinvite.md` §1–§9 (esp. §3 feature coverage, §4 ADRs, §6 workarounds, §7 liftability, §9 Q1, Q10)
- `analysis/feasibility.md` §2.1 (P1 required changes), §6 Q1
- `analysis/risks.md`: R-P1-poc-extend, R-P1-role-downgrade, R-P1-link-secrecy, R-P1-acceptance
- `research/github-invitations.md` §3 (role precedence)
- `research/link-security-and-gh-terms.md` §1.5 (link secrecy controls)
- `ideas/P1-github-invite.md` (P1-Q09, P1-Q23)
