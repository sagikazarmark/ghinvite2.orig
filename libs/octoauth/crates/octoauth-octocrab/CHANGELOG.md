# Changelog

All notable changes to this crate are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the crate follows
[Semantic Versioning](https://semver.org/spec/v2.0.0.html): while it is `0.x`, a
minor release may break the API, and every break gets a migration note. A
breaking octocrab release is a breaking release of this crate.

## [Unreleased]

Not yet published; developed inside the ghinvite repository.

### Added

- `builder`: octocrab's builder for an `octoauth::App`'s API, with bounded
  connect and read times and no auth; `authenticated` derives a client per
  credential.
- `Error`: `octocrab::Error` reduced to `octoauth::ApiError` or a fixed
  client error (`gh:octocrab:client`), without upstream text; `api_error`
  for the classification alone.
- `is_unauthorized` and `unauthorized`: the 401 signals of octotoken's
  `with_token` and `octoauth::App::with_credential`.
- Built against octocrab 0.54.
