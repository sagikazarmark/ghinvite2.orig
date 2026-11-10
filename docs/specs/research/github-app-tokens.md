# GitHub App authentication: installation tokens, user tokens, org policy (desk check)

Status: desk research, 2026-10-15. Covers components C1 (installation token broker), C2 (user token vault) and C5 (app scaffolding) from `PLAN.md` §2, and checks the §3 findings. Sources are primary where possible: docs.github.com (fetched through its article API), github.blog changelog, `github/rest-api-description`, and the dependabot-core tracker. Community threads and third-party reports are labelled as such. Anything no source confirms is marked **UNVERIFIED**.

Projects: P1 GitHub Invite (the app sends org and repo invites), P2 Dependaboard (cross-org Dependabot PR dashboard; bulk merge; posts `@dependabot rebase` **as the user**), P3 repo manager. Stack: Rust, Restate (journals `ctx.run` results), Cloudflare Workers.

---

## Q1. Installation access tokens

**Short answer.**
- **Lifetime.** Installation tokens live 1 hour.
- **Scoping.** You can narrow a token when you mint it: `repositories` (names) or `repository_ids` (up to 500), plus a `permissions` object. You can only narrow, never widen.
- **Revocation.** `DELETE /installation/token` revokes the token you call it with.
- **Caching.** GitHub's docs say to cache tokens and reuse them until they expire.
- **Mint rate limit.** No dedicated limit is documented for `POST /app/installations/{id}/access_tokens`. The general secondary limits apply: 900 points/min per endpoint, with a POST costing 5 points, so about 180 mints/min. There is also a separate "2,000 OAuth access token requests per hour" limit. Whether that one counts installation-token mints is **UNVERIFIED**; the wording ("OAuth access token") suggests it targets the user/OAuth token exchange.
- **Primary rate limits.**
  - Installation tokens: 5,000/h base, plus 50/h per repo above 20 repos and 50/h per org user above 20 users, capped at 12,500/h. Enterprise Cloud orgs get a flat 15,000/h.
  - User tokens: they share the user's own 5,000/h with all other apps and PATs, or get 15,000/h if the *app* is owned by an Enterprise Cloud org.
- **New in 2026: token format.** Since 2026-04-27 installation tokens are stateless JWT-format strings (`ghs_…`, about 520 chars, two dots). The rollout was completed 2026-10-02. The opt-out header stops working 2026-11-30.

**Evidence**
- "The installation access token will expire after 1 hour." / "you can use the `repositories` or `repository_ids` body parameters to specify individual repositories … You can list up to 500 repositories." / "use the `permissions` body parameter … The installation access token cannot be granted permissions that the app was not granted." https://docs.github.com/en/apps/creating-github-apps/authenticating-with-a-github-app/generating-an-installation-access-token-for-a-github-app
- "Installation tokens expire one hour from the time you create them. Using an expired token produces a status code of `401 - Unauthorized`, and requires creating a new installation token." Response codes are 201/401/403/404/422. https://docs.github.com/en/rest/apps/apps#create-an-installation-access-token-for-an-app (same text in `rest-api-description`)
- `DELETE /installation/token`: "Revokes the installation token you're using to authenticate … Once an installation token is revoked, the token is invalidated and cannot be used." Returns 204. https://docs.github.com/en/rest/apps/installations#revoke-an-installation-access-token. Whether revocation behaves the same for the new stateless tokens is **UNVERIFIED**: no source discusses it.
- Caching: "User access tokens and installation access tokens are meant to be used until they expire. You should cache tokens that you create. Before you create a new token, check your cache to see if you already have a valid token." https://docs.github.com/en/apps/creating-github-apps/about-creating-github-apps/best-practices-for-creating-a-github-app
- Rate limits:
  - "GitHub Apps authenticating with an installation access token use the installation's minimum rate limit of 5,000 requests per hour. If the installation is on a GitHub Enterprise Cloud organization, the installation has a rate limit of 15,000 requests per hour."
  - "Installations that have more than 20 repositories receive another 50 requests per hour for each repository … more than 20 users receive another 50 requests per hour for each user. The rate limit cannot increase beyond 12,500 requests per hour."
  - "Primary rate limits for GitHub App user access tokens … are dictated by the primary rate limits for the authenticated user. This rate limit is combined with any requests that another GitHub App or OAuth app makes on that user's behalf".
  - Source: https://docs.github.com/en/rest/using-the-rest-api/rate-limits-for-the-rest-api
- Secondary limits (same page):
  - "No more than 100 concurrent requests"
  - "No more than 900 points per minute are allowed for REST API endpoints"
  - "Most REST API `POST` … requests: 5" points
  - "no more than 80 content-generating requests per minute and no more than 500 content-generating requests per hour"
  - "No more than 2,000 OAuth access token requests per hour are allowed for GitHub Apps and OAuth apps."
- No official answer exists on mint throttling. The community thread https://github.com/orgs/community/discussions/196652 (2026-05, no GitHub staff answer) only offers opinions that it falls under "shared authentication throttling, secondary rate limits".
- Token format:
  - "By default, all newly minted GitHub App installation tokens will be in the stateless ghs_APPID_JWT format … about 520 characters long instead of 40. Token permissions, repository scoping, the one-hour expiration, and the installation access token REST API endpoint are unchanged." https://github.blog/changelog/2026-10-02-stateless-github-app-installation-tokens-rolled-out/
  - The `X-GitHub-Stateless-S2S-Token` override header and its recommended regex `ghs_[A-Za-z0-9\.\-_]{36,}`: https://github.blog/changelog/2026-05-15-github-app-installation-tokens-per-request-override-header

**Confidence:** high on lifetime, scoping, revocation endpoint, caching guidance and primary limits. Medium on mint throttling: it is undocumented, but even worst-case secondary limits are far above our need.

## Q2. App JWT

**Short answer.** The JWT is signed with RS256. `exp` can be at most 10 minutes ahead. Set `iat` 60 s in the past to absorb clock drift. `iss` is the app's client ID (recommended) or app ID. It must be sent as `Authorization: Bearer`.

The private key GitHub hands you is a PEM in **PKCS#1** (`BEGIN RSA PRIVATE KEY`) format. An app can hold up to 25 keys, and keys never expire.

Workers concerns:
- Workers WebCrypto supports RSASSA-PKCS1-v1_5 sign and `importKey`. WebCrypto's `importKey` takes `pkcs8`/`jwk`, not PKCS#1, so convert the key once at provisioning (`openssl pkcs8 -topk8 -nocrypt`) or store it as a JWK. This is standard WebCrypto behaviour, but we did not test it against Workers (**UNVERIFIED**).
- In Rust, either call WebCrypto through `worker`/`web-sys`, or use a pure-Rust `rsa` + `sha2` signer that compiles to wasm32. Avoid crates that depend on `ring` RSA signing on `wasm32-unknown-unknown` unless verified (**UNVERIFIED**).
- CPU cost is one RSA-2048 signature per JWT. A JWT can be reused for up to about 9 minutes across many mints, so this is negligible.

**Evidence**
- "Your JWT must be signed using the `RS256` algorithm" / `exp`: "no more than 10 minutes into the future" / `iat`: "we recommend that you set this 60 seconds in the past" / `iss`: "The client ID or application ID … Use of the client ID is recommended." / "if you are passing a JSON web token (JWT), you must use `Authorization: Bearer`." https://docs.github.com/en/apps/creating-github-apps/authenticating-with-a-github-app/generating-a-json-web-token-jwt-for-a-github-app
- "the PEM file you download will be in `PKCS#1 RSAPrivateKey` format." / "You can create up to 25 private keys for an app." / "Private keys do not expire". https://docs.github.com/en/apps/creating-github-apps/authenticating-with-a-github-app/managing-private-keys-for-github-apps
- The Workers Web Crypto algorithm table lists "RSASSA PKCS1 v1.5" with sign/verify, generateKey, exportKey and importKey. https://developers.cloudflare.com/workers/runtime-apis/web-crypto/

**Confidence:** high on the claims; medium on the wasm crate choice, which needs a 30-minute spike.

## Q3. User access tokens (GitHub App user-to-server)

**Short answer.**
- **Lifetimes.** The access token (`ghu_`) lives 8 h (`expires_in` is always 28800). The refresh token (`ghr_`) lives 6 months (`refresh_token_expires_in` is always 15897600). Every refresh returns a new refresh token with a fresh 6-month window, so an actively refreshed grant can live indefinitely.
- **Rotation is single-use.** "Once you use a refresh token, that refresh token and the old user access token will no longer work." So refreshing also kills the old access token immediately. In-flight requests that still hold the old `ghu_` will get 401.
- **Concurrent refresh / grace window.** GitHub documents **no** grace window and no idempotent replay. The two practical consequences:
  - Two parallel refreshes with the same refresh token: assume one wins and the other gets `bad_refresh_token`. Third-party reports agree ("no grace period"); this is **UNVERIFIED** against GitHub itself.
  - A crash after GitHub answered but before we persisted the answer loses the grant.
- **Errors.** `bad_refresh_token` means "invalid or expired" and requires a new web/device flow. Other errors: `incorrect_client_credentials`, `unsupported_grant_type`, `unverified_user_email`. The OAuth endpoint reports errors in the response body; whether it does so with HTTP 200 for the refresh grant is **UNVERIFIED** (the docs say so only for the device flow). Parse the body and never trust the status code alone.
- **Effective permissions.** These are the intersection of app permissions, the user's own access, and the installations: the app must be installed on the account that owns the resource.
- **Narrowing at mint.** You can pass `repository_id` (a single repo) when generating a user token, or later use `POST /applications/{client_id}/token/scoped` for repo- or permission-scoped user tokens.
- **App-side revocation.**
  - `DELETE /applications/{client_id}/token` revokes one token.
  - `DELETE /applications/{client_id}/grant` deletes the whole authorization and all its tokens.
  - Both use Basic auth with `client_id:client_secret`.
  - `POST /applications/{client_id}/token` checks a token without burning failed-login limits.
- **Ten-token cap.** For OAuth apps, GitHub documents a cap of 10 tokens per user/app/scope plus a limit of 10 new tokens/h. The 11th token revokes an older one (never-used first, then least recently used). The docs place this under OAuth apps. A third-party report (2026-10) says it also applies to GitHub App user tokens, where each new web/device-flow sign-in is a new token. Treat it as applying (**UNVERIFIED** for GitHub Apps).
- **Opting in without app config.** Expiring tokens can be forced per authorization by requesting the `offline_access` scope.

**Evidence**
- "The user access token expires after eight hours, and the refresh token expires after six months." / "Once you use a refresh token, that refresh token and the old user access token will no longer work." / `expires_in` "will always be `28800`", `refresh_token_expires_in` "will always be `15897600`". / "the user access token is limited to the permissions that both your app and the user have." / "The only supported value is `offline_access`". https://docs.github.com/en/apps/creating-github-apps/authenticating-with-a-github-app/refreshing-user-access-tokens
- "If the refresh token that you specified is invalid or expired, you will receive a `bad_refresh_token` error. To resolve this error, you must restart the web application flow or device flow". The same page documents `repository_id` ("Use this parameter to restrict the access of the user access token further") and the PKCE `code_challenge` (S256 only). https://docs.github.com/en/apps/creating-github-apps/authenticating-with-a-github-app/generating-a-user-access-token-for-a-github-app
- "The app can only access resources that the user has access to … The app can only access resources that it has permission to access … The app can only access resources in an account where it is installed." https://docs.github.com/en/apps/creating-github-apps/authenticating-with-a-github-app/authenticating-with-a-github-app-on-behalf-of-a-user
- "Deleting an application's grant will also delete all OAuth tokens associated with the application for the user." / "You must use Basic Authentication … Use the application's client_id as the username and the client_secret as the password." / "Reset a token … changes take effect immediately." https://docs.github.com/en/rest/apps/oauth-applications
- Scoped user token: `POST /applications/{client_id}/token/scoped`: "Use a non-scoped user access token to create a repository-scoped and/or permission-scoped user access token." https://docs.github.com/en/rest/apps/apps#create-a-scoped-access-token
- "There is a limit of ten tokens that are issued per user/application/scope combination, and a rate limit of ten tokens created per hour. If an application creates more than ten tokens … GitHub revokes one of the existing tokens". https://docs.github.com/en/authentication/keeping-your-account-and-data-secure/token-expiration-and-revocation and https://docs.github.com/en/apps/oauth-apps/building-oauth-apps/authorizing-oauth-apps. Applicability to GitHub Apps is reported by a third party: https://github.com/henningfutrell/hopper/issues/514 (2026-10-08; observed, not official).
- "GitHub will automatically revoke an OAuth token or personal access token when the token hasn't been used in one year." (same token-expiration page)
- No grace window, third-party: https://github.com/getsentry/sentry/issues/107873 lists GitHub's grace period as "None"; https://nango.dev/blog/github-app-oauth-refresh-token-bad-refresh-token/. Neither is official.

**Confidence:** high on lifetimes, rotation semantics, error name, intersection and revocation endpoints. **Low** on concurrent-refresh behaviour, which is undocumented and needs a spike. Medium on the ten-token cap applying to GitHub Apps.

## Q4. Webhook on user revocation

**Short answer.** Yes. `github_app_authorization` (action `revoked`) fires when a user revokes their authorization. Apps receive it by default and cannot unsubscribe. Revoking does not uninstall the app. After revocation, API calls with the token return `401 Bad Credentials`. Uninstall and suspend events come separately via `installation` (`deleted`, `suspend`, `unsuspend`, `new_permissions_accepted`, `created`).

**Evidence**
- "This event occurs when a user revokes their authorization of a GitHub App … A GitHub App receives this webhook by default and cannot unsubscribe from this event … Revoking the authorization of a GitHub App does not uninstall the GitHub App." https://docs.github.com/en/webhooks/webhook-events-and-payloads#github_app_authorization
- "If your app continues to use a revoked access token, it will receive the `401 Bad Credentials` error." (generating-a-user-access-token page)
- `installation` action types: "`created`, `deleted`, `new_permissions_accepted`, `suspend`, `unsuspend`" (webhook events page)

**Confidence:** high. Webhooks can be missed, so C2 must also treat a 401 or `bad_refresh_token` as a revocation signal.

## Q5. Acting as the user, and Dependabot commands

**Short answer.**
- **Attribution.** Comments made with a user token are attributed to the **user**: their avatar is shown with the app's identicon badge. The audit log lists the user as actor with `programmatic_access_type` "GitHub App user-to-server token". The comment payload will therefore have `user` = the human and `performed_via_github_app` = our app. That payload shape is inferred from the REST issue-comment schema and from #9147 below; it was not directly observed for user tokens.
- **Comments from the app's own identity are rejected.** Dependabot refuses commands posted with an *installation* token, i.e. by the `app[bot]` identity: "Sorry, only users with push access can use that command." This is a long-standing open issue (since 2023, still open). Nothing documents whether Dependabot also inspects `performed_via_github_app` and rejects user-to-server comments. Our case should pass Dependabot's check, because the comment author is a human who has push access. This is the central **UNVERIFIED** assumption for P2, and it needs a spike.
- **Commands still supported** (per the docs page after the 2026-01-27 retirement): `rebase`, `recreate`, `ignore this dependency|major|minor|patch version`, `show DEP ignore conditions`, and for grouped PRs `ignore DEP [major|minor|patch version]` and `unignore *|DEP|DEP COND`. **Retired:** `merge`, `cancel merge`, `squash and merge`, `close`, `reopen`.
- **Other Dependabot constraints:**
  - Dependabot stops auto-rebasing a PR that has been unmerged for 30 days. (The docs leave open whether the manual `@dependabot rebase` still works after that.)
  - Dependabot stops rebasing once extra commits are pushed. So `update-branch` (a merge commit) as a fallback disables later Dependabot rebases, unless commits carry `[dependabot skip]`.
- **The §3 "private repo" report is not an auth issue.** dependabot-core #15566 (rebase/recreate failing on a private repo since about 2026-06-29) is still open. It is an npm-in-subdirectory updater failure (`package.json not found`) that a maintainer could not reproduce. It has nothing to do with who posted the comment. #13195 ("already up-to-date" despite conflicts; `recreate` as a workaround) is a separate flakiness report.

**Evidence**
- "API requests made by an app on behalf of a user will be attributed to that user. For example, if your app posts a comment on behalf of a user, the GitHub UI will show the user's avatar photo along with the app's identicon badge … the logs will list the user as the actor but will state that the "programmatic_access_type" is "GitHub App user-to-server token"." https://docs.github.com/en/apps/creating-github-apps/authenticating-with-a-github-app/authenticating-with-a-github-app-on-behalf-of-a-user
- "Installation access tokens will attribute activity to your app … User access tokens will attribute activity to a user and to your app." / "if your GitHub App takes an action on behalf of a user, it should always use a user access token". https://docs.github.com/en/apps/creating-github-apps/about-creating-github-apps/best-practices-for-creating-a-github-app
- dependabot-core #9147 "Support Dependabot commands by GitHub apps" (open since 2024-02-27): "Whenever a Dependabot command (e.g. `@dependabot rebase`) is filed via a GitHub app … `Sorry, only users with push access can use that command.`" The payload quoted there shows `user.type: "Bot"` with `performed_via_github_app`. https://github.com/dependabot/dependabot-core/issues/9147. Earlier community thread from 2023-02, still reported in 2025: https://github.com/orgs/community/discussions/48010
- Retirement: "These comment commands are no longer supported: @dependabot merge, @dependabot cancel merge, @dependabot squash and merge, @dependabot close, @dependabot reopen … For merging, closing, or reopening pull requests, we recommend using GitHub's built-in UI, the GitHub CLI, and the REST API endpoints". https://github.blog/changelog/2026-01-27-changes-to-github-dependabot-pull-request-comment-commands/
- Current command list: `@dependabot rebase` "Rebases the pull request."; `@dependabot recreate`; ignore/show/unignore. https://docs.github.com/en/code-security/reference/supply-chain-security/dependabot-pull-request-comment-commands
- "Dependabot will react with a "thumbs up" emoji to acknowledge the command" / "If a pull request has not been merged for 30 days, Dependabot will stop rebasing the pull request." / "By default, Dependabot will stop rebasing a pull request once extra commits have been pushed to it." https://docs.github.com/en/code-security/dependabot/working-with-dependabot/managing-pull-requests-for-dependency-updates
- https://github.com/dependabot/dependabot-core/issues/15566 (open; maintainer: "In every case recreate/rebase succeeded, so I couldn't trigger the `package.json not found` error.") and https://github.com/dependabot/dependabot-core/issues/13195

**Confidence:** high on attribution, the bot-token rejection and the command list. **Low** on user-token comments being accepted. Nothing contradicts it, but nothing confirms it either.

## Q6. Org policies that can block apps

**Short answer.**
- **OAuth app access restrictions** apply to *OAuth apps*, not GitHub Apps. A GitHub App reaches org data only through an **installation**, which org owners control. A user token likewise reaches an org's resources only if the app is installed there.
- **Who can install.**
  - Owners can restrict installs to owners only.
  - Repo admins can otherwise install apps only if the app requests **no org permissions and no repo Administration** permission.
  - Members and outside collaborators can *request* installs, if app access requests are enabled. They are by default, and owners can disable them.
- **Permission changes** require each installing account's owner to approve. Until they do, the installation keeps the old permissions; it does not break.
- **SAML SSO (Enterprise Cloud).**
  - When the user authorizes the app, they need an active SSO session for each SSO org. Otherwise the token simply has **no access** to that org, which silently shows up as missing repos or 404s.
  - After SSO is enabled, users must re-authorize.
  - GitHub tells apps to call `GET /user/installations` regularly to see which orgs a token can reach.
  - "Access tokens created by apps are automatically authorized for SAML SSO". This covers installation tokens; for user tokens it holds only for orgs with an active session at sign-in.
  - Whether a *refreshed* token keeps the SSO credential authorizations of the original sign-in is **UNVERIFIED**. It is likely, since the docs tie authorization to the "sign-in session with the app", but not stated.
- **IP allow lists (Enterprise Cloud).** These **do** apply to GitHub App user-to-server and installation tokens. The app can publish its own allow list, which orgs can choose to inherit. Cloudflare Workers egress does not come from a small fixed IP set unless you pay for a dedicated egress product (**UNVERIFIED** for the current CF offering). So an org with an IP allow list will block our Workers-hosted calls unless the org adds the ranges or we egress through a fixed-IP proxy.

**Evidence**
- "When OAuth app access restrictions are enabled, organization members and outside collaborators cannot authorize OAuth app access to organization resources." https://docs.github.com/en/organizations/managing-oauth-access-to-your-organizations-data/about-oauth-app-access-restrictions
- "For GitHub Apps this is a request for installation … For OAuth apps this is a request to allow the app through the OAuth app access restrictions" / "By default, app access requests are enabled." / "repository admins can install GitHub Apps … if the app does not request organization permissions or the "repository administration" permission … As an organization owner, you can restrict GitHub App installations to only organization owners." https://docs.github.com/en/organizations/managing-programmatic-access-to-your-organization/limiting-oauth-app-and-github-app-access-requests-and-installations
- "When you do so, the owner of each account where the app was installed will be prompted to approve the new permissions. If the account owner does not approve the new permissions, their installation will continue to use the old permissions." https://docs.github.com/en/apps/creating-github-apps/registering-a-github-app/choosing-permissions-for-a-github-app
- "You must have an active SSO session each time you authorize an OAuth app or GitHub App in order to access an organization that uses or enforces SSO … the app will be unable to access that organization." / "after you authenticate via SSO for the first time, you must reauthorize any OAuth apps or GitHub Apps". https://docs.github.com/en/authentication/authenticating-with-saml-single-sign-on/about-authentication-with-saml-single-sign-on
- "When you sign in to an app, a credential authorization is created for each organization that you have an SSO session for … The SSO credential authorization is tied to the specific sign-in session with the app." https://docs.github.com/en/enterprise-cloud@latest/apps/using-github-apps/saml-and-github-apps
- "Access tokens created by apps are automatically authorized for SAML SSO." https://docs.github.com/en/enterprise-cloud@latest/rest/authentication/authenticating-to-the-rest-api
- "If an organization uses SAML SSO and a user has not performed SAML SSO, the user access token will not have access to that organization. You should use the `GET /user/installations` REST API endpoint regularly". (best-practices page)
- IP allow list covers "GitHub App user-to-server or installation tokens" / "If you select **Enable IP allow list configuration for installed GitHub Apps** … IP addresses from installed GitHub Apps are added to your allow list." https://docs.github.com/en/enterprise-cloud@latest/organizations/keeping-your-organization-secure/managing-security-settings-for-your-organization/managing-allowed-ip-addresses-for-your-organization

**Confidence:** high on the policy mechanics. Medium on the SSO-plus-refresh interaction. Medium on the Workers egress-IP problem, which is a CF product question.

## Q7. Permission UX at install time

**Short answer.**
- The install page lists the requested repository, organization and account permissions. For apps with repo permissions it offers **All repositories** or **Only select repositories**; the app always gets read access to public repos.
- The button reads **Install**, **Install and request**, or **Request**, depending on how much of the requested access the person may grant.
- An app requesting **org Members: write** (P1 org invites) or **repo Administration: write** (P1 repo collaborator invites, P3) can only be installed by an **org owner**. Non-owners can only *request* it, and only if requests are enabled.
- Once an owner has installed it, repo admins can add their repos to the existing installation.
- Authorization is a separate screen ("Act on your behalf", plus any account permissions such as email read for P1's domain check). It can happen without an install, and vice versa.
- The exact wording on the install screen for these permissions is **UNVERIFIED**: docs show only a screenshot of a different app.
- Relevant endpoint permissions:
  - `POST /orgs/{org}/invitations`: org Members write
  - `PUT /repos/{o}/{r}/collaborators/{u}`: repo Administration write
  - `PUT …/pulls/{n}/merge` and `merge-async`: Contents write
  - `POST …/issues/{n}/comments`: Issues or Pull requests write

**Evidence**
- "If the app requires repository permissions, select **All repositories** or **Only select repositories**. The app will always have at least read-only access to all public repositories on GitHub." / "Click **Install**, **Install and request**, or **Request**. The button that is presented depends on whether your organization owner must approve none, some, or all of the requested access". https://docs.github.com/en/apps/using-github-apps/installing-a-github-app-from-a-third-party
- "Repository admins can install GitHub Apps in the organization that owns the repository if the app does not request any organization permissions or the "Repository administration" permission." / "Organization members and outside collaborators that cannot install an app on the organization can still select the organization during the install process. Instead of installing the app, GitHub will send a notification to the organization owner". (same page)
- The authorization screen grants "Verify your GitHub identity", "Know which resources you can access", and "Act on your behalf". https://docs.github.com/en/apps/using-github-apps/authorizing-github-apps
- Permission tables: https://docs.github.com/en/rest/authentication/permissions-required-for-github-apps. Sections: "Organization permissions for "Members"", "Repository permissions for "Administration"", "Repository permissions for "Contents"", "Repository permissions for "Issues"/"Pull requests"".
- Pending requests can be read by the app via `GET /app/installation-requests` ("List installation requests for the authenticated app"). https://docs.github.com/en/rest/apps/apps

**Confidence:** high on the mechanics; low on the exact screen copy.

---

## Design implications

### C1: Installation token broker

1. **One broker per (installation, scope) as a Restate Virtual Object.** Key it `inst:{installation_id}:{scope_hash}`. Its exclusive `get_token` handler returns a cached token when `expires_at - now > margin` (e.g. 10 min) and otherwise mints a new one. The single writer per key gives single-flight minting for free. Restate docs: "At most one handler with write access can run at a time per object key" (https://docs.restate.dev/foundations/services). Mint errors to handle:
   - 401 on mint means a bad JWT or clock skew. Re-sign and retry once.
   - 404 means uninstalled. Mark the installation gone and fail terminally.
   - 403 means suspended or not allowed. Fail terminally and surface it to the user.
   - 422: back off.
2. **Scope down by default, but keep the scope set small.** Mint tokens with only the permissions a workflow kind needs: P1 `members:write`; P2 `contents:write, pull_requests:write, workflows:write?`; reads `metadata, pull_requests:read, checks:read`. Avoid one token per repo: limit the scope set to a few named profiles. Each profile gets one cache entry, so minting stays at about 1 per profile per installation per 50 min.
3. **Workflows never hold a token across a suspension.** Fetch the token right before each GitHub call batch. A workflow that sleeps (P1 invite waiting 7 days, bulk-op review step) re-asks the broker after waking. Do not journal tokens in workflow state. If a token has to pass through `ctx.run` output, it goes through the C3 codec, because 520-char JWT-format tokens are now the norm.
4. **TokenExpired handling.** A GitHub call that returns 401 with a broker token calls `broker.invalidate(token_id)` and retries once with a fresh token. A second 401 is a terminal error. Use `expires_at` from the mint response, not "now + 1h".
5. **The JWT is cached inside the broker too.** Sign once, reuse for about 8 min, keep `iat = now-60`. Convert the PKCS#1 key to PKCS#8 or JWK at provisioning. Support several active keys: GitHub allows 25, and multiple keys allow zero-downtime rotation.
6. **Storage and redaction.** Size columns and Restate state for tokens of 600+ chars. Redaction patterns must match `ghs_[A-Za-z0-9.\-_]{36,}`, `ghu_`, `ghr_`. Never put tokens in error messages, because Restate journals failures in plain text (see PLAN §3).
7. **Rate budgets.** P1 is bounded by the invite limit (50 or 500 per day) long before token limits. For P2 reads, use installation tokens: each installation has its own 5k–12.5k/h bucket. Keep user tokens for actions that must be attributed to the user, so we don't spend the user's personal 5k/h.
8. **Enterprise IP allow lists.** Configure the app's IP allow list only if we have fixed egress. Otherwise document the limitation in onboarding (see Q6).

### C2: User token vault with exactly-once refresh

1. **One Virtual Object per (user, app).** State holds the encrypted `{access, access_expires_at, refresh, refresh_expires_at, generation}`. All reads of a valid token go through a *shared* handler. Refresh runs only in the *exclusive* `refresh(expected_generation)` handler, so two callers can never send the same refresh token. That gives single-flight refresh within one Restate cluster.
2. **Generation check, CAS-style.** A caller that saw generation *g* and got a 401 calls `refresh(g)`. If state is already at *g+1*, the handler returns the new token without calling GitHub. This kills the thundering herd and the "refresh twice" bug.
3. **The window Restate cannot close.** The refresh POST goes inside `ctx.run`. If the Worker dies after GitHub rotated the token but before the `ctx.run` result is acknowledged into the journal, Restate retries the step and sends the *old* refresh token. GitHub then answers `bad_refresh_token`, and the grant is lost. GitHub offers no idempotency key and documents no grace window, and nothing on our side can make this external call idempotent. Mitigations:
   - Keep the window tiny: no work between the HTTP response and the end of `ctx.run`.
   - Refresh rarely (see 4).
   - Treat a `bad_refresh_token` that follows a retried step as "lost grant", and send the user a one-click re-auth link (a notification, not a silent failure).
   - If the spike shows a grace window (the same response returned for a replay within N seconds), the window disappears.
4. **When to refresh.**
   - **Lazily**, when an action needs a token and `access_expires_at - now < 5 min`.
   - **On 401.**
   - **Keep-alive timer:** a Restate delayed self-call, e.g. every 30 days, that refreshes only if `refresh_expires_at - now < 60 days`. This keeps idle users' 6-month refresh tokens alive with few rotations.
   - Do *not* refresh every user every 8 h. That multiplies exposure to the crash window and to the "2,000 OAuth access token requests per hour" per-app limit.
5. **Don't use old access tokens after a refresh.** In-flight calls with the old `ghu_` will 401. They must re-read from the vault, not retry with a cached copy.
6. **Revocation signals.** Any of these moves the vault to `revoked` and stops all P2 user actions:
   - the `github_app_authorization` webhook
   - 401 on a fresh token
   - `bad_refresh_token`
   - the refresh token reaching expiry

   The UI then prompts re-auth. On user disconnect, call `DELETE /applications/{client_id}/grant` (Basic auth), so orphaned grants don't use up the 10-token cap.
7. **Ten-token cap: separate sign-in from acting.**
   - If Clerk or any login flow does a GitHub *web flow with the same app* on every sign-in, each sign-in mints a new user token. The 11th can revoke the vault's long-lived token (UNVERIFIED for GitHub Apps; reported by a third party).
   - Options: a separate GitHub App or OAuth app for login identity; or run the authorize flow for the vault only once and on explicit re-auth.
   - Never mint user tokens in tests against the production app and user.
8. **SSO.** After authorization and periodically, call `GET /user/installations` and store which orgs the token reaches. In the dashboard, show "SSO session needed for org X → re-authorize" instead of silently showing fewer PRs.
9. **Narrow tokens per action.** Optionally derive per-action tokens with `POST /applications/{client_id}/token/scoped` (one repo, `pull_requests:write`). Whether a scoped token is independent of later refreshes of the parent is **UNVERIFIED**.
10. **The OAuth token endpoint's error format.** Parse the JSON `error` field whatever the HTTP status. Map `bad_refresh_token` to a terminal "lost grant". Map `incorrect_client_credentials` to an ops alert: client secret rotated. Do not mark the user revoked in that case.

### C5 / product notes

- P1 needs org owners to install it, because of Members: write and Administration: write. Split into two apps: org invites (Members) and repo invites (Administration). That way a cautious owner can grant only the half they want, which matches `research/github-invitations.md` §10. Use `GET /app/installation-requests` to show pending requests in onboarding.
- P2 bulk merge:
  - Use `PUT …/pulls/{n}/merge-async`, which supports merge queues and the `sha` guard, or `…/merge`. Both need Contents: write.
  - Use the user token when the merge is a user decision (best practice, and it enforces the user's own rights). Use the installation token only for policy-driven automation.
  - Content-creation secondary limits (80/min, 500/h) apply to merges and comments. Pace bulk operations.
  - Merging PRs that touch `.github/workflows/` likely also needs the **Workflows** permission. This comes from many third-party reports and is **UNVERIFIED** in the docs.

---

## Risk verdicts

| Risk | Verdict | One line |
|---|---|---|
| R-C1-token-limits | **resolved-feasible** | 1 h tokens, cached per (installation, scope profile), mean about 1 mint per profile per installation per hour. That is orders of magnitude under any documented secondary limit (≈180 POST/min/endpoint), and invites are capped at 50–500/day anyway. Mint throttling itself is undocumented. |
| R-C2-refresh | **feasible-with-changes** | A Virtual Object exclusive handler plus generation CAS gives single-flight refresh. One irreducible crash window remains (GitHub rotated, journal not acked), so the design must accept a rare forced re-auth with a good UX. A spike should check for any GitHub grace window. |
| R-P2-rebase-as-user | **needs-spike** | Attribution to the user is documented, and the only known rejection is for bot/installation identities (#9147). Nothing documents whether Dependabot accepts comments carrying `performed_via_github_app`. #15566 is an unrelated npm updater bug. |
| R-P2-merge | **resolved-feasible** | `PUT /pulls/{n}/merge` or `merge-async` works with both UAT and IAT given Contents: write. Branch protection and rulesets apply. Workflow-file PRs probably need Workflows: write (UNVERIFIED). |
| R-ALL-org-policy | **feasible-with-changes** | Nothing blocks a correctly installed app *silently* except three cases: SSO orgs without an active session at authorization (missing orgs); Enterprise IP allow lists (Workers egress); and permission upgrades awaiting owner approval. Each needs explicit detection and UX: `GET /user/installations`, error surfacing, an onboarding checklist. |

## Spikes to settle the open items

All of these need real GitHub credentials: a test org, a test GitHub App with a private key and client secret, and a second test user. S5 also needs an Enterprise Cloud org with SAML, or a trial.

1. **S1 Rebase as user** (decides P2 scope).
   - Setup: test org, private repo with Dependabot version updates producing a PR. Test app with `pull_requests:write` installed there. Authorize it as a user with write access. Post `@dependabot rebase` via `POST /repos/{o}/{r}/issues/{n}/comments` with the `ghu_` token. Repeat on a public repo, and with an installation token as the control.
   - **Pass:** Dependabot reacts 👍 and the PR head SHA changes or "already up to date" is reported, without "only users with push access". **Fail:** a rejection message or no reaction within 15 min.
   - Also try `@dependabot recreate`.
2. **S2 Refresh replay / concurrency.**
   - Setup: an authorized test user with an expiring token. (a) Fire two refreshes with the same `ghr_` in parallel. (b) Refresh, then replay the same old `ghr_` after 1 s, 10 s and 60 s. (c) Check whether the old `ghu_` still works right after the refresh.
   - **Pass** (good news): a replay inside some window returns the same new pair, i.e. a grace window exists. **Fail** (expected): `bad_refresh_token`. Record the HTTP status of the error response.
   - Record whether a lost race also invalidates the *winner's* new pair. If it does (reuse detection), the crash window costs even more.
3. **S3 Crash-safe vault in Restate** (no GitHub needed, fake server). Fault-inject Worker death (a) before the POST, (b) after the HTTP response but inside `ctx.run`, (c) after the journal ack. **Pass:** (a) and (c) never lose the grant, and (b) ends in a clean "lost grant" state with a re-auth prompt, not a stuck object. Also run 50 concurrent callers and **pass** only if exactly one refresh happens.
4. **S4 JWT on Workers wasm.** Sign an RS256 JWT in Rust on `wasm32` (WebCrypto via `worker` and pure `rsa`) with a PKCS#8-converted key, then call `GET /app`. **Pass:** 200 and CPU time under 10 ms per signature. Needs only the app ID and private key.
5. **S5 SSO plus refresh** (Enterprise Cloud org with SAML). Authorize with an SSO session, refresh the token, then drop the SSO session and refresh again. **Pass:** the refreshed token still reaches the SSO org. Also test without a session at authorization; it is expected to be missing from `GET /user/installations`.
6. **S6 Ten-token cap for GitHub App user tokens.** Run the web flow 11 times for the same user and app, then test token #1. **Pass** (cap absent): #1 still valid. **Fail:** #1 revoked. If it fails, C5 must separate login from the acting app.
7. **S7 Merge of a workflow-touching Dependabot PR.** Merge with an IAT lacking Workflows permission, then with it, then with a UAT. Record which succeed. This settles the Workflows-permission requirement for P2's app manifest.
8. **S8 Revoke a stateless installation token** (cheap). Mint a token, call `DELETE /installation/token`, then reuse it. **Pass:** 401.
