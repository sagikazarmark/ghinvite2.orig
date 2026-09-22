---
status: accepted
date: 2026-09-22
---

# Delivery and settlement keep distinct prerequisite checks

Delivery, settlement and availability each ask whether an installation can
still reach a repository before acting. The checks look alike but answer
different questions, so each keeps its own. This refines the module structure
behind [ADR 0004](0004-delivery-identity-and-uncertain-results.md) and does not
change its policy.

| Caller | Reads identity from | Installation check | Answers |
|---|---|---|---|
| Delivery (`GithubCreate`, `crates/ghinvite-workflows/src/delivery.rs`) | The input-bound `CreateCommand` | Local active installation only | Blocked Delivery, Throttled Delivery or Delivery Outcome Unknown in the create receipt |
| Settlement (`crates/ghinvite-workflows/src/settlement.rs`, context in `settlement/context.rs`) | The SQL `github_invitations` row | Verified with GitHub, including suspension | "Cannot observe now" (`None`); nothing is settled |
| Availability (`crates/ghinvite-workflows/src/availability.rs`) | The account and link scope | Account installation state | Account-level admission eligibility |

Two pieces are shared because their meaning is identical for every caller:

- Repository access verification
  (`crates/ghinvite-workflows/src/repository_access.rs`): whether an already
  chosen installation reaches a repository of a fixed scope by numeric ID.
  Each caller maps `Verified`, `Unavailable` and `Unread` itself.
- Plan submission (`submit_plan` in `delivery.rs`): sending each planned create
  and recording its submission, one repository at a time. The request
  lifecycle and delivery recovery both use it.

## Rejected: one prerequisites module returning `Ready | Blocked`

A unified check would push Blocked Delivery semantics into settlement and
admission, where "blocked" means nothing: settlement only declines to observe,
and admission rejects or reports unknown availability. It would also invite
delivery to read its identity from SQL, which ADR 0004 forbids: SQL is the read
projection, not create-operation authority, and the retained `CreateCommand`
is.

`crates/ghinvite-workflows/src/github_invitation.rs` looks shallow but is the
`GithubInvitation` Restate object's registration surface: its handler names and
input types are a wire contract with Restate and webhook callers. Do not fold
it into settlement.
