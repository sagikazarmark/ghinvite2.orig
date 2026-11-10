# App-specific organization observation and registry stage

The workflow's AccountInstallation observer now chooses the read policy for its
logical app. The repository path retains REPO_READ and complete installation
repository pagination. The organization path verifies the App JWT installation's
numeric App ID, installation/account identities, organization account type,
member permission and suspension state, then mints ORG_READ and reads the
organization by login. Only the matching numeric organization is available;
organization observations carry no repository selection and never call
`/installation/repositories`. Missing/suspended or mismatched scope is unavailable;
unverifiable reads remain unknown and cannot grant authority.

A bounded org-owner helper checks retained active installation state, repeats
live installation verification, reads the organization, resolves the actor's
current login from its numeric user ID, and verifies membership user/org IDs.
Only active/admin yields Owner. Active/member or membership404 yields NonOwner
for the caller's explicit delegation rule. Pending membership, substituted
identities and inactive installation yield Denied; unverifiable upstream reads
yield Unavailable. Absent is established by the retained org owner, never inferred
from a failing HTTP read. Every request uses a typed read endpoint and ORG_READ;
there is no organization invitation/member/team write. The caller integrates this
with the one account-settings writer described in ID-39.

`P1_APP_STAGE` selects the actual deployment registry in both native and Worker
composition roots. Omission or exact `repo` selects the repository app. Exact
`repo-and-org` requires complete org ID/client ID/signing keys and, on web, webhook
secrets. Empty/unknown values fail with fixed field diagnostics; native invalid
Unicode is not treated as absence. Default Wrangler templates explicitly remain
`repo`. This is the production configuration path, not an alternate fixture-only
registry. See ID-40. The org target/create refusal and delivery activation gates
remain in place. SP-01, onboarding copy from SP-02, and SP-19a are not established
by scripted upstream fixtures.

Local proof (2026-10-16):

- Five focused observer/helper tests and two profile-policy tests pass, covering
  exact mint permissions, pending/member/owner, missing membership, substituted
  numeric user/org identities and inactive installation without HTTP.
  `/tmp/platform-org-observation-unit-final.log`,
  `/tmp/platform-org-profile-contract.log`.

- Actual Restate1.8 plus HTTP fixture passes
  `org_installation_observation`: exact repository vs organization mint permission
  bodies, independent app state, numeric identities, suspended/missing scope,
  upstream uncertainty, recovery, and retained uninstall without further HTTP.
  `/tmp/platform-org-observation-runtime2.log` (1 test, 2.79s).
- The signed workflow Worker plus pinned Restate1.7.9 passes
  `ORG_OBSERVATION_ONLY=1 node tests/worker/admission.mjs`. It exercises actual
  wasm handlers, exact app JWT issuers and broker mint bodies, both app scopes, org owner/active member/
  pending membership, canonical repo-key delegation writer, decision-authority
  reads, identity substitutions, transient503, recovery and uninstall.
  `/tmp/platform-org-observation-workerd-final.log`.
- Actual production startup paths pass 23 web Worker cases and 17 workflow
  Worker cases. Complete org configuration succeeds; invalid/empty stages and
  missing/partial org configuration fail before outbound calls, without marker
  disclosure. Existing signature/key-rotation assertions remain intact.
  `/tmp/platform-org-stage-web-startup.log` and
  `/tmp/platform-org-stage-workflow-startup.log`.
- Native app configuration10 tests pass, including exact stage/credential
  contracts and unchanged manifests: `/tmp/platform-app-stage-native.log`.

The Worker artifacts above predate the concurrent link-pause implementation;
these results prove the organization slice, not pause behavior. After the core
pause-field migration settled, actual native startup passed: the web suite checks
8 fail-closed cases, and the workflow suite has 6 tests including real discovery
with both stages, invalid/non-Unicode stage and incomplete org configuration
before SQL initialization (`/tmp/platform-org-stage-native-startup-final.log`).
Final strict-check results are appended below. New owner service calls and authorization behavior require ID-21 endpoint
pinning/draining for existing retained journals; no automatic deployment migration
or live GitHub permission proof is claimed.

The numeric user lookup is still marked unverified in the toolkit's upstream
contract. Scripted identity responses establish local numeric binding and request
ordering; they do not establish GitHub's live endpoint/permission behavior.

Final strict checks pass after the shared core/UI migration settled:
`cargo clippy` for both Worker runtime-test targets on wasm32 and for
`ghinvite-config` plus both native servers with all targets, each with
`-D warnings`. Logs: `/tmp/platform-org-stage-wasm-clippy-final2.log` and
`/tmp/platform-org-stage-native-clippy-final2.log`.

## Advisory organization eligibility presentation (B1-12)

The authorized Console creation form and admin link page read
`OrganizationEligibility/{org:account}/facts`. The web checks the returned
account ID, current active org installation ID, observation time and strict
one-hour freshness before presenting facts. Missing, failed, future, stale or
misbound reads display unknown conditions. Personal accounts make no facts call;
anonymous and concealed accounts never reach the read. No facts read alters
`can_create_targets`, target membership validation or the create command.

The shared server/island panel shows seats, required 2FA, default member repository
access and managed-user policy, retaining unknown values individually. Warnings
remain advisory. Base-access warnings follow the selected repository grants and
explicitly describe existing organization members; they never establish that a
requester is a member. The island receives exactly the server's admitted facts
snapshot and recomputes warnings on selection/grant edits. Creation under a
reported seat shortage remains possible; delivery still performs fresh checks.

Native proof: full Console115 passed (three browser-server fixtures ignored),
including successful creation under NoSeat, warning visibility on admin detail,
unknown fallback, account/installation/time binding and concealed read boundaries
(`/tmp/platform-eligibility-console-final.log`). Shared UI/island/parity194 pass
(`/tmp/platform-eligibility-ui.log`); native all-target strict passes
(`/tmp/platform-eligibility-native-clippy2.log`). The final island is472KiB gzip,
within600KiB (`/tmp/platform-eligibility-island-final.log`).

This presentation slice does not establish minimal-app live field visibility,
Cloud team-sync detection or live error taxonomy. The requester-specific 2FA
notice is a separate remaining presentation seam; no full B1-12 completion claim.

Final browser verification passes **360 tests** in2.6 minutes
(`/tmp/platform-eligibility-browser-final.log`), including eight native/enhanced
eligibility cases across desktop/mobile and light/dark. They prove warning
retention across hydration, per-repository grant changes and enabled creation.
The initial enhanced test exposed a missing entry-point prop; forwarding is now
explicit and the component prop is required, preventing silent defaulting.
The final UI/island/parity194 and native all-target strict reruns pass
(`/tmp/platform-eligibility-ui-final.log`,
`/tmp/platform-eligibility-native-clippy-final.log`). Actual web-worker wasm strict
passes (`/tmp/platform-eligibility-wasm-clippy.log`), and the refreshed ordinary
Worker still passes the typed domain/D1/KV privacy gate with logs scanned after
runtime disposal (`/tmp/platform-domain-signin-workerd-final2.log`).

### Positive policy facts after a rejected write

A prerequisite can change after the last pre-write read. Organization/team S4 now performs one additional bounded, numeric-bound observation after an explicit generic403/422 rejection. It uses the existing ORG_READ profile and installation/org/user/team validation. Only positive paid-seat exhaustion or a positively externally managed target team replaces the generic result with NoSeat or IdpSyncedTeam. It does not promote a concurrent membership observation to success.

An unavailable read, wrong numeric identity, absent optional fields or unrelated result preserves the original rejection. A rate limit encountered by that read is retained for budget closure. The original positive rejection still supplies the exact rejected fence generation and unused reservation evidence. A server error or uncertain transport never enters this classifier and retains its charge/fence. No additional invitation or membership write occurs during classification.

The [team membership API reference](https://docs.github.com/en/rest/teams/members?apiVersion=2022-11-28#add-or-update-team-membership-for-a-user) documents synchronization restrictions, but does not provide a distinct stable payload. The [organization member API reference](https://docs.github.com/en/enterprise-cloud%40latest/rest/orgs/members?apiVersion=2022-11-28) does not establish stable no-seat/EMU refusal codes. Therefore this implementation uses positive structured facts, not guessed response prose. Cloud synchronization visibility, SP-01 no-seat response capture and an authenticated EMU response corpus remain explicit gaps. Missing Organization-plan permission can make seat facts unavailable. There is no new permission or token-profile expansion.

The actual native Restate policy gate passes3 tests/seven scenarios (`/tmp/org-positive-policy-runtime5.log`): seat capacity changing only after422, wrong organization identity, unavailable facts, generic422, positive external team management after403 versus generic403, and uncertain502 preserving one write/fence/charge. Positive refusals release only the acknowledged rejected generation and unused slot; the team policy remains blocking without additional PUTs. The generic403 control may legitimately retry after its positive rejection. Focused workflow/web/UI strict passes (`/tmp/onboarding-policy-final-strict.log`). The production source is included in `/tmp/m2-bulk-operations-workflow-capture.log`; no dedicated signed Worker policy runtime is claimed from that build alone.
