# octoemu: a GitHub App emulator, plan

| Field | Value |
|---|---|
| Project | `octoemu` (working name), a stateful local emulator of the GitHub surface a GitHub App uses: REST, App and installation auth, the OAuth web flow, webhook delivery |
| Status | 2026-11-24. Plan accepted (§7); GE-01 to GE-09 done (GE-08's live half: the first live record run, 2026-11-24); GE-10 open. Research: prior art, a code inventory, and GitHub's documented behaviour (sources linked inline) |
| Model | AWS's [aws/aws-emulator](https://github.com/aws/aws-emulator) (anykms CI already runs its AWS suite against it, anykms#45): real auth, explicit gaps, fidelity measured by one suite run against both the real service and the emulator |
| Replaces | `crates/ghinvite-github/src/stub.rs` and the inline GitHub fakes in about 17 `tests/worker/*.mjs` drivers; later the wiremock OAuth fakes in `ghinvite-web` |
| Does not replace | The live and deployed gates: SP-16, AC-LIVE-1/2/3, the `docs/deploy.md` checklist, #61. `deploy/github-apps/README.md` already says fixture proofs don't satisfy SP-19a |

**Conventions.** Task IDs `GE-nn`. One task is one agent session and one PR. **UNVERIFIED** marks a claim no source or test backs yet. [D] documented by GitHub, [C] community-reported, [M] must be measured.

---

## 1. Why

1. **The existing fakes disagree with each other, and none is checked against GitHub.** The code inventory found:
   - **Rate limits in four shapes:** the stub sends 403 + `Retry-After: 1`; the corpus sends 429 + `retry-after`; `retained_delivery.rs` sends 429 with `{}`; the octoauth tests send `x-ratelimit-remaining: 0`.
   - **`GET /app/installations/{id}` in three shapes.**
   - **`access_tokens` responses that differ by file.**
   - **Barely any `Link` pagination:** only `installation.mjs` sends it.
   - **Client IDs that differ:** `Iv1.signin` in Rust vs `Iv.fixture-signin` in JS.
   - **Helpers copied 4–6 times** across the JS drivers.
   - **A partial stub state model:**
     - a 201 PUT never makes the user a collaborator;
     - there is no route to accept an invitation;
     - there are no org or team routes;
     - `creator` is hardcoded as admin.
2. **Unverified GitHub behaviour is encoded as fact in several places:**
   - the 201/204 split on PUT collaborator;
   - `GET /user/{id}`;
   - `X-GitHub-SSO` parsing;
   - the 403 outside-collaborator texts;
   - the `spammed` shape.
   
   An emulator with a fidelity suite turns each of these into a measured fact, or a labelled assumption.
3. **Sign-in can't be tested end to end today.** GitHub's authorize page needs a real browser and account. An emulated authorize page (pick a seeded user, auto-consent) makes the whole stack, including the browser tests, runnable with no human step.
4. **GitHub changed under the fakes.**
   - Installation tokens are now stateless `ghs_<APPID>_<JWT>`, about 520 characters ([rollout](https://github.blog/changelog/2026-10-02-stateless-github-app-installation-tokens-rolled-out/)). The fakes mint `test-token-for-{id}` or similar.
   - octoscrub's patterns and octoauth's 8 KiB cap already handle the new format, but no test feeds them a realistic token.
   - API version `2026-03-10` now exists; ghinvite pins `2022-11-28`, which is still the default ([versions](https://docs.github.com/en/rest/about-the-rest-api/api-versions)).

## 2. Build or reuse

Build. Nothing that exists is both embeddable in Rust tests and checked against real GitHub.

| Candidate | Verdict |
|---|---|
| [vercel-labs/emulate](https://github.com/vercel-labs/emulate) (TypeScript, Apache-2.0, active) | **Closest, and the design reference.** It is stateful, verifies RS256 App JWTs, mints installation tokens with permissions, has an authorize page with seeded users and sends HMAC-signed webhooks. Gaps for us: no `/orgs/{org}/invitations`; `PUT /orgs/{org}/memberships` goes `active` immediately, where GitHub makes it `pending`; no conformance against real GitHub; Node only, so it can't be embedded in Rust tests |
| @simulacrum/github-api-simulator | Doesn't verify tokens |
| fakehub (Rust) | Stalled at 0.0.10 (2024); no App auth, OAuth or webhooks |
| @kie/mock-github, octokit/fixtures, Prism with the GitHub spec | Stubs, replays, or stateless shape mocks |
| Gitea / Forgejo | Their own `/api/v1`; no GitHub App model |

Lessons taken from the mature emulators:
- **[aws-emulator](https://docs.aws.amazon.com/emulator/latest/userguide/fidelity.md):**
  - real auth with per-run credentials;
  - 501 `NotImplemented` for anything not built;
  - a public fidelity suite in which each test is verified against real accounts;
  - per-service k-of-n scores published with each release;
  - an embedded mode with state per instance;
  - `-p 0` plus a JSON startup line.
- **[LocalStack parity tests](https://docs.localstack.cloud/contributing/parity-testing/):** snapshots recorded against the real service, normalised by transformers, then replayed against the emulator, with a "validated" marker per test.
- **[octokit/fixtures](https://github.com/octokit/fixtures):** normalised recordings, plus a scheduled re-record job that opens a PR when GitHub's responses change.
- **[stripe-mock](https://github.com/stripe/stripe-mock):** shows what happens without this discipline. It is OpenAPI-driven and stateless, and Stripe itself says behaviour is out of its scope.
- **Known failure modes to avoid:**
  - an emulator that accepts what the real service refuses (Azurite, fake-gcs-server);
  - an emulator that never models security behaviour (aws-emulator and Firebase both say so).

## 3. Design

### 3.1 Shape

- **Implementation:** Rust and axum, with in-memory state behind one lock. There is no persistence; each test gets a fresh instance.
- **Two modes:**
  - **Embedded:** `octoemu::Emulator::start(seed)` in a test returns a handle with `api_base`, `web_base`, an admin client, and drop-to-stop.
  - **Binary:** `octoemu --port 0 --seed seed.json` prints one JSON line with `{api_base, web_base, admin_base, pid}`, for the Worker drivers, the browser tests and the native D1 bridge tests. An `--idle-timeout` flag stops forgotten instances.
- **One port, routed by path:**
  - the API at `/`;
  - github.com's OAuth pages at `/login/oauth/*`;
  - avatars at `/avatars/u/{id}`;
  - the admin API under `/__octoemu/`.
  
  Control routes stay off the API namespace: the stub's `/installation-repositories` is one character away from the real `/installation/repositories`.
- **Unimplemented routes return 501**, with `{"message":"octoemu: not implemented","route":…}`. They never return 404, which a client could treat as a real answer.
- **Seed:** users, orgs (plan, seats, age), teams, repos, Apps (RSA public key, client ID and secret, webhook URL and secrets, permissions, events), installations, and existing collaborators and memberships. The seed is JSON plus a Rust builder.

### 3.2 Real checks (no shortcuts)

| Area | Behaviour | Source |
|---|---|---|
| App JWT | Verified with RS256 against the App's registered public key. Checks `iss` (client ID or App ID), `exp` at most 10 min ahead, and `iat`. Errors are 401 with GitHub's messages | [D] [JWT](https://docs.github.com/en/apps/creating-github-apps/authenticating-with-a-github-app/generating-a-json-web-token-jwt-for-a-github-app); messages [C]; skew tolerance [M] |
| Installation tokens | `ghs_<APPID>_<JWT>`, about 520 characters and signed by the emulator. 1 h expiry. Scope narrows by `permissions` / `repositories` / `repository_ids` (at most 500) and can never widen. The response has `permissions`, `repository_selection` and `repositories`. `DELETE /installation/token` revokes | [D] [REST](https://docs.github.com/en/rest/apps/apps#create-an-installation-access-token-for-an-app); error texts [M] |
| Permission enforcement | Each route declares the permission it needs. A token without it gets 403 `Resource not accessible by integration` with `X-Accepted-GitHub-Permissions` | [D] [troubleshooting](https://docs.github.com/en/rest/using-the-rest-api/troubleshooting-the-rest-api); per-route table from [permissions](https://docs.github.com/en/rest/authentication/permissions-required-for-github-apps) |
| Suspension | Suspended installations refuse token creation and API calls | [D] that access is blocked; status and message [M] |
| OAuth web flow | The authorize page lists the seeded users (or honours `login=`). `redirect_uri` must match a registered callback. PKCE is S256 only. A code lasts 10 min, works once, and is bound to its client and redirect. The token response is `ghu_…` with `expires_in` 28800, `ghr_…` refresh (single use), `token_type` bearer. Error codes are GitHub's; the status code is [M]. `DELETE /applications/{client_id}/token` and `/grant` use Basic auth | [D] [user tokens](https://docs.github.com/en/apps/creating-github-apps/authenticating-with-a-github-app/generating-a-user-access-token-for-a-github-app), [refresh](https://docs.github.com/en/apps/creating-github-apps/authenticating-with-a-github-app/refreshing-user-access-tokens), [errors](https://docs.github.com/en/apps/oauth-apps/maintaining-oauth-apps/troubleshooting-oauth-app-access-token-request-errors) |
| Webhooks | Real HTTP delivery to the App's URL with `X-GitHub-Event`, `X-GitHub-Delivery` (GUID), `X-Hub-Signature-256` (HMAC over the body with the App's secret), the `X-GitHub-Hook-Installation-Target-*` headers and `User-Agent: GitHub-Hookshot/…`. 10 s timeout, no automatic retry. Every attempt is recorded and served at `GET /app/hook/deliveries[/{id}]` and `POST …/attempts` (JWT auth) | [D] [events](https://docs.github.com/en/webhooks/webhook-events-and-payloads), [deliveries API](https://docs.github.com/en/rest/apps/webhooks) |
| Errors | `{message, documentation_url}`; 422 adds `errors[{resource, field, code}]`. Private resources return 404, never 403 | [D]; whether a `status` field is sent [M] |
| Rate limits | `x-ratelimit-*` headers on every response, and a primary budget per token. Secondary limits come only from injected faults, since GitHub's exact shape is [M] | [D] [limits](https://docs.github.com/en/rest/using-the-rest-api/rate-limits-for-the-rest-api) |
| Pagination | `link` with next/prev/first/last; `per_page` above 100 silently becomes 100; the default is 30 | [D] [pagination](https://docs.github.com/en/rest/using-the-rest-api/using-pagination-in-the-rest-api) |
| Clock | One virtual clock, advanced through the admin API. The `Date` header follows it: octoauth uses `Date` for JWT skew and token issued-at. Expiry of invitations (7 d), tokens, codes and the invite budgets runs on it | — |

### 3.3 State model (ghinvite's surface first)

The route list comes from the code inventory (`crates/ghinvite-github/src/endpoints.rs:159-326` and the octoauth and octologin clients). About 40 routes.

- **Repos:**
  - Collaborators carry a role: `pull/triage/push/maintain/admin` or a custom role.
  - `PUT …/collaborators/{u}` returns 201 with a new invitation, or 204 for a user who already has access. A permission change also returns 204 [D].
  - `GET …/permission` returns `permission` and `role_name` as the highest role across all sources, including team and org-base inheritance.
  - Invitations: list, PATCH, DELETE; 7-day expiry with the `expired` flag; 50 per repo per 24 h.
  - The invitee accepts via `PATCH /user/repository_invitations/{id}`, which fires `member.added`.
  - The `/repositories/{id}/invitations` alias that ghinvite's pagination follows.
- **Orgs:**
  - `GET /orgs/{org}` with `plan{seats, filled_seats}`, the 2FA flag and the default permission.
  - Invitations by `invitee_id` or `email`, `role`, `team_ids`; 50 or 500 per 24 h depending on plan and age [D]; a seat check on paid plans.
  - `failed_invitations`, `invitations/{id}/teams`, DELETE.
  - Memberships `pending`/`active`, accepted with `PATCH /user/memberships/orgs/{org}`.
  - Team memberships `pending`/`active`.
  - `organization.member_invited|member_added|member_removed` and `membership.added|removed` (team scope) events.
- **Installations and apps:**
  - `GET /app/installations/{id}`, `/installation/repositories` (paged), `/app/installation-requests`.
  - `installation.*` and `installation_repositories.*` events when admin actions change them.
- **Users:**
  - `/user`, `/user/emails` (needs the Email addresses permission), `/users/{login}`, `/user/{id}`.
  - Avatars: a PNG served by id.

Expiry and SP-07-style edge cases (a re-PUT on a pending or expired invitation; team PUT for a non-member) start as **assumed** behaviour, each with a label (§3.5), until the fidelity suite has measured them.

### 3.4 Admin API (`/__octoemu/`)

- **State:**
  - `GET /state` dumps all state;
  - `POST /seed` adds to it;
  - `POST /clock/advance`;
  - `POST /act/{user}/…` runs actions as a seeded user: accept, decline or leave. These are thin wrappers over the real user-token routes, so the user-side paths are exercised too;
  - `POST /tokens/user` mints a user token for a seeded user without the browser.
- **Observation:** `GET /calls` records method, path, credential kind and status, with secrets redacted. `GET /deliveries` lists webhook attempts.
- **Faults:** rules match on method, path pattern, credential and the nth call. Actions:
  - return a given status, body and headers;
  - delay;
  - hold until released;
  - apply then lose the response (the existing `loseDeleteResponse`);
  - close the connection.
  
  This covers the stub's 7 `/outcomes` and the JS drivers' `heldPut`, `networkFault` and `observe` hooks.
- **Compatibility (removed):** GE-07 gave the stub's control routes (`/identity`, `/outcomes`, `/calls`, `/reset`, …) a temporary shim under `/__octoemu/compat/`, so the about 40 workflow test files could switch over before they were rewritten. GE-09c rewrote them on the native seed and admin API (`ghinvite_e2e::github`) and deleted the shim.

### 3.5 Fidelity labels

Every route and every modelled behaviour has an entry in a checked-in manifest (`fidelity.toml`), with one of these labels:
- `verified` + date + scenario id, set by the live suite;
- `documented` + doc link;
- `assumed` + reason.

`octoemu --fidelity` prints the table. Tests can ask the emulator to refuse `assumed` behaviour, so a test that would rest on a guess fails loudly instead of passing quietly.

## 4. Fidelity suite

- **One scenario crate, `octoemu-conformance`, run against `Target::Emulator` or `Target::Live`.**
  - The live target reads credentials from an env file: the test Apps, the org and the invitee's personal access token, as in the earlier answer on live gates. It lives in `.octoemu-live/` at the repository root, which git ignores (the suite refuses an unignored path inside the repository), or anywhere `OCTOEMU_LIVE_ENV` names; setup in [`docs/live-github-tests.md`](../live-github-tests.md).
  - Scenarios use only public GitHub routes, never the admin API. The one exception is the clock, which a live run replaces with waiting or skips (7-day expiry is live-only, run once).
- **Snapshots, LocalStack style.**
  - A live run records status, the headers we rely on, and the body, normalised: IDs, timestamps, tokens, GUIDs and URLs become placeholders.
  - The snapshots are checked in, and the emulator run compares against them.
  - Field order and fields we don't model are ignored, but listed.
- **Schema check.** Every emulator response is also validated against the pinned [rest-api-description](https://github.com/github/rest-api-description) 3.1 schemas, with the `jsonschema` crate. Only the operations we use are extracted into the repository, since the bundled spec is about 14 MB. This checks shape; the scenarios check behaviour.
- **Live runs.**
  - **On demand:** a manual CI workflow with repository secrets.
  - **Weekly re-verify:** it stamps `verified` dates and opens a PR when a snapshot changes.
  - **Destructive scenarios** run only on request against a fresh free org, because they use up real budget. Examples: SP-08 (the invite bucket), the 50-per-repo limit, the 10-tokens-per-hour cap.
  - **The release report** is k-of-n per area (auth, repo, org, webhooks, OAuth).
- **Initial scenarios:**
  - **Spikes:** SP-01, SP-07, SP-08, SP-18, SP-19, SP-19a (the parts that don't need GitHub's own consent screen), SP-23.
  - **[M] behaviours** from the research:
    1. JWT error matrix and skew tolerance.
    2. Token down-scoping errors.
    3. Revocation of stateless tokens after DELETE, uninstall or suspend.
    4. Suspended installations.
    5. The status code on OAuth exchange errors.
    6. The 10-token caps for `ghu_` tokens.
    7. Permission-less `/user`.
    8. A re-PUT on a pending invitation.
    9. Expired invitations.
    10. Invite-limit and `spammed` bodies.
    11. The outside-collaborator policy error.
    12. The org no-seat and limit errors, and the 202 on membership accept.
    13. Team PUT for a non-member.
    14. The `member.edited` changes shape.
    15. The secondary-limit shape and the 400 vs 410 for an unsupported API version.
  
  Two documentation conflicts stayed open until measured: 400 vs 410 for a bad API version (measured on 2026-11-24: 400), and which permission delivers `member` events (still open).

## 5. ghinvite integration

- **Config seam.** Today no runtime reads a GitHub base URL: `DEFAULT_API_BASE` and literal `https://api.github.com` are hardcoded in both binaries and both Workers, and `SignInApp{web,api}_base_url` is always `None`. Add `GITHUB_API_BASE`, `GITHUB_WEB_BASE` and `GITHUB_AVATAR_BASE` and plumb them to `WebGithub`, `RepositoryGithub`, `parse_*_apps` and `SignInApp`.
  - The overrides are accepted only outside the production stage.
  - Validation is unified on octoauth's rule: https, or http on loopback only. Today `RestEnv::parse_api_base` allows http on any host (`crates/ghinvite-github/src/rest.rs:129-140`), while octoauth and octologin don't.
- **Workers.** The drivers point the Workers' `runtime-tests` builds at the emulator binary with the `GITHUB_*_BASE` bindings (GE-09a, GE-09b); no Miniflare `outboundService` intercepts GitHub's hostnames. A driver whose `outboundService` handles another origin (Restate) passes the emulator's requests through unchanged.
- **"Production" (GE-09a).** No configuration variable names a production stage (`P1_APP_STAGE` is `repo`/`repo-and-org`), so each composition root decides from what it already is: `ghinvite-web-server` without `--local-dev`, `ghinvite-workflows-server` without `--local-unsigned`, and Worker builds without the `runtime-tests` feature (the deployable ones) are production and refuse the three variables. The Workers' `runtime-tests` builds accept them as bindings.
- **What stays.** `MockTransport` and the wiremock unit tests in octoauth, octotoken and octologin stay: they pin exact request bytes, which is unit-level work. The frozen wire corpus (`fixtures/http-fixtures.json`) is regenerated from live snapshots and kept as a regression check.

## 6. Tasks

| ID | Task | Depends | Exit |
|---|---|---|---|
| GE-01 | Crate skeleton: embedded and binary modes, seed, 501 fallback, admin `/state` `/calls` `/clock`, fidelity manifest | — | `cargo test -p octoemu`; startup line; 501 on any unbuilt route. **Done** (#131): `libs/octoauth/crates/octoemu` |
| GE-02 | App auth: JWT verification, stateless installation tokens, scoping, revocation, suspension, permission enforcement | 01 | octoauth's and octotoken's flows pass against the emulator (a new test next to the wiremock ones). **Done** (#132): `crates/octoemu/tests/app_auth.rs` |
| GE-03 | Repo surface: collaborators, permission, invitations, user-side accept and decline, expiry on the clock, `member` events | 02 | ghinvite-github endpoint tests pass in a parallel emulator-backed run. **Done** (#133): `crates/octoemu/tests/repos.rs`, `crates/ghinvite-github/tests/emulator.rs`; user-side accept/decline via the admin `act` routes until GE-05 |
| GE-04 | Webhook delivery and the deliveries API; `installation` / `installation_repositories` events from admin actions | 02 | ghinvite-web webhook intake test fed by the emulator (signature, rotation, dedupe). **Done** (#134): `crates/octoemu/tests/webhooks.rs`, `crates/ghinvite-web/tests/emulator_webhooks.rs`; rotation modelled as "the first seeded secret signs" |
| GE-05 | OAuth web flow, user tokens, `/user`, `/user/emails`, revoke; the authorize page | 01 | octologin `sign_in` tests pass; a ghinvite-web sign-in test without wiremock. **Done** (#135): `crates/octoemu/tests/oauth.rs`, `crates/octologin/tests/emulator.rs`, `crates/ghinvite-web/tests/emulator_signin.rs`; the invitee's `/user/repository_invitations` routes take user tokens; token-error status 200, the token caps and PAT scopes are `assumed` |
| GE-06 | Org surface: invitations, failed invitations, memberships, teams, seats, invite budgets, org events | 03, 04 | The JS org drivers' scenarios rebuilt as emulator tests. **Done** (#136): `crates/octoemu/tests/orgs.rs`, `crates/ghinvite-github/tests/emulator_orgs.rs`, org intake in `crates/ghinvite-web/tests/emulator_webhooks.rs`; `plan` needs `organization_plan` (documented); expiry into `failed_invitations`, the budget/no-seat/`spammed` bodies and team PUT for a non-member are `assumed` |
| GE-07 | Fault rules plus the stub-compatibility shim | 03 | The about 40 workflow test files pass with `ghinvite_e2e::installed_apps` pointed at the emulator. **Done** (#137): `crates/octoemu/src/faults.rs` (admin `/__octoemu/faults`, `Emulator::add_fault`), `src/compat.rs` (`/__octoemu/compat/`, `compat::seed`), `tests/faults.rs`, `tests/compat.rs`; `ghinvite_e2e::Harness::start` serves the emulator and every suite that used the stub runs on it, middleware faults replaced by fault rules; the suites with their own scenario fakes (organization, `email_delivery`, `repository_protocol`, `settlement_evidence`, `sweep_schedule`, `installation_availability`) keep them until GE-09 |
| GE-08 | Fidelity suite: scenario crate, both targets, normalised snapshots, schema check, manual live workflow | 03–06 | Emulator target green. **Done**: emulator half, then the live half. **Emulator half done** (GE-08): `libs/octoauth/crates/octoemu-conformance`. `Target::Emulator` (a fresh embedded emulator per scenario, seeded through the admin API as setup) and `Target::Live` (the env file `OCTOEMU_LIVE_ENV` names, refused inside the repository; `--ignored` test, never in normal CI). 20 scenarios over public routes only (the clock the one exception), IDs in `fidelity.toml`'s new `[scenarios]` table (`octoemu --fidelity` shows them per entry): `jwt-error-matrix`, `jwt-skew-tolerance`, `token-downscoping-errors`, `token-revocation-delete`, `token-revocation-uninstall` (destructive), `suspended-installation`, `oauth-exchange-errors`, `user-permissionless` (live needs a consent-screen token, else skipped), `reput-pending-invitation`, `expired-invitation` (slow: live-only, skipped), `repo-invitation-limit` (destructive), `outside-collaborator-policy` (live needs a policy organization), `member-edited-changes` (payload via the deliveries API), `org-membership-accept`, `org-invitation-limit` (destructive), `org-spammed` (emulator only), `org-no-seat` (live needs a full paid organization), `team-put-non-member`, `secondary-limit-shape` (destructive, live only), `api-version-unsupported`; the `ghu_` token caps (§4 item 6) need the consent screen and have no scenario. Snapshots under `snapshots/`, normalised, generated from the emulator and marked `source: emulator (unverified)`: the first live record run overwrites them, and its diff is the fidelity signal. Schema check with `jsonschema` 0.61.2 against the octoemu operations cut out of github/rest-api-description `c9c95fe` (OpenAPI 3.1, 2022-11-28; `schemas/extract.py`), no exceptions needed. It found octoemu's users, organizations, teams and repositories missing schema-required fields (URL templates, counts, flags, a team's `type`), now fixed. Scenarios brought `DELETE /app/installations/{installation_id}` (documented), the `X-GitHub-Api-Version` 400 (`unsupported-api-version`, assumed), the outside-collaborator policy (`outside-collaborator-policy`, assumed) and the `secondary-rate-limit` label. CI: the emulator run in the toolkit job; `.github/workflows/fidelity-live.yml` (`workflow_dispatch` only) writes the env file from repository secrets. **Live half done**: the first live record run on 2026-11-24 against a throwaway Free organization (credentials in the ignored `.octoemu-live/`, setup in [`docs/live-github-tests.md`](../live-github-tests.md)); 11 scenarios recorded (`source: live`), 9 skipped (destructive: `token-revocation-uninstall`, `repo-invitation-limit`, `org-invitation-limit`, `org-spammed`, `secondary-limit-shape`; slow: `expired-invitation`; optional extras: `user-permissionless`, `outside-collaborator-policy`, `org-no-seat`), whose snapshots stay the emulator's. octoemu was changed to answer as GitHub did: a re-PUT keeps a pending invitation's role; `DELETE /installation/token` takes effect elsewhere after 500 ms on the virtual clock (`token-revocation-propagation`: GitHub accepted 2 of 8 uses right after it, refused every use from 0.78 s, and a second `DELETE` at once), so `token-revocation-delete` now polls and records whether the token is refused within 2 s (re-recorded live); the org accept is a 202; App JWTs allow `iat` 60 s ahead and a numeric string, bound `exp` by now only and carry GitHub's per-case texts; the token endpoint answers an unknown client 404 and reads any other grant type as the code grant; tokens are 383 characters, narrowed tokens answer only the permissions asked and `repository_selection: selected`, with GitHub's 422 texts and `documentation_url`; the suspended-token text, `suspended_by` (the App's bot), the organization installation `html_url`, sorted `events`, the bot's `%5Bbot%5D` URLs and `GET /users/{slug}[bot]`, `member.edited` without `old_permission` and the API-version 400's shape. The measured facts are in each `fidelity.toml` entry; the labels stay until GE-10 stamps `verified`. World differences fixed on the suite side: `installations_count` normalised to `<varies>`, the emulator App's homepage URL and repository settings seeded as the live ones |
| GE-09 | ghinvite migration: config seam, JS drivers off their inline fakes, browser sign-in through the emulator, delete `stub.rs` and the compat shim | 07, 05 | `rg` finds no inline GitHub fake in `tests/worker`; `stub.rs` gone; CI green. **Done** (GE-09a #139; GE-09b #142, #143; GE-09c #144, #145). GE-09a (config seam): `GITHUB_API_BASE`, `GITHUB_WEB_BASE`, `GITHUB_AVATAR_BASE` (`ghinvite_config::github`), plumbed to `WebGithub`, `RepositoryGithub`, `parse_*_apps`, `SignInApp` and avatar reads; refused by production roots (native startup without `--local-dev`/`--local-unsigned`, Worker builds without `runtime-tests`); validation unified on octoauth's rule (`ghinvite_github::rest::parse_base_url`, now also `RestEnv`'s); `crates/ghinvite-web/tests/emulator_config.rs`. GE-09b in two PRs. GE-09b-1 (sign-in): `tests/worker/octoemu.mjs` starts the emulator binary for the Worker drivers (`--port 0 --idle-timeout`, seeded through the admin API, faults and the call log instead of hooks) and the sign-in drivers (`pending-signin`, `signin-cutover`, `domain-signin`, `decision-queue`, `request-history`) run on it through the bindings; the CSP `form-action` names the configured web origin (`middleware::csp::policy`; GitHub's in production), and `tests/browser/emulator-signin.config.mjs` signs a browser in on the emulator's authorize page, including the expired-session bounce. GE-09b-2: the `admission.mjs` family (with `browser-admission` and `invitation-journey`), `operational-signals` and `broker` (now in CI) run on the emulator binary: a native world (`tests/worker/github-world.mjs` or each sub-driver's `seed`), GitHub-side actions through the admin API, fault rules for held/stalled/lost writes and injected refusals, the call log for counts; `build-stub.mjs` and `app-config.mjs`'s `signinRevocation` are gone. octoemu gained the `stall` and `hold_response` fault actions, `query_contains` matching, and the admin `users/{login}/rename` (`user-rename`, documented) and `repository_invitations/{id}/expire` routes. `transport.mjs` and `deadlines.mjs` keep their own `transport.test` upstream: they test the generic transport, not GitHub, and are outside the no-inline-fake check. GE-09c in two PRs. #144: the native D1 cutover rehearsal starts the emulator binary, and both the pinned workflows Worker and the signed `native-bridge-endpoint` example reach it through `GITHUB_API_BASE` (the example requires it; `BRIDGE_FIXTURE_GITHUB_URL` is gone); `ghinvite-github`'s `test-stub` feature, `src/stub.rs` and `examples/stub.rs` are deleted (the frozen wire-corpus router moved into the crate's own tests). #145: `ghinvite_e2e::github` (the suites' seed and `World`, which takes GitHub-side actions through the admin API and stages GitHub's answers to the App's writes as fault rules labelled `world: …`) replaces the shim in every Rust suite, and the shim is deleted; `settlement_evidence`, `sweep_schedule`, `installation_availability`, `email_delivery`, `org_installation_observation`, `organization_reconcile` and `organization_authority` drop their scenario fakes. octoemu gained the admin organization rename (`orgs/{org}/rename`, `org-rename`, documented; an owner of repositories is refused, as for users). **Kept fakes:** `repository_protocol` and `organization_delivery`. Their AC-DLV-3 sequence properties (`tests/common/delivery_sequence.rs`, the Python verifier) script one generated answer per write, including answers GitHub never gives to that route (a 204 to an organization invitation POST). `repository_protocol`'s fake also reads ghinvite's own write fence from SQLite as each request arrives, tells token generations apart by their fixed names, and empties the list right after a counted listing (`presence_listings`). An emulator offering that would misrepresent GitHub, so they stay outside it like `transport.mjs`/`deadlines.mjs`. Follow-up: `canonical_link` signs `creator` and `alice` in through the emulator's OAuth web flow (a seeded sign-in App; authorize with `login=`, the code exchange, `/user`, revocation), and its canned sign-in fault rules are gone, none kept: the token, `/user` and revocation answers are the emulator's own, and the `/user/memberships/orgs/acme` and `/user/installations/1/repositories` rules were dead (sign-in has not called either since ADR 0007 removed user-token reads). octoemu gained `GET /user/installations/{installation_id}/repositories` (documented: a GitHub App user access token, the installation's repositories the user has explicit access to with `permissions`, paged; another App's installation is a 404; assumed: a classic PAT is a 403 (`user-installations-pat`), a suspended installation is always a 404, and an active one the user reaches nothing of is a 404 except an empty list for the account's owner (`user-installation-access`), `repository_selection` is the installation's (`user-installation-repository-selection`)) |
| GE-10 | Weekly live re-verify, k-of-n report, `--fidelity` table in the README | 08 | First scheduled run recorded |

**Effort, UNVERIFIED.**
- GE-01–07 is about the size of the kekring redesign: a few agent sessions each, mostly sequential.
- GE-08's emulator half fits in one session; its live half waited on credentials (done 2026-11-24).
- GE-09 is the long tail: about 17 JS drivers and about 40 Rust test files, mechanical once the shim exists.

**Order with the live gates.** GE-08's first live run is the API-spike work from the live-gates answer. If credentials arrive early, run SP-01 first, without waiting for the emulator: its kill criterion decides whether M1 keeps org targets, which decides whether GE-06 is needed at all.

## 7. Decisions

Accepted on 2026-11-15:

1. **Name and home: `octoemu`, at `libs/octoauth/crates/octoemu`,** a native-only crate in the toolkit workspace. This lifts the HANDOFF §4 deferral for the emulator only. Extracting ghinvite-github's transport, REST and pagination plumbing stays deferred. On 2026-11-15, `octoemu`, `octofake`, `octosim`, `octostub`, `octomock` and `github-emulator` were all unregistered on crates.io.
   **Amended 2026-11-25 (user decision):** octoemu is ghinvite's test infrastructure, not part of the GitHub App toolkit, so the toolkit can be extracted and published without it. It and `octoemu-conformance` moved to their own workspace, `libs/octoemu` (consumed by path, like the toolkit), with the toolkit's dependency versions kept as locked. The toolkit's crates no longer depend on it: octologin's emulator test moved to `libs/octoemu/crates/octoemu/tests/octologin.rs`, and the toolkit's example App tests against wiremock. CI runs it in its own `emulator` job (`dagger check ghinvite:emulator`). Earlier paths in this spec name the old home.
2. **vercel-labs/emulate is a design reference only.** No code is ported from it. Its state model differs from GitHub's in places that matter here, such as membership state.
3. **The first release covers ghinvite's routes only.** Everything else returns 501.

Still open:

4. **Live target.** You need to provide the test Apps, a fresh free org and an invitee account. Also decide whether CI gets them as repository secrets (for GE-08 and GE-10) or whether live runs stay local only. GE-01 to GE-07 don't need this.

## 8. Risks

- **False confidence.** An `assumed` behaviour that is wrong passes tests that would fail against GitHub. Mitigations: the labels, refuse-assumed mode, the live re-verify, and keeping the AC-LIVE gates mandatory.
- **Drift.** GitHub changes things, as the token format just did. The weekly live run and the schema pin catch some of it; nothing catches undocumented behaviour between runs.
- **Too lenient.** The emulator accepting what GitHub refuses (the Azurite and fake-gcs failure mode). Every check in §3.2 has a negative test.
- **Scope creep.** Routes outside ghinvite's surface return 501 until a consumer needs them.
- **Live-run cost.** Real invitations, invite budgets and token caps. Destructive scenarios are opt-in and run on a throwaway org.
