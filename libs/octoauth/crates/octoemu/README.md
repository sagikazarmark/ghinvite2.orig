# octoemu

A stateful local emulator of the GitHub surface a GitHub App uses (REST, App
and installation auth, the OAuth web flow, webhook delivery), for tests.
Native only (on wasm32 the crate is empty). Not published.

Plan, design and task list: [`docs/specs/github-emulator.md`](../../../../docs/specs/github-emulator.md)
in the ghinvite repository. Modelled on AWS's emulator: real checks, explicit
gaps, fidelity measured against the real service.

## Status: repositories (GE-03)

Built: the server, the two modes, the seed model and its validation, the
virtual clock, the admin API, the call log, the fidelity manifest (GE-01);
App JWT verification, stateless installation tokens with scoping,
revocation and suspension, and per-route credential and permission checks
(GE-02); repositories, collaborators, permissions, repository invitations
with expiry and the daily limit, the invitee's side (through the admin
API until GE-05), users, avatars and the webhook event outbox (GE-03).

### Implemented routes

| Route | Credential | Notes |
|---|---|---|
| `GET /app` | App JWT | The authenticated App |
| `GET /app/installations` | App JWT | Paged |
| `GET /app/installations/{installation_id}` | App JWT | 404 for an unknown installation or another App's |
| `POST /app/installations/{installation_id}/access_tokens` | App JWT | 201 `{token, expires_at, permissions, repository_selection, repositories?}`; body `permissions`, `repositories`, `repository_ids` narrow |
| `PUT`/`DELETE /app/installations/{installation_id}/suspended` | App JWT | 204 |
| `GET /app/installation-requests` | App JWT | Paged |
| `GET /installation/repositories` | installation token, `metadata: read` | Paged; only the token's repositories |
| `DELETE /installation/token` | installation token | 204; the token stops working |
| `GET /repos/{owner}/{repo}` | installation token, `metadata: read` | Private and internal repositories outside the token are 404 |
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

Every other API and web route answers:

```http
HTTP/1.1 501 Not Implemented
content-type: application/json; charset=utf-8

{"message":"octoemu: not implemented","route":"GET /repos/acme/widgets","documentation_url":"…"}
```

It is never a 404, which a client could take for GitHub's answer. Routes
arrive with GE-04 (webhooks), GE-05 (OAuth web flow, `/user`, the invitee's
`/user/repository_invitations/{id}`) and GE-06 (organizations); anything
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
  repository, owning its organization (admin), and the organization's base
  permission for active members. Team grants arrive with GE-06.
- **Invitations** expire 7 days after `created_at` on the virtual clock and
  then show `expired: true` (they stay listed until deleted or replaced).
  A repository gets at most 50 invitations per sliding 24 hours through
  the API.
- **What the App does** is attributed to its bot account
  (`{slug}[bot]`, `type: "Bot"`, ID 1 000 000 000 + App ID).
- **The invitee's side** (accept, decline, leave) are domain operations
  (`src/collaboration.rs`), reached today through the admin API's `act`
  routes; GE-05's user-token routes call the same functions.

### Event outbox

Every change GitHub would announce by webhook appends an event to an
outbox: `{"id", "at", "event", "action", "targets": [{"app_id",
"installation_id"}], "payload"}`. Targets are the non-suspended
installations that can access the repository and subscribe to the event;
the payload leaves out `installation`, which each delivery adds. Today:
`member` `added` (an invitation accepted, or an organization member added
directly), `edited` (a role change, with `changes`) and `removed`
(`DELETE …/collaborators/{username}` or leaving). Read it at
`GET /__octoemu/events` or `Emulator::events()`; GE-04 delivers it.

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
   - Anything else (user tokens until GE-05, PATs, Basic): 401 `Bad
     credentials`.
2. **The right kind.** No credential on a route that needs one: 401
   `Requires authentication`. The wrong kind: 401.
3. **The route's permission**, for installation tokens: without it, 403
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

`Emulator::builder(seed)` adds `.bind(addr)`, `.idle_timeout(d)` and
`.refuse_assumed()`. The handle also has `avatar_base`, `admin_base`, `now`,
`seed` (merge more), `state`, `clear_calls`, `stopped`,
`suspend_installation`, `unsuspend_installation` and `uninstall`.

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
  "settings": {"start_time": "2030-01-01T00:00:00Z", "refuse_assumed": false},
  "users": [{"id": 1, "login": "octocat", "name": "The Octocat", "email": "octocat@example.com"},
            {"id": 3, "login": "hubot", "type": "Bot"}],
  "orgs": [{"id": 2, "login": "acme", "plan": {"name": "team", "seats": 10},
            "created_at": "2020-01-01T00:00:00Z", "two_factor_requirement_enabled": false,
            "default_repository_permission": "read"}],
  "teams": [{"id": 3, "org": "acme", "slug": "core", "privacy": "closed"}],
  "repos": [{"id": 4, "owner": "acme", "name": "widgets", "private": true,
             "visibility": "private"}],
  "apps": [{"id": 5, "slug": "invite-bot", "client_id": "Iv23.example", "client_secret": "…",
            "public_keys": ["-----BEGIN PUBLIC KEY-----\n…"],
            "webhook": {"url": "http://127.0.0.1:8787/webhooks", "secrets": ["…"]},
            "callback_urls": ["http://127.0.0.1:8787/callback"],
            "permissions": {"members": "write", "metadata": "read"}, "events": ["member"]}],
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
  "team_memberships": [{"team": "acme/core", "user": "octocat", "role": "maintainer"}]
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
and is not one of GitHub's names; that an invitation names a user who is
neither a collaborator nor invited already, and one inviter (a user or an
App); and that only an organization's repository is `internal`. A merged seed is validated with the existing world and
applies whole or not at all; it may not set `start_time`.

## Admin API (`/__octoemu/`)

| Route | Does |
|---|---|
| `GET /health` | `{"status": "ok", "version": …}` |
| `GET /state` | The world (as changed by suspensions and uninstalls), the virtual time, the call count, and minted tokens as redacted IDs with their scope and expiry (never the token); App client secrets and webhook secrets redacted |
| `POST /seed` | Merges a seed; 422 `{"message": "octoemu: seed rejected", "errors": [{path, message}]}` |
| `GET /clock` | `{"now", "advanced_seconds"}` |
| `POST /clock/advance` | `{"seconds": N}` moves the clock forward (never back) |
| `GET /calls` | `{"calls": […], "dropped": n}` |
| `DELETE /calls` | Empties the call log (204) |
| `GET /fidelity` | The manifest as JSON |
| `POST /tokens/installation` | `{"installation_id", "permissions"?, "repositories"?, "repository_ids"?}`: mints an installation token without a JWT, answered (narrowed, refused) exactly as `POST /app/installations/{id}/access_tokens` |
| `POST /installations/{id}/suspend` | Suspends an installation (204; 404 if unknown) |
| `POST /installations/{id}/unsuspend` | Lifts a suspension |
| `DELETE /installations/{id}` | Uninstalls: the installation disappears and its tokens stop working |
| `GET /events` | `{"events": […]}`: the webhook outbox, oldest first |
| `POST /act/{login}/repository_invitations/{id}/accept` | The invitee accepts: collaborator with the invited role, `member.added` (204; 404 if not theirs or expired) |
| `POST /act/{login}/repository_invitations/{id}/decline` | The invitee declines (204, no event) |
| `POST /act/{login}/repos/{owner}/{repo}/leave` | A collaborator leaves (204, `member.removed`) |

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
`{"kind": "app", "app_id"}` or `{"kind": "installation", "app_id",
"installation_id", "token_id"}` (`token_id` is the first 16 bytes of the
token's SHA-256 in hex, as octoauth's `TokenId` prints it), `null` without
or with a rejected credential. Query values
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
`app-bot-account`; the routes `GET /repositories/{repository_id}/invitations`,
`GET /user/{account_id}` and `GET /avatars/u/{id}`.

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

`tests/app_auth.rs` runs octoauth's App client and octotoken's provider
against an embedded emulator: minting, pagination, expiry on the clock and
`with_token`'s replacement, revocation, suspension, unknown installations,
narrowing, clock-skew correction through `Date`, and the negative JWT and
token cases.
