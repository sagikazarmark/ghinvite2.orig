# Changelog

All notable changes to this crate are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the crate follows
[Semantic Versioning](https://semver.org/spec/v2.0.0.html): while it is `0.x`, a
minor release may break the API, and every break gets a migration note.

## [Unreleased]

Not yet published; developed inside the ghinvite repository.

### Added

- `InstallationTokenProvider` trait and `AppTokenProvider`, an in-memory
  cache of installation tokens per installation and scope (natively,
  concurrent misses wait for one creation).
- `with_token`: one call, and after a 401 one replacement token and one
  retry.
- `DynInstallationTokenProvider`: the object-safe form, implemented for
  every provider, so consumers can hold `Arc<dyn DynInstallationTokenProvider>`.
- `send-wrapper` feature: its futures are `Send` on wasm32 too.
- `ProviderRegistry`: isolate-scope providers keyed by App ID, for Workers
  that build their context per request.
- `tracing` feature: events for token creation, replacement and failure
  (also enables octoauth's). Their fields never hold a token.
- `InstallationTokenProvider::invalidate` (stop serving a rejected token,
  creating nothing, for requests that must not be repeated right away, such
  as fenced writes) and `invalidate_installation` (forget every scope's
  token of an installation, e.g. after its permissions changed); also on
  `DynInstallationTokenProvider`. Both are required: a provider implements
  them too (a provider that caches nothing does nothing).
- `WithTokenError::map` and `WithTokenError::into_error`: convert the call's
  error, and flatten into the caller's own error type when it implements
  `From<TokenError>`.

### Changed

- `WithTokenError` is no longer `#[non_exhaustive]`: its two variants are the
  complete outcome of a retry helper, so matching it needs no `_` arm. Not a
  breaking change for callers.
