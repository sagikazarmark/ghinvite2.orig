# octoemu

A stateful local emulator of the GitHub surface a GitHub App uses (REST, App
and installation auth, the OAuth web flow, webhook delivery), for tests.
Native only (on wasm32 the crate is empty). Not published.

Plan, design and task list: [`docs/specs/github-emulator.md`](../../../../docs/specs/github-emulator.md)
in the ghinvite repository. Modelled on AWS's emulator: real checks, explicit
gaps, fidelity measured against the real service.

## Status: skeleton (GE-01)

Built: the server, the two modes, the seed model and its validation, the
virtual clock, the admin API, the call log, the fidelity manifest, and the
response basics later routes share.

**No GitHub route exists yet.** Every API and web route answers:

```http
HTTP/1.1 501 Not Implemented
content-type: application/json; charset=utf-8

{"message":"octoemu: not implemented","route":"GET /repos/acme/widgets","documentation_url":"…"}
```

It is never a 404, which a client could take for GitHub's answer. Routes
arrive with GE-02 (App auth), GE-03 (repositories), GE-04 (webhooks), GE-05
(OAuth web flow, `/user`) and GE-06 (organizations); anything outside
ghinvite's surface stays 501.

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
`seed` (merge more), `state`, `clear_calls` and `stopped`.

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
  "users": [{"id": 1, "login": "octocat", "name": "The Octocat", "email": "octocat@example.com"}],
  "orgs": [{"id": 2, "login": "acme", "plan": {"name": "team", "seats": 10},
            "created_at": "2020-01-01T00:00:00Z", "two_factor_requirement_enabled": false,
            "default_repository_permission": "read"}],
  "teams": [{"id": 3, "org": "acme", "slug": "core", "privacy": "closed"}],
  "repos": [{"id": 4, "owner": "acme", "name": "widgets", "private": true}],
  "apps": [{"id": 5, "slug": "invite-bot", "client_id": "Iv23.example", "client_secret": "…",
            "public_keys": ["-----BEGIN PUBLIC KEY-----\n…"],
            "webhook": {"url": "http://127.0.0.1:8787/webhooks", "secrets": ["…"]},
            "callback_urls": ["http://127.0.0.1:8787/callback"],
            "permissions": {"members": "write", "metadata": "read"}, "events": ["member"]}],
  "installations": [{"id": 6, "app_id": 5, "account": "acme",
                     "repository_selection": "selected", "repositories": ["acme/widgets"]}],
  "collaborators": [{"repo": "acme/widgets", "user": "octocat", "role": "push"}],
  "memberships": [{"org": "acme", "user": "octocat", "role": "admin", "state": "active"}],
  "team_memberships": [{"team": "acme/core", "user": "octocat", "role": "maintainer"}]
}
```

Validation reports every problem at once, each with its JSON path
(`installations[0].app_id: no App with ID 99`). It checks unique IDs, logins
(users and organizations share one namespace, ignoring case), slugs and client
IDs; that every reference resolves to the right kind of account; that
installations only select their account's repositories and never widen the
App's permissions or events; that App public keys parse as RSA; and that team
memberships rest on an organization membership (an active one for an active
team membership). A merged seed is validated with the existing world and
applies whole or not at all; it may not set `start_time`.

## Admin API (`/__octoemu/`)

| Route | Does |
|---|---|
| `GET /health` | `{"status": "ok", "version": …}` |
| `GET /state` | The world, the virtual time and the call count; App client secrets and webhook secrets redacted |
| `POST /seed` | Merges a seed; 422 `{"message": "octoemu: seed rejected", "errors": [{path, message}]}` |
| `GET /clock` | `{"now", "advanced_seconds"}` |
| `POST /clock/advance` | `{"seconds": N}` moves the clock forward (never back) |
| `GET /calls` | `{"calls": […], "dropped": n}` |
| `DELETE /calls` | Empties the call log (204) |
| `GET /fidelity` | The manifest as JSON |

**Virtual clock.** It starts at `settings.start_time` (or the real time),
ticks with real time, and jumps on advance. Every response, admin included,
carries `Date` from it (IMF-fixdate, `Tue, 01 Jan 2030 00:00:00 GMT`):
octoauth reads `Date` for JWT clock skew and token issued-at.

**Call log.** One record per API, web or avatar request (admin requests are
not recorded): `seq`, `at` (virtual), `method`, `path`, `surface`, `route`
(the matched `METHOD /pattern`, `null` for the 501 fallback), `credential`,
`status`, `request_id`. `credential` is the `Authorization` header's shape
(`none`, `jwt`, `installation_token`, `user_token`, `oauth_token`,
`personal_access_token`, `basic`, `unknown`), never its value. Query values
named like secrets (`code`, `state`, `client_secret`, `*token*`, …) become
`<redacted>`, and octoscrub runs over the rest. Bodies are never recorded.
The newest 10 000 records are kept.

**Response basics.** API responses carry `x-ratelimit-*` (stub values until
GE-02) and every response an `X-GitHub-Request-Id`. Errors use GitHub's
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
`"behaviour"`, and `assumed` headers (today the rate-limit stub and the
request ID) are left out.

## Tests

```bash
cd libs/octoauth
cargo test -p octoemu
cargo clippy -p octoemu --all-targets -- -D warnings
```
