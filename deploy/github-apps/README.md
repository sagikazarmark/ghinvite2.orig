# P1 GitHub App registration templates

These manifests encode P1 §20.2. `p1.example.com` is a reserved example host,
not a deployed service. Before registration, replace that host consistently with
`P1_HOST`, choose unique App names, and review the resulting JSON. No App IDs,
client IDs, keys or secrets are checked in. The manifest permission/event tests
are in `crates/ghinvite-web/tests/app_deployment.rs` (AC-APP-1).

- Sign-in requests only `emails: read`, has no repository or organization
  permissions, disables webhooks, and returns to `/oauth/callback`.
- Repo requests `administration: write` and `metadata: read`, uses
  `/webhooks/repo` and `/setup/repo`, and has no organization permissions/events.
- Org requests `members: write` and `metadata: read`, subscribes to organization
  and membership events, and uses `/webhooks/org` and `/setup/org`.

All three disable automatic OAuth on installation. Installation and repository
selection webhook delivery without explicit event subscription remains a live
registration check. Set “Expire user authorization tokens” manually on the
sign-in App and capture the SP-19a evidence. These templates do not assert that
registration, permission behavior, callbacks or the hard SP-19a gate have passed.
Installed-app routes are app-scoped in the current source. Token-free sign-in
through this App is web's only sign-in path (legacy OAuth was removed in GM-09);
local implementation and fixture proofs do not satisfy SP-19a.

The manifest registration-completion `redirect_url` is deliberately omitted:
it is not an OAuth callback and this repository has no manifest-conversion route.
The sign-in OAuth callback is expressed only by `callback_urls`; installed apps
use their `setup_url`. Add a registration-completion URL only when a real
manifest-conversion flow is implemented.
