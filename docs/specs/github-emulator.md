# octoemu: a GitHub App emulator, plan

| Field | Value |
|---|---|
| Project | `octoemu` (working name), a stateful local emulator of the GitHub surface a GitHub App uses: REST, App and installation auth, the OAuth web flow, webhook delivery |
| Status | 2026-11-16. Plan accepted (§7); GE-01 to GE-07 done. Research: prior art, a code inventory, and GitHub's documented behaviour (sources linked inline) |
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
- **Compatibility:** the stub's control routes (`/identity`, `/outcomes`, `/calls`, `/reset`, …) get a temporary shim under `/__octoemu/compat/`, so the about 40 workflow test files can switch over before they're rewritten. The shim is deleted in GE-09.

### 3.5 Fidelity labels

Every route and every modelled behaviour has an entry in a checked-in manifest (`fidelity.toml`), with one of these labels:
- `verified` + date + scenario id, set by the live suite;
- `documented` + doc link;
- `assumed` + reason.

`octoemu --fidelity` prints the table. Tests can ask the emulator to refuse `assumed` behaviour, so a test that would rest on a guess fails loudly instead of passing quietly.

## 4. Fidelity suite

- **One scenario crate, `octoemu-conformance`, run against `Target::Emulator` or `Target::Live`.**
  - The live target reads credentials from an env file outside the repository: the test Apps, the org and the invitee's personal access token, as in the earlier answer on live gates.
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
  
  Two documentation conflicts stay open until measured: 400 vs 410 for a bad API version, and which permission delivers `member` events.

## 5. ghinvite integration

- **Config seam.** Today no runtime reads a GitHub base URL: `DEFAULT_API_BASE` and literal `https://api.github.com` are hardcoded in both binaries and both Workers, and `SignInApp{web,api}_base_url` is always `None`. Add `GITHUB_API_BASE`, `GITHUB_WEB_BASE` and `GITHUB_AVATAR_BASE` and plumb them to `WebGithub`, `RepositoryGithub`, `parse_*_apps` and `SignInApp`.
  - The overrides are accepted only outside the production stage.
  - Validation is unified on octoauth's rule: https, or http on loopback only. Today `RestEnv::parse_api_base` allows http on any host (`crates/ghinvite-github/src/rest.rs:129-140`), while octoauth and octologin don't.
- **Workers.** Miniflare `outboundService` keeps intercepting the real hostnames and forwards them to the emulator's `api_base`. That interception is the only seam on Workers until the config change reaches them.
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
| GE-08 | Fidelity suite: scenario crate, both targets, normalised snapshots, schema check, manual live workflow | 03–06 | Emulator target green. **First live run needs operator credentials** |
| GE-09 | ghinvite migration: config seam, JS drivers off their inline fakes, browser sign-in through the emulator, delete `stub.rs` and the compat shim | 07, 05 | `rg` finds no inline GitHub fake in `tests/worker`; `stub.rs` gone; CI green |
| GE-10 | Weekly live re-verify, k-of-n report, `--fidelity` table in the README | 08 | First scheduled run recorded |

**Effort, UNVERIFIED.**
- GE-01–07 is about the size of the kekring redesign: a few agent sessions each, mostly sequential.
- GE-08's emulator half fits in one session; its live half waits on credentials.
- GE-09 is the long tail: about 17 JS drivers and about 40 Rust test files, mechanical once the shim exists.

**Order with the live gates.** GE-08's first live run is the API-spike work from the live-gates answer. If credentials arrive early, run SP-01 first, without waiting for the emulator: its kill criterion decides whether M1 keeps org targets, which decides whether GE-06 is needed at all.

## 7. Decisions

Accepted on 2026-11-15:

1. **Name and home: `octoemu`, at `libs/octoauth/crates/octoemu`,** a native-only crate in the toolkit workspace. This lifts the HANDOFF §4 deferral for the emulator only. Extracting ghinvite-github's transport, REST and pagination plumbing stays deferred. On 2026-11-15, `octoemu`, `octofake`, `octosim`, `octostub`, `octomock` and `github-emulator` were all unregistered on crates.io.
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
