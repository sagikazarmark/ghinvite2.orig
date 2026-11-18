# Changelog

All notable changes to this crate are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the crate follows
[Semantic Versioning](https://semver.org/spec/v2.0.0.html): while it is `0.x`, a
minor release may break the API, and every break gets a migration note.

## [Unreleased]

Not yet published; developed inside the ghinvite repository.

### Added

- The fidelity suite (GE-08): `Target::Emulator` (a fresh embedded octoemu
  per scenario, seeded with the fidelity world) and `Target::Live` (real
  GitHub, from the env file `OCTOEMU_LIVE_ENV` names, kept outside the
  repository); 20 scenarios over public routes, each with a stable ID in
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
