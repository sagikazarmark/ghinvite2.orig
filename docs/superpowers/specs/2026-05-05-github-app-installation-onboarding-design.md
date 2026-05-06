# GitHub App Installation Onboarding Design

**Goal:** Make GitHub App installation reliably return users to ghinvite, verify the installation belongs to the signed-in user, and persist the installation so account dashboards and repository pickers become usable immediately.

**Primary configuration:** GitHub App **Setup URL** is `{GHINVITE_BASE_URL}/setup/github`. Enable **Redirect on update** so repository-selection changes also return to the app.

**Non-goal:** Do not require GitHub's "Request user authorization during installation" setting. The OAuth callback can still defensively handle `installation_id`, but docs and tests treat the Setup URL path as the supported install return path.

---

## Current Failure

The app currently has no complete onboarding path after installation.

- `README.md` instructs users to set repository permission `Members`, but GitHub App repository permissions do not include `Members`; collaborator management requires repository `Administration: Read & write`.
- The app install link sends users to GitHub without a reliable return to ghinvite because no Setup URL is documented.
- `crates/web/src/routes/oauth.rs` accepts `installation_id` in the callback query but only logs it.
- `crates/web/src/routes/webhook.rs` handles `installation.created` by logging only. It never calls `Installation::onboard`.
- `crates/web/src/routes/home.rs` redirects signed-in users only if `storage.list_active_installations()` returns a row. New installs never create that row, so users loop back to the install CTA.

## Design

### User Flow

1. User signs in to ghinvite.
2. User clicks `/install` and installs the GitHub App on a user account or organization.
3. GitHub redirects to `/setup/github?installation_id=<id>&setup_action=install`.
4. If the session is missing, ghinvite redirects to `/login?return_to=/setup/github?...` and resumes after OAuth.
5. ghinvite calls `GET /user/installations` with the user access token.
6. ghinvite finds the installation whose `id` matches the query parameter.
7. ghinvite maps the installation account to `domain::Account` fields and calls `Installation::onboard` through Restate, waiting for it to complete before redirecting.
8. ghinvite redirects to `/accounts/{account_login}`.

The same route handles `setup_action=update` after repository-selection changes. For updates, the route verifies the installation, refreshes the stored selected-repository ids, and redirects to the account dashboard.

### Trust Boundary

The `installation_id` query parameter is untrusted because anyone can call the setup URL manually. The route must not persist anything from the query string alone.

Verification requires all of the following:

- The request has an authenticated ghinvite session with a GitHub user token.
- `GET /user/installations` returns an installation with the same `id`.
- The returned installation has an account with `id`, `login`, and `type` equal to `User` or `Organization`.
- For organization dashboards, existing admin authorization remains enforced by `RequireAdminOf` using `/user/memberships/orgs/{login}`.

This separates **installation association** from **admin authorization**. Association means the user can see the app installation. Authorization means the user may administer ghinvite for that installed account.

### GitHub API Additions

Add user-token response types in `crates/github/src/payloads.rs`:

- `GhUserInstallationList { total_count, installations }`
- `GhUserInstallation { id, account, repository_selection, target_type, target_id }`

Reuse `GhUser` for the `account` shape because it already includes `id`, `login`, optional `avatar_url`, and optional GitHub `type`.

Add `UserApiClient::list_user_installations()` in `crates/github/src/oauth.rs`:

```text
GET /user/installations?per_page=100
```

Pagination can stay at page 1 for this fix because existing repo-list methods already use the same v1 simplification. A later reconciliation pass can add pagination across GitHub App installation APIs.

### Web Route Additions

Create `crates/web/src/routes/setup.rs` and register it in `crates/web/src/routes/mod.rs` and `crates/web/src/lib.rs`.

The route shape is:

```text
GET /setup/github?installation_id=<id>&setup_action=<install|update>
```

Behavior:

- Missing `installation_id` returns a bad request page/error.
- Missing session redirects to `/login?return_to=<validated setup path>`.
- Unknown installation id returns a GitHub OAuth/setup error and does not write storage.
- Successful verification calls `Installation::onboard` with:
  - `installation_id` from the verified GitHub response
  - `actor_user_id` from the signed-in session
  - `account_id` from the verified installation account
  - `account_login` from the verified installation account
  - `account_type` parsed from account `type` or `target_type`
  - `selected_repos` from `repository_selection`: `all` maps to `SelectedRepos::All`, `selected` maps to `SelectedRepos::Subset(ids)` after listing installation repositories
  - `installed_at` as `Utc::now()` because the minimal current domain row stores one timestamp and the user-visible behavior does not depend on GitHub's exact creation time
- Successful onboarding redirects to `/accounts/{account_login}`.

Update `crates/web/src/session.rs` so `validate_return_to()` accepts `/setup/github?installation_id=...` in addition to existing `/i/...` paths. It must still reject absolute URLs, protocol-relative URLs, and paths containing `..`. Setup redirects must URL-encode the full setup path before appending it to `/login?return_to=` so query parameters survive the round trip.

### Repository Selection State

For a selected-repository installation, the installation list response only tells us `repository_selection=selected`; it does not include every repository id. After verifying the installation, the route calls `GET /user/installations/{id}/repositories` and persists the returned repository ids as `SelectedRepos::Subset(ids)`. This keeps settings and dashboard state accurate immediately after install instead of storing an empty subset.

### Webhook Role

Webhooks are reconciliation signals, not the primary onboarding path.

- `installation.deleted` remains responsible for uninstall cleanup.
- `installation_repositories` remains responsible for access changes after install.
- `installation.created` may be enhanced later to onboard from payload as a backup, but the setup route is the reliable path because it can verify the acting user and return a dashboard immediately.

### Documentation Changes

Update `README.md` and `docs/deploy.md`:

- Add Setup URL: `{GHINVITE_BASE_URL}/setup/github`.
- Recommend enabling Redirect on update.
- Replace repository permission `Members` with `Administration: Read & write` and `Metadata: Read-only`.
- Explain that `installation` and `installation_repositories` are default app-level webhook events, not repository-level selectable webhook types.
- Keep webhook URL and secret instructions because invitation and reconciliation events still need HMAC verification.

### Tests

Add tests at the lowest practical layer:

- `crates/github/src/payloads.rs`: decode a minimal `GET /user/installations` response for an organization install and a user install.
- `crates/github/src/oauth.rs`: `list_user_installations()` sends `GET /user/installations?per_page=100` with user-token auth and decodes the response.
- `crates/web/src/routes/setup.rs`: setup route rejects missing `installation_id`, redirects unauthenticated users through login with a safe `return_to`, rejects spoofed installation ids, and sends `Installation::onboard` for a verified org install.
- `crates/web/src/session.rs`: `validate_return_to()` accepts safe setup paths and still rejects open redirects and path traversal.
- Route smoke: the web router includes `/setup/github`.
- Docs check can be manual unless the repo already has markdown tests.

### Success Criteria

- A user who installs the GitHub App on an organization returns to ghinvite and lands on `/accounts/{org}`.
- A user who installs the GitHub App on a personal account returns to ghinvite and lands on `/accounts/{login}`.
- Spoofed setup URLs do not create installation rows.
- Repository picker pages have repositories available after installation.
- Setup instructions match GitHub's actual permission names and install-return settings.
