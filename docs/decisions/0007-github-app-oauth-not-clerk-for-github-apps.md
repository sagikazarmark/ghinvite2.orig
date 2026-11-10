# 0007. GitHub products sign in with their own GitHub OAuth, not Clerk; sign-in is separate from the acting grant

- **Status:** Proposed. This is a Gate-1 decision the user hasn't made yet. The recommendation below is the planning default until the user confirms or changes it.
- **Date:** 2026-10-15
- **Scope:** P1 and P2, and later P3 if it gets a web UI. P6 is out of scope and keeps Clerk.

## Context

- **Clerk can't supply GitHub App user tokens.** It documents GitHub *OAuth Apps* only, refreshes on read, and never exposes the refresh token, so there is no rotation hook. R-ALL-clerk-github-token is **resolved-infeasible** (`research/klassroom-livekit-and-auth.md` Q8d). P2's acting grant must come from its own GitHub App OAuth, held in L1 (ADR 0002).
- **Clerk does work on Workers** as an identity layer: dioxus-clerk has a `worker` feature, and v2 JWTs verify on wasm. The caveats (R-ALL-clerk-workers, `kls` Q8a–c, SP-22):
  - `azp` must be enforced
  - no server functions on Workers
  - custom roles cost $100/mo
  - orgs cap at 20 members
- **The PoC uses GitHub App OAuth directly** and never adopted Clerk. Admin authority is derived from GitHub: an org owner via `GET /user/memberships/orgs/{login}` with the session's user token, cached 60 s (`analysis/poc-ghinvite.md` §2, §7 C6).
- **The PoC session depends on the user token.** The session stores it for 30 days, with no refresh. If token expiry is on, admin checks break after 8 h (PoC §6.9).
- **Ten-token cap.** GitHub documents at most 10 tokens per user/app/scope for OAuth apps. The 11th revokes an older one. A third-party report says this also applies to GitHub App user tokens, where every web-flow sign-in mints one (**UNVERIFIED** for GitHub Apps → SP-14). If it applies, repeated sign-ins through the *acting* app can revoke the long-lived token that L1 holds. The change is the same either way: separate sign-in from the acting grant (R-C5-token-cap; `research/github-app-tokens.md` Q3, "Design implications → C2" item 7).
- **What each product needs from the user:**
  - P1 needs identity, verified emails (one-shot `GET /user/emails`, "Email addresses: read" account permission) and an authority check for link creators. It stores **no** long-lived user token (`analysis/feasibility.md` §5 correction 9).
  - P2 needs a long-lived acting grant for approvals, merges and comments.
- **P1 has two acting apps** (ADR 0008, Proposed). Signing in through either one would be arbitrary.

## Decision (recommended)

1. **No Clerk in P1 or P2.** Users sign in with GitHub through the product's own OAuth flow. Clerk stays the identity layer for P6, and is reconsidered for P1/P2 only if non-admin RBAC or delegation (P1-F24, *unsure*) is pulled into scope. In that case Clerk sits on top as identity only (with `azp` enforced and roles in D1), never as the source of GitHub tokens.
2. **A dedicated sign-in GitHub App per product.** It requests **no repository or organization permissions**. P1's also requests the "Email addresses: read" account permission. It is never installed on orgs. Its user token is used during the OAuth callback only:
   - read `GET /user` (numeric ID and login)
   - for P1 requesters, `GET /user/emails` for the domain check

   Then it is **discarded**. The session stores the GitHub numeric user ID and login, not the token.
3. **The session doesn't depend on a user token.** Authority checks for console actions use the **installation tokens of the acting apps**:
   - an org owner check via `GET /orgs/{org}/memberships/{username}` with the org app (Members permission)
   - for repo-only installations, a repo-admin check via `GET /repos/{o}/{r}/collaborators/{username}/permission` with the repo app

   The required permission for the repo-app check is **UNVERIFIED** (expected: Metadata read). Whether "Account Admin" means org owner or repo admin for repo-only installations is a C6 question for the P1 spec.
4. **The acting grant (P2) is separate.** P2's acting app (the merger app) runs its authorize flow **once at connect time and again only on explicit re-auth** (lost grant, revocation). Its tokens go straight into L1. Sign-ins never run the acting app's web flow. When a user disconnects, call `DELETE /applications/{client_id}/grant`.
5. **SP-14 still runs** (moved into GitHub session 1 by `plans/P1-build-plan.md` §4; `analysis/spikes-pending.md` §5). Either outcome keeps this design. A "pass" (no cap for GitHub Apps) only lowers the urgency.

### Alternatives considered

| Option | For | Against |
|---|---|---|
| **A. Own sign-in GitHub App + separate acting grant (recommended)** | No vendor. Proven by the PoC's OAuth code. Least privilege for random requesters (the sign-in app holds no repo or org permissions). The sign-in token count can't touch L1's grant. One sign-in across P1's two acting apps | A third GitHub App registration for P1 (sign-in, org, repo). Authority checks move from user tokens to installation tokens, which is new work over the PoC. **UNVERIFIED:** that a GitHub App with no repo or org permissions can complete the web flow and read `/user` and `/user/emails` (SP-19a, defined in P1 spec §16.1) |
| B. Clerk for sign-in (GitHub social login via Clerk's OAuth App) + own GitHub App acting grant | Shared auth across P1, P2 and P6. Clerk UI and session management for free. Clerk's OAuth App is a separate app, so it also avoids the token cap | An extra vendor and cost. The Clerk user must be linked to the GitHub numeric ID of the acting grant (the two are separate GitHub authorizations). `azp` enforcement. P1 email check: Clerk's GitHub token comes from an OAuth App with its own scopes, and reading it through Clerk is SP-17 territory (optional) |
| C. One app for sign-in and acting (the PoC today) | Fewest registrations | Every sign-in mints a token on the acting app (R-C5-token-cap). Requesters authorize an app with Members or Administration write. The session relies on an expiring token (PoC §6.9). Ambiguous with two acting apps |
| D. Plain GitHub OAuth App for sign-in (`read:user`, `user:email`) | Well-documented scopes. Separate from the acting grant | Non-expiring tokens to manage (we discard them anyway). A different app type from the rest of the stack. Org OAuth-app restrictions don't matter for `/user` endpoints, but they add support confusion. An acceptable fallback if the UNVERIFIED point in A fails |

## Consequences

- P1 spec: the PoC's `/login` and `/oauth/callback` move to the sign-in app. The `RequireConsoleAdminOf` extractor (PoC `middleware/auth.rs`) is reimplemented on installation tokens. The authority cache stays (P1 spec §7.6 raises it from 60 s to 5 min).
- Onboarding shows up to three GitHub screens (sign-in authorization, plus the install of the org and/or repo app). The P1 spec must sequence them.
- P2 has two authorizations: sign-in, then "connect" (the acting grant). The UI must explain the second one.
- C5 / L2 must support several GitHub Apps per product (sign-in plus the acting apps). P2 needs this anyway (viewer and merger apps).
- If P1-F24 (delegation to non-admins) is later wanted, revisit Clerk (option B) or build D1 roles (C6).
- Risks: R-ALL-clerk-github-token stays resolved-infeasible (no SP-17 needed). R-C5-token-cap is mitigated by design and SP-14 confirms it. R-ALL-clerk-workers matters for P6 only.

## Evidence

- `research/klassroom-livekit-and-auth.md` Q8a–d (Clerk on Workers; GitHub token through Clerk)
- `research/github-app-tokens.md` Q3 (ten-token cap, grant revocation), Q7 (authorization vs installation screens), "Design implications → C2" items 6–7
- `research/github-invitations.md` §6 (verified emails), §10 (email permission sits on the login app)
- `analysis/poc-ghinvite.md` §2 (Auth row), §6.9 (session token, no refresh), §7 (C6), §9 Q3–Q4
- `analysis/feasibility.md` §2.1 required change 7 and 12, §2.2 F11, §5 corrections 5 and 9, §6 Q2
- `analysis/risks.md`: R-ALL-clerk-github-token, R-ALL-clerk-workers, R-C5-token-cap, R-P1-email-domain
- `analysis/spikes-pending.md`: SP-14, SP-17, SP-19, SP-22
