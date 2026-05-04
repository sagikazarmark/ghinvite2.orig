# ghinvite implementation plans

This directory holds the v1 implementation roadmap, decomposed into 8 plans. Each plan produces working, testable software on its own; later plans depend on earlier ones at the code level (types, traits) but are independently mergeable.

The decomposition was agreed during brainstorming on 2026-05-04. Order is fixed: each plan was deliberately sized so the next one can reach into the previous plan's *implemented* code rather than its *specified* shape, which catches drift early.

## Plan map

| # | Plan | Filename | Status | Produces | Depends on |
|---|------|----------|--------|----------|------------|
| 1 | **Foundations** | `2026-05-04-ghinvite-foundations.md` | Implemented | `crates/domain` (types, IDs, slug, state machines), `crates/audit` (event types), `crates/storage` (trait + sqlx impl), migrations, parameterized test suite | none |
| 2 | **GitHub clients** | `2026-05-04-ghinvite-github-clients.md` | Implemented | `crates/github` — user-OAuth client (oauth2 crate), installation-token client (JWT signing + token cache), HMAC verification helper, mock transport for tests | 1 |
| 3 | **Restate handlers** | `2026-05-04-ghinvite-restate-handlers.md` | Implemented | `crates/restate-svc` — `Installation`, `ShareLink`, `InvitationRequest`, `GithubInvitation`, `Reconcile` services with full workflows + tests | 1, 2 |
| 4 | **Web binary core** | `2026-05-04-ghinvite-web-binary.md` | Implemented | `crates/web` — axum 0.8 app, tower-sessions, OAuth login + install flow, three Dioxus 0.7 SSR layouts (Home / Dashboard / Invitation), Tailwind v4 + DaisyUI v5 build, home page, 501 stubs for Plans 5–6 | 1, 2 |
| 5 | **Dashboard** | `2026-05-04-ghinvite-dashboard.md` | Written | Account dashboard + link CRUD form + revoke + approval queue + settings page; form POSTs routed to Restate via ingress | 4, 3 |
| 6 | **Recipient flow** | _not yet written_ | Pending | `/i/:slug` landing + request submission + pending page + webhook receiver wiring | 4, 3 |
| 7 | **Workers + D1 deployment** | _not yet written_ | Pending | `D1Storage` impl, two `wrangler.toml`s (web + restate-svc), secrets management, end-to-end deploy run | 5, 6 |
| 8 | **E2E + CI** | _not yet written_ | Pending | Restate-in-Docker integration test, GitHub-stub server, GitHub Actions workflow | 7 |

## Why one-at-a-time

Plans 2–8 will reach into Plan 1's actual code (trait shapes, types, error variants, etc.). Writing them now against the spec/Plan-1 *text* would mean discovering drift after Plan 1 ships and rewriting. Writing each plan against the *implemented* prior is meant to catch type/API mismatches at the boundary.

If you'd rather speculate further out — for example to estimate total effort, or to spot architectural problems early — write Plans 2–8 against the spec but treat them as drafts that need re-validation after the prior plan ships.

## v1 scope reminders

These follow the spec (see `../specs/2026-05-04-ghinvite-v1-design.md`):

- **In v1**: GitHub App login + install (org and personal account), repo-collaborator invites with permission level, share-link creation with expiration / max-uses / approval-required, recipient flow with optional justification, admin approval queue, webhook reconciliation primary + daily polling failsafe, audit *capture* (no UI), 5 terminal states.
- **v1.1**: audit log UI + CSV, auto-refresh recipient pending page.
- **v2+**: notifications, cascade revoke, link editing, quorum approval, team/org membership, app-internal admin roles, GDPR redaction tooling.

## Where to look for context

- **The spec** (`docs/superpowers/specs/2026-05-04-ghinvite-v1-design.md`) — locked design decisions, data model, state machines, auth flows, security checklist.
- **The autoplan review** (`docs/superpowers/reviews/2026-05-04-autoplan-foundations.md`) — full CEO / Eng / DX subagent outputs, the original arguments behind the amendments folded into Plan 1.
- **Plan 1's audit trail** (footer of `2026-05-04-ghinvite-foundations.md`) — one-line-per-decision summary of every /autoplan choice (challenges rejected, taste decisions accepted, auto-decided fixes).
