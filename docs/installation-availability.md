# Installation availability (#60)

The web caller may obtain scope evidence before admission. Link admission consumes
that evidence as immutable command data and performs no GitHub or SQL read: only
an available=false observation for a required app, at most five minutes old and
not from the future, refuses a new attempt. Missing/stale evidence permits the
ordinary guardrail decision. Exact retained attempt receipts replay unchanged.

## Command ownership

`Installation/<app>:<numeric installation ID>` validates its key and retains the numeric
account binding and permanent uninstall tombstone. An uninstall received before
onboarding prevents that identity from ever being onboarded. Repository-change
payloads carry the app identity, but their selection is ignored: webhooks and setup returns request a refresh.

`AccountInstallation/<app>:<numeric account ID>` serializes onboarding, repository
refresh, uninstall, status, and admission observations across replacement
installations. Fresh onboarding retains current identity and observations in
Restate without adopting SQL rows. The shared `current_installation` read returns
only the retained account context for delivery; missing authority returns 503.
`InstallationProjection` receives
ordered durable sends for SQL/audit writes on a separate account-keyed object,
so persistence retries cannot hold availability, approval, or replay hostage.
Deploy these services together.

Onboarding verifies the App-authenticated GitHub installation's numeric account
ID. A replacement can supersede an existing identity only after GitHub confirms
the old installation is absent. Neither account login, caller event time, nor
numeric installation ordering chooses the replacement. Retired identities remain
tombstoned. Late old repository/uninstall events cannot mutate the replacement.

Repository refresh runs inside account serialization, follows all pages, and
retains exact numeric repository IDs even for an installation configured for all
repositories. Inconsistent/incomplete pagination is unknown. Duplicate events
repeat the current observation rather than applying old deltas. Refreshes caused
by installation events update retained context and the SQL/D1 selected-repository
view; unavailable/unknown observations retain and project an empty set.
The public account `status` distinguishes that uncertainty from known absence.
Unavailable/unknown observations schedule a durable 60-second recheck until
restoration or uninstall. A successful observation also projects restored scope,
including when triggered by admission, so blocked delivery can recover.

Refresh work is acknowledged by its durable send before it runs. Onboarding,
refresh and uninstall need no SQL reads, so projection outages only hold the
independent projector. The former SQL-adoption retry protocol is removed in the
clean prelaunch model (#119). Installation-ID routing and uninstall tombstones
remain retained; an unknown routing identity cannot reconstruct authority from
SQL. Refreshes for superseded, retired, or unknown identities observe nothing.

## Admission and approval

The caller can ask `AccountInstallation.admission_evidence` for its immutable
repository scope. Recent positive observations are answered from retained state;
other cases perform a complete live observation. Unknown HTTP results stay
unknown, while missing expected account authority returns503. This API's
`valid_until` describes the observation handoff; the caller converts it to the
required app-scoped evidence supplied to admission. A fresh negative produces
`ScopeUnavailable`; restoration never changes the retained rejection.

GitHub requests have explicit [30-second network deadlines](network-deadlines.md),
including on Workers. A stalled observation becomes unknown and completes its
exclusive handler, allowing queued status to proceed and durable recheck to run.

Admission observations retain Restate state and durably arrange projection without
waiting for SQL. Onboarding verifies GitHub and establishes retained authority;
an existing SQL row alone cannot initialize that authority. Link admission and
receipt replay do not depend on SQL projection completion. Revocation, decisions,
and request status do not call availability. Original deadlines, uses, revocation,
scope, dispatch plans, and confirmed delivery history survive uninstall/reinstall.
Available scope is necessary but does not override a link's original guardrails.

Setup verifies the numeric app/installation/account through an app JWT and
records no installer. Signed installation events record the sender. Payload-hash
receipts bind exact input; older signed update timestamps cannot change the
installer, and equal timestamps with different numeric senders fail closed.
Delegation starts off and only the retained installing owner can toggle it in
M0. Changes have operation-bound receipts and immutable audit events. M1 org
owner verification remains a separate gate.

`recheck_permissions` verifies GitHub facts, retains and queues the full revised snapshot,
invalidates the local installation broker cache, then explicitly requests a
fresh token. Mint422 and double401 blocked delivery outcomes send that recheck;
a still-unapproved grant does not create an unbounded mint loop.

GitHub observations are not atomic with admission or subsequent GitHub effects.
An external change after an observation may block delivery even after acceptance.
Refresh failure never proves an effect happened, failed, or can safely be repeated.

## Installing Owner recovery (repo app only)

If the recorded installer has left an organization, a current organization owner
must re-install the repo app or accept an applicable permission update, as required
by P1 §7.6/OQ-12. This is an operator-owned GitHub action, not a database repair.
Before proceeding, identify the numeric account, app and installation and retain
redacted current owner/scope observations. Setup redirects alone record no
installer and cannot transfer this authority.

Keep delegation off unless an already-authorized account owner explicitly changes
it. Verify the replacement/update through the authenticated app-specific webhook:
the retained numeric sender, app/account binding, current installation identity
and signed event ordering must agree. Replacement also requires the old
installation to be positively absent; a delayed old event cannot reassert it.
Wait for the corresponding projection, then have the new Installing Owner prove
current administrator rights on every target through the normal Console flow.
Confirm the former installer cannot act without independently valid authority.
Never edit `installed_by`, seed SQL ownership or replay an unsigned payload to
force recovery. If delivery or verification is uncertain, leave authority
unavailable and inspect redacted invocation metadata; do not grant delegation as
a workaround. Installing the org app provides the separately verified current-org-
owner authority path, rather than importing the old installer as an org owner.

These steps document the supported authority contract. Actual GitHub re-install,
permission acceptance and recovery against a deployed account remain live gates;
no such operation is claimed by the local fixtures.

## Verification

```sh
bash scripts/test-restate.sh installation_authority
bash scripts/test-restate.sh installation_availability
bash scripts/test-restate.sh installation_audit_replay
cargo test -p ghinvite-web --test console_flow
npm run test:admission --prefix tests/worker
```

The native runtime scenario exercises the public commands with GitHub HTTP stubs:
absence, partial scope, unknown observations, replay, original pending deadlines,
approval and blocked delivery, replacement before uninstall, duplicate/late old
events, uninstall before onboard, pagination, identity mismatch, scope/use/history
retention, and restored eligibility without overriding exhaustion or revocation.
It also covers fresh onboarding, acknowledged repository webhooks, admission and
retained reads during an installation projection outage, duplicate/unknown events,
convergence on restoration with no further event, and delayed retired events.
The Worker gate uses the production account availability integration with actual
D1 bindings; missing user parents keep admission projections unavailable until
restoration, and it repeats the projection outage on those bindings. Protocol-only tests
explicitly opt out of installation integration.

## Deployment boundary

App-scoped keys, canonical snapshots, command inputs and journal step order are
incompatible with earlier numeric-key owners. Keep earlier deployments available
for pinned invocations and drain them before routing new work to this version;
there is no implicit numeric-key or old-journal migration. Webhook receipts retain only the authenticated payload hash key and its expiry.
Each schedules one durable callback after seven days; cleanup checks one key,
uses a journaled clock, and cannot erase a later renewed receipt. Signed update
timestamps and retired-installation tombstones continue to fence delayed events
after receipt expiry. Setup/delegation operation receipts retain their original
input and result for exact operation replay.

Permission recheck retains fresh installation facts and durably queues the monotonic snapshot projection before broker invalidation and ensure_fresh. It never waits for SQL projection completion: an unavailable read database must not hold the account owner and prevent uninstall or replacement. The queued projection converges after recovery without another webhook (ID-31).
