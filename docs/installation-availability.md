# Installation availability (#60)

## Command ownership

`Installation/<numeric installation ID>` validates its key and retains the numeric
account binding and permanent uninstall tombstone. An uninstall received before
onboarding prevents that identity from ever being onboarded. Repository-change
payloads retain their old wire shape for compatibility, but their selection is
ignored: webhooks and setup returns request a refresh.

`AccountInstallationV1/<numeric account ID>` serializes onboarding, repository
refresh, uninstall, status, and admission observations across replacement
installations. It adopts an existing active installation row once, then retains
current identity and observations in Restate. `InstallationProjectionV1` receives
ordered durable sends for SQL/audit writes on a separate account-keyed object,
so persistence retries cannot hold availability, approval, or replay hostage.
Deploy these services together and
drain old installation invocations before upgrading: their journal sequence has
changed. Keep obsolete endpoints isolated as in the admission cutover procedure.

Onboarding verifies the App-authenticated GitHub installation's numeric account
ID. A replacement can supersede an existing identity only after GitHub confirms
the old installation is absent. Neither account login, caller event time, nor
numeric installation ordering chooses the replacement. Retired identities remain
tombstoned. Late old repository/uninstall events cannot mutate the replacement.

Repository refresh runs inside account serialization, follows all pages, and
retains exact numeric repository IDs even for an installation configured for all
repositories. Inconsistent/incomplete pagination is unknown. Duplicate events
repeat the current observation rather than applying old deltas. Refreshes caused
by installation events update the SQL/D1 selected-repository view used by existing
delivery prerequisites; unavailable/unknown observations project an empty set.
The public account `status` distinguishes that uncertainty from known absence.
Unavailable/unknown observations schedule a durable 60-second recheck until
restoration or uninstall. A successful observation also projects restored scope,
including when triggered by admission, so blocked delivery can recover.

Refresh work is acknowledged by its durable send before it runs, so an account
object whose first adoption cannot read installation storage retains that work
as a durable continuation with bounded backoff (one second, doubling up to the
recheck cadence) instead of failing it away (#66). This is reachable for an
installation command that already retains its numeric account binding while the
account object has never adopted. One continuation is retained per identity:
duplicate events join the scheduled one and only it retires its own slot, and a
continuation for an identity the account does not adopt — superseded or retired
— observes nothing. The periodic recheck keeps its own slot across the outage.

## Admission and approval

The link command compares retained operation input/outcome **before** consulting
availability. A fresh attempt requests an account-keyed observation. Known absent,
suspended, or identity-mismatched installations reject with
`installation_unavailable`; any missing repository in the immutable link scope
rejects with `repository_unavailable`. Both are retained business outcomes.
Transient GitHub failures return 503 without recording a business rejection;
retry the same attempt. Restoration never changes an old rejection: use a fresh
operation for a fresh attempt.

GitHub requests have explicit [30-second network deadlines](network-deadlines.md),
including on Workers. A stalled observation becomes unknown and completes its
exclusive handler, allowing queued status to proceed and durable recheck to run.

Admission observations retain Restate state and durably arrange projection without
waiting for SQL. Initial adoption requires installation storage to be readable;
an adoption failure returns 503 rather than indefinitely holding exclusivity.
Synchronous observation keeps that prompt failure — `status`, `eligibility`, and
`onboard` never wait out an outage; only acknowledged refresh work is retained.
Subsequent link admission and
receipt replay do not depend on SQL projection completion. Revocation, decisions,
and request status do not call availability. Original deadlines, uses, revocation,
scope, dispatch plans, and confirmed delivery history survive uninstall/reinstall.
Available scope is necessary but does not override a link's original guardrails.

The Console's authoritative lifecycle path may resolve historical installations
after uninstall. The same numeric personal-owner or current numeric organization
membership check still authorizes access. Pending requests may be approved;
Console/requester copy explains that unavailable repositories can block delivery.
#56's retained delivery receiver continues available repositories and preserves
blocked/unknown/confirmed effects, using its existing recovery commands and timer.

GitHub observations are not atomic with admission or subsequent GitHub effects.
An external change after an observation may block delivery even after acceptance.
Refresh failure never proves an effect happened, failed, or can safely be repeated.

## Verification

```sh
bash scripts/test-restate.sh installation_availability
cargo test -p ghinvite-web --test console_flow
npm run test:admission --prefix tests/worker
```

The native runtime scenario exercises the public commands with GitHub HTTP stubs:
absence, partial scope, unknown observations, replay, original pending deadlines,
approval and blocked delivery, replacement before uninstall, duplicate/late old
events, uninstall before onboard, pagination, identity mismatch, scope/use/history
retention, and restored eligibility without overriding exhaustion or revocation.
It also covers #66: an acknowledged repository webhook for an installation whose
command predates its account object, an adoption outage that admission and the
public `status` still fail promptly through, duplicate and never-adopted events,
convergence on restoration with no further event, and a delayed event for a
retired identity. That scenario seeds the pre-existing command state through
Restate's admin state API, since a first adoption is otherwise unreachable.
The Worker gate uses the production account availability integration with actual
D1 bindings; missing user parents keep admission projections unavailable until
restoration, and it repeats #66's outage on those bindings. Protocol-only tests
explicitly opt out of installation integration.
