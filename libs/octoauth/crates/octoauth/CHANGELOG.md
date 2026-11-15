# Changelog

All notable changes to this crate are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the crate follows
[Semantic Versioning](https://semver.org/spec/v2.0.0.html): while it is `0.x`, a
minor release may break the API, and every break gets a migration note.

## [Unreleased]

Not yet published; developed inside the ghinvite repository.

### Added

- `App`: RS256 app credential, reused for 5 minutes and corrected for a
  skewed host clock (one retry after a 401, using GitHub's `Date`);
  `App::credential` for app-level endpoints the crate does not wrap.
- `Installation::access_token`, `Installation::get` and
  `Installation::is_suspended`.
- `ApiError::classify` for any client's GitHub REST error responses.
- Secret types (`SigningKey`, `InstallationToken`, `AppCredential`) with
  redacted `Debug` and no `Display` or `Serialize`.
- Errors with stable codes, listed in the workspace's `error-codes.txt`
  (append-only, checked by a test).
- `middleware` feature: send through `reqwest_middleware`.
- `tracing` feature: events for installation token creation and failure,
  clock-skew correction and app credential signing. Their fields are IDs,
  error codes and seconds, never a secret.
- Builds for `wasm32-unknown-unknown` (Cloudflare Workers).
