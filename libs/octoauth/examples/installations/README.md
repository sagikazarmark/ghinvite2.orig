# Example: list installations and their repositories

A minimal GitHub App as a command-line program. It prints every account the
App is installed on and the repositories each installation can reach:

```text
acme (Organization, installation 5, all repositories)
  acme/api
  acme/web (private)
octocat (User, installation 6, selected repositories)
  octocat/dotfiles
globex (Organization, installation 7, all repositories)
  cannot list repositories: gh:installation_suspended
```

It uses both credentials a GitHub App has:

1. **As the App** (its credential, through `octoauth::App::with_credential`):
   `GET /app/installations`. A 401 that GitHub's `Date` shows a skewed host
   clock caused gets one retry with a corrected credential.
2. **As each installation** (installation token from octotoken's
   `AppTokenProvider`, scoped to `metadata: read`, through
   `octotoken::with_token`): `GET /installation/repositories`. A 401 gets one
   replacement token and one retry.

The GitHub client is octocrab, set up by `octoauth-octocrab`: a base client
without auth for the App's API (`octoauth::App::base_url`), a client derived
per credential, no automatic retries or redirects, errors reduced to fixed
codes before they are printed.

## Run it

Against GitHub, with a GitHub App of your own. It needs no webhook (untick
"Active") and no permissions beyond the mandatory `Metadata: read`. Install it
on an account or two, then:

```sh
GITHUB_APP_ID=123 GITHUB_APP_KEY=path/to/app.private-key.pem \
    cargo run -p example-installations
```

Against octoemu: start `cargo run -p octoemu -- --seed seed.json` with an App
whose public key is `tests/fixtures/rsa2048.pub.pem`, then add
`GITHUB_API_URL=<api_base>` and use `tests/fixtures/rsa2048.pem` as the key.

## Tests

`tests/emulator.rs` runs the lister against an embedded octoemu: paging
(pages of 2), a selected-repositories installation, a suspended installation
(`gh:installation_suspended`) and a token revoked behind the provider's back
(exactly one replacement).

```sh
cargo test -p example-installations
```

## What it needs from the toolkit

- **Crates:** `octotoken` (it re-exports `octoauth`) and
  `octoauth-octocrab`; `octoemu` as a dev-dependency. Not `octologin` or
  `octoscrub`.

## Friction found, and what became of it

Building the first version of this example turned up six problems:

1. **The octocrab error bridge was copied** from
   `crates/octotoken/examples/octocrab.rs`. **Fixed:** crate
   `octoauth-octocrab` (`Error`, `api_error`); the octotoken example uses it
   too. This is the octocrab adapter ADR 0014 §5 deferred.
2. **App-level calls got no clock-skew retry.** A request built from
   `App::credential` was rejected for good by a skewed host clock. **Fixed:**
   `App::with_credential`, the app-credential counterpart of `with_token`;
   `octoauth_octocrab::unauthorized` supplies its 401 signal.
3. **`WithTokenError` needed boilerplate:** a dead `_` arm (it was
   `#[non_exhaustive]`) and the same `From` in every caller. **Fixed:** no
   longer `#[non_exhaustive]`, and `map` plus `into_error`
   (`error.map(Error::from).into_error()`); `WithCredentialError` has the
   same.
4. **The API base was configured twice**, on octoauth's builder and
   octocrab's. **Fixed:** `octoauth_octocrab::builder(&app)` takes it from
   `App::base_url`. The user agent stays octocrab's: octocrab always sends
   its own, and adding another would send two `User-Agent` headers.
5. **Paging is by hand.** octocrab's `all_pages` follows `Link` URLs without
   checking their host, and `GET /installation/repositories` is not an
   octocrab `Page`. **Left as is:** paging belongs to the client, and a
   page-number loop is short. The hazard is documented in
   `octoauth-octocrab`.
6. **A fixed scope needs `expect`.** **Left as is:** `Permissions::new`
   validates real rules (e.g. `workflows` is write-only), and
   `Permissions` is a `BTreeMap`, so it cannot be built in a `const`.
   `expect` on a constant set is the idiom.
