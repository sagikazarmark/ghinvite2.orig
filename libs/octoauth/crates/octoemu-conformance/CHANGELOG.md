# Changelog

All notable changes to this crate are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the crate follows
[Semantic Versioning](https://semver.org/spec/v2.0.0.html): while it is `0.x`, a
minor release may break the API, and every break gets a migration note.

## [Unreleased]

Not yet published; developed inside the ghinvite repository.

### Changed

- The first live record run (2026-11-24, a throwaway Free organization):
  11 snapshots are GitHub's (`"source": "live"`); the 9 scenarios it
  skipped (destructive, slow, or needing an optional extra) keep the
  emulator's.
- `installations_count` is `<varies>`: how many installations the App has
  is the world's (three on the emulator), not behaviour. The live
  snapshots' recorded counts were normalised the same way.
- The emulator's world describes the live one: the fidelity App's homepage
  URL is the one the setup guide registers, and the repository's settings
  are the live private repository's (no wiki, downloads or forking).
- `token-revocation-delete` measures how soon a revocation takes effect: it
  polls after the `DELETE` and records whether the token was refused within
  2 s (`revocation-propagates`), then the use after it. Right after the
  `DELETE` GitHub sometimes still accepts the token, so the old immediate
  use was a coin toss; re-recorded live.

### Added

- The fidelity suite (GE-08): `Target::Emulator` (a fresh embedded octoemu
  per scenario, seeded with the fidelity world) and `Target::Live` (real
  GitHub, from `.octoemu-live/live.env` at the repository root or the env
  file `OCTOEMU_LIVE_ENV` names; inside the repository the env file and
  the App's private key must be ignored by git, and a relative key path is
  resolved against the env file); 20 scenarios over public routes, each with a stable ID in
  octoemu's `fidelity.toml` `[scenarios]` table; `destructive` and `slow`
  scenarios run live only on request.
- Normalised snapshots under `snapshots/` (compare and record modes; field
  order ignored, fields one side lacks listed). The initial snapshots are
  generated from the emulator and marked `"source": "emulator
  (unverified)"`; a live record run overwrites them, an emulator record run
  never overwrites a live one.
- The schema check: every response validated with `jsonschema` against
  `schemas/rest-api-subset.json`, the operations octoemu implements
  extracted (`schemas/extract.py`) from github/rest-api-description at
  `c9c95fe34ced05fe076001f5c55704742bc8e7f8` (OpenAPI 3.1, API version
  2022-11-28); documented exceptions in `schemas/exceptions.toml`.
- The report (text, and JSON at `OCTOEMU_CONFORMANCE_REPORT`): per scenario
  the outcome, mismatches, unmodelled and extra fields, schema violations
  and notes.
