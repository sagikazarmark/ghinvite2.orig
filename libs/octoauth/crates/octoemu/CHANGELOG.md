# Changelog

All notable changes to this crate are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the crate follows
[Semantic Versioning](https://semver.org/spec/v2.0.0.html): while it is `0.x`, a
minor release may break the API, and every break gets a migration note.

## [Unreleased]

Not yet published; developed inside the ghinvite repository.

### Added

- Fidelity suite support (GE-08, emulator half): `fidelity.toml` gains a
  `[scenarios]` table mapping each `octoemu-conformance` scenario ID to the
  entries it measures (`Manifest::scenarios`, `Manifest::scenarios_for`);
  a `verified` entry must name a listed scenario; `octoemu --fidelity`
  shows a `SCENARIOS` column and counts the covered entries.
- `DELETE /app/installations/{installation_id}` (documented): the App
  uninstalls itself; 204, `installation.deleted` sent by its bot, its tokens
  stop working; 404 for an unknown installation or another App's.
- `X-GitHub-Api-Version` check (`unsupported-api-version`, assumed): a value
  other than `2022-11-28` or `2026-03-10` is a 400 `Unsupported API version
  <value>` on every REST route, before the credential; refused in
  refuse-assumed mode.
- The outside-collaborator policy (`outside-collaborator-policy`, assumed):
  an organization seeded `outside_collaborators_restricted`
  (`Org::restrict_outside_collaborators`) answers an App's collaborator PUT
  for a non-member with 403 `Inviting outside collaborators is restricted by
  organization policy.`; refused in refuse-assumed mode.
- `secondary-rate-limit` (assumed, a label): no secondary limit is modelled;
  the fidelity suite's live run records GitHub's shape.
- `GET /user/installations/{installation_id}/repositories` (documented): a
  GitHub App's user access token lists the installation's repositories the
  user has explicit access to, each with the user's `permissions`, paged,
  with `total_count`; 404 for another App's installation. Assumed, with
  fidelity entries: a classic PAT is a 403 (`user-installations-pat`); a
  suspended installation is always a 404, its account's owner included; an
  active one the user reaches no repository of is a 404, except for its
  account's owner, who gets an empty list (`user-installation-access`);
  `repository_selection` is the installation's
  (`user-installation-repository-selection`).
- Fault rules (GE-07): `POST`/`GET`/`DELETE /__octoemu/faults`, `GET`/`DELETE
  /__octoemu/faults/{id}`, `POST /__octoemu/faults/{id}/release` and
  `…/wait`, and `Emulator::add_fault`, `faults`, `fault`, `remove_fault`,
  `clear_faults`, `release_fault` and `wait_for_held` (`octoemu::faults`).
  Rules match method, path pattern (axum-style `{param}`/`{*rest}`, `*`,
  `**`), credential kind and a body substring, count `nth`/`times`, and
  respond, delay (real or virtual time), hold until released, apply then
  lose the response (a status or a closed connection), close or reset the
  connection, or answer from a Rust closure. They apply to the API, web and
  avatar surfaces, never the admin API. Fault-injected responses are not
  GitHub behaviour and carry no fidelity label.
- Call log: `fault` (`rule`, `action`, `handler_status`) on calls a rule
  fired on, `status` 0 for a dropped connection, the credential's owner as
  `identity` when a rule answered instead of the route, and `body_fields`
  (the non-secret request body fields `permission`, `permissions`, `role`,
  `state`, `invitee_id`, `team_ids`, `repository_ids`, `repositories`).
- Admin organization rename (GE-09c): `POST /__octoemu/orgs/{org}/rename`
  `{"login"}`; the organization keeps its ID, and its memberships, teams,
  invitations, custom roles, installations and Apps follow it (`org-rename`,
  documented). An owner of repositories is refused (422), as for users.
- Fault actions `stall` and `hold_response`, `query_contains` matching, and
  the admin `users/{login}/rename` (`user-rename`, documented) and
  `repository_invitations/{id}/expire` routes (GE-09b).

The temporary stub-compatibility shim of GE-07 (`octoemu::compat`, `--seed
compat`, `/__octoemu/compat/`) was never released and is gone (GE-09c).

- Organizations (GE-06): `GET /orgs/{org}` (`plan` with
  `organization_plan`), `GET /orgs/{org}/members`, `GET`/`PUT`/`DELETE
  /orgs/{org}/memberships/{username}` (a non-member is invited and
  `pending`), `GET`/`POST /orgs/{org}/invitations` (by `invitee_id` or
  `email`, `role`, `team_ids`), `DELETE
  /orgs/{org}/invitations/{invitation_id}`,
  `GET /orgs/{org}/invitations/{invitation_id}/teams`,
  `GET /orgs/{org}/failed_invitations`, `GET /orgs/{org}/teams`,
  `GET /orgs/{org}/teams/{team_slug}`, `GET`/`PUT`/`DELETE
  /orgs/{org}/teams/{team_slug}/memberships/{username}`, and the user's
  `GET /user/memberships/orgs[/{org}]` and `PATCH
  /user/memberships/orgs/{org}` (accept). Invitations expire after 7 days
  on the clock (then listed as failed, assumed); the 50-or-500 per 24 hours
  budget; the paid-plan seat check; `organization.member_invited`,
  `member_added`, `member_removed` and `membership.added`/`removed` events.
- Team repository grants (with nested teams) feed effective repository
  access: the permission route, `affiliation=all` and PUT collaborator's
  204.
- Seed: `org_invitations` (`seed::OrgInvitation`, `seed::InvitationRole`),
  a team's `parent` and `repositories` (`seed::TeamRepository`).
- Admin API: `POST /act/{login}/orgs/{org}/accept|decline|leave` and
  `POST /orgs/{org}/spammed`.

- OAuth web flow and user tokens (GE-05): the authorize page
  (`GET /login/oauth/authorize`: registered callbacks, S256 PKCE, `login=`
  with auto-consent, an HTML page of the seeded users with a consent form at
  `POST /login/oauth/authorize`, `redirect_uri_mismatch`, `access_denied`,
  `application_suspended`) and the token endpoint
  (`POST /login/oauth/access_token`: form or JSON, single-use 10-minute
  codes, `ghu_` tokens with 8-hour expiry and single-use `ghr_` refresh
  tokens, GitHub's error codes); the 10-token cap and an hourly limit
  (`settings.user_tokens_per_hour`).
- Routes `GET /user`, `GET /user/emails`, `GET /user/repository_invitations`,
  `PATCH`/`DELETE /user/repository_invitations/{invitation_id}` (the
  invitee's side, `member.added`), `POST`/`PATCH`/`DELETE
  /applications/{client_id}/token` and `DELETE
  /applications/{client_id}/grant`; `GET /repos/{owner}/{repo}` also takes
  user tokens.
- Seed: a user's `emails` (`seed::Email`, `seed::EmailVisibility`) and
  `noreply_email`; an App's `user_permissions`, `expire_user_tokens` and
  `suspended`; `settings.user_tokens_per_hour`.
- Admin API: `POST /tokens/user` (a `ghu_` token or a classic `ghp_` PAT
  without the browser), `GET`/`POST /oauth`, `POST`/`DELETE /oauth/deny`;
  `GET /state` lists user and refresh tokens, redacted. Handle methods
  `mint_user_token`, `mint_personal_access_token`, `deny_authorization`,
  `allow_authorization` and `set_auto_consent`.
- `calls::Identity` gains `UserToken`, `PersonalAccessToken` and `Client`.

- Webhooks (GE-04): every outbox event is delivered to each target App's
  active webhook (`X-GitHub-*` headers, `X-Hub-Signature-256` and the
  legacy `X-Hub-Signature`, signed with the first secret; 10 s timeout, no
  retry), in event order per App, with `auto` and `manual` dispatch
  (`Builder::dispatch`, `Builder::webhook_timeout`); every attempt is
  recorded (`webhooks::Delivery`, `Emulator::deliveries`).
- Routes `GET`/`PATCH /app/hook/config`, `GET /app/hook/deliveries` (cursor
  pages, `status` filter), `GET /app/hook/deliveries/{delivery_id}` and
  `POST /app/hook/deliveries/{delivery_id}/attempts` (redelivery).
- `installation` (`created`, `deleted`, `suspend`, `unsuspend`,
  `new_permissions_accepted`) and `installation_repositories` (`added`,
  `removed`) events from the admin API, the handle and the App's
  suspension routes.
- Admin API: `POST /installations`, `POST /installations/{id}/permissions`,
  `POST /installations/{id}/repositories`, `PUT`/`DELETE
  /apps/{id}/webhook`, `GET /deliveries`, `POST /deliveries/dispatch`,
  `/wait` and `/mode`; the suspend, unsuspend and uninstall routes take an
  optional `{"sender"}`. Handle methods `install`, `deliveries`,
  `dispatch_deliveries`, `set_dispatch`, `set_webhook` and
  `wait_for_deliveries`.
- Repositories (GE-03): `GET /repos/{owner}/{repo}`; collaborators (`GET`
  list with `affiliation`, `PUT` with the 201/204 split, `DELETE`,
  `GET …/permission` with `permission` and `role_name`); repository
  invitations (`GET` list and its `/repositories/{repository_id}/invitations`
  alias, `PATCH`, `DELETE`) with 7-day expiry and 50 per repository per 24 h
  on the virtual clock; `GET /users/{username}`, `GET /user/{account_id}`
  and PNG avatars at `/avatars/u/{id}`.
- `events`: a webhook outbox (`member` `added`/`edited`/`removed`), at
  `GET /__octoemu/events` and `Emulator::events`.
- Admin API `act` routes: accept or decline a repository invitation, leave
  a repository, as a seeded user.
- Seed: `repository_roles` (custom roles), `repository_invitations`, a
  user's `type` (`User`/`Bot`) and a repository's `visibility` (with
  `internal`); custom collaborator roles must now name a seeded custom role.
- App auth (GE-02): App JWT verification (RS256 against any registered key,
  `iss`, `iat`, `exp` on the virtual clock); stateless installation tokens
  (`ghs_<APPID>_<JWT>`, 1 h) narrowed by `permissions`, `repositories` and
  `repository_ids`, looked up on every call; `DELETE /installation/token`;
  suspension (`PUT`/`DELETE …/suspended`) blocking token creation and use.
- Routes `GET /app`, `GET /app/installations`,
  `GET /app/installations/{installation_id}`,
  `POST /app/installations/{installation_id}/access_tokens`,
  `PUT`/`DELETE /app/installations/{installation_id}/suspended`,
  `GET /app/installation-requests`, `GET /installation/repositories` and
  `DELETE /installation/token`.
- Per-route credential kinds and permissions: 401 for a bad credential or the
  wrong kind, 403 `Resource not accessible by integration` with
  `X-Accepted-GitHub-Permissions`.
- GitHub pagination (`per_page`, `page`, `Link`) for list routes.
- Seed: `installation_requests`, and an installation's `suspended_by` and
  `updated_at`.
- Admin API: `POST /tokens/installation`, `POST /installations/{id}/suspend`
  and `/unsuspend`, `DELETE /installations/{id}`; `GET /state` lists minted
  tokens by redacted ID. Handle methods `suspend_installation`,
  `unsuspend_installation` and `uninstall`.
- `calls::Identity` and `Call::identity`: the verified caller (App,
  installation and redacted token ID).
- Refuse-assumed mode withholds `assumed` error texts
  (`octoemu: assumed message withheld`) while keeping documented statuses.
- Skeleton (GE-01): an axum server with in-memory state behind one lock;
  embedded mode (`Emulator::start`, `Emulator::builder`, stop on drop) and
  the `octoemu` binary (`--port`, `--bind`, `--seed`, `--idle-timeout`,
  `--refuse-assumed`, `--fidelity`) with a one-line JSON startup message.
- `seed`: users, organizations, teams, repositories, Apps, installations,
  collaborators and memberships, as JSON or a Rust builder, with validation
  that reports every problem with its JSON path.
- A virtual clock behind every response's `Date` header.
- Admin API under `/__octoemu/`: `health`, `state`, `seed`, `clock`,
  `clock/advance`, `calls` (GET, DELETE) and `fidelity`.
- A call log that never records credentials, secret query values or bodies.
- `fidelity.toml` with `verified`/`documented`/`assumed` labels, a route
  registry checked against it, and refuse-assumed mode.
- 501 `octoemu: not implemented` for every API and web route not built.

### Changed

- Response objects carry every field their OpenAPI schema requires, as the
  fidelity suite's schema check found missing: users (`simple-user`'s nine
  URL templates, on App bots too), organizations (`organization-simple`'s
  `repos_url`, `events_url`, `hooks_url`, `issues_url`, `members_url`,
  `public_members_url`), teams (`type: "organization"`) and repositories
  (every `*_url` template on the minimal form; installation repository
  lists, token `repositories` and `GET /repos/{owner}/{repo}` now use the
  full `repository` form with clone URLs, `default_branch`, zeroed counts,
  `has_*` flags, `license`, `mirror_url`, `topics`; the full form adds
  `network_count` and `subscribers_count`). Webhook payloads carry the same
  objects.
- The server runs its own HTTP/1.1 accept loop (hyper) instead of
  `axum::serve`, so a fault rule can drop a connection; shutdown still
  waits for in-flight requests and releases held ones (GE-07).
- `email` in `GET /users/{username}` is now the user's public primary
  address (it was always null).
