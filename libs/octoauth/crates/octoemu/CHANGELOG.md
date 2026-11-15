# Changelog

All notable changes to this crate are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the crate follows
[Semantic Versioning](https://semver.org/spec/v2.0.0.html): while it is `0.x`, a
minor release may break the API, and every break gets a migration note.

## [Unreleased]

Not yet published; developed inside the ghinvite repository.

### Added

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
- 501 `octoemu: not implemented` for every API and web route (none is built
  yet).
