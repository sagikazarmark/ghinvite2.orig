# ghinvite v1 — Design

**Status:** approved (interview phase complete; ready for implementation planning)
**Date:** 2026-05-04
**Scope:** v1 specification, with explicit v1.1 and v2+ deferral lists

---

## 1. Product summary

**ghinvite** lets a GitHub account holder — an organization owner, or the holder of a personal GitHub account — generate a shareable URL that grants someone else collaborator access to specific repositories on that account. The recipient clicks the URL, signs in with GitHub, and either receives the GitHub invitation immediately or waits for an admin to approve the request.

**Differentiation from existing tooling** (informed by prior-art research):

- **Shareable-link UX.** No assumption that the recipient is in your IdP, Slack, or contact list — a URL works wherever people communicate. JIT access platforms (Opal, Entitle, P0) all assume IdP-anchored requesters; the closest direct competitor (`probot/invite`) is abandoned.
- **Durable-workflow correctness.** Restate gives "exactly-once-effects" on outbound GitHub API calls and a clean model for the "wait up to 7 days for the recipient to act" pattern.
- **Audit log as system of record.** GitHub's audit log retains 90 days; SOC2 expects ~12 months. Our audit table is append-only and indefinite by default.

**Out-of-scope for v1 (explicit v2 list at §19).** This is not a generic JIT access platform; not an SSO replacement; not a seat-management tool.

---

## 2. Architecture

```
   GitHub webhook  ─►  Web Worker (verifies HMAC) ──┐
       browser     ─►  Web Worker (auth, reads D1)  │
                          │                         │
                          │ start/send via          │ start/send
                          │ Restate Cloud API       │
                          ▼                         ▼
                   Restate Cloud (managed: journal, timers, retries)
                          │
                          │ invokes handlers via HTTPS
                          ▼
                   Restate Service Worker
                   (executes handlers, writes D1, calls GitHub)
                          │
                          ▼
                          D1 (SQLite)
```

**Three deployable artifacts:**

1. **Web Worker** (Cloudflare Worker, `wasm32-unknown-unknown`). Hosts the dashboard (Dioxus SSR + hydration), public invitation-link pages, OAuth callback, GitHub webhook receiver. Reads D1 directly. Initiates state changes by calling Restate Cloud's invocation API. Does not write D1 except for OAuth/session tables.
2. **Restate Service Worker** (Cloudflare Worker, `wasm32-unknown-unknown`). Hosts Restate handler endpoints. Called only by Restate Cloud over HTTPS. All domain-state writes happen here. Holds the GitHub App private key, mints installation tokens, calls the GitHub API.
3. **Restate Cloud** (managed). Durable journal, timers, retries, exactly-once-effects.

**Local development:** native binaries for both Workers via the same Rust crates compiled for the host target. Restate runs locally in Docker. SQLite file as DB.

**Production:** two Workers deployed via `wrangler`, D1 as DB, Restate Cloud as the durable runtime.

### 2.1 Why two binaries

- **Separation of concerns.** Web binary does HTTP (auth, rendering, webhook ingest). Restate binary does workflows (state mutation, GitHub API). Different attack surfaces, different deploy cadences.
- **Secret scoping.** GitHub App private key never enters the web binary's address space. Webhook secret stays in the web binary; never enters the Restate binary.
- **Rate-limit isolation.** GitHub installation-token rate limits are managed in one place (Restate binary); the web binary's GitHub use is user-scoped only.

### 2.2 Workspace layout

```
ghinvite/
├── Cargo.toml                  # workspace
├── crates/
│   ├── domain/                 # types, state machines, business rules (no I/O)
│   ├── storage/                # Storage trait + sqlx impl + D1 impl
│   ├── github/                 # User-OAuth client + installation-token client
│   ├── audit/                  # AuditEvent type + helpers
│   ├── web/                    # web binary (Dioxus + axum native / worker wasm)
│   └── restate-svc/            # Restate handler binary
├── migrations/                 # plain SQLite-portable SQL files
├── wrangler/
│   ├── web.toml                # web Worker config
│   └── restate-svc.toml        # Restate Service Worker config
└── docs/
    └── superpowers/specs/      # this file lives here
```

---

## 3. Tech stack

| Concern | Choice |
|---|---|
| Language | Rust (one workspace, native + `wasm32-unknown-unknown`) |
| Frontend | Dioxus fullstack — SSR + hydration, server functions for state-changing actions |
| Styling | Tailwind + DaisyUI; three theme zones (home, dashboard, invitation) |
| Sessions | `tower-sessions` with KV-backed store on Workers, file/sqlite store in dev |
| OAuth client | `oauth2` crate (GitHub user-auth flow) |
| GitHub API | `octocrab` for both user tokens and installation tokens |
| Database | `sqlx` (native) and `worker::D1Database` (wasm) behind one `Storage` trait |
| IDs | `ulid` (sortable, time-prefixed) |
| Workflows | Restate Rust SDK |
| Deploy | `wrangler` (Workers) + Restate Cloud |

The user has previously verified that `tower-sessions`, `oauth2`, and `octocrab` work on the Cloudflare Workers wasm target.

---

## 4. Deployment model

**B+ multi-tenant publicly hostable.** Anyone with a GitHub account or organization can install the App on their account. No separate sign-up form (installation = onboarding). No billing in v1. No quota enforcement in v1.

The tenancy boundary is the GitHub installation, keyed by `account_id` (numeric, immutable across renames). Reinstallation produces a new `installation_id` per the GitHub docs; we keep the old row for audit and treat it as a new active installation.

---

## 5. Authorization model

App-admin authority is derived from GitHub authority. There is no app-internal role assignment in v1.

- **For organization installations:** anyone whose GitHub identity is currently an org *owner* of the installed account is an app-admin for that account's dashboard. Determined by `GET /user/memberships/orgs/{login}` returning `role == 'admin'` and `state == 'active'`. Cached for **60 seconds** per session (was 5 minutes — shortened per /autoplan finding: a demoted owner had a 5-minute window to act on stale authority, both a security hole and a retroactive-failure UX problem).
- **For personal-account installations:** the account holder themselves (and only that user) is the admin. Determined by `session.user_id == installation.account_id`.

**Re-checked at every state-changing action**, so a demoted org owner cannot continue to act through a stale session.

**Recipients** (people clicking invitation links) need only a valid GitHub login. The link itself is the authorization to *request*. There is no app-internal recipient authorization beyond GitHub OAuth identity.

**Self-approve allowed.** An admin who happens to also be a recipient (e.g., testing their own link) may approve their own request. There is no security boundary they are crossing.

---

## 6. Invitation scope (what v1 does and doesn't grant)

v1 supports exactly one form of invitation: **repo-collaborator** invitations via `PUT /repos/{owner}/{repo}/collaborators/{user}` with one of the five GitHub permission levels: `pull` / `triage` / `push` / `maintain` / `admin`.

An invitation link grants the recipient access to **N selected repos at one permission level**. (Multi-repo, single-level-per-link is the v1 shape.)

Not in v1:
- Org-membership invitations (would consume seats).
- Team additions.
- Per-repo permission within a single link.

---

## 7. Data model (D1 / SQLite)

All schema is SQLite-portable so it runs identically against `sqlx` (native dev) and D1 (production). One set of migration files in `migrations/` applied via `sqlx-migrate` locally and `wrangler d1 migrations apply` in production.

### 7.1 Tables

```sql
-- One row per (App, account) ever installed; kept on uninstall for audit.
CREATE TABLE installations (
  installation_id   INTEGER PRIMARY KEY,
  account_id        INTEGER NOT NULL,
  account_login     TEXT    NOT NULL,
  account_type      TEXT    NOT NULL CHECK (account_type IN ('User','Organization')),
  installed_at      TEXT    NOT NULL,
  uninstalled_at    TEXT,
  selected_repos    TEXT    NOT NULL  -- 'all' | JSON array of repo IDs
);
CREATE INDEX idx_installations_account_login ON installations(account_login);
-- At most one active installation per account:
CREATE UNIQUE INDEX idx_installations_active_account
  ON installations(account_id) WHERE uninstalled_at IS NULL;

-- Cached GitHub user info for everyone we've seen.
CREATE TABLE users (
  user_id      INTEGER PRIMARY KEY,
  login        TEXT NOT NULL,
  avatar_url   TEXT,
  last_seen_at TEXT NOT NULL
);

-- tower-sessions backing store. Schema owned by tower-sessions.

CREATE TABLE invitation_links (
  id                 TEXT PRIMARY KEY,                -- ulid
  slug               TEXT NOT NULL UNIQUE,            -- 16 base62 chars
  installation_id    INTEGER NOT NULL REFERENCES installations(installation_id),
  account_id         INTEGER NOT NULL,                -- denormalized
  created_by         INTEGER NOT NULL REFERENCES users(user_id),
  created_at         TEXT    NOT NULL,
  expires_at         TEXT,
  max_uses           INTEGER,                          -- NULL = unlimited
  uses_count         INTEGER NOT NULL DEFAULT 0,
  permission         TEXT    NOT NULL,                 -- validated by domain enum; not CHECKed in SQL (see §7.2)
  approval_required  INTEGER NOT NULL,                 -- 0/1
  internal_note      TEXT,
  revoked_at         TEXT,
  revoked_by         INTEGER REFERENCES users(user_id)
);
CREATE INDEX idx_invitation_links_account ON invitation_links(account_id);

CREATE TABLE invitation_link_repos (
  invitation_link_id   TEXT    NOT NULL REFERENCES invitation_links(id),
  repo_id         INTEGER NOT NULL,
  repo_full_name  TEXT    NOT NULL,
  PRIMARY KEY (invitation_link_id, repo_id)
);

CREATE TABLE invitation_requests (
  id              TEXT PRIMARY KEY,
  invitation_link_id   TEXT NOT NULL REFERENCES invitation_links(id),
  requester_id    INTEGER NOT NULL REFERENCES users(user_id),
  justification   TEXT,
  state           TEXT NOT NULL,                       -- validated by domain enum; not CHECKed in SQL (see §7.2)
  decided_by      INTEGER REFERENCES users(user_id),
  decided_at      TEXT,
  decline_reason  TEXT,
  created_at      TEXT NOT NULL
);
-- One pending request per (link, requester) at a time:
CREATE UNIQUE INDEX idx_one_pending_per_link_per_user
  ON invitation_requests(invitation_link_id, requester_id) WHERE state = 'pending';
CREATE INDEX idx_requests_link ON invitation_requests(invitation_link_id);

-- One row per (request, repo) — the GitHub-side invitation.
CREATE TABLE github_invitations (
  id                     TEXT PRIMARY KEY,
  invitation_request_id  TEXT NOT NULL REFERENCES invitation_requests(id),
  repo_id                INTEGER NOT NULL,
  github_invitation_id   INTEGER,
  state                  TEXT NOT NULL,                  -- validated by domain enum; not CHECKed in SQL (see §7.2)
  error_message          TEXT,
  created_at             TEXT NOT NULL,
  updated_at             TEXT NOT NULL
);
CREATE INDEX idx_github_invitations_request ON github_invitations(invitation_request_id);
CREATE INDEX idx_github_invitations_github_id ON github_invitations(github_invitation_id);

-- Append-only audit log.
CREATE TABLE audit_events (
  id           TEXT PRIMARY KEY,
  account_id   INTEGER NOT NULL,
  occurred_at  TEXT    NOT NULL,
  event_type   TEXT    NOT NULL,
  actor_kind   TEXT    NOT NULL,                          -- validated by domain enum; not CHECKed in SQL (see §7.2)
  actor_id     INTEGER,
  target_kind  TEXT    NOT NULL,
  target_id    TEXT    NOT NULL,
  metadata     TEXT,
  request_id   TEXT
);
CREATE INDEX idx_audit_account_time ON audit_events(account_id, occurred_at);
CREATE INDEX idx_audit_target ON audit_events(target_kind, target_id);
```

### 7.2 CHECK constraints

The schema deliberately omits SQL `CHECK (col IN (...))` constraints on enum-shaped columns (`permission`, `*.state`, `actor_kind`). SQLite cannot alter a CHECK constraint without a 12-step `ALTER TABLE` recreate (create new table, copy rows, drop old, rename, recreate indexes), and several of these enums are expected to grow (new GitHub permission levels, the `cancelled` request state in v2, custom org roles in v2). The `installations.account_type` CHECK is kept because its values (`User`, `Organization`) are GitHub-defined and effectively immutable.

Validation lives one level up: the Rust enums in `crates/domain` are the canonical source of legal values; storage methods accept those types and stringify on write. A bad value reaching storage is a code bug, not a data-integrity event.

### 7.3 Storage trait

The storage trait exposes:
- Read methods returning typed records (`list_invitation_links`, `get_invitation_link_by_slug`, `list_pending_requests_for_account`, etc.).
- Write methods scoped to specific transitions (`insert_invitation_link`, `mark_invitation_link_revoked`, `insert_invitation_request`, `mark_request_decided`, `insert_github_invitation`, `update_github_invitation_state`).
- A single `audit(event)` method. **No `update_audit` or `delete_audit` exists in the trait.**

This shape makes "update audit" a compile-time impossibility, not a runtime check.

Two impls:
- `SqlxStorage` — native Rust, uses `sqlx::SqlitePool`. Used in dev and tests.
- `D1Storage` — wasm, uses `worker::D1Database`. Used in production.

---

## 8. Domain state machines

### 8.1 invitation_link

States are mostly **derived** rather than stored:

| Stored | `revoked_at`, `uses_count` |
|---|---|
| Computed at read time | `is_active` = `revoked_at IS NULL AND (expires_at IS NULL OR expires_at > now) AND (max_uses IS NULL OR uses_count < max_uses)` |

Why derived: no scheduler needed to flip a state column; idempotent reads; impossible to drift between "what the row says" and "what the time says."

**`uses_count` semantics:** incremented atomically when an `InvitationRequest` row is created (not on page visit, not on sign-in). A use is "a request was created via this link." If a request is declined, the use is not refunded — re-clicking creates a new request and consumes another use. This is the simplest and most obvious model; admins who don't want that behavior can leave `max_uses` null.

### 8.2 invitation_request

```
pending ──approve──► approved
        ──decline──► declined
        ──timer-fires──► expired   (timer = min(link.expires_at, created_at + 7d))
```

`cancelled` is reserved for v2 (when a link revoke cancels still-pending requests). Not used in v1.

### 8.3 github_invitation (per repo)

```
sending ──API 201──► sent ──webhook accepted──► accepted
                          ──webhook declined──► declined
                          ──webhook cancelled──► cancelled
                          ──reconciler/timer──► expired       (7d after sent)
                          ──admin/system DELETE──► cancelled
sending ──API 204──► accepted   (recipient already a member; GitHub returns 204)
sending ──API 4xx──► failed
```

The 7-day expiration timer is GitHub's hard limit on repository invitations — not configurable by us.

---

## 9. Restate workflows

All durable-state changes flow through these handlers. They are idempotent, and Restate's exactly-once-effects guarantee combines with our ulid-based deterministic IDs to give safe retries.

### 9.1 `InvitationLink` (Virtual Object, key = `invitation_link_id`)

- `create(input)` — writes link + repos rows; emits `invitation_link.created` audit.
- `revoke(actor)` — sets `revoked_at`; emits `invitation_link.revoked`.
- `tick_expiration()` — scheduled at `expires_at` if set; emits `invitation_link.expired` audit. (v2 will additionally cascade to cancel still-pending requests and still-pending GitHub invitations created via this link.)

### 9.2 `InvitationRequest` (Workflow, key = `request_id`)

```
1. Re-read invitation_link; reject if not is_active
2. Insert row state='pending'; emit request.created; increment invitation_links.uses_count
3. If link.approval_required == false: jump to step 5 with auto-approve
4. Else: race awakeable<Decision>  vs.  timer min(link.expires_at - now, 7d)
5. On approved:
     for repo in link.repos: GithubInvitation::create.send(github_invitation_id, ...)
     mark request approved; emit request.approved
6. On declined: mark request declined; emit request.declined
7. On timeout: mark request expired; emit request.expired
```

Step 4 uses a Restate `awakeable` resolved by the admin's approve/decline server function. The two timers race; Restate guarantees only one branch wins.

### 9.3 `GithubInvitation` (Virtual Object, key = our `github_invitation_id`)

- `create(input)` — mints installation token; calls `PUT /repos/.../collaborators/{user}`. Branches:
  - 201 → store `github_invitation_id`, mark `sent`, emit `invitation.sent`, schedule `tick_expire()` at +7d.
  - 204 → mark `accepted`, emit `invitation.accepted` (already a member).
  - 4xx terminal → mark `failed` with `error_message`, emit `invitation.send_failed`.
  - 5xx → Restate retry policy.
- `on_webhook(event)` — called from the webhook receiver when a `repository_invitation` event matches our row. State transition + audit.
- `cancel()` — calls `DELETE /repos/.../invitations/{github_id}`; marks `cancelled`; emits `invitation.cancelled`.
- `tick_expire()` — confirms via `GET /repos/.../invitations`; if still pending, marks `expired`, emits `invitation.expired`.

### 9.4 `Reconcile` (Service)

- `daily_run()` — invoked once per ~24h. For each non-uninstalled installation, lists pending GitHub invitations across selected repos and reconciles drift against our `github_invitations` rows. Source of authority is GitHub.

### 9.5 `Installation` (Virtual Object, key = `installation_id`)

- `onboard(installation_id)` — fetches account + repo metadata; writes `installations` row; emits `installation.created`.
- `repos_changed(payload)` — handles `installation_repositories` webhook; updates `selected_repos`; emits `installation.repos_changed`.
- `uninstall()` — sets `uninstalled_at`; emits `installation.uninstalled`.

---

## 10. Authentication & authorization flows

### 10.1 GitHub OAuth (sign-in)

1. User hits a protected route or `/login`. Web binary redirects to `https://github.com/login/oauth/authorize` with `state` (CSRF) cookie and `scope=read:user`.
2. GitHub redirects to `/oauth/callback?code&state` (and optionally `installation_id`, `setup_action` if this is post-install).
3. Web binary verifies state, exchanges code for user access token, fetches `/user`, upserts the `users` row, creates a session.
4. If `installation_id` is present, web binary calls `Installation::onboard.send(installation_id)`.

**Session contents:** `user_id`, `login`, encrypted user access token, CSRF token, last admin-check timestamp per account.

### 10.2 Install (without prior login)

1. User clicks "Install" → `/install` → redirects to GitHub App install URL.
2. After install, GitHub redirects to the same `/oauth/callback` with `installation_id` and `code` (we register it as both setup URL and OAuth callback). Same flow as 10.1 from step 3.

### 10.3 Authorization for state-changing actions

Every server function:
1. Validates the session.
2. Resolves `account_login → account_id` from `installations`.
3. Re-checks admin status:
   - User account: `session.user_id == account_id`.
   - Org account: `GET /user/memberships/orgs/{login}` returns `role=admin, state=active`. Cached 60 sec in session.
4. Loads the relevant DB row (link, request, etc.).
5. Calls Restate (one-way `send`) with explicit IDs.
6. Returns optimistic UI response.

The web binary **never writes domain state directly** (except OAuth/session tables).

### 10.4 Logout

`/logout` clears the session, redirects to `/`. No GitHub-side token revocation in v1 (GitHub user tokens already auto-expire); audit event optional.

---

## 11. URL routing (web binary)

| Route | Purpose | Auth |
|---|---|---|
| `/` | Logged-out marketing / logged-in redirect to last account | Mixed |
| `/login` | Start GitHub OAuth | Public |
| `/logout` | Clear session | Any session |
| `/oauth/callback` | GitHub OAuth + install callback | Public |
| `/install` | Redirect to GitHub App install URL | Public |
| `/accounts/:login` | Account dashboard | Account admin |
| `/accounts/:login/links/new` | Create link form | Account admin |
| `/accounts/:login/links/:link_id` | Link detail (settings + request history + "stop accepting new requests" action) | Account admin |
| `/accounts/:login/requests` | Pending approval queue (across all links for the account) | Account admin |
| `/accounts/:login/audit` | (v1.1) Audit log + CSV export | Account admin |
| `/accounts/:login/settings` | Installation status, repo selection, GitHub-uninstall link | Account admin |
| `/i/:slug` | Recipient landing (preview repos + permission, sign-in CTA) | Public |
| `/i/:slug/request` | Submit request | Recipient (signed in) |
| `/i/:slug/pending/:request_id` | Recipient's request status (manual refresh in v1) | Request owner |
| `/webhooks/github` | GitHub webhook receiver, HMAC verified | Verified |

Restate-handler routes live in the **Restate Service Worker** (separate binary), called only by Restate Cloud. They are not routed by the web binary.

---

## 12. Frontend layouts

Three Dioxus layouts, each with its own DaisyUI theme zone:

- **HomeLayout** (`/`, `/login`) — marketing tone, install CTA, logged-out friendly.
- **DashboardLayout** (`/accounts/...`) — admin chrome: top nav with account switcher, badge for pending request count, user avatar dropdown, utility look.
- **InvitationLayout** (`/i/...`) — recipient-friendly card-centered flow, no admin chrome, focused conversion.

**Account switcher** lists all installations the current user is admin of (orgs where they're an owner + their personal account if installed); footer link "Install on another account" → `/install`. Last-used account stored in cookie; URL is canonical.

**Recipient pending page** in v1: manual refresh. (Auto-refresh deferred to v1.1.)

**Recipient request page (`/i/:slug/request`)** must display the currently signed-in GitHub identity ("Signed in as @{login}") with a "not you? sign out and sign in again" link before the submit button. Defends against the wrong-account failure mode where the link recipient is logged into a different GitHub account than they intend to grant access to.

**Link create form (`/accounts/:login/links/new`)** defaults: `permission = pull` (least privilege), `expires_at = now + 30 days`, `max_uses = unlimited`, `approval_required = false`. Admins must consciously opt out of these defaults. Justification: a never-expiring `admin`-level link is exactly the misuse this product is positioned against; defaults should make that the harder path.

**Action label.** A revoked invitation link is described to admins as "stop accepting new requests" rather than "revoke." In v1 link revocation does *not* cascade to pending downstream invitations (that's a v2 feature); calling the action "revoke" would mislead admins into thinking it does. The internal terminology in code (`revoked_at`, `mark_invitation_link_revoked`) is unchanged — only the UI label is adjusted.

**Server functions** (`#[server]`) are used for every state-changing action. Server-side errors return `Result` via Dioxus's standard pattern; client renders inline error states.

---

## 13. Webhook handling

GitHub events subscribed at App registration:
- `repository_invitation`
- `member`
- `installation`
- `installation_repositories`

`POST /webhooks/github` flow:
1. Read **raw** body (required for HMAC).
2. Verify `X-Hub-Signature-256` constant-time against the App webhook secret.
3. Parse minimal envelope (event, action, delivery id).
4. Idempotency key for Restate = `X-GitHub-Delivery` header.
5. Route by event:
   - `repository_invitation` → look up our `github_invitations` row by `invitation.id`; call `GithubInvitation::on_webhook.send(payload)`.
   - `member` (action=added) → backup acceptance signal; matched by repo + user; treats as `accepted` if not already.
   - `installation` (action=created) → backup onboarding path.
   - `installation` (action=deleted) → `Installation::uninstall.send()`.
   - `installation_repositories` (added/removed) → `Installation::repos_changed.send(payload)`.
6. Return 200 fast; all real work is async via Restate.

Failure modes:
- Bad HMAC: 401, no enqueue, structured-log entry (not an audit event).
- Unknown event type: 200 (silently ignore).

---

## 14. GitHub API surface

**Restate Service Worker (installation tokens):**
- `POST /app/installations/{id}/access_tokens`
- `GET /installation/repositories`
- `GET /repos/{owner}/{repo}` (display metadata)
- `PUT /repos/{owner}/{repo}/collaborators/{username}`
- `DELETE /repos/{owner}/{repo}/invitations/{invitation_id}`
- `GET /repos/{owner}/{repo}/invitations` (reconciler)
- `GET /repos/{owner}/{repo}/collaborators/{username}` (acceptance confirmation)

**Web Worker (user tokens):**
- `GET /user`
- `GET /user/memberships/orgs/{login}`

GitHub App private key (RS256) is held only by the Restate Service Worker. The web binary never mints installation tokens.

---

## 15. Audit log (capture in v1, UI in v1.1)

### 15.1 Events captured in v1

| Event | Actor | Target |
|---|---|---|
| `installation.created` | user (installer) | installation |
| `installation.repos_changed` | github | installation |
| `installation.uninstalled` | github | installation |
| `invitation_link.created` | user (admin) | invitation_link |
| `invitation_link.revoked` | user (admin) | invitation_link |
| `invitation_link.expired` | system | invitation_link |
| `invitation_link.exhausted` | system | invitation_link |
| `request.created` | user (requester) | invitation_request |
| `request.approved` | user (admin) or system (auto-approve) | invitation_request |
| `request.declined` | user (admin) | invitation_request |
| `request.expired` | system | invitation_request |
| `invitation.sent` | system | github_invitation |
| `invitation.accepted` | github | github_invitation |
| `invitation.declined` | github | github_invitation |
| `invitation.expired` | system | github_invitation |
| `invitation.cancelled` | user / system | github_invitation |
| `invitation.send_failed` | system | github_invitation |

### 15.2 Discipline

- All audit writes are emitted by Restate handlers, not by the web binary.
- The Storage trait exposes `audit(event)` only; no update or delete.
- Append-only at the trait level (not enforced at the SQL level — by design choice; we want to keep the table debuggable in dev).
- Indefinite retention in v1.

### 15.3 Deferred to v1.1

- Admin-facing audit page at `/accounts/:login/audit` with filters and pagination.
- CSV export endpoint.

---

## 16. Error handling

| Failure | Behavior |
|---|---|
| GitHub 401/403 (token revoked) | Bubble up; web binary surfaces "App needs reinstallation"; audit `invitation.send_failed` with reason. |
| GitHub 4xx terminal (e.g. 422 user-not-found) | Mark `github_invitation` failed; capture message in `error_message`; audit. |
| GitHub 5xx | Restate retries with backoff per its default policy; eventually exhausts and audits `invitation.send_failed`. |
| Webhook HMAC mismatch | 401, no enqueue, structured-log entry. |
| Restate connectivity from web binary | Web binary surfaces "we couldn't process your action; please retry"; no DB drift since nothing was written. |
| OAuth state mismatch / replay | Reject; "session expired, try again." |
| Recipient hits invalid / revoked / expired / exhausted link | Generic 404 (no information leak). |
| Two admins approve same request simultaneously | Restate single-flight per request_id; first wins; second sees "already decided" toast. |
| Two recipients click same one-use link simultaneously | Use counter incremented atomically inside the InvitationRequest workflow; second request fails the `is_active` check at step 1 and gets a generic 404. |

---

## 17. Testing strategy

- **Unit tests** in `crates/domain` for state machines, validators, slug generation.
- **Storage suite** in `crates/storage`: parameterized harness running the same scenarios against `SqlxStorage` (in-memory SQLite) and `D1Storage` (`wrangler dev` local D1). Migrations applied fresh per test.
- **Restate handler tests** using Restate's local test container; exercise full workflows including timer races via virtual time.
- **GitHub client tests** with a mock transport recording calls and returning canned 201/204/4xx responses.
- **Webhook fixture tests:** signed-payload fixtures for every handled event; happy path + HMAC mismatch per event.
- **End-to-end smoke** in CI: spin up web + restate-svc + Restate locally, stub the GitHub backend, drive happy-path through HTTP.

---

## 18. Security checklist

- HTTPS everywhere (Workers default).
- HMAC verification on every webhook, raw body, constant-time comparison.
- CSRF tokens on every state-changing form (Dioxus server function pattern).
- Constant-time slug comparison.
- Generic 404 for invalid / revoked / expired / exhausted links — never confirm existence.
- GitHub App private key + webhook secret in Workers secrets (never in code or env files).
- Session cookie: `Secure`, `HttpOnly`, `SameSite=Lax`.
- User access token encrypted at rest in session storage; symmetric key with rotation supported.
- CSP set on dashboard responses.
- Audit log write-only by trait design.
- No personal data beyond GitHub user_id (numeric, stable) and login (mutable). Recipient email is never stored — it's a GitHub-side concept.

---

## 19. Deferred work

### v1.1 (immediately after v1 ships)

- Audit log UI (`/accounts/:login/audit`) with filtering, pagination.
- Audit log CSV export.
- Auto-refresh of recipient pending page (SSE or polling).

### v2+ (no commitment, listed for scope clarity)

- Notifications (email and/or Slack) on request created / approved / declined / accepted.
- Cascade revoke: when a link is revoked, cancel still-pending requests and still-pending GitHub invitations downstream.
- Editing invitation links (max-uses, expiration).
- Quorum / multi-approver requirements.
- Per-link approver list.
- Per-repo permission within a single link.
- Org-membership invitations (seat-consuming).
- Team-add invitations.
- App-internal admin role assignment (decouple from GitHub org owners).
- Soft-delete / GDPR redaction tooling.
- Multi-language / i18n.
- Webhook delivery retries / dead-letter queue beyond what GitHub does for us natively.
- Read receipts on invitation links.
- Custom per-install branding.

---

## 20. Open implementation considerations

These are not blocking design questions but are flagged for the implementation plan to address:

- **Workers wasm + Restate Rust SDK compatibility.** The Rust SDK is not officially wasm-tested. Early implementation should validate this against a trivial handler before committing to the full design. If incompatibility is discovered, the fallback is to host the Restate Service binary on a small VM (Fly.io / Railway) while keeping the Web Worker on Cloudflare. This affects deployment but not the architecture.
- **Workers wasm + octocrab installation-token JWT signing.** User has verified end-to-end use; the implementation should establish a minimal smoke test of installation-token mint + repo-collaborator PUT before building outward.
- **Migration tooling.** Need to confirm `sqlx-migrate` and `wrangler d1 migrations apply` accept the same SQL files; small dialect differences may force light splitting.
- **Session encryption key management.** Workers secrets allow setting a key; rotation strategy (re-encrypt on next sign-in vs. dual-key window) to be decided at implementation time.

---

## 21. Locked decisions (summary)

| # | Decision |
|---|---|
| Q1 | B+ multi-tenant, publicly hostable, installation = onboarding, no billing v1 |
| Q2 | Org owners are app-admins for org installations; account holder for personal installations; recipients need only GitHub login; v2 escape hatch deferred |
| Q3 | Repo-collaborator invitations only, both org and personal-account installs; multi-repo, single permission level per link |
| Q4 | Restate (workflows) + DB projection (CQRS), DB read by web binary directly |
| Q5 | Two wasm-targetable binaries (web + restate-svc); pluggable Storage trait (sqlx + D1); Restate Cloud; Workers prod; native local dev |
| Q6 | Webhooks primary + daily reconciler; 5 terminal states for github_invitation |
| Q7 | Opaque slug `/i/{slug}`, 16 base62 chars, generic 404 on invalid/revoked/expired/exhausted, preview repos before sign-in |
| Q8 | Single-step approval, any current account admin; decline reasons internal-only; request expires at min(link expiry, 7d); optional justification kept in v1 |
| Q9 | Dioxus fullstack SSR + hydration, server functions for state changes, server-side errors via `Result` |
| Q10 | Audit capture in v1; UI + CSV export deferred to v1.1 |
| Q11 | URL structure under `/accounts/:login`; three frontend zones (home, dashboard, invitation); manual-refresh recipient pending page; `/logout` route present |
| Q12 | Personal-account installs in v1 (no account-vs-org rename pain — just a `account_type` branch in auth) |
