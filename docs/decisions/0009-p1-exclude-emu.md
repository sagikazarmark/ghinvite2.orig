# 0009. P1 excludes Enterprise Managed Users (EMU) enterprises

- **Status:** Proposed. This is a Gate-1 decision the user hasn't made yet. The recommendation below is the planning default until the user confirms or changes it.
- **Date:** 2026-10-15

## Context

- In EMU enterprises, "your IdP provisions new user accounts on GitHub, with access to your enterprise". Members come from SCIM, not invitations. The docs don't say outright that personal accounts can't be invited, so the exclusion rests on **MEDIUM** confidence (R-P1-org-eligibility).
- Other org-eligibility constraints are real but don't need exclusion (R-P1-org-eligibility; `research/github-invitations.md` §1–§2):
  - A paid per-seat org needs a free seat; an invite otherwise fails with 422 (HIGH).
  - A 2FA-required org blocks acceptance until the invitee enables 2FA (verified).
  - Outside collaborators may be restricted by enterprise or org policy (HIGH).
- P1's verdict is "feasible with changes, **for orgs outside EMU enterprises**" (`analysis/feasibility.md` §1, §2.1).
- Supporting EMU would mean integrating with the customer's IdP and SCIM provisioning. That's a different product, not an invite link.

## Decision (recommended)

1. **P1 v1 doesn't support orgs owned by an EMU enterprise**, as invitation *targets*. This is documented on the product's requirements page and in onboarding.
2. **Detection is best-effort:**
   - **UNVERIFIED:** whether the REST API exposes "this org belongs to an EMU enterprise" to an installation token. A desk check in the P1 spec phase decides whether link creation can detect it up front.
   - Regardless of up-front detection, a delivery failure that identifies EMU or managed-user restrictions maps to **Blocked Delivery** with a reason, never a retry loop.
3. **Seats, 2FA and outside-collaborator policy are supported with warnings, not excluded.**
   - They are checked at link creation as far as the API exposes them (seat counts via `GET /orgs/{org}` `plan.seats`/`filled_seats`, to be confirmed in SP-01) and shown on the link page.
   - A delivery 422 maps to Blocked Delivery with the reason.
4. **Revisit trigger:** an EMU customer matters. Then confirm the exclusion against an EMU trial before any design work.

### Alternatives considered

| Option | For | Against |
|---|---|---|
| **A. Exclude EMU (recommended)** | No scope for an IdP/SCIM product. Matches the evidence | Some enterprise orgs can't use P1 at all |
| B. Support EMU through SCIM/IdP integration | Covers managed enterprises | A different product (provisioning, not invitations). Needs customer IdP access. No evidence that links fit EMU |
| C. Verify the EMU fact now, with an EMU trial | Turns MEDIUM into HIGH | Costs an Enterprise trial plus IdP setup (as for SP-15) for a segment that isn't targeted |

## Consequences

- The P1 spec has an "Org eligibility" section with link-creation checks, link-page warnings and Blocked Delivery reasons (`analysis/feasibility.md` §2.1 required change 10).
- **UNVERIFIED, requester side:** EMU *managed user accounts* signing in as requesters may be unable to join orgs or repos outside their enterprise. If so, delivery to them fails as well. The P1 spec should treat this the same way (Blocked Delivery with a reason) and confirm it in the same desk check.
- The kill criteria stay as written: outside EMU enterprises, P1 can't be killed by its open risks (`analysis/risks.md` §4).

## Evidence

- `analysis/risks.md`: R-P1-org-eligibility (EMU MEDIUM, 2FA verified, seats and outside collaborators HIGH), §4 kill criteria
- `analysis/feasibility.md` §1 (P1 verdict), §2.1 (scope exclusion, required change 10), §5 correction 19, §6 Q3
- `research/github-invitations.md` §1 (seats), §2 (outside-collaborator restrictions)
- `analysis/spikes-pending.md`: SP-01 (no-seat case and seat-count fields)
