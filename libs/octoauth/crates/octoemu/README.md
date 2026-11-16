# octoemu

A stateful local emulator of the GitHub surface a GitHub App uses (REST, App
and installation auth, the OAuth web flow, webhook delivery), for tests.
Native only (on wasm32 the crate is empty). Not published.

Plan, design and task list: [`docs/specs/github-emulator.md`](../../../../docs/specs/github-emulator.md)
in the ghinvite repository. Modelled on AWS's emulator: real checks, explicit
gaps, fidelity measured against the real service.

## Status: organizations (GE-06)

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
(GE-06).

### Implemented routes

| Route | Credential | Notes |
|---|---|---|
| `GET /app` | App JWT | The authenticated App |
| `GET /app/installations` | App JWT | Paged |
| `GET /app/installations/{installation_id}` | App JWT | 404 for an unknown installation or another App's |
| `POST /app/installations/{installation_id}/access_tokens` | App JWT | 201 `{token, expires_at, permissions, repository_selection, repositories?}`; body `permissions`, `repositories`, `repository_ids` narrow |
| `PUT`/`DELETE /app/installations/{installation_id}/suspended` | App JWT | 204 |
| `GET /app/installation-requests` | App JWT | Paged |
| `GET /app/hook/config` | App JWT | `{url, content_type: "json", secret: "********", insecure_ssl}`; 404 without a webhook |
| `PATCH /app/hook/config` | App JWT | `url`, `content_type` (`json` only), `secret` (replaces every seeded secret; `""` stops signing), `insecure_ssl` |
| `GET /app/hook/deliveries` | App JWT | Newest first; `per_page`, `cursor` (`next`/`prev` in `Link`), `status=success\|failure` (200–399 is a success) |
| `GET /app/hook/deliveries/{delivery_id}` | App JWT | With `url`, `request {headers, payload}`, `response {headers, payload}`; 404 for another App's |
| `POST /app/hook/deliveries/{delivery_id}/attempts` | App JWT | 202 `{}`; the same body and GUID again, `redelivery: true` |
| `GET /installation/repositories` | installation token, `metadata: read` | Paged; only the token's repositories |
| `DELETE /installation/token` | installation token | 204; the token stops working |
| `GET /repos/{owner}/{repo}` | installation or user token, `metadata: read` | Private and internal repositories outside the token's reach are 404 |
| `GET /repos/{owner}/{repo}/collaborators` | installation token, `metadata: read` | Paged; `affiliation=direct\|outside\|all` (default all); each with `permissions` and `role_name` |
| `PUT /repos/{owner}/{repo}/collaborators/{username}` | installation token, `administration: write` | Body `{permission}` (`pull`, `triage`, `push` (default), `maintain`, `admin` or a custom role); 201 with the invitation, 204 for a user with access (a role change, an organization member added directly); 422 for another value or past 50 invitations per 24 h |
| `DELETE /repos/{owner}/{repo}/collaborators/{username}` | installation token, `administration: write` | 204 |
| `GET /repos/{owner}/{repo}/collaborators/{username}/permission` | installation token, `metadata: read` | `permission` (`admin\|write\|read\|none`), `role_name` (exact, custom included), `user`; the highest role across sources |
| `GET /repos/{owner}/{repo}/invitations` | installation token, `administration: read` | Paged; `Link` URLs on the alias below |
| `GET /repositories/{repository_id}/invitations` | installation token, `administration: read` | The same list by repository ID |
| `PATCH /repos/{owner}/{repo}/invitations/{invitation_id}` | installation token, `administration: write` | Body `{permissions}` (`read`, `write`, `triage`, `maintain`, `admin`); 200 with the invitation |
| `DELETE /repos/{owner}/{repo}/invitations/{invitation_id}` | installation token, `administration: write` | 204 |
| `GET /users/{username}` | none needed | Users and organizations |
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
| `PATCH /user/memberships/orgs/{org}` | user token, `members: write` | `{"state": "active"}`: accepts; 200 (GitHub's asynchronous 202 is not modelled) |
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
  (`{slug}[bot]`, `type: "Bot"`, ID 1 000 000 000 + App ID).
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
  with. Errors (`incorrect_client_credentials`, `redirect_uri_mismatch`,
  `bad_verification_code`, `bad_refresh_token`, `unverified_user_email` for
  an unverified primary address, `unsupported_grant_type`) carry GitHub's
  documented `error_description` and `error_uri`, with HTTP 200 (assumed;
  refused in refuse-assumed mode).
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
  added directly), `edited` (a role change, with `changes`) and `removed`
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
     at most 30 s in the future; `exp` an integer, in the future, at most
     600 s after the virtual clock's now and after `iat`. Rejections carry
     GitHub's community-reported messages (`A JSON web token could not be
     decoded`, `Integration not found`, the `'Issued at' claim ('iat')…` and
     `'Expiration time' claim ('exp')…` texts). A JWT sent as `token <jwt>`
     is refused. octoauth's skew correction works: the emulator's `Date`
     tells it how far off it is.
   - **Installation token** (`Bearer ghs_…` or `token ghs_…`): looked up on
     every call. Unknown, forged, expired (1 h, on the virtual clock),
     revoked or uninstalled: 401 `Bad credentials`. Suspended: 403 `This
     installation has been suspended`, until unsuspended.
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
about 520 characters, signed with a key drawn per instance (the inner
claims are octoemu's own; treat tokens as opaque). A token's scope is the
installation's grant (its `permissions`, else the App's; its repository
selection) narrowed by the request:

- `permissions` may only ask for granted permissions at granted levels or
  lower; anything else is 422. A token narrowed without `metadata` keeps
  `metadata: read` when the installation has it.
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
`mint_personal_access_token`, `deny_authorization`, `allow_authorization`
and `set_auto_consent`.

**Binary**, for the Worker drivers, browser tests and other processes:

```console
$ octoemu --port 0 [--bind 127.0.0.1] [--seed seed.json] [--idle-timeout SECS] [--refuse-assumed]
{"api_base":"http://127.0.0.1:41234","web_base":"http://127.0.0.1:41234","admin_base":"http://127.0.0.1:41234/__octoemu","port":41234,"pid":4242}
```

Exactly one JSON line on stdout, once listening; nothing else goes to stdout.
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
            "default_repository_permission": "read"}],
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
credential. Query values
named like secrets (`code`, `state`, `client_secret`, `*token*`, …) become
`<redacted>`, and octoscrub runs over the rest. Bodies are never recorded.
The newest 10 000 records are kept.

**Response basics.** API responses carry `x-ratelimit-*` (stub values; no
per-credential budget yet) and every response an `X-GitHub-Request-Id`. Lists
are paged GitHub's way: `per_page` (default 30, silently capped at 100),
`page`, and a `Link` header with `prev`, `next`, `last` and `first`. Errors use GitHub's
`{"message", "documentation_url"}`; 422s add `errors[{resource, field, code}]`.

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

`assumed` today: `jwt-error-messages`, `jwt-iat-tolerance`,
`jwt-exp-after-iat`, `wrong-credential-kind`, `installation-token-format`,
`token-error-messages`, `token-repositories-union`,
`token-implicit-metadata`, `token-revocation-immediate`,
`suspended-installation`, `suspended-installation-tokens`,
`rate-limit-headers`, `request-id-header`; and for repositories
`reput-pending-invitation` (201 with the same invitation, role updated),
`reput-expired-invitation` (replaced by a new one),
`expired-invitations-listed`, `accept-expired-invitation` (refused),
`invitation-alias-links`, `custom-role-invitation-permissions`,
`collaborator-affiliation` (beyond `direct`), `permission-without-access`
(`none`/`none`), `remove-non-collaborator` (204), `unknown-user-404`,
`out-of-scope-repository` (403), `repo-error-messages`,
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
`email-invitation-accept` and `user-token-org-membership-list`; the routes `GET /repositories/{repository_id}/invitations`,
`GET /user/{account_id}`, `GET /avatars/u/{id}` and
`POST /login/oauth/authorize` (the consent form).

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

`tests/app_auth.rs` runs octoauth's App client and octotoken's provider
against an embedded emulator: minting, pagination, expiry on the clock and
`with_token`'s replacement, revocation, suspension, unknown installations,
narrowing, clock-skew correction through `Date`, and the negative JWT and
token cases.
