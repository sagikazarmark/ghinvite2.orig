# Changelog

All notable changes to this crate are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the crate follows
[Semantic Versioning](https://semver.org/spec/v2.0.0.html): while it is `0.x`, a
minor release may break the API, and every break gets a migration note.

## [Unreleased]

Not yet published; developed inside the ghinvite repository.

### Added

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
