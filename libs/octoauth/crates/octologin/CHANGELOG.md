# Changelog

All notable changes to this crate are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the crate follows
[Semantic Versioning](https://semver.org/spec/v2.0.0.html): while it is `0.x`, a
minor release may break the API, and every break gets a migration note.

## [Unreleased]

Not yet published; developed inside the ghinvite repository.

### Added

- Sign-in web flow for GitHub Apps: authorize URL with state and PKCE, code
  exchange, `GET /user`, verified emails and token revocation. Never
  persists a token.
- `SignInError` with stable codes, listed in the workspace's
  `error-codes.txt`.
