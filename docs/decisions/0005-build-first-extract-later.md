# 0005. Build P1 first, extract shared libraries later

- **Status:** Accepted (D5, decided by the user in conversation; `PLAN.md` §9)
- **Date:** 2026-10-15

## Context

- `PLAN.md` §7 F4 planned to target and spec shared components (C1–C5) before the projects. The expected shortlist was C1, C2, C3 and C5.
- P1 is priority 1. A substantial PoC exists, with `ghinvite-github` (JWT, token cache, transport, pagination, redaction, OAuth) as a C1/C5 library in disguise. It is about 5.5 kLOC and depends on `ghinvite-core` only for `Permission` (`analysis/poc-ghinvite.md` §7).
- P1 stores no user tokens. Its email check is a one-shot `GET /user/emails` with the in-session token (`analysis/feasibility.md` §5 correction 9; R-P1-email-domain). So P1 can't validate C2.
- P2 needs both the GitHub toolkit and C2 (`analysis/feasibility.md` §2.2).
- Components designed from one consumer tend to fit that consumer only (`PLAN.md` §7 F4 rule of thumb: don't design components that only one project uses).

## Decision

1. **Build P1 end to end first.** The GitHub toolkit code (App JWT, installation token broker per ADR 0003, transport with rate-limit handling, pagination, redaction, webhook verification, OAuth) lives in **its own domain-free crate** inside the P1 workspace from day one.
2. **Then extract L2**, the GitHub App toolkit (C1 + C5), from P1 into its own repo or crate, check it against P2's needs, and switch P1 over to the extracted L2.
3. **L1** (the rotating-credential library, ADR 0002) is planned and built **from `spikes/restate-refresh-rotation` plus P2's needs**, not from P1.
4. **P2 uses L1 and L2 directly.** The L2 GitHub user-token refresher adapter for L1 is built at that point.
5. **Deferred:** C8, the bulk-operations engine, until P1 and P2 both exist, then extracted from both. C7 audit isn't extracted, because it's too thin.
6. **Work in existing repos** goes ahead as needed. It isn't extraction:
   - anykms: local keyring and AAD convention (ADR 0001), a prerequisite for P1
   - restate-worker / SDK fork: rebase onto 0.12.1, then SP-16 (ADR 0010)
   - restate-toolkit: `Sealed<T>`, only if needed (ADR 0003)

### "Domain-free" rule (testable)

- The toolkit crate depends on no P1 crate. `cargo tree -p <toolkit>` shows no `ghinvite-*`/P1 package. A CI check enforces this.
- Its public API uses no P1 domain types (links, requests, deliveries). GitHub concepts (installation, permission, role, repository identity) are its own types.
- It builds for wasm32 and native, as `ghinvite-github` does today.

## Consequences

- F4 (component targeting) shrinks to choosing L1/L2 boundaries. Gate 2 confirms L1 and L2 instead of a C1–C5 list. The planning sequence is the one in `PLAN.md` §9 (P1 → P1 build → L2 → L1 → P2 → P6).
- The P1 schedule doesn't wait on library design. The cost is an L2 extraction step and a P1 migration later, kept small by the domain-free rule.
- L2's API is first shaped by P1 alone. The extraction step must check it against P2's needs:
  - user-token actions
  - viewer and merger apps
  - search and GraphQL
  - the 500/h content-write governor

  API breaks at that point are expected and accepted.
- C1's cross-invocation cache, multiple App identities and scoped tokens (gaps listed in `analysis/poc-ghinvite.md` §7) are built in P1's toolkit crate as P1 needs them (two apps, ADR 0008), not up front.
- Whether "build P1" means extending the PoC or restarting is ADR 0006 (Proposed).

## Evidence

- `PLAN.md` §7 (F4), §9 D5, "Project map", "Planning sequence"
- `analysis/poc-ghinvite.md` §7 (shared components in disguise; `ghinvite-github` liftability)
- `analysis/feasibility.md` §2.1 (P1 required changes), §2.2 (P2 needs C2 and two identities), §5 correction 9
- `spikes/restate-refresh-rotation/README.md` (L1 source)
- `analysis/risks.md`: R-P1-email-domain, R-C2-refresh, R-P1-poc-extend
