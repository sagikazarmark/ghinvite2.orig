# Example: sign in with GitHub, see your repositories

A minimal GitHub App as a web app. You sign in with GitHub and it lists the
repositories the App can reach that you can access too, grouped by account:

```text
Repo Lens
[avatar] Signed in as octocat                              [Sign out]
Your access as of sign-in, 5 minutes ago. Re-check access

acme
  acme/api
  acme/web  private
octocat
  octocat/dotfiles

Install the App on an account
```

## How it works

It uses each of the three credentials a GitHub App has for what only that
credential can do:

1. **The user's token, once, at sign-in** (`octologin`). The app reads who
   signed in (`GET /user`) and what they may see: the App's installations
   they can access (`GET /user/installations`) and, in each, the
   repositories they can access (`GET /user/installations/{id}/repositories`).
   The session keeps the IDs. The reads run inside
   `SignIn::with_user_token`, which revokes the token before the callback
   answers, whether reading succeeded or not, so the app never holds a user
   token.
2. **An installation token, on every page load** (`octotoken`,
   `with_token`). The app lists `GET /installation/repositories` and keeps
   only the repositories the user may see. Without that filter, an
   organization member would see every repository the App was granted,
   including private ones they cannot open. Each reload is fresh, so a
   repository the App loses disappears at once.
3. **The App's credential, at startup** (`App::with_credential`). The app
   reads `GET /app` for the App's name and its installation page, so a wrong
   App ID or key fails at launch, not on the first visit.

What the user may see is a snapshot from sign-in. **Re-check access** signs
in again; GitHub skips the consent screen for a user who already authorized
the App, so it is a quick round trip.

Sessions live in server memory (`src/session.rs`, about 180 lines with docs). The
browser holds only a random 256-bit ID:

| Cookie | Holds | Lifetime |
|---|---|---|
| `login` | a pending sign-in: the state and the PKCE verifier, kept server side | 10 minutes, one callback |
| `session` | who signed in and what they may see (never a token) | 8 hours from sign-in |

- **Cookie attributes:** both cookies are `HttpOnly` and `SameSite=Lax`, and
  `Secure` unless `PUBLIC_URL` is plain `http`. `Lax` rather than `Strict`,
  because GitHub's redirect back is a cross-site navigation.
- **New ID on every sign-in:** a sign-in always gets a fresh ID and removes
  the session it replaces.
- **Limit and restart:** the store holds at most 10,000 sessions, because
  `/login` needs no credentials. A restart signs everyone out.
- **Response headers:** every response is `Cache-Control: no-store` and has a
  strict content security policy, with no scripts and no framing.

### After an installation

The App's **Setup URL** is `<PUBLIC_URL>/setup`. Tick **Redirect on update**
so that changing the selected repositories comes back here as well.

- **`install` and `update`:** these start a sign-in, so the new access is
  read back from GitHub. The `installation_id` GitHub adds to the URL is
  ignored, because anyone can put any ID there.
- **`request`:** someone without the right to install asked an owner to
  approve the App. They get a page that says so.

With **Request user authorization (OAuth) during installation** ticked,
GitHub sends the installer to the callback URL instead, with a code and no
`state`. Nothing shows that this browser started that authorization, and
there is no PKCE verifier for it. So the code is not exchanged and a normal
sign-in follows.

## Run it

You need a GitHub App of your own:

- **Callback URL:** `http://localhost:3000/callback`.
- **Setup URL:** `http://localhost:3000/setup`, with **Redirect on update**
  ticked.
- **Webhook:** none (untick **Active**).
- **Permissions:** none beyond the mandatory `Metadata: read`.

Generate a private key and a client secret, install the App on an account or
two, then run:

```sh
GITHUB_APP_ID=123 GITHUB_APP_KEY=path/to/app.private-key.pem \
GITHUB_CLIENT_ID=Iv23… GITHUB_CLIENT_SECRET=… \
    cargo run -p example-repositories
```

Open <http://localhost:3000>.

- **`PUBLIC_URL`:** the address browsers use (default
  `http://localhost:3000`).
- **`LISTEN`:** the socket address to bind (default `127.0.0.1:3000`).
- **`GITHUB_API_URL` and `GITHUB_WEB_URL`:** point the app at another
  GitHub.

## Tests

`tests/github.rs` drives the router against a local GitHub stand-in
(wiremock), with lists in pages of 2. It covers:

- the whole sign-in: the state and PKCE in the redirect, the exchange,
  exactly one revocation, and the filtered page;
- reloads, which reuse cached installation tokens and make no user-token
  calls;
- re-checking access, which replaces the session;
- signing out;
- a forged state (no exchange, and the pending sign-in is used up);
- a callback with no pending sign-in;
- a declined consent;
- the setup redirects, including a code without `state` that is never
  exchanged;
- a suspended installation, which does not hide the others.

```sh
cargo test -p example-repositories
```

## What it needs from the toolkit

- **Crates:** `octologin` (`with_user_token`), `octotoken` (it re-exports
  `octoauth`) and `octoauth-octocrab`. Not `octoscrub`, because the app
  logs no upstream text.
- **Application code:** axum, cookies, HTML escaping and the session store
  are the application's own.

## Friction found, and what became of it

1. **The user token was revoked by hand on every path.** `exchange` returned
   a `UserToken` that the app had to pass to `revoke` whatever happened
   next. That meant an `async` block around the reads, so that an early `?`
   could not skip the revocation. **Fixed:** `SignIn::with_user_token`
   exchanges the code, runs a closure with the token, then always revokes
   it. It is the sign-in counterpart of octotoken's `with_token`. A failed
   revocation comes back in `Revoked::revocation`, and the sign-in still
   goes ahead, because the token expires anyway. Errors flatten with
   `error.into_error()`.
2. **The API base is configured twice.** `GITHUB_API_URL` goes to both
   `App::builder(...).base_url` and `SignIn::builder(...).api_base_url`, and
   the two clients (octologin's reqwest client and octocrab) are set up
   separately. **Left as is:** it is two lines in `main`, and tying
   octologin's configuration to an `App` would make a sign-in-only app
   build one.
3. **octologin cannot list what a user may access.** The user token on
   octocrab (`octoauth_octocrab::authenticated(.., token.expose_secret())`)
   covers it in about 40 lines. **Fixed in the docs:** `authenticated` and
   octologin's crate docs now name user tokens and
   `GET /user/installations`. The toolkit README's known gap "no
   user-installation listing" is now "not planned": it is a plain API call,
   so it belongs to the application's client (ADR 0014 §1).
4. **The repository paging loop was copied** from the first example (the
   installation-token half). **Left as is:** ADR 0014 keeps paging in the
   client, so this is duplicated application code, not missing toolkit
   code. octocrab adds a quirk: the page number of `GET /user/installations`
   is a `u8`.
5. **octologin's docs did not cover the setup redirect.** With OAuth during
   installation, the callback receives a code without `state`, which
   `PendingAuthorization::verify` cannot accept. **Fixed in the docs:**
   octologin's "After an installation" section says not to exchange that
   code and not to trust `installation_id`, and to sign in again instead.
6. **Sessions are the application's job.** `PendingAuthorization` has
   public fields so that a store can persist it, and the in-memory store
   here is about 180 lines with docs. **Left as is:** a session store would
   tie the toolkit to a web framework.
