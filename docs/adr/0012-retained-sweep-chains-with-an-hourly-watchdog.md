---
status: accepted
date: 2026-11-24
---

# Retained sweep chains with an hourly watchdog

Issue #86 asked for an operated contract for automatic GitHub invitation
lifecycle reconciliation. The maintainer accepted it on 2026-11-24. This record
covers that contract and the choices made to implement it.

## Contract

- **Runtime.** Restate. Each `SweepSchedule/<app>:<account>` retains one
  self-rescheduling chain: hourly while it tracks deliveries waiting on GitHub
  (Created, AlreadyPending, OutcomeUnknown), daily when idle. Deliveries keep
  registering and unregistering through the existing verified hints. The chain
  (B0-18) already existed; this work adds the parts below to it.
- **Missed runs.** The web Worker's hourly cron (`5 * * * *`) is a watchdog. It
  re-arms, idempotently, any chain that is missing or overdue. A re-armed chain
  catches up with one sweep, not one sweep per missed run.
- **Expiry and disappearance.** An invitation still pending more than 7 days
  after creation settles **Expired** after one more GitHub observation. A known
  invitation that has disappeared settles under the existing evidence rules
  (Accepted, or Declined, meaning "no longer pending"), never Expired. Absence
  never resolves an uncertain create; only positive presence does.
- **Bounds.** One sweep per account at a time (object exclusivity plus the
  schedule token). Each repository walk reads every page, with a page cap per
  run. GitHub throttling defers the run with backoff instead of failing it.
  Overlapping or duplicate triggers are no-ops.
- **Observability.** The daily digest records the oldest overdue schedule and
  the failed sweeps. There is no notification channel; #46 is deferred.
- **Credentials and ownership.** The existing App installation credentials and
  the web's authenticated Restate ingress (#77). No new secrets. The operator
  owns the cron; production enablement belongs to #61.

## Choices

**Where the decision lives.** The web supplies only the account set. It reads
active installations of the repository app, plus the organization app when
organization delivery is on, from its installation projection. It sends them to
`SweepWatchdog/check` in batches of at most 100. The check reads each chain
through `SweepSchedule.status`, a shared handler, so a run holding the lock
cannot block it. It sends a one-way `ensure_scheduled` to any chain that is
missing (never armed) or overdue (due more than 15 minutes ago and not yet
run). `ensure_scheduled` makes the same assessment again under exclusivity. A
missing chain is armed on its usual cadence. An overdue chain gets a new token
and an immediate run, so every older delayed run is inert. A healthy chain is
left unchanged. Onboarding calls the same handler, so the cron and onboarding
cannot fork a chain. A lagging projection can only delay a re-arm by an hour;
it can never create work.

**Chains survive failed fan-outs.** `run` used to propagate a terminal error
from `Reconcile.sweep`, which ended the chain. It now counts the failure and
schedules the successor. The watchdog is the backstop for invocations that never
complete at all.

**Expiry evidence.** GitHub's repository invitation listing has `created_at`
and `expired`. octoemu models both: `expired: true` after 7 days, which its
fidelity table documents as `invitation-expiry`, and the invitation stays listed
afterwards, which octoemu marks as assumed. `Evidence::Listed` now carries the
listing's own `created_at`. The settlement reducer settles Expired when GitHub
flags the invitation expired, or when the observation's `at` is more than
604,800 seconds after the creation time in that same listing. `at` is journaled
before the read, so the observation itself is the "one more observation" after
the deadline. A retained creation time never settles anything without a fresh
listing. Provenance `listed_past_lifetime` records which rule applied. The rule
applies to owned (Created) and tracked foreign (AlreadyPending) invitations
alike. It never applies to OutcomeUnknown, and never to absence.

**Organization invitations are in scope.** GitHub moves an organization
invitation that outlives its 7 days to `failed_invitations`. octoemu models that
with `failed_at` at the expiry and `failed_reason: "Invitation expired"`, both
marked assumed. Before this change the owner treated failed-list presence as "no
change", so an expired organization invitation stayed tracked and was polled
every hour indefinitely. Organization chains already run when organization
delivery is enabled, and the watchdog covers them. `OrganizationObservation::expired`
treats an invitation as expired when this complete observation lists it, pending
or failed, more than 7 days after creation. A failed entry stamped before its
lifetime ended stays unclassified, as before. It also takes precedence when the
same observation lists the invitation as pending too: the pending and failed
lists are separate sequential reads, and an early failure is the more specific
fact, so the invitation is not settled Expired and the next sweep reads again. Team settlements follow the
organization invitation. Email invitations already settle on failed-list
presence and are unchanged.

**Bounds.** A run fans out at most 25 targets. A larger sweep set rotates
through a retained cursor, so every target is walked within ⌈targets/25⌉ runs.
Each walk reads at most 40 pages (`list_*_within`). A longer walk fails like any
other incomplete listing and supplies no evidence. That caps listing reads at
1,000 pages per run (each organization walk reads two listings), plus the
per-absence verification reads.

**Throttling and failure accounting.** A walk that yields no evidence sends a
one-way `SweepSchedule.report` naming its run token. A rate limit counts as
*throttled* and carries GitHub's retry guidance unclamped. The delivery path's
one-hour `throttle::backoff` bound does not apply here, because no lock is held
across the wait; the 24-hour cap below is the only upper bound. Any other
read failure, including the page cap, counts as *failed*. Each run counts at most
once per kind, and a report for any run but the latest is ignored. A throttled
run defers the chain by `min(24 h, max(guidance, 1 h × 2^(n−1)))`, where n is the
number of consecutive throttled runs. If that is later than the next run, the
chain is re-armed at that time. Until the deferral ends, registrations cannot
pull the chain forward. The first run that is not throttled resets n.

**Digest.** Each complete check is recorded in `sweep_digest_days` (migration
0029), one row per UTC day: checks, the most schedules read, the re-arms
summed, the oldest overdue chain, the number of chains with a failed sweep in
the past 24 hours and the latest failure. Seven days are kept. A check that is
incomplete (a refused batch or an unreadable projection) records nothing, logs
`sweep_watchdog_failed` and fails the scheduled event. The blocked-delivery
digest is recorded independently.

**Configuration.** `GH_SWEEP_WATCHDOG_ENABLED` takes the literals `true` and
`false`. If unset, the watchdog is on for development roots and runtime-test
Workers and off in production. `wrangler/web.toml` sets it to `false`
explicitly. The native web server runs the same check in its hourly
maintenance loop.

## Consequences

- New handlers (`SweepSchedule.report`, `SweepWatchdog.check`), new named
  clock reads (`report_time`, `watchdog_time`, and `schedule_time` on every
  `ensure_scheduled`), the changed `run` journal and the new `Evidence::Listed`
  field require ID-21 endpoint pinning or draining for retained journals. Old
  `Listed` evidence without `created_at` still decodes and settles on the flag
  alone.
- The rehearsals are local: real Restate with a journaled fixture clock and
  octoemu (`crates/ghinvite-workflows/tests/sweep_schedule.rs`), and the actual
  scheduled Worker against a recorded ingress (`tests/worker/daily-digest.mjs`).
  They are not live GitHub evidence. GitHub's behaviour for an expired
  invitation that is still listed, and for the organization failed list, is
  still assumed in octoemu's fidelity table.
- Production enablement, the deployed cron and the release-gate record remain
  #61.
