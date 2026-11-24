# octoemu

A stateful local emulator of the GitHub surface a GitHub App uses (REST, App
and installation auth, the OAuth web flow, webhook delivery), for tests.
Native only (on wasm32 the crate is empty). Not published.

Plan, design and task list: [`docs/specs/github-emulator.md`](../../../../docs/specs/github-emulator.md)
in the ghinvite repository. Modelled on AWS's emulator: real checks, explicit
gaps, fidelity measured against the real service.

## Status

Built: the server, the two modes, the seed model and its validation, the
virtual clock, the admin API, the call log, the fidelity manifest (GE-01);
App JWT verification, stateless installation tokens with scoping,
revocation and suspension, and per-route credential and permission checks
(GE-02); repositories, collaborators, permissions, repository invitations
with expiry and the daily limit, the invitee's side (through the admin
API until GE-05), users, avatars and the webhook event outbox (GE-03);
webhook delivery with GitHub's headers and signatures, recorded attempts,
the deliveries API and the App's hook configuration, and `installation` /
`installation_repositories` events from admin actions (GE-04); the OAuth
web flow (the authorize page with auto-consent, the token endpoint with
PKCE, codes and refresh tokens), GitHub App user access tokens and classic
PATs, `/user`, `/user/emails`, the `/applications/{client_id}` token routes
and the invitee's `/user/repository_invitations` routes (GE-05);
organizations, memberships, organization invitations (by user or email,
with teams; expiry, the failed list, the daily budget and seats), teams
and team memberships with their repository grants, the user's
`/user/memberships/orgs` routes, and `organization` / `membership` events
(GE-06); fault rules (GE-07; `stall`, `hold_response` and
`query_contains` since GE-09b), the admin user-rename and
invitation-expiry routes (GE-09b), the admin organization rename
(GE-09c), and the user's
`/user/installations/{installation_id}/repositories` (after GE-09), and
the fidelity suite's emulator half (GE-08: [`octoemu-conformance`](#fidelity-suite-octoemu-conformance),
which also brought `DELETE /app/installations/{installation_id}`, the
`X-GitHub-Api-Version` check, the outside-collaborator policy and
schema-complete user, organization, team and repository objects), and
its live half (GE-08: the first live record run against GitHub, and
octoemu changed to answer as GitHub did). The temporary stub-compatibility shim
of GE-07 is gone (GE-09c): ghinvite's suites build their world from the
native seed and admin API (`ghinvite_e2e::github`).

### Implemented routes

| Route | Credential | Notes |
|---|---|---|
| `GET /app` | App JWT | The authenticated App |
| `GET /app/installations` | App JWT | Paged |
| `GET /app/installations/{installation_id}` | App JWT | 404 for an unknown installation or another App's |
| `DELETE /app/installations/{installation_id}` | App JWT | The App uninstalls itself: 204, `installation.deleted` sent by its bot, its tokens stop working; 404 for an unknown installation or another App's |
| `POST /app/installations/{installation_id}/access_tokens` | App JWT | 201 `{token, expires_at, permissions, repository_selection, repositories?}`; body `permissions`, `repositories`, `repository_ids` narrow (`permissions` answers exactly what was asked) |
| `PUT`/`DELETE /app/installations/{installation_id}/suspended` | App JWT | 204 |
| `GET /app/installation-requests` | App JWT | Paged |
| `GET /app/hook/config` | App JWT | `{url, content_type: "json", secret: "********", insecure_ssl}`; 404 without a webhook |
| `PATCH /app/hook/config` | App JWT | `url`, `content_type` (`json` only), `secret` (replaces every seeded secret; `""` stops signing), `insecure_ssl` |
| `GET /app/hook/deliveries` | App JWT | Newest first; `per_page`, `cursor` (`next`/`prev` in `Link`), `status=success\|failure` (200–399 is a success) |
| `GET /app/hook/deliveries/{delivery_id}` | App JWT | With `url`, `request {headers, payload}`, `response {headers, payload}`; 404 for another App's |
| `POST /app/hook/deliveries/{delivery_id}/attempts` | App JWT | 202 `{}`; the same body and GUID again, `redelivery: true` |
| `GET /installation/repositories` | installation token, `metadata: read` | Paged; only the token's repositories |
| `DELETE /installation/token` | installation token | 204; the token stops working on other routes 500 ms later (on the virtual clock); a second `DELETE` is 401 at once |
| `GET /user/installations/{installation_id}/repositories` | a GitHub App's user access token | Paged `{total_count, repository_selection, repositories}`: the installation's repositories the user has explicit access to, each with the user's `permissions`; 404 for another App's installation or one the user reaches nothing of (assumed: an empty list for the account's owner, 404 while suspended, 403 for a classic PAT) |
| `GET /repos/{owner}/{repo}` | installation or user token, `metadata: read` | Private and internal repositories outside the token's reach are 404 |
| `GET /repos/{owner}/{repo}/collaborators` | installation token, `metadata: read` | Paged; `affiliation=direct\|outside\|all` (default all); each with `permissions` and `role_name` |
| `PUT /repos/{owner}/{repo}/collaborators/{username}` | installation token, `administration: write` | Body `{permission}` (`pull`, `triage`, `push` (default), `maintain`, `admin` or a custom role); 201 with the invitation (a pending one again, its role unchanged), 204 for a user with access (a role change, an organization member added directly); 422 for another value or past 50 invitations per 24 h; 403 `Inviting outside collaborators is restricted by organization policy.` for a non-member of an organization seeded `outside_collaborators_restricted` (assumed) |
| `DELETE /repos/{owner}/{repo}/collaborators/{username}` | installation token, `administration: write` | 204 |
| `GET /repos/{owner}/{repo}/collaborators/{username}/permission` | installation token, `metadata: read` | `permission` (`admin\|write\|read\|none`), `role_name` (exact, custom included), `user`; the highest role across sources |
| `GET /repos/{owner}/{repo}/invitations` | installation token, `administration: read` | Paged; `Link` URLs on the alias below |
| `GET /repositories/{repository_id}/invitations` | installation token, `administration: read` | The same list by repository ID |
| `PATCH /repos/{owner}/{repo}/invitations/{invitation_id}` | installation token, `administration: write` | Body `{permissions}` (`read`, `write`, `triage`, `maintain`, `admin`); 200 with the invitation |
| `DELETE /repos/{owner}/{repo}/invitations/{invitation_id}` | installation token, `administration: write` | 204 |
| `GET /users/{username}` | none needed | Users, organizations and App bots (`{slug}[bot]`) |
| `GET /user/{account_id}` | none needed | The same by numeric ID |
| `GET /avatars/u/{id}` | none needed | A small PNG for any seeded account or App bot |
| `GET /user` | user token | The signed-in user (`id`, `login`, `avatar_url`, `created_at`, `type`, `name`, `email`, …); `email` is the public primary address, else null |
| `GET /user/emails` | user token, `email_addresses: read` (a PAT: `user` or `user:email`) | Paged `[{email, primary, verified, visibility}]` |
| `GET /user/repository_invitations` | user token | Paged; the user's invitations |
| `PATCH /user/repository_invitations/{invitation_id}` | user token | Accept: 204, `member.added`; 404 if not theirs or expired |
| `DELETE /user/repository_invitations/{invitation_id}` | user token | Decline: 204 |
| `GET /orgs/{org}` | installation token | `plan` only with `organization_plan: read` (documented); `two_factor_requirement_enabled`, `default_repository_permission` and the private repository counts only with `organization_administration: read` (assumed) |
| `GET /orgs/{org}/members` | installation token, `members: read` | Paged active members; `role=all\|admin\|member` |
| `GET /orgs/{org}/memberships/{username}` | installation token, `members: read` | `{url, state, role, organization_url, organization, user}`; an open invitation is `pending`; else 404 |
| `PUT /orgs/{org}/memberships/{username}` | installation token, `members: write` | `{role}` `admin\|member`: 200; a member's role changes; a non-member is invited (`pending`, `organization.member_invited`) |
| `DELETE /orgs/{org}/memberships/{username}` | installation token, `members: write` | 204: removes a member (and their team memberships; `organization.member_removed`) or cancels an invitee's invitation |
| `GET /orgs/{org}/invitations` | installation token, `members: read` | Paged open invitations; `login` null for an email invitee; `role=` |
| `POST /orgs/{org}/invitations` | installation token, `members: write` | `{invitee_id \| email, role, team_ids}`: 201, `organization.member_invited`; 422 for an existing member, an open invitation, an invalid role or team, past the budget, without a seat, or while `spammed` |
| `DELETE /orgs/{org}/invitations/{invitation_id}` | installation token, `members: write` | 204; 404 unknown, failed or expired |
| `GET /orgs/{org}/invitations/{invitation_id}/teams` | installation token, `members: read` | Paged |
| `GET /orgs/{org}/failed_invitations` | installation token, `members: read` | Paged; seeded failures and expired invitations (`failed_reason: "Invitation expired"`, assumed) |
| `GET /orgs/{org}/teams` | installation token, `members: read` | Paged, with `parent` |
| `GET /orgs/{org}/teams/{team_slug}` | installation token, `members: read` | `members_count`, `repos_count`, `organization` |
| `GET /orgs/{org}/teams/{team_slug}/memberships/{username}` | installation token, `members: read` | `{url, role, state}`; 404 when not in the team |
| `PUT /orgs/{org}/teams/{team_slug}/memberships/{username}` | installation token, `members: write` | `{role}` `member\|maintainer`: 200; active for a member (`membership.added`), pending otherwise (a non-member is invited, assumed) |
| `DELETE /orgs/{org}/teams/{team_slug}/memberships/{username}` | installation token, `members: write` | 204 (`membership.removed`) |
| `GET /user/memberships/orgs` | user token | Paged; `state=active\|pending` |
| `GET /user/memberships/orgs/{org}` | user token, `members: read` | 200 or 404 |
| `PATCH /user/memberships/orgs/{org}` | user token, `members: write` | `{"state": "active"}`: accepts; 202 with the membership, already active (as GitHub answers; its docs say 200) |
| `POST /applications/{client_id}/token` | `Basic client_id:client_secret` | Check `{access_token}`: 200 with the authorization, 404 invalid, 422 without `access_token` |
| `PATCH /applications/{client_id}/token` | `Basic …` | Reset: 200 with a new token; the old one stops working |
| `DELETE /applications/{client_id}/token` | `Basic …` | Revoke: 204 (and its refresh token) |
| `DELETE /applications/{client_id}/grant` | `Basic …` | Revoke every token the App holds for that user: 204 |
| `GET /login/oauth/authorize` | none (web) | The authorize page; see below |
| `POST /login/oauth/authorize` | none (web) | The page's consent form |
| `POST /login/oauth/access_token` | none (web) | Codes and refresh tokens for user access tokens; see below |

Every other API and web route answers:

```http
HTTP/1.1 501 Not Implemented
content-type: application/json; charset=utf-8

{"message":"octoemu: not implemented","route":"GET /repos/acme/widgets","documentation_url":"…"}
```

It is never a 404, which a client could take for GitHub's answer. Anything
outside ghinvite's surface stays 501.

### Repository model

- **Roles.** A collaborator or invitation has one of GitHub's five roles
  (`pull < triage < push < maintain < admin`) or a custom role seeded in
  `repository_roles` over a base (`read`, `triage`, `write`, `maintain`),
  ranked as its base. The permission API reports the legacy `permission`
  (maintain is `write`, triage `read`) and the exact `role_name` (`read`,
  `triage`, `write`, `maintain`, `admin` or the custom name); invitations
  report `permissions` the same way.
- **Effective access** is the highest of: a direct collaboration, owning the
  repository, owning its organization (admin), the organization's base
  permission for active members, and a team's role on the repository for
  the team's active members and the active members of its child teams.
- **Invitations** expire 7 days after `created_at` on the virtual clock and
  then show `expired: true` (they stay listed until deleted or replaced).
  A repository gets at most 50 invitations per sliding 24 hours through
  the API.
- **What the App does** is attributed to its bot account
  (`{slug}[bot]`, `type: "Bot"`, ID 1 000 000 000 + App ID, URLs with the
  brackets percent-encoded as GitHub writes them, `/users/{slug}%5Bbot%5D`;
  `GET /users/{slug}[bot]` answers it).
- **Settings.** A repository's `has_wiki`, `has_downloads` and
  `allow_forking` are seeded (`Repo::has_wiki` and the like); true, the
  REST API's defaults for a new repository, when absent.
- **The invitee's side** (accept, decline, leave) are domain operations
  (`src/collaboration.rs`), reached through the user-token routes
  (`/user/repository_invitations…`) and the admin API's `act` routes alike.

### Organization model

- **Memberships** are `active` or `pending`. A seed may hold pending ones;
  an open invitation (sent by `POST …/invitations`, `PUT
  …/memberships/{username}` or a team PUT for a non-member) is the
  invitee's pending membership, and its `team_ids` their pending team
  memberships, until it is accepted, declined, cancelled or expires.
- **Accepting** (`PATCH /user/memberships/orgs/{org}`, or the admin
  `act … accept`) turns the invitation (by login, or an email invitation
  matching one of the user's verified addresses, assumed) or the pending
  membership into an active membership with the invited role, and its
  teams into active team memberships: `organization.member_added`, then
  `membership.added` per team.
- **Invitations** expire 7 days after `created_at` on the virtual clock:
  they leave the pending list and show in `failed_invitations` with
  `failed_at` at the expiry (assumed). Cancelled and declined invitations
  disappear (assumed).
- **Budget** (documented): 50 invitations per organization per sliding 24
  hours, 500 once the organization is older than a month (`created_at`) or
  on a paid plan (any plan but `free`); past it, 422 `Over invitation rate
  limit` (assumed).
- **Seats**: on a paid plan with `seats > 0`, an invitation that would make
  a member needs `filled_seats < seats`; `filled_seats` is the plan's
  pinned value, else memberships plus open member invitations (assumed).
- **`reinstate`** gives a user removed by this instance their old role.
- **Events** go to the organization's non-suspended installations that
  subscribe to `organization` / `membership`, with `organization` and
  `sender`; the App's bot sends what an installation token does.

### OAuth web flow and user tokens

The web pages live on the same port as the API (`web_base` = `api_base`):
point a client's GitHub web origin at `web_base` and its API base at
`api_base`.

- **Authorize** (`GET /login/oauth/authorize`): `client_id`, `redirect_uri`
  (one of the App's `callback_urls` exactly, or a loopback URL differing
  only in port; omitted, the first), `state`, `code_challenge` with
  `code_challenge_method=S256` (both or neither; 43 base64url characters),
  `login`, `allow_signup` (ignored), `prompt=select_account`.
  - With `login=<seeded user>` and auto-consent on (the default), it
    answers `302` to `redirect_uri?code=…&state=…` at once: no browser.
  - Otherwise it renders an HTML page listing the seeded users (bots
    excluded), one form each posting the choice back to
    `POST /login/oauth/authorize`, plus Cancel (`access_denied`).
    `prompt=select_account` always shows the page.
  - Errors as GitHub's troubleshooting page documents, redirected with
    `error`, `error_description`, `error_uri` and `state`:
    `redirect_uri_mismatch` (to the first registered callback),
    `access_denied` (`POST /__octoemu/oauth/deny`, optionally for one
    `login`, or Cancel) and `application_suspended` (an App seeded
    `suspended`). An unknown `client_id` (404) and malformed PKCE parameters
    (400) are an HTML error page (assumed).
- **Codes** are 20 characters, last 10 minutes on the virtual clock, work
  once and are bound to their App and redirect URI.
- **Token endpoint** (`POST /login/oauth/access_token`): a form or JSON body
  (or query parameters). `Accept: application/json` answers JSON,
  otherwise form-encoded (documented). A code (`client_id`,
  `client_secret`, `code`, `redirect_uri`, `code_verifier` required exactly
  when the authorization sent a challenge) or `grant_type=refresh_token`
  with `refresh_token`. Success: `access_token` (`ghu_` + 36), and with
  expiring tokens (the App's `expire_user_tokens`, default true)
  `expires_in` 28800, `refresh_token` (`ghr_` + 76),
  `refresh_token_expires_in` 15897600; then `scope` `""` and `token_type`
  `bearer`. A refresh token works once and kills the access token it came
  with. Errors (`incorrect_client_credentials` for a wrong secret,
  `redirect_uri_mismatch`, `bad_verification_code`, `bad_refresh_token`,
  `unverified_user_email` for an unverified primary address) carry
  GitHub's `error_description` and an absolute `error_uri`, with HTTP 200;
  a `client_id` naming no App is a 404 `{"error": "Not Found"}`, and any
  `grant_type` but `refresh_token` is read as the code grant (as GitHub
  answers; assumed, refused in refuse-assumed mode).
- **User tokens** are looked up on every call: unknown, expired, revoked:
  401 `Bad credentials`. A `ghu_` token holds its App's `user_permissions`
  and `permissions`; on resource routes it reaches only what the user can
  access and an installation of the App covers. Without the route's
  permission: 403 `Resource not accessible by integration` with
  `X-Accepted-GitHub-Permissions`. Classic PATs (`ghp_`, admin-minted only)
  have scopes, checked only on `/user/emails` (assumed).
- **Caps (assumed).** At most 10 live tokens per user and App: the 11th
  revokes the oldest. After `user_tokens_per_hour` (default 10; seed
  `settings` or `POST /__octoemu/oauth`) new tokens for a user and App in
  an hour, `login=` stops auto-consenting and the page asks to authorize
  again; its form still issues a code.

### Event outbox

Every change GitHub would announce by webhook appends an event to an
outbox: `{"id", "at", "event", "action", "targets": [{"app_id",
"installation_id"}], "payload"}`. Read it at `GET /__octoemu/events` or
`Emulator::events()`. Today:

- `organization` `member_invited` (`invitation`, `user`), `member_added`
  and `member_removed` (`membership {url, state, role, organization_url,
  user}`); `membership` `added` and `removed` (`scope: "team"`, `member`,
  `team`). Targets: the organization's non-suspended installations
  subscribed to the event; `organization` and `sender` in every payload.
- `member` `added` (an invitation accepted, or an organization member
  added directly), `edited` (a role change, with `changes: {permission:
  {from, to}}`) and `removed`
  (`DELETE …/collaborators/{username}` or leaving). Targets: the
  non-suspended installations that can access the repository and
  subscribe to `member`. The payload leaves out `installation`, which
  each delivery adds as `{"id", "node_id"}`.
- `installation` `created`, `deleted`, `suspend`, `unsuspend`,
  `new_permissions_accepted` and `installation_repositories` `added`,
  `removed`, from the admin API, the handle and `PUT`/`DELETE
  /app/installations/{id}/suspended`. They go to the installation's App
  whatever it subscribes to (documented), suspended or not (assumed), and
  carry the full `installation`, `repositories` (or `repository_selection`,
  `repositories_added`, `repositories_removed` as `{id, node_id, name,
  full_name, private}`), `requester: null` and `sender`. Without a named
  sender, the account itself or an organization's first active owner is
  the sender; the App's own suspension route names its bot.

### Webhook delivery

Each target whose App has an active `webhook` becomes a delivery: a POST
to the App's URL with `Content-Type: application/json`, `User-Agent:
GitHub-Hookshot/octoemu`, `X-GitHub-Event`, `X-GitHub-Delivery` (a random
GUID), `X-GitHub-Hook-ID`, `X-GitHub-Hook-Installation-Target-Type:
integration`, `X-GitHub-Hook-Installation-Target-ID: <App ID>` and, with a
secret, `X-Hub-Signature-256: sha256=<hex HMAC-SHA256 of the body>` and the
legacy `X-Hub-Signature: sha1=…`.

- **Rotation model (assumed).** GitHub keeps one current secret per App.
  octoemu signs with the first of `webhook.secrets`, read at send time; the
  others are never used. To test a receiver mid-rotation, configure it with
  `[new, old]` and switch the emulator's secret
  (`PUT /__octoemu/apps/{id}/webhook`, `Emulator::set_webhook`,
  `PATCH /app/hook/config`).
- **Order.** One sender per App sends that App's deliveries one at a time
  in event order (assumed); Apps send concurrently.
- **Failure.** A non-2xx response, a refused connection or no complete
  response within 10 s (`Builder::webhook_timeout` shortens it for tests)
  is a failed delivery. Nothing is retried (documented).
- **Dispatch.** `auto` (default) sends at once; `manual`
  (`Builder::dispatch(Dispatch::Manual)`, `POST /__octoemu/deliveries/mode`)
  holds deliveries until `POST /__octoemu/deliveries/dispatch`.
  `POST /__octoemu/deliveries/wait` (or `Emulator::wait_for_deliveries`)
  returns once nothing is queued or in flight, so a test can assert right
  after it.
- **Attempts** are recorded with `id`, `guid`, `delivered_at` (virtual
  clock), `redelivery`, `duration` (real seconds), `status` (`OK`, `Invalid
  HTTP Response: <code>`, `timed out`, `Couldn't connect to server`; the
  last three assumed and withheld in refuse-assumed mode), `status_code` (0
  without a response), `event`, `action`, `installation_id`,
  `repository_id`, `url`, the request's headers and payload and the
  response's headers and body (first 64 KiB), plus `app_id` and
  `event_id`. They are kept for the instance's life and all can be
  redelivered (GitHub's 3-day window is not modelled). The deliveries API
  serves them to their App; `GET /__octoemu/deliveries` and
  `Emulator::deliveries()` serve all of them.

### Auth model

Every route declares what it accepts when it is registered; the credential
is checked before the handler runs, in this order:

1. **Verified, on every route.** A credential that is sent must be valid,
   even where none is needed: otherwise 401.
   - **App JWT** (`Authorization: Bearer <jwt>`): RS256, verified against
     any of the App's `public_keys` (so a rotation can list old and new);
     `iss` the client ID or the App ID (string or number); `iat` an integer
     (or a string of one) at most 60 s in the future; `exp` the same, in
     the future, at most 600 s after the virtual clock's now (not bounded
     by `iat`). Rejections carry GitHub's messages as the fidelity suite
     measured them (`Bad credentials` for a malformed or `alg: none` JWT,
     a missing or past `exp` and a future `iat`; `A JSON web token could
     not be decoded` for a bad signature; `'Issuer' claim ('iss') must be
     an Integer`; `Missing 'issued at' claim ('iat') in assertion`;
     `'Expiration time' claim ('exp') is too far in the future`). A JWT
     sent as `token <jwt>` is refused. octoauth's skew correction works:
     the emulator's `Date` tells it how far off it is.
   - **Installation token** (`Bearer ghs_…` or `token ghs_…`): looked up on
     every call. Unknown, forged, expired (1 h, on the virtual clock),
     uninstalled, or revoked more than 500 ms ago on the virtual clock
     (GitHub's revocation takes a moment to propagate): 401 `Bad
     credentials`. Suspended: 403 `This GitHub App installation is
     currently suspended.`, until unsuspended.
   - **User token** (`Bearer ghu_…`/`ghp_…`, or `token …`): looked up on
     every call; unknown, expired or revoked: 401 `Bad credentials`.
   - **Client credentials** (`Basic base64(client_id:client_secret)`): a
     seeded App's pair, else 401 `Bad credentials` (assumed).
   - Anything else (`gho_`, `github_pat_`, a `ghr_` refresh token): 401
     `Bad credentials`.
2. **The right kind.** No credential on a route that needs one: 401
   `Requires authentication`. The wrong kind: 401.
3. **The route's permission**, for installation and user tokens: without it, 403
   `Resource not accessible by integration` with
   `X-Accepted-GitHub-Permissions: <permission>=<level>`.

**Installation tokens** are GitHub's stateless format, `ghs_<APPID>_<JWT>`,
383 characters like GitHub's, signed with a key drawn per instance (the inner
claims are octoemu's own; treat tokens as opaque). A token's scope is the
installation's grant (its `permissions`, else the App's; its repository
selection) narrowed by the request:

- `permissions` may only ask for granted permissions at granted levels or
  lower; anything else is 422. A token narrowed without `metadata` keeps
  `metadata: read` when the installation has it (the answer lists only
  what was asked). A token narrowed by `permissions` reports
  `repository_selection: "selected"`.
- `repositories` (names) and `repository_ids` narrow to repositories the
  installation can access, 500 at most; an unknown one is 422; both
  together cover the union. A narrowed token reports
  `repository_selection: "selected"` and lists its `repositories`.

Many of these details are guesses, each labelled `assumed` in
`fidelity.toml` (see below).

## Modes

**Embedded**, one instance per test:

```rust
use octoemu::{Emulator, seed::{Seed, User}};

let emulator = Emulator::start(Seed::new().user(User::new(1, "octocat"))).await?;
let api_base = emulator.api_base();   // instead of https://api.github.com
let web_base = emulator.web_base();   // instead of https://github.com
emulator.advance_clock(std::time::Duration::from_secs(3600));
let calls = emulator.calls();
// Stops when dropped, or: emulator.shutdown().await?;
```

`Emulator::builder(seed)` adds `.bind(addr)`, `.idle_timeout(d)`,
`.refuse_assumed()`, `.dispatch(Dispatch::Manual)` and
`.webhook_timeout(d)`. The handle also has `avatar_base`, `admin_base`,
`now`, `seed` (merge more), `state`, `clear_calls`, `stopped`,
`suspend_installation`, `unsuspend_installation`, `uninstall`, `install`,
`events`, `deliveries`, `dispatch_deliveries`, `set_dispatch`,
`set_webhook`, `wait_for_deliveries`, `mint_user_token`,
`mint_personal_access_token`, `deny_authorization`, `allow_authorization`,
`set_auto_consent` and, for [fault rules](#fault-rules), `add_fault`,
`faults`, `fault`, `remove_fault`, `clear_faults`, `release_fault` and
`wait_for_held`.

**Binary**, for the Worker drivers, browser tests and other processes:

```console
$ octoemu --port 0 [--bind 127.0.0.1] [--seed seed.json] [--idle-timeout SECS] [--refuse-assumed]
{"api_base":"http://127.0.0.1:41234","web_base":"http://127.0.0.1:41234","admin_base":"http://127.0.0.1:41234/__octoemu","port":41234,"pid":4242}
```

Exactly one JSON line on stdout, once listening;
nothing else goes to stdout.
`--idle-timeout` exits (status 0) after that many seconds without a request.
SIGTERM or Ctrl-C stops it. A bad argument or seed exits with status 2.
`octoemu --fidelity` prints the fidelity table.

One port serves everything, routed by path:

| Path | Surface |
|---|---|
| `/` | REST API (`api.github.com`) |
| `/login/oauth/*` (any `/login/…`) | github.com's web pages |
| `/avatars/*` (`/avatars/u/{id}`) | avatars (`avatars.githubusercontent.com`) |
| `/__octoemu/*` | admin API |

## Seed

JSON (the binary's `--seed`, `POST /__octoemu/seed`) or the Rust builder in
`octoemu::seed`. Every list is optional; unknown fields are rejected.

```json
{
  "settings": {"start_time": "2030-01-01T00:00:00Z", "refuse_assumed": false,
               "user_tokens_per_hour": 10},
  "users": [{"id": 1, "login": "octocat", "name": "The Octocat",
             "emails": [{"email": "octocat@example.com", "primary": true, "verified": true,
                         "visibility": "public"},
                        {"email": "work@example.com", "verified": false}],
             "noreply_email": true},
            {"id": 3, "login": "hubot", "type": "Bot"}],
  "orgs": [{"id": 2, "login": "acme", "plan": {"name": "team", "seats": 10},
            "created_at": "2020-01-01T00:00:00Z", "two_factor_requirement_enabled": false,
            "default_repository_permission": "read",
            "outside_collaborators_restricted": false}],
  "teams": [{"id": 3, "org": "acme", "slug": "core", "privacy": "closed",
             "parent": null, "repositories": [{"repo": "acme/widgets", "role": "push"}]}],
  "repos": [{"id": 4, "owner": "acme", "name": "widgets", "private": true,
             "visibility": "private"}],
  "apps": [{"id": 5, "slug": "invite-bot", "client_id": "Iv23.example", "client_secret": "…",
            "public_keys": ["-----BEGIN PUBLIC KEY-----\n…"],
            "webhook": {"url": "http://127.0.0.1:8787/webhooks", "secrets": ["…"]},
            "callback_urls": ["http://127.0.0.1:8787/callback"],
            "permissions": {"members": "write", "metadata": "read"}, "events": ["member"],
            "user_permissions": {"email_addresses": "read"}, "expire_user_tokens": true,
            "suspended": false}],
  "installations": [{"id": 6, "app_id": 5, "account": "acme",
                     "repository_selection": "selected", "repositories": ["acme/widgets"],
                     "permissions": {"members": "write", "metadata": "read"},
                     "suspended_at": null, "suspended_by": null}],
  "installation_requests": [{"id": 7, "app_id": 5, "account": "acme", "requester": "octocat"}],
  "collaborators": [{"repo": "acme/widgets", "user": "octocat", "role": "push"}],
  "repository_roles": [{"id": 8, "org": "acme", "name": "auditor", "base_role": "maintain"}],
  "repository_invitations": [{"id": 9, "repo": "acme/widgets", "invitee": "hubot",
                              "inviter": "octocat", "role": "auditor",
                              "created_at": "2029-12-30T00:00:00Z"}],
  "memberships": [{"org": "acme", "user": "octocat", "role": "admin", "state": "active"}],
  "team_memberships": [{"team": "acme/core", "user": "octocat", "role": "maintainer"}],
  "org_invitations": [{"id": 10, "org": "acme", "invitee": "hubot", "role": "direct_member",
                       "inviter": "octocat", "team_ids": [3], "team_roles": {"3": "maintainer"},
                       "created_at": "2029-12-30T00:00:00Z"},
                      {"id": 11, "org": "acme", "email": "new@example.com",
                       "failed_at": "2029-12-31T00:00:00Z", "failed_reason": "Invitation expired"}]
}
```

Validation reports every problem at once, each with its JSON path
(`installations[0].app_id: no App with ID 99`). It checks unique IDs, logins
(users and organizations share one namespace, ignoring case), slugs and client
IDs; that every reference resolves to the right kind of account; that
installations only select their account's repositories and never widen the
App's permissions or events; that App public keys parse as RSA; that
installation requests name an App, an account and a requesting user; that team
memberships rest on an organization membership (an active one for an active
team membership); that a custom role exists in the repository's organization
and is not one of GitHub's names; that a team's parent is a team of the
same organization without cycles and its repositories are the
organization's; that an organization invitation names exactly one of a
user (without a membership or another open invitation) and an email
address, teams of that organization and one inviter; that an invitation names a user who is
neither a collaborator nor invited already, and one inviter (a user or an
App); that only an organization's repository is `internal`; and that a
user's `emails` are addresses, unique, with exactly one primary (the only
one with a `visibility`) which `email`, if also set, must name. A user's
`email` alone stands for one primary, verified, public address. A merged seed is validated with the existing world and
applies whole or not at all; it may not set `start_time`.

## Admin API (`/__octoemu/`)

| Route | Does |
|---|---|
| `GET /health` | `{"status": "ok", "version": …}` |
| `GET /state` | The world (as changed by suspensions and uninstalls), the virtual time, the call count, minted installation tokens, user tokens and PATs (`user_tokens`), refresh tokens (`refresh_tokens`) as redacted IDs with their scope, user, expiry and revocation (never the token), and the `oauth` settings; App client secrets and webhook secrets redacted |
| `POST /seed` | Merges a seed; 422 `{"message": "octoemu: seed rejected", "errors": [{path, message}]}` |
| `GET /clock` | `{"now", "advanced_seconds"}` |
| `POST /clock/advance` | `{"seconds": N}` moves the clock forward (never back) |
| `GET /calls` | `{"calls": […], "dropped": n}` |
| `DELETE /calls` | Empties the call log (204) |
| `GET /fidelity` | The manifest as JSON |
| `POST /tokens/installation` | `{"installation_id", "permissions"?, "repositories"?, "repository_ids"?}`: mints an installation token without a JWT, answered (narrowed, refused) exactly as `POST /app/installations/{id}/access_tokens` |
| `POST /tokens/user` | `{"login", "app_id"}` mints a `ghu_` user access token of that App (with a `ghr_` refresh token when its tokens expire; counted like a new grant), `{"login", "kind": "pat", "scopes"}` a classic `ghp_` PAT: 201 `{token, token_id, expires_at, refresh_token}`; 422 for an unknown user or App |
| `GET /oauth` | The authorize page's settings: `{auto_consent, deny_all, denied_users, user_tokens_per_hour}` |
| `POST /oauth` | Changes any of those |
| `POST /oauth/deny` | `{"login"}`: that user declines every authorization (`access_denied`); empty body: everyone |
| `DELETE /oauth/deny` | Nobody declines any more |
| `POST /installations` | `{"installation": <seed installation>, "sender"?: login}` installs an App: 201 and `installation.created`; 422 with the seed problems |
| `POST /installations/{id}/suspend` | Suspends an installation (204; 404 if unknown), `installation.suspend`; optional body `{"sender"}` |
| `POST /installations/{id}/unsuspend` | Lifts a suspension, `installation.unsuspend` |
| `DELETE /installations/{id}` | Uninstalls: the installation disappears, its tokens stop working, `installation.deleted` |
| `POST /installations/{id}/permissions` | `{"permissions", "sender"?}`: the owner accepts them (never wider than the App's), `installation.new_permissions_accepted` |
| `POST /installations/{id}/repositories` | `{"repository_selection"? (default `selected`), "repositories", "sender"?}`: the new selection; `installation_repositories.added` for gained and `removed` for lost repositories |
| `PUT /apps/{id}/webhook` | A seed webhook (`{"url", "secrets", "active"}`) replaces the App's (204) |
| `DELETE /apps/{id}/webhook` | No more deliveries for the App |
| `GET /events` | `{"events": […]}`: the webhook outbox, oldest first |
| `GET /deliveries` | `{"deliveries": […], "mode", "held", "pending"}`: every attempt, oldest first |
| `POST /deliveries/dispatch` | Sends what manual mode holds: `{"dispatched": n}` |
| `POST /deliveries/wait` | `{"timeout_ms"?}` (default 30 000): 200 `{"idle": true}` once nothing is queued or in flight (held ones aside), 504 at the timeout |
| `POST /deliveries/mode` | `{"mode": "auto"\|"manual"}`; switching to auto sends what was held |
| `POST /act/{login}/repository_invitations/{id}/accept` | The invitee accepts: collaborator with the invited role, `member.added` (204; 404 if not theirs or expired) |
| `POST /act/{login}/repository_invitations/{id}/decline` | The invitee declines (204, no event) |
| `POST /act/{login}/repos/{owner}/{repo}/leave` | A collaborator leaves (204, `member.removed`) |
| `POST /act/{login}/orgs/{org}/accept` | The user accepts their invitation or pending membership, as `PATCH /user/memberships/orgs/{org}` (204; 404 without one) |
| `POST /act/{login}/orgs/{org}/decline` | The user declines their invitation (204, no event) |
| `POST /act/{login}/orgs/{org}/leave` | A member leaves (204, `organization.member_removed`) |
| `POST /orgs/{org}/spammed` | `{"spammed": bool}`: the organization's invitation writes (create, cancel) answer GitHub's `spammed` 422 (assumed shape) while on (204; 404 unknown) |
| `POST /users/{login}/rename` | `{"login"}`: the user changes their login; ID, collaborations, memberships and invitations stay theirs (`user-rename`, documented). 204; 404 unknown; 422 for a taken login or a user owning repositories, an App or an installation (an emulator limitation: GitHub redirects a renamed owner's repositories) |
| `POST /orgs/{org}/rename` | `{"login"}`: the organization changes its login; ID, members, teams, invitations and installations stay its own (`org-rename`, documented). 204; 404 unknown; 422 for a taken login or an organization owning repositories (an emulator limitation: GitHub redirects their old names) |
| `POST /repository_invitations/{id}/expire` | Dates the invitation back past its 7 days, so it is expired now without moving the clock for everything else (204; 404) |
| `GET /faults` | `{"faults": […]}`: every [fault rule](#fault-rules) with `id`, its fields, `matched`, `fired`, `held` and `released` |
| `POST /faults` | Adds a rule: 201 with it as listed; 422 for a bad pattern, method, status or header |
| `DELETE /faults` | Removes every rule (204), letting held requests go on |
| `GET /faults/{id}` | One rule (404 if unknown) |
| `DELETE /faults/{id}` | Removes it (204; 404), letting its held requests go on |
| `POST /faults/{id}/release` | A hold, hold-response or stall rule lets what it holds go on (a stall ends) and holds no more (204; 404) |
| `POST /faults/{id}/wait` | `{"held"?: n (1), "timeout_ms"?}`: 200 `{"held": n}` once the hold, hold-response or stall rule holds at least `n` requests, 504 at the timeout (default 30 s) |

**Virtual clock.** It starts at `settings.start_time` (or the real time),
ticks with real time, and jumps on advance. Every response, admin included,
carries `Date` from it (IMF-fixdate, `Tue, 01 Jan 2030 00:00:00 GMT`):
octoauth reads `Date` for JWT clock skew and token issued-at.

**Call log.** One record per API, web or avatar request (admin requests are
not recorded): `seq`, `at` (virtual), `method`, `path`, `surface`, `route`
(the matched `METHOD /pattern`, `null` for the 501 fallback), `credential`,
`identity`, `status`, `request_id`. `credential` is the `Authorization`
header's shape (`none`, `jwt`, `installation_token`, `user_token`,
`oauth_token`, `personal_access_token`, `basic`, `unknown`), never its value.
`identity` is who the route verified the caller to be:
`{"kind": "app", "app_id"}`, `{"kind": "installation", "app_id",
"installation_id", "token_id"}` (`token_id` is the first 16 bytes of the
token's SHA-256 in hex, as octoauth's `TokenId` prints it),
`{"kind": "user_token", "app_id", "user_id", "login", "token_id"}`,
`{"kind": "personal_access_token", "user_id", "login", "token_id"}` or
`{"kind": "client", "app_id"}`, `null` without or with a rejected
credential (when a fault rule answers instead of the route: the
credential's owner, if it is valid). Query values
named like secrets (`code`, `state`, `client_secret`, `*token*`, …) become
`<redacted>`, and octoscrub runs over the rest. Bodies are never recorded,
except `body_fields`: the top-level JSON request body fields that say what
was asked for (`permission`, `permissions`, `role`, `state`, `invitee_id`,
`team_ids`, `repository_ids`, `repositories`), never free text, addresses or
secrets. A call a fault rule fired on carries `fault: {"rule", "action",
"handler_status"?}` (`handler_status`: what the real handler answered,
when it ran) and `status` 0 when the connection was closed without a
response. The newest 10 000 records are kept.

**Response basics.** API responses carry `x-ratelimit-*` (stub values; no
per-credential budget yet) and every response an `X-GitHub-Request-Id`. Lists
are paged GitHub's way: `per_page` (default 30, silently capped at 100),
`page`, and a `Link` header with `prev`, `next`, `last` and `first`. Errors use GitHub's
`{"message", "documentation_url", "status"}`, `status` being the HTTP status as
a string (`"404"`); 422s add `errors[{resource, field, code}]`. The web flow's
token endpoint answers OAuth's `{"error", "error_description", "error_uri"}`
instead.
An `X-GitHub-Api-Version` other than `2022-11-28` or `2026-03-10` is a 400
`Bad Request` whose `errors` names the value and the supported versions, on
every REST route, before the credential (as GitHub answers; the docs
disagree on 400 or 410); without the header the default version applies. Users, organizations, teams and repositories
carry every field their OpenAPI schema requires (`simple-user`'s and
`organization-simple`'s URL templates, a repository's `*_url` templates,
clone URLs, zeroed counts and `has_*` flags, a team's `type`), checked by
the fidelity suite's schema check; counts GitHub keeps (stars, forks,
issues, size) are always 0.

## Fault rules

Injected failures on the API, web and avatar surfaces (never the admin
API), for testing a client's failure handling. **A fault-injected response
is not GitHub behaviour**: it has no fidelity label, refuse-assumed mode
does not touch it, and nothing in it is a claim about GitHub. Every call a
rule fires on is marked in the call log.

```json
{"method": "PUT", "path": "/repos/{owner}/{repo}/collaborators/{username}",
 "credential": "installation_token", "body_contains": "\"admin\"",
 "nth": 2, "times": 1, "label": "lost write",
 "action": {"kind": "apply_then_lose", "status": 502}}
```

- **Matching.** `method` (any when absent or `*`); `path`, matched against
  the request path without the query: literal segments, axum-style
  `{param}` (one segment) and `{*rest}` (the rest), `*` (one segment, or a
  run of characters within one) and `**` (any number of segments);
  `credential` (the call log's credential kinds); `body_contains` (a
  substring of the raw body); `query_contains` (a substring of the query
  string, e.g. `page=2`).
- **Counting.** A rule counts every request it matches and fires from the
  `nth` (default 1) for `times` requests (default: every one after). A
  rule that matches without firing lets later rules look; the first rule
  that fires decides. Rules apply in the order they were added.
- **Actions** (`kind`):
  - `respond`: `status`, `headers`, a JSON `body` or a `text` body; the
    handler does not run, so nothing changes (a 503 on a read, a 401 once,
    a secondary-rate-limit 403 with `Retry-After`);
  - `delay`: `ms`, `clock: "real"` (the response comes that much later) or
    `"virtual"` (the virtual clock jumps first; expiry sees the time
    pass), then the handler runs;
  - `hold`: the request waits until the rule is released, removed or the
    emulator stops, then the handler runs; `POST /faults/{id}/wait`
    (`Emulator::wait_for_held`) returns once requests are held, so a test
    can act while a write is in flight;
  - `hold_response`: the handler runs (its changes happen), then its
    response waits until the rule is released, removed or the emulator
    stops, so a later request can overtake it (a slow answer to an applied
    write); `wait` counts held responses;
  - `stall`: the upstream stops answering and the handler does not run.
    `phase: "headers"`: no response starts (the call is logged at once,
    with `status` 0); `phase: "body"`: the status (`status`, default 200),
    `application/json` and the first bytes of a JSON body are sent, and the
    body never finishes. A stall lasts until the client gives up or the
    rule is released or removed (or the emulator stops); then the
    connection closes, the body unfinished. For testing a client's
    deadlines;
  - `apply_then_lose`: the handler runs (its changes happen), then its
    response is lost: `status` (e.g. 502, with a short text body) is sent
    instead, or, without `status`, the connection closes unanswered (the
    stub's `loseDeleteResponse`);
  - `close`: the connection closes without a response; the handler does
    not run;
  - `reset`: the connection is reset (TCP RST); the handler does not run;
  - `custom` (embedded only, `FaultAction::custom`): a Rust closure gets the
    method, path, pattern parameters and credential kind and returns the
    response.
- **Rust.** `Emulator::add_fault(Fault::on("PUT", "/repos/**",
  FaultAction::Hold).once())` returns the rule's ID; `octoemu::faults` has
  the builder (`Fault`, `FaultAction`, `Reply`, `DelayClock`).
- **Connections.** octoemu runs its own HTTP/1.1 accept loop (hyper) so a
  rule can drop the connection a request came on.
- The organization `spammed` switch (`POST /orgs/{org}/spammed`) stays an
  `assumed` GitHub behaviour, not a fault rule.

## Fidelity

`fidelity.toml` labels every implemented route (`[routes."GET /app"]`) and
every modelled behaviour (`[behaviours.date-header]`):

| Label | Fields | Meaning |
|---|---|---|
| `verified` | `date = "YYYY-MM-DD"`, `scenario` | The live fidelity suite measured it (GE-08) |
| `documented` | `doc` (https URL) | GitHub's documentation says so |
| `assumed` | `reason` | A guess |

New routes go through the registry in `src/api.rs`; a test (and
`Emulator::start`) fails when a registered route or behaviour has no entry, or
an entry names nothing implemented.

`[scenarios]` maps each [fidelity-suite](#fidelity-suite-octoemu-conformance)
scenario ID to the entries it measures (route keys and behaviour IDs). A
`verified` entry must name a listed scenario, and the suite's tests fail
when its scenarios and the table disagree. `octoemu --fidelity` prints
every entry with its label, the scenarios measuring it (`SCENARIOS`, `-`
for none) and its detail, then the counts per label.

**Refuse-assumed mode** (`settings.refuse_assumed`, `Builder::refuse_assumed`,
`--refuse-assumed`) makes guesses fail loudly: an `assumed` route answers 501
`{"message": "octoemu: refusing assumed behaviour", "route": …, "reason": …}`,
a handler relying on an `assumed` behaviour answers the same with
`"behaviour"` (a JWT accepted only on the `iat` tolerance, a revoked or
suspended token's refusal, a token scope that relies on the union or the
implicit `metadata`, …), `assumed` headers (today the rate-limit stub and
the request ID) are left out, and an error whose status is documented but
whose text is a guess keeps its status with the message
`octoemu: assumed message withheld`.

The first live run of the fidelity suite (2026-11-24) measured many of
the `assumed` entries; their reasons now say what GitHub answered, and
where it contradicted the guess, octoemu was changed to match. They stay
`assumed` until a live run stamps them `verified` (GE-10).

`assumed` today: `jwt-error-messages`, `jwt-iat-tolerance`,
`jwt-exp-after-iat`, `wrong-credential-kind`, `installation-token-format`,
`token-error-messages`, `token-repositories-union`,
`token-implicit-metadata`, `token-narrowed-selection` (left out),
`token-revocation-propagation`, `token-revocation-immediate` (an
uninstall),
`suspended-installation`, `suspended-installation-tokens`,
`rate-limit-headers`, `request-id-header`, `unsupported-api-version`
(refused), `secondary-rate-limit` (a label: no secondary limit is
modelled); and for repositories
`reput-pending-invitation` (201 with the same invitation, role kept),
`reput-expired-invitation` (replaced by a new one),
`expired-invitations-listed`, `accept-expired-invitation` (refused),
`invitation-alias-links`, `custom-role-invitation-permissions`,
`collaborator-affiliation` (beyond `direct`), `permission-without-access`
(`none`/`none`), `remove-non-collaborator` (204), `unknown-user-404`,
`out-of-scope-repository` (403), `repo-error-messages`,
`outside-collaborator-policy` (refused),
`member-edited-changes` (left out of the payload in refuse-assumed mode),
`member-added-directly` (not emitted in refuse-assumed mode) and
`app-bot-account`; for webhooks `webhook-secret-rotation`,
`webhook-ordering`, `webhook-delivery-retention`, `webhook-hook-id`,
`webhook-status-texts` (withheld), `webhook-deliveries-cursor` (refused
with a cursor), `webhook-config-details`, `installation-event-sender`,
`installation-events-while-suspended`, `installation-repositories-events`
and `suspended-installation-webhooks` (delivery runs in the background,
so most of these are labels only, not refused); for OAuth and user tokens
`oauth-error-status` (HTTP 200 on token errors; refused),
`bad-refresh-token-text` (withheld), `unsupported-grant-type`,
`pkce-exchange-errors`, `authorize-page-errors`,
`redirect-uri-loopback-port`, `user-token-cap`, `user-token-hourly-limit`,
`user-token-invitations` (a `ghu_` token on the invitee routes),
`pat-scopes`, `client-credential-errors`, `applications-errors` and
`noreply-email`; for organizations `org-admin-fields` (withheld),
`org-error-messages` (withheld), `org-invitation-limit-response`,
`org-no-seat`, `org-spammed`, `expired-org-invitations-failed`,
`org-invitation-cancel`, `org-invitation-decline`, `org-invitation-roles`,
`org-reput-pending`, `team-put-non-member`,
`invited-team-membership-pending`, `org-accept-team-events` (not emitted),
`remove-org-non-member`, `remove-team-non-member`, `org-out-of-scope`,
`email-invitation-accept` and `user-token-org-membership-list`; for
`GET /user/installations/{installation_id}/repositories`
`user-installations-pat` (refused), `user-installation-access` (refused
for a suspended installation or one the user reaches nothing of) and
`user-installation-repository-selection` (left out); the routes `GET /repositories/{repository_id}/invitations`,
`GET /user/{account_id}`, `GET /avatars/u/{id}` and
`POST /login/oauth/authorize` (the consent form).

## Fidelity suite (`octoemu-conformance`)

The sibling crate [`crates/octoemu-conformance`](../octoemu-conformance)
(native only, not published) runs one set of scenarios against an embedded
octoemu or against real GitHub (plan §4, GE-08).

- **Scenarios** use public GitHub routes only. On the emulator each starts
  from a fresh instance seeded with the fidelity world (an organization,
  a private repository with the live one's settings (no wiki, downloads or
  forking), a team, the fidelity App installed with
  `administration: write`, `members: write`, `metadata: read` and the
  `member`/`organization`/`membership` events and the homepage URL the
  setup guide registers, an invitee outside the organization with a PAT, a
  sign-in App, an organization that keeps outside collaborators to owners,
  a paid one with every seat filled).
  Seeding is setup and may use the admin API; the one admin call a step
  makes is the clock.
- **Snapshots** (`snapshots/<scenario>.json`): each step's request line,
  status, the headers relied on (`content-type`, `link`, `location`,
  `retry-after`, `x-accepted-github-permissions`, `x-github-sso`,
  `x-ratelimit-remaining`, `x-ratelimit-resource`) and the body,
  normalised (IDs, timestamps, tokens, GUIDs, origins and the world's
  names become placeholders; profile fields and counts, the App's
  `installations_count` among them, become `<varies>`). Compare mode
  checks a run against them, ignoring field order; fields one side lacks
  are listed (on the emulator: unmodelled), not failures. The first
  snapshots were generated from octoemu (`"source": "emulator
  (unverified)"`). **The first live record run (2026-11-24, a throwaway
  Free organization) replaced 11 of them with GitHub's answers
  (`"source": "live"`)**; the 9 it skipped (destructive, slow, or needing
  an optional extra: a consent-screen token, a policy or a full paid
  organization) are still the emulator's. The emulator run is compared
  with GitHub's answers, and an emulator record run never overwrites a
  live snapshot. The live
  target pointed at another endpoint (`OCTOEMU_LIVE_API_BASE`, e.g. an
  `octoemu` binary for a smoke test) records `"source": "custom endpoint
  (not GitHub)"` and never overwrites a live snapshot either.
- **Cleanup** is checked: a scenario whose cleanup request fails (an
  invitation, collaboration, membership or suspension left behind) fails,
  even when its steps passed or skipped.
- **Schema check**: every response is validated with the `jsonschema`
  crate against `schemas/rest-api-subset.json`, the operations octoemu
  implements cut out of [github/rest-api-description](https://github.com/github/rest-api-description)
  at commit `c9c95fe34ced05fe076001f5c55704742bc8e7f8`
  (`descriptions-next/api.github.com/api.github.com.2022-11-28.json`,
  OpenAPI 3.1.0, sha256 `42f012de…a33aad` in the file's `source`). An
  error status the description does not list is checked as `basic-error`;
  an undocumented success status, a body where none is documented and a
  non-JSON body where JSON is are violations. Violations name the
  operation, the instance pointer and the keyword, never the response's
  values.
  `schemas/exceptions.toml` holds documented exceptions where the
  published schema is known to be wrong (none yet). To move the pin,
  download the description into a fresh directory outside the
  repository and run `schemas/extract.py` (stdlib Python, run with
  `python3 -I`; its docstring has the commands); a test fails when octoemu
  gains a REST route the subset lacks.

| Scenario | Measures | On live |
|---|---|---|
| `jwt-error-matrix` | App JWT rejections and their messages | runs |
| `jwt-skew-tolerance` | how far ahead `iat` and `exp` may be | runs |
| `token-downscoping-errors` | narrowing an installation token | runs |
| `token-revocation-delete` | the token's shape, how soon `DELETE /installation/token` takes effect | runs |
| `token-revocation-uninstall` | tokens after the App uninstalls itself | destructive (reinstall by hand) |
| `suspended-installation` | suspend and unsuspend, tokens, minting | runs (always unsuspends) |
| `oauth-exchange-errors` | token endpoint errors: status and body | runs |
| `user-permissionless` | `/user`, `/user/emails` with a permission-less user token | needs `OCTOEMU_LIVE_USER_TOKEN` (consent screen), else skipped |
| `reput-pending-invitation` | re-PUT on a pending repository invitation | runs |
| `expired-invitation` | an invitation past 7 days | live-only/slow: skipped (needs 7 days) |
| `repo-invitation-limit` | 50 invitations per repository per day | destructive |
| `outside-collaborator-policy` | PUT for a non-member under the owner-only policy | needs the policy organization, else skipped |
| `member-edited-changes` | the `member.edited` payload's `changes` (via the deliveries API) | runs |
| `org-membership-accept` | pending, accept (202), active | runs |
| `org-invitation-limit` | the organization invitation budget | destructive |
| `org-spammed` | the `spammed` body | emulator only (GitHub decides; skipped live) |
| `org-no-seat` | no seat left | needs a full paid organization, else skipped |
| `team-put-non-member` | team PUT for a non-member | runs |
| `secondary-limit-shape` | a secondary rate limit's shape | destructive; skipped on the emulator (not modelled) |
| `api-version-unsupported` | 400 or 410 for an unsupported API version | runs |

Run it (from `libs/octoauth`):

```console
$ cargo test -p octoemu-conformance -- --nocapture            # emulator, compare; prints the report
$ OCTOEMU_CONFORMANCE=record cargo test -p octoemu-conformance --test emulator
$ OCTOEMU_CONFORMANCE=record \
    cargo test -p octoemu-conformance --test live -- --ignored --nocapture
```

Variables: `OCTOEMU_CONFORMANCE` (`compare`, the default, or `record`),
`OCTOEMU_CONFORMANCE_ONLY` (comma-separated scenario IDs),
`OCTOEMU_CONFORMANCE_REPORT` (write the report as JSON there),
`OCTOEMU_LIVE_DESTRUCTIVE=1` and `OCTOEMU_LIVE_SLOW=1` (live only: the
emulator always runs destructive scenarios). The emulator run fails on a
mismatch, a failed scenario or a schema violation; a live run fails on a
failed scenario or a mismatch against a live snapshot (against an
emulator snapshot the diff is reported as the fidelity signal). CI runs
the emulator target in the toolkit job; the manual workflow
`.github/workflows/fidelity-live.yml` (`workflow_dispatch` only) runs the
live target from repository secrets.

**The live env file** is `.octoemu-live/live.env` at the repository root
(ignored by the root `.gitignore`), or wherever `OCTOEMU_LIVE_ENV` points.
Inside the repository, the suite refuses an env file or a private key git
does not ignore; a relative `OCTOEMU_LIVE_APP_PRIVATE_KEY_PATH` is resolved
against the env file's directory. Format: `KEY=VALUE` lines, `#` comments
(also after an unquoted value), unknown keys refused. Setting up the Apps,
organizations and users it names, step by step:
[`docs/live-github-tests.md`](../../../../docs/live-github-tests.md).
Placeholder values:

```sh
# The fidelity App: a GitHub App installed on OCTOEMU_LIVE_ORG with
# Administration: write, Members: write, Metadata: read, subscribed to the
# Member, Organization and Membership events, with an active webhook (any
# URL; deliveries are read back through the deliveries API).
OCTOEMU_LIVE_APP_ID=123456
OCTOEMU_LIVE_APP_CLIENT_ID=Iv23.exampleclientid       # optional: the JWT iss (else the App ID)
OCTOEMU_LIVE_APP_PRIVATE_KEY_PATH=fidelity-app.pem   # next to this file
OCTOEMU_LIVE_APP_WEBHOOK_SECRET=REPLACE_WITH_THE_WEBHOOK_SECRET  # optional, reserved
OCTOEMU_LIVE_INSTALLATION_ID=12345678
# A fresh free organization, one repository and one team in it.
OCTOEMU_LIVE_ORG=example-fidelity-org
OCTOEMU_LIVE_REPO=sandbox
OCTOEMU_LIVE_TEAM=sandbox-team
# The invitee: a user outside the organization, and their classic PAT
# (scopes repo, user, admin:org).
OCTOEMU_LIVE_INVITEE_LOGIN=example-invitee
OCTOEMU_LIVE_INVITEE_PAT=REPLACE_WITH_THE_INVITEES_CLASSIC_PAT
# The sign-in App: a GitHub App without user permissions (its OAuth client).
OCTOEMU_LIVE_SIGNIN_CLIENT_ID=Iv23.examplesignin
OCTOEMU_LIVE_SIGNIN_CLIENT_SECRET=REPLACE_WITH_THE_CLIENT_SECRET
# Optional: the invitee's user access token of the sign-in App (from the
# consent screen) for user-permissionless.
OCTOEMU_LIVE_USER_TOKEN=REPLACE_WITH_A_USER_ACCESS_TOKEN
# Optional: the fidelity App installed on an organization where only owners
# may invite outside collaborators, with a repository (outside-collaborator-policy).
OCTOEMU_LIVE_POLICY_INSTALLATION_ID=23456789
OCTOEMU_LIVE_POLICY_ORG=example-restricted-org
OCTOEMU_LIVE_POLICY_REPO=vault
# Optional: the fidelity App installed on a paid organization with every
# seat filled (org-no-seat).
OCTOEMU_LIVE_NO_SEAT_INSTALLATION_ID=34567890
OCTOEMU_LIVE_NO_SEAT_ORG=example-full-org
# Optional overrides.
OCTOEMU_LIVE_API_BASE=https://api.github.com
OCTOEMU_LIVE_WEB_BASE=https://github.com
```

Scenarios clean up after themselves (pending invitations, the invitee's
collaboration and membership, a suspension), so the live organization
stays as it was, except after the destructive ones. Stamping `verified`
into `fidelity.toml` from a live run, the weekly re-verify and the k-of-n
report are GE-10.

## Tests

```bash
cd libs/octoauth
cargo test -p octoemu
cargo clippy -p octoemu --all-targets -- -D warnings
```

`tests/repos.rs` covers the repository surface over HTTP: the 201/204
split, role mapping and custom roles, pagination and the alias `Link`,
expiry and the daily limit on the clock, the admin `act` routes, the
outbox, permission and scope refusals, 422s, users, avatars and
refuse-assumed mode. In the ghinvite workspace,
`crates/ghinvite-github/tests/emulator.rs` runs ghinvite-github's real
repository client against an embedded emulator.

`tests/webhooks.rs` runs a local axum receiver: the signature (checked
against the docs' test vector), the header set, payloads, unsigned and
inactive webhooks, the rotation model, manual and auto dispatch with
`wait`, failures on a 500, a stall past the timeout and a refused
connection, no retries, the deliveries API (cursor pages, `status`,
redelivery, cross-App 404s), the hook configuration, the installation
events of every admin action, the suspension rule and `member` events. In
the ghinvite workspace, `crates/ghinvite-web/tests/emulator_webhooks.rs`
feeds ghinvite-web's real webhook intake from the emulator (signature,
rotation, dedupe).

`tests/oauth.rs` covers the OAuth web flow and user tokens: the full flow
with PKCE (authorize, exchange, `/user`, `/user/emails`, revoke, then 401),
the token endpoint's error matrix (reuse, expiry, redirect, verifier,
secret, unverified email), form and JSON answers, refresh rotation, a
non-expiring App, the authorize errors, the HTML page and its form, the
token caps, the `/applications` routes, admin-minted tokens and PATs, user
tokens on `GET /repos/…`, invitations accepted and declined through the
user routes (with `member.added` delivered under manual dispatch) and
refuse-assumed mode. `crates/octologin/tests/emulator.rs` runs octologin's
sign-in against an embedded emulator (including `access_denied` and the
`Incomplete` rule for more than 100 addresses); in the ghinvite workspace,
`crates/ghinvite-web/tests/emulator_signin.rs` drives ghinvite-web's real
`/login` → authorize → `/oauth/callback` through it.

`tests/orgs.rs` covers the organization surface: `GET /orgs/{org}` by
permission, invitations by user and by email with teams, their teams and
pagination, the 422s, acceptance through `PATCH /user/memberships/orgs`
(memberships and team memberships activate, team grants apply, the events
are delivered under manual dispatch), email acceptance, expiry into the
failed list, the 50/500 budget on the clock, seats, membership and team
PUT/DELETE, the `act` routes, permission and scope refusals, `spammed` and
refuse-assumed mode. In the ghinvite workspace,
`crates/ghinvite-github/tests/emulator_orgs.rs` rebuilds the GitHub side
of the JS org drivers over ghinvite-github's real organization client, and
`crates/ghinvite-web/tests/emulator_webhooks.rs` feeds the organization
events to ghinvite-web's org webhook intake.

`tests/faults.rs` covers fault rules over HTTP: matching by method,
pattern, credential and body, `nth`/`times`, every action (a dropped
connection is an error at the client, a lost response still changes
state, a virtual delay expires a token), holds and their release, the
admin API and the call log's marks. In the ghinvite workspace,
`ghinvite_e2e::Harness::start` serves the Restate workflow suites
(`crates/ghinvite-workflows/tests`) an emulator seeded with
`ghinvite_e2e::github::seed`, and `ghinvite_e2e::github::World` takes their
GitHub-side actions through the admin API.

`tests/app_auth.rs` runs octoauth's App client and octotoken's provider
against an embedded emulator: minting, pagination, expiry on the clock and
`with_token`'s replacement, revocation, suspension, unknown installations,
narrowing, clock-skew correction through `Date`, and the negative JWT and
token cases.
