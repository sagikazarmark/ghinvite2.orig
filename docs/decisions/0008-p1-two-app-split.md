# 0008. P1 ships as two acting GitHub Apps: an org app and a repo app with no org permissions

- **Status:** Proposed. This is a Gate-1 decision the user hasn't made yet. The recommendation below is the planning default until the user confirms or changes it.
- **Date:** 2026-10-15

## Context

- The notes make two apps a **must** (P1-F12): "GitHub Invite to Organizations" and "GitHub Invite to Repositories". The author's reason is that "an app can't request optional permissions", so repo-only users would never grant org-level access (`ideas/P1-github-invite.md` F12).
- **Permissions are all-or-nothing per app** (HIGH). No optional-permissions feature exists (MEDIUM) (`research/github-invitations.md` §10).
- **Both apps are owner-install only.** Org Members:write and repo Administration:write each require an org owner. A repo admin can only *request* the install (`GET /app/installation-requests`). So the split **doesn't** let repo admins self-serve (`analysis/feasibility.md` §5 correction 7; `research/github-app-tokens.md` Q7). SP-02 confirms this and captures the install screens; it is an adoption input only.
- **Repo Administration:write is broad.** It also allows `DELETE` and `PATCH /repos/{o}/{r}`.
- **Acceptance webhooks need org permissions.** Observing acceptance through `member` / `organization.member_added` webhooks needs org Members:read, which would defeat the split. Without it, the repo app can only detect acceptance by **polling** repo invitations plus collaborators (R-P1-two-apps, R-P1-acceptance; `research/github-invitations.md` §5).
- **What the split does buy:**
  - An owner who only wants repo links isn't asked for org-wide Members:write, which can invite owners and remove members.
  - The repo app works on personal accounts.
- The PoC has **one** app: Administration RW, Metadata R, plus optional Org Members R and the `member` webhook (`analysis/poc-ghinvite.md` §2).
- L2 must support several apps per product anyway: the sign-in app (ADR 0007) and P2's viewer and merger apps (`analysis/feasibility.md` §2.2 change 5).

## Decision (recommended)

Adopt the split, as specified below.

1. **Org app** ("Invite to Organizations"). Permissions: org Members:write and Metadata:read. Webhooks: `organization` (`member_added`, `member_invited`, `member_removed`) and `membership`. It handles org invitations with role and `team_ids`, team membership PUTs, org invitation cancel, and member removal.
2. **Repo app** ("Invite to Repositories"). Permissions: repo Administration:write and Metadata:read, with **no organization permissions**, so no `member` webhook. It handles repo collaborator invitations, per-repo permission, cancel and removal. Acceptance, decline and expiry are detected **only by the polling reconciler** (the PoC's `Reconcile::daily_run` pattern, scheduled per ADR 0006, PoC #86). Acceptance latency is bounded by the sweep interval, which the P1 spec sets (§7.7: 1 h while anything is tracked, PD-H). The PoC's daily sweep is too slow for the "already used" UX, so the spec should consider hourly or faster for links with pending invites.
3. **The sign-in app** is separate (ADR 0007). P1 therefore registers three GitHub Apps.
4. **Links that span org and repos** (P1-F44) need both apps installed on the account. Link creation checks the installations and their permissions, and refuses with a clear "install the X app" message.
5. **Build order:** the repo app first (it's the PoC's app minus the optional org permission and webhook), then the org app.

### Acceptance checks (testable)

- The repo app's manifest declares zero organization permissions and no org-scoped webhook events.
- An accepted repo invitation is reflected as accepted within one sweep interval, with no webhook delivered.
- A link that targets org + repos can't be created unless both apps are installed on that account.

### Alternatives considered

| Option | For | Against |
|---|---|---|
| **A. Two acting apps, repo app without org permissions (recommended)** | Matches the must-have P1-F12. Least privilege: owners can withhold Members:write. The repo app works on personal accounts. Its multi-app cost is shared with P2 via L2 | Three app registrations including sign-in. Two install flows. Repo acceptance by polling only (latency). Doesn't enable repo-admin self-serve |
| B. One app with Members:write + Administration:write | One install, webhooks for everything, simplest onboarding | Every owner must grant both broad permissions, even for repo-only use, which works against P1-F12. Heavier ask for cautious owners (R-P1-install-consent, an adoption note) |
| C. Two apps, but the repo app also gets org Members:read for `member` webhooks | Webhook-based acceptance for repos | Brings back an org-level permission, which defeats the split's purpose (`research/github-invitations.md` §10) |
| D. Repo app only (the PoC's scope); org links later | Least new work | Drops the org half of P1 (P1-F02, F08, F44, F45, F48), which is the main gap in the PoC |

## Consequences

- C5 / L2 needs multi-app configuration from the start: per-app ID, private keys, webhook secrets and installation records, all keyed by (app, installation).
- Onboarding UX must explain which app to install and why, using SP-02's screenshots (adoption input; `analysis/risks.md` §2.8).
- The reconciler becomes load-bearing for the repo app. Its schedule, paging cost and failure handling belong in the P1 spec (R-P1-acceptance, R-P1-pending-revoke, SP-18).
- If the user picks B, items 1–2 collapse into one app with webhooks, and the polling reconciler stays only for expiry, decline and cancel.

## Evidence

- `ideas/P1-github-invite.md` P1-F12 (must; source quote L29)
- `research/github-invitations.md` §5 (acceptance webhooks need Members:read), §10 (two-app split)
- `research/github-app-tokens.md` Q6, Q7 (install permissions, owner-only install, installation requests)
- `analysis/poc-ghinvite.md` §2 (one app today), §3 (acceptance tracking, reconciler), §8
- `analysis/feasibility.md` §2.1 F12 row and required change 5, §5 correction 7, §6 Q3
- `analysis/risks.md`: R-P1-two-apps, R-P1-acceptance, R-P1-pending-revoke, R-P1-install-consent (§2.8)
- `analysis/spikes-pending.md`: SP-02, SP-18
