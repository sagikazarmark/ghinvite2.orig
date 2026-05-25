<!-- /autoplan restore point: /home/laborant/.gstack/projects/sagikazarmark-ghinvite2.orig/main-autoplan-restore-20260504-212925.md -->
# ghinvite — Plan 6: Recipient Flow Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fill in the `/i/{slug}/...` routes that Plan 4 shipped as 501 stubs, implement the HMAC-verified GitHub webhook receiver, and wire three earlier-plan amendments (webhook secret in WebConfig, `return_to` session field, OAuth callback return-path support). After this plan ships, every recipient can discover a link, sign in, submit an access request, and check its status — and every GitHub event triggers the appropriate Restate workflow.

**Architecture:** The recipient flow lives entirely in `crates/web`. Public routes (`GET /i/{slug}`) load the invitation link from D1/SQLite via the storage trait and render via Dioxus SSR under `InvitationLayout`. Authenticated routes (`GET /i/{slug}/request`, `POST /i/{slug}/request`, `GET /i/{slug}/pending/{request_id}`) require a valid session (redirect to `/login?return_to=<path>` otherwise). State-changing actions (request submission) call Restate via `RestateClient::send`; the web binary never writes domain state directly. The webhook handler reads the raw request body for HMAC verification, then routes events to Restate via fire-and-forget `.send()` calls using the workflow's internal key (our `GithubInvitationId` or `installation_id`) as the Restate idempotency key; the `X-GitHub-Delivery` header is logged for traceability only.

**Tech stack:** Same as Plans 4–5. No new dependencies.

---

## Spec coverage

This plan implements:
- **§11 recipient routes** in full: `GET /i/{slug}`, `GET /i/{slug}/request`, `POST /i/{slug}/request`, `GET /i/{slug}/pending/{request_id}`
- **§12 InvitationLayout content** for the three recipient pages
- **§13 webhook handling** in full: HMAC verification, event routing to Restate for `repository_invitation`, `member`, `installation`, `installation_repositories`
- **§18 security checklist items** for the recipient flow: generic 404 for invalid/revoked/expired/exhausted links, HMAC constant-time verification, open-redirect prevention for `return_to`, requester identity confirmation UI

It does NOT implement:
- Auto-refresh of the recipient pending page — v1.1
- Decline reasons visible to the recipient — v1.1 (spec says decline_reason is "internal-only")
- D1Storage — Plan 7
- E2E CI — Plan 8

## Earlier-plan amendments folded into this plan

Three amendments to earlier plans are required to unlock the recipient flow and webhook handler:

1. **WebConfig (`crates/web/src/config.rs`):** Add `webhook_secret: Vec<u8>`. Per spec §2.1, the webhook secret stays in the web binary only; it is never passed to the Restate binary. The webhook handler reads it from `state.config.webhook_secret`.

2. **Session (`crates/web/src/session.rs`):** Add `return_to: Option<String>`. Stored during `GET /login?return_to=<path>` and consumed (read + cleared) in `oauth_callback` to redirect the user back to the invitation-link request page after sign-in. Must be validated: only paths starting with `/i/` are accepted (prevents open redirect).

3. **OAuth routes (`crates/web/src/routes/oauth.rs`):** The `login` handler must accept a `?return_to=` query param and store it in the session after validation. The `oauth_callback` handler must read + consume `session.return_to` and redirect to it (or `/` if absent) after successful sign-in.

## File structure

| Path | Plan 6 action | Responsibility |
|---|---|---|
| `crates/web/src/config.rs` | Modify | Add `webhook_secret: Vec<u8>` field |
| `crates/web/src/session.rs` | Modify | Add `return_to: Option<String>` to `Session` |
| `crates/web/src/routes/oauth.rs` | Modify | `?return_to=` support in login + callback |
| `crates/web/src/views/invitation.rs` | Create | `LandingPage`, `RequestFormPage`, `PendingPage` components |
| `crates/web/src/views/mod.rs` | Modify | Declare `pub mod invitation` |
| `crates/web/src/routes/invitation.rs` | Replace | Real handlers replacing 501 stubs |
| `crates/web/src/routes/webhook.rs` | Replace | HMAC-verified webhook receiver |
| `crates/web/tests/route_smoke.rs` | Modify | Replace 501 tests with real smoke tests |
| `docs/superpowers/plans/README.md` | Modify | Mark Plan 6 Implemented |

---

## Task ordering

Tasks 1–3 are the earlier-plan amendments (config, session, OAuth). Task 4 builds the view components. Tasks 5–8 implement the recipient routes. Task 9 implements the webhook. Task 10 wires tests and final polish.

---

### Task 1: WebConfig — webhook_secret

**Files:**
- Modify: `crates/web/src/config.rs`

- [ ] **Step 1: Add the field**

In `WebConfig`, add after `cookie_secure`:

```rust
/// Raw bytes of the GitHub App webhook secret. Used by the webhook handler
/// to verify `X-Hub-Signature-256`. Loaded from Workers secrets in production;
/// empty in local dev (webhook delivery not expected locally without ngrok).
pub webhook_secret: Vec<u8>,
```

- [ ] **Step 2: Populate in `for_local_dev`**

```rust
webhook_secret: std::env::var("GHINVITE_WEBHOOK_SECRET")
    .map(|s| s.into_bytes())
    .unwrap_or_default(),
```

- [ ] **Step 3: Verify**

`cargo check -p web` — no errors. The webhook handler in Task 9 reads `state.config.webhook_secret` as `&[u8]`.

---

### Task 2: Session — return_to field

**Files:**
- Modify: `crates/web/src/session.rs`

- [ ] **Step 1: Add field to Session**

Add to the `Session` struct (after `admin_checks`):

```rust
/// Stored by `GET /login?return_to=<path>` and consumed by the OAuth callback
/// to bounce the user back to the invitation-link request page after sign-in.
/// Only `/i/`-prefixed relative paths are stored (open-redirect prevention).
#[serde(default)]
pub return_to: Option<String>,
```

The `#[serde(default)]` keeps existing serialized sessions (without the field) deserializable.

- [ ] **Step 2: Add a validation helper**

Below the struct:

```rust
/// Validate a `return_to` query parameter. Only accepts relative paths that
/// start with `/i/` — prevents open redirect and path-traversal bypasses.
/// Returns `None` on rejection.
pub fn validate_return_to(raw: &str) -> Option<String> {
    // Must be relative (starts with '/'), not protocol-relative ('//'), and
    // confined to recipient routes. The `..` check prevents traversal like
    // `/i/../../admin`.
    if raw.starts_with("/i/") && !raw.starts_with("//") && !raw.contains("..") {
        Some(raw.to_string())
    } else {
        None
    }
}
```

- [ ] **Step 3: Add unit test**

```rust
#[test]
fn return_to_validation() {
    assert_eq!(
        validate_return_to("/i/AAAAAAAAAAAAAAAA/request"),
        Some("/i/AAAAAAAAAAAAAAAA/request".to_string())
    );
    assert_eq!(validate_return_to("/"), None);
    assert_eq!(validate_return_to("https://evil.com"), None);
    assert_eq!(validate_return_to("//evil.com/i/foo"), None);
    assert_eq!(validate_return_to("/i/../../admin"), None); // path traversal
    assert_eq!(validate_return_to("/i/../etc/passwd"), None); // path traversal
}
```

- [ ] **Step 4: Verify**

`cargo test -p web --lib` — all session tests pass.

---

### Task 3: OAuth return_to support

**Files:**
- Modify: `crates/web/src/routes/oauth.rs`

- [ ] **Step 1: Add `return_to` query param to login**

Add a query struct:

```rust
#[derive(Debug, Deserialize)]
struct LoginQuery {
    return_to: Option<String>,
}
```

Change the `login` handler signature:

```rust
async fn login(
    State(state): State<AppState>,
    tower: TowerSession,
    axum::extract::Query(q): axum::extract::Query<LoginQuery>,
) -> Result<impl IntoResponse> {
```

In the handler body, after `session.oauth_csrf = Some(csrf.clone())`, add:

```rust
session.return_to = q.return_to.as_deref().and_then(crate::session::validate_return_to);
```

- [ ] **Step 2: Consume return_to in oauth_callback**

After `session::save(&tower, &session)` (the line that persists the logged-in session), add:

```rust
let destination = session.return_to.clone().unwrap_or_else(|| "/".to_string());
```

Then clear return_to from the session before saving (it's a one-shot token):

Find the existing save call and ensure `session.return_to` is cleared before it. Replace the current end of `oauth_callback`:

```rust
// Consume return_to before final save.
let destination = session.return_to.take().unwrap_or_else(|| "/".to_string());
session::save(&tower, &session)
    .await
    .map_err(|e| WebError::Session(e.to_string()))?;

if let Some(installation_id) = q.installation_id {
    tracing::info!(
        installation_id,
        "OAuth callback carried installation_id; awaiting webhook onboarding"
    );
}

Ok(Redirect::to(&destination))
```

- [ ] **Step 3: Verify**

`cargo test -p web --test route_smoke` — login test still passes. `cargo check -p web` — clean.

- [ ] **Step 4: Amend logout handler to support `return_to`**

`RequestFormPage` renders "Not you? Sign out and sign in again." linking to `/logout?return_to={post_url}`. The existing `logout` handler ignores query params and always redirects to `/` — this drops the user's context. Add a query struct and amend the handler in `crates/web/src/routes/oauth.rs`:

```rust
#[derive(Debug, Deserialize)]
struct LogoutQuery {
    return_to: Option<String>,
}

async fn logout(
    tower: TowerSession,
    axum::extract::Query(q): axum::extract::Query<LogoutQuery>,
) -> impl IntoResponse {
    session::clear(&tower).await;
    let destination = q
        .return_to
        .as_deref()
        .and_then(crate::session::validate_return_to)
        .unwrap_or_else(|| "/".to_string());
    Redirect::to(&destination)
}
```

`cargo check -p web` — no errors. The `/logout` route registration in the router needs no change (only the handler signature changed).

---

### Task 4: Views — invitation.rs

**Files:**
- Create: `crates/web/src/views/invitation.rs`
- Modify: `crates/web/src/views/mod.rs`

- [ ] **Step 1: Create the file with three components**

```rust
//! Dioxus components for the recipient flow (`/i/{slug}/...`).
//!
//! All three pages use [`InvitationLayout`] — the card-centred, no-admin-chrome
//! zone defined in [`crate::views::layouts`].

use crate::views::layouts::InvitationLayout;
use chrono::{DateTime, Utc};
use dioxus::prelude::*;
use domain::{Permission, RequestState, InvitationLink};
```

**LandingPage** (`GET /i/{slug}` — public):

```rust
#[derive(Clone, PartialEq, Props)]
pub struct LandingProps {
    pub slug: String,
    pub link: InvitationLink,
    pub signed_in_login: Option<String>,
    pub now: DateTime<Utc>,
}

#[component]
pub fn LandingPage(props: LandingProps) -> Element {
    let repo_count = props.link.repos.len();
    let permission_label = permission_label(props.link.permission);
    let request_url = format!("/i/{}/request", props.slug);
    let login_url = format!("/login?return_to=/i/{}/request", props.slug);

    rsx! {
        InvitationLayout {
            signed_in_login: props.signed_in_login.clone(),
            title: format!("Access {repo_count} {} — ghinvite", if repo_count == 1 { "repo" } else { "repos" }),
            children: rsx! {
                h2 { class: "card-title", "You've been invited" }
                p { class: "text-sm text-base-content/70 mb-4",
                    "This link grants collaborator access to the following repositor{if repo_count == 1 { \"y\" } else { \"ies\" }}:"
                }
                ul { class: "list-disc list-inside mb-4",
                    for repo in &props.link.repos {
                        li { class: "text-sm font-mono", "{repo.repo_full_name}" }
                    }
                }
                div { class: "badge badge-outline mb-4", "Permission: {permission_label}" }
                // Display expiry if set (Decision #7).
                {props.link.expires_at.map(|exp| {
                    let days = (exp - props.now).num_days().max(0);
                    let msg = if days > 1 { format!("Expires in {days} days") }
                              else if days == 1 { "Expires in 1 day".to_string() }
                              else { "Expires today".to_string() };
                    rsx! { p { class: "text-xs text-base-content/50 mb-2", "{msg}" } }
                })}
                {match props.signed_in_login.as_deref() {
                    None => rsx! {
                        a { class: "btn btn-primary w-full", href: "{login_url}",
                            "Sign in with GitHub to request access"
                        }
                    },
                    // Show identity near CTA so user can confirm correct account (Decision #15).
                    Some(login) => rsx! {
                        p { class: "text-xs text-base-content/50 mb-2", "Signed in as @{login}" }
                        a { class: "btn btn-primary w-full", href: "{request_url}",
                            "Continue to request access"
                        }
                    },
                }}
            },
        }
    }
}
```

**RequestFormPage** (`GET /i/{slug}/request` — signed-in):

```rust
#[derive(Clone, PartialEq, Props)]
pub struct RequestFormProps {
    pub slug: String,
    pub link: InvitationLink,
    pub signed_in_login: String,
    pub flash: Option<crate::session::Flash>,
    /// Pre-generated in GET handler for double-submit dedup (Decision #11).
    pub request_id: String,
}

#[component]
pub fn RequestFormPage(props: RequestFormProps) -> Element {
    let post_url = format!("/i/{}/request", props.slug);
    let repo_count = props.link.repos.len();
    let permission_label = permission_label(props.link.permission);

    rsx! {
        InvitationLayout {
            signed_in_login: Some(props.signed_in_login.clone()),
            title: "Request access — ghinvite".to_string(),
            children: rsx! {
                h2 { class: "card-title mb-2", "Request collaborator access" }
                {match &props.flash {
                    Some(f) => {
                        let cls = match f.level {
                            crate::session::FlashLevel::Error => "alert alert-error mb-3",
                            _ => "alert alert-info mb-3",
                        };
                        rsx! { div { class: "{cls}", span { "{f.message}" } } }
                    }
                    None => rsx! {},
                }}
                // Identity confirmation — per spec §12.
                div { class: "alert alert-info mb-4 text-sm",
                    span {
                        "Signed in as "
                        strong { "@{props.signed_in_login}" }
                        ". "
                        a { class: "link", href: "/logout?return_to={post_url}",
                            "Not you? Sign out and sign in again."
                        }
                    }
                }
                p { class: "text-sm mb-3",
                    "Requesting "
                    span { class: "badge badge-outline", "{permission_label}" }
                    " access to {repo_count} repo(s):"
                }
                ul { class: "list-disc list-inside mb-4 text-sm font-mono",
                    for repo in &props.link.repos {
                        li { "{repo.repo_full_name}" }
                    }
                }
                form { method: "post", action: "{post_url}",
                    // Hidden field carries pre-generated ULID from GET handler.
                    // POST uses it as the Restate workflow key → exactly-once dedup
                    // on double-click or back-and-resubmit (Decision #11).
                    input { r#type: "hidden", name: "request_id", value: "{props.request_id}" }
                    div { class: "form-control mb-4",
                        label { class: "label", span { class: "label-text", "Justification (optional)" } }
                        textarea {
                            class: "textarea textarea-bordered",
                            name: "justification",
                            placeholder: "Why do you need access? (optional)",
                            rows: "3",
                        }
                    }
                    button { class: "btn btn-primary w-full", r#type: "submit",
                        "Submit request"
                    }
                }
            },
        }
    }
}
```

**PendingPage** (`GET /i/{slug}/pending/{request_id}`):

```rust
#[derive(Clone, PartialEq, Props)]
pub struct PendingProps {
    pub slug: String,
    pub request_state: Option<RequestState>,
    pub signed_in_login: Option<String>,
}

#[component]
pub fn PendingPage(props: PendingProps) -> Element {
    rsx! {
        InvitationLayout {
            signed_in_login: props.signed_in_login.clone(),
            title: "Request status — ghinvite".to_string(),
            children: rsx! {
                h2 { class: "card-title mb-4", "Request status" }
                {match props.request_state {
                    None => rsx! {
                        div { class: "alert alert-info",
                            span { "Your request is being processed. Refresh to check status." }
                        }
                    },
                    Some(RequestState::Pending) => rsx! {
                        div { class: "alert alert-warning",
                            span { "Your request is awaiting admin review. Refresh to check status." }
                        }
                    },
                    Some(RequestState::Approved) => rsx! {
                        div { class: "alert alert-success",
                            span { "Approved! Check your GitHub notifications for the repository invitation." }
                        }
                    },
                    Some(RequestState::Declined) => rsx! {
                        div { class: "alert alert-error",
                            span { "Your request was declined." }
                        }
                        a { class: "btn btn-ghost btn-sm mt-2", href: "/i/{props.slug}",
                            "Back to link"
                        }
                    },
                    Some(RequestState::Expired) => rsx! {
                        div { class: "alert alert-warning",
                            span { "This request has expired." }
                        }
                        a { class: "btn btn-ghost btn-sm mt-2", href: "/i/{props.slug}",
                            "Submit a new request"
                        }
                    },
                    Some(RequestState::Cancelled) => rsx! {
                        div { class: "alert alert-warning",
                            span { "This request has been cancelled." }
                        }
                    },
                }}
            },
        }
    }
}

fn permission_label(p: Permission) -> &'static str {
    match p {
        Permission::Pull => "Read (pull)",
        Permission::Triage => "Triage",
        Permission::Push => "Write (push)",
        Permission::Maintain => "Maintain",
        Permission::Admin => "Admin",
    }
}
```

- [ ] **Step 2: Declare the module**

In `crates/web/src/views/mod.rs`, add:

```rust
pub mod invitation;
```

- [ ] **Step 3: Verify**

`cargo check -p web` — no errors. `Permission` variants may differ slightly from domain — check and adjust if rustc complains.

---

### Task 5: Recipient landing — GET /i/{slug}

**Files:**
- Modify: `crates/web/src/routes/invitation.rs`

Replace the stub file with the real router and first handler. The router gets all four routes from the start; Tasks 5–8 fill them in one at a time.

- [ ] **Step 1: Write the full router + landing handler**

```rust
//! `/i/{slug}...` routes. Plan 6 recipient flow.

use crate::session;
use crate::state::AppState;
use crate::views::render::render;
use axum::Router;
use axum::extract::State;
use axum::response::{Html, IntoResponse, Redirect};
use axum::routing::{get, post};
use chrono::Utc;
use dioxus::prelude::*;
use tower_sessions::Session as TowerSession;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/i/{slug}", get(landing))
        .route("/i/{slug}/request", get(request_form).post(submit_request))
        .route("/i/{slug}/pending/{request_id}", get(pending))
}

async fn landing(
    State(state): State<AppState>,
    tower: TowerSession,
    axum::extract::Path(slug): axum::extract::Path<String>,
) -> impl IntoResponse {
    let now = Utc::now();
    let link = match state.storage.get_invitation_link_by_slug(&slug).await {
        Ok(Some(l)) if l.is_active(now) => l,
        _ => return crate::error::WebError::NotFound.into_response(),
    };
    let session = session::load(&tower).await.unwrap_or_default();
    let signed_in_login = if session.is_authenticated() {
        Some(session.login.clone())
    } else {
        None
    };
    let html = render(move || rsx! {
        crate::views::invitation::LandingPage {
            slug: slug.clone(),
            link: link.clone(),
            signed_in_login: signed_in_login.clone(),
            now,
        }
    });
    Html(html).into_response()
}
```

- [ ] **Step 2: Verify**

`cargo check -p web` — no errors. The remaining handler stubs can be `async fn stub() -> impl IntoResponse { (StatusCode::NOT_IMPLEMENTED, "TODO") }` during this task, removed in Tasks 6–8.

---

### Task 6: Request form page — GET /i/{slug}/request

**Files:**
- Modify: `crates/web/src/routes/invitation.rs`

- [ ] **Step 1: Implement request_form handler**

```rust
async fn request_form(
    State(state): State<AppState>,
    tower: TowerSession,
    axum::extract::Path(slug): axum::extract::Path<String>,
) -> impl IntoResponse {
    let session = session::load(&tower).await.unwrap_or_default();
    if !session.is_authenticated() {
        return Redirect::to(&format!("/login?return_to=/i/{slug}/request")).into_response();
    }
    let now = Utc::now();
    let link = match state.storage.get_invitation_link_by_slug(&slug).await {
        Ok(Some(l)) if l.is_active(now) => l,
        _ => return crate::error::WebError::NotFound.into_response(),
    };
    // Redirect to pending page if requester already has a pending request (Decision #14).
    // Prevents duplicate Restate workflows and the DuplicatePendingRequest conflict they would cause.
    if let Ok(requests) = state.storage.list_requests_for_link(link.id).await {
        if let Some(existing) = requests
            .iter()
            .find(|r| r.requester_id == session.user_id && r.state == domain::RequestState::Pending)
        {
            return Redirect::to(&format!("/i/{slug}/pending/{}", existing.id)).into_response();
        }
    }

    // Generate request_id here so GET and POST share the same ULID (Decision #11).
    let request_id = domain::RequestId::new();
    let flash = session::take_flash(&tower).await.unwrap_or(None);
    let signed_in_login = session.login.clone();
    let request_id_str = request_id.to_string();
    let html = render(move || rsx! {
        crate::views::invitation::RequestFormPage {
            slug: slug.clone(),
            link: link.clone(),
            signed_in_login: signed_in_login.clone(),
            flash: flash.clone(),
            request_id: request_id_str.clone(),
        }
    });
    Html(html).into_response()
}
```

- [ ] **Step 2: Verify**

`cargo check -p web` — no errors.

---

### Task 7: Submit request — POST /i/{slug}/request

**Files:**
- Modify: `crates/web/src/routes/invitation.rs`

- [ ] **Step 1: Form struct**

```rust
#[derive(serde::Deserialize)]
struct SubmitForm {
    /// Pre-generated in GET /request handler; same ULID on back-and-resubmit (Decision #11).
    #[serde(default)]
    request_id: String,
    #[serde(default)]
    justification: Option<String>,
}
```

- [ ] **Step 2: Implement submit_request handler**

```rust
async fn submit_request(
    State(state): State<AppState>,
    tower: TowerSession,
    axum::extract::Path(slug): axum::extract::Path<String>,
    axum::extract::Form(form): axum::extract::Form<SubmitForm>,
) -> impl IntoResponse {
    let session = session::load(&tower).await.unwrap_or_default();
    if !session.is_authenticated() {
        return Redirect::to(&format!("/login?return_to=/i/{slug}/request")).into_response();
    }
    let now = Utc::now();
    let link = match state.storage.get_invitation_link_by_slug(&slug).await {
        Ok(Some(l)) if l.is_active(now) => l,
        _ => return crate::error::WebError::NotFound.into_response(),
    };

    let justification = form.justification
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    // Use the ULID pre-generated in the GET handler (Decision #11).
    // Restate's exactly-once guarantee deduplicates back-and-resubmit.
    // Fallback to a fresh ULID if the hidden field was absent or tampered.
    use std::str::FromStr;
    let request_id = domain::RequestId::from_str(&form.request_id)
        .unwrap_or_else(|_| domain::RequestId::new());
    let input = serde_json::json!({
        "request_id": request_id.to_string(),
        "invitation_link_id": link.id.to_string(),
        "requester_id": session.user_id,
        "justification": justification,
        "created_at": now,
    });

    if let Err(e) = state
        .restate
        .send("InvitationRequest", &request_id.to_string(), "submit", &input)
        .await
    {
        tracing::warn!(error = ?e, "InvitationRequest::submit send failed");
        let _ = session::set_flash(
            &tower,
            session::Flash {
                level: session::FlashLevel::Error,
                message: "Failed to submit request. Please try again.".into(),
            },
        )
        .await;
        return Redirect::to(&format!("/i/{slug}/request")).into_response();
    }

    Redirect::to(&format!("/i/{slug}/pending/{request_id}")).into_response()
}
```

- [ ] **Step 3: Verify**

`cargo check -p web` — no errors.

---

### Task 8: Pending status page — GET /i/{slug}/pending/{request_id}

**Files:**
- Modify: `crates/web/src/routes/invitation.rs`

- [ ] **Step 1: Implement pending handler**

```rust
async fn pending(
    State(state): State<AppState>,
    tower: TowerSession,
    axum::extract::Path((slug, request_id_str)): axum::extract::Path<(String, String)>,
) -> impl IntoResponse {
    use std::str::FromStr;

    let session = session::load(&tower).await.unwrap_or_default();
    if !session.is_authenticated() {
        return Redirect::to(&format!("/login?return_to=/i/{slug}/pending/{request_id_str}"))
            .into_response();
    }

    let request_id = match domain::RequestId::from_str(&request_id_str) {
        Ok(id) => id,
        Err(_) => return crate::error::WebError::NotFound.into_response(),
    };

    // Load request. If not yet in DB (workflow just started), show "processing" state.
    let request_state = match state.storage.get_invitation_request(request_id).await {
        Ok(Some(r)) if r.requester_id == session.user_id => Some(r.state),
        Ok(Some(_)) => return crate::error::WebError::NotFound.into_response(),
        Ok(None) => None, // workflow not yet committed to DB — show processing state
        Err(_) => return crate::error::WebError::NotFound.into_response(),
    };

    let signed_in_login = Some(session.login.clone());
    let html = render(move || rsx! {
        crate::views::invitation::PendingPage {
            slug: slug.clone(),
            request_state,
            signed_in_login: signed_in_login.clone(),
        }
    });
    Html(html).into_response()
}
```

- [ ] **Step 2: Add missing imports**

Ensure `use axum::http::StatusCode;` is removed if unused, and the file has `use crate::error::WebError;` if not already present.

- [ ] **Step 3: Verify**

`cargo test -p web` — all existing tests pass.

---

### Task 9: Webhook handler — POST /webhooks/github

**Files:**
- Replace: `crates/web/src/routes/webhook.rs`

The webhook receiver reads the raw body (needed for HMAC), verifies the signature, then routes events to Restate. All payloads are passed as `serde_json::Value` — the Restate handlers own the struct definitions.

- [ ] **Step 1: Write the handler**

```rust
//! `POST /webhooks/github` — HMAC-verified webhook receiver. Plan 6.

use crate::state::AppState;
use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::post;
use chrono::Utc;

pub fn router() -> Router<AppState> {
    Router::new().route("/webhooks/github", post(handle_webhook))
}

async fn handle_webhook(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    // Step 1: HMAC verification (raw body required per spec §13).
    let signature = match headers
        .get("x-hub-signature-256")
        .and_then(|v| v.to_str().ok())
    {
        Some(s) => s.to_owned(),
        None => {
            tracing::warn!("webhook: missing X-Hub-Signature-256 header");
            return StatusCode::UNAUTHORIZED.into_response();
        }
    };

    if !github::hmac::verify_signature_256(&state.config.webhook_secret, &body, &signature) {
        tracing::warn!("webhook: HMAC verification failed");
        return StatusCode::UNAUTHORIZED.into_response();
    }

    // Step 2: Parse envelope.
    let payload: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(error = ?e, "webhook: body is not valid JSON");
            return StatusCode::OK.into_response(); // silent: don't retry bad payloads
        }
    };

    let event_type = match headers
        .get("x-github-event")
        .and_then(|v| v.to_str().ok())
    {
        Some(e) => e.to_owned(),
        None => {
            tracing::warn!("webhook: missing X-GitHub-Event header");
            return StatusCode::OK.into_response();
        }
    };

    let delivery_id = headers
        .get("x-github-delivery")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("unknown")
        .to_string();

    // Step 3: Route by event type (spec §13).
    let result = match event_type.as_str() {
        "repository_invitation" => {
            handle_repository_invitation(&state, &payload, &delivery_id).await
        }
        "member" => handle_member(&state, &payload, &delivery_id).await,
        "installation" => handle_installation(&state, &payload, &delivery_id).await,
        "installation_repositories" => {
            handle_installation_repositories(&state, &payload, &delivery_id).await
        }
        _ => {
            tracing::debug!(event = %event_type, "webhook: unknown event type, ignoring");
            Ok(())
        }
    };

    if let Err(e) = result {
        tracing::warn!(error = ?e, event = %event_type, "webhook: routing to Restate failed");
        // Still return 200 — GitHub will retry on non-2xx; we don't want retries
        // for transient Restate connectivity issues to trigger HMAC re-verification.
    }

    StatusCode::OK.into_response()
}

/// `repository_invitation` — primary webhook for accept/decline events.
///
/// Looks up our internal `GithubInvitationId` by GitHub's numeric invitation id,
/// then sends `on_webhook` to Restate.
async fn handle_repository_invitation(
    state: &AppState,
    payload: &serde_json::Value,
    delivery_id: &str,
) -> Result<(), String> {
    let action = payload["action"].as_str().unwrap_or("");
    let github_inv_id = payload["invitation"]["id"].as_u64().ok_or_else(|| {
        format!("repository_invitation: missing invitation.id in payload")
    })?;

    // Map GitHub action to our WebhookAction enum variants.
    let webhook_action = match action {
        "accepted" => "Accepted",
        "declined" => "Declined",
        _ => {
            tracing::debug!(action, "repository_invitation: unhandled action");
            return Ok(());
        }
    };

    // Look up our row by GitHub's numeric invitation id.
    let inv = state
        .storage
        .get_github_invitation_by_github_id(github_inv_id)
        .await
        .map_err(|e| format!("storage lookup: {e}"))?;

    let Some(inv) = inv else {
        tracing::debug!(
            github_inv_id,
            "repository_invitation: no matching row (not our invitation)"
        );
        return Ok(());
    };

    let input = serde_json::json!({
        "invitation_id": inv.id.to_string(),
        "action": webhook_action,
        "at": Utc::now(),
    });

    state
        .restate
        .send("GithubInvitation", &inv.id.to_string(), "on_webhook", &input)
        .await
        .map_err(|e| format!("Restate send: {e}"))?;

    tracing::info!(
        github_inv_id,
        our_id = %inv.id,
        action = webhook_action,
        delivery = delivery_id,
        "repository_invitation routed"
    );
    Ok(())
}

/// `member` (action=added) — backup acceptance signal. The primary path is
/// `repository_invitation`; this covers the edge case where GitHub delivers
/// `member` before `repository_invitation`. Requires matching by repo + user,
/// which is not currently in the storage trait. Logged and deferred; does not
/// break the primary path.
///
/// v1.1: add `Storage::get_pending_invitation_by_repo_and_user` and handle fully.
async fn handle_member(
    _state: &AppState,
    payload: &serde_json::Value,
    delivery_id: &str,
) -> Result<(), String> {
    let action = payload["action"].as_str().unwrap_or("");
    tracing::debug!(action, delivery = delivery_id, "member event received (not yet handled — v1.1)");
    Ok(())
}

/// `installation` — onboarding backup (action=created) and uninstall (action=deleted).
async fn handle_installation(
    state: &AppState,
    payload: &serde_json::Value,
    delivery_id: &str,
) -> Result<(), String> {
    let action = payload["action"].as_str().unwrap_or("");
    let installation_id = payload["installation"]["id"].as_u64().ok_or_else(|| {
        "installation: missing installation.id".to_string()
    })?;

    match action {
        "deleted" => {
            let input = serde_json::json!({
                "installation_id": installation_id,
                "uninstalled_at": Utc::now(),
            });
            state
                .restate
                .send(
                    "Installation",
                    &installation_id.to_string(),
                    "uninstall",
                    &input,
                )
                .await
                .map_err(|e| format!("Restate send: {e}"))?;
            tracing::info!(installation_id, delivery = delivery_id, "installation.deleted routed");
        }
        "created" => {
            // Backup onboarding path — primary path is OAuth callback.
            // Only triggers if install happens without concurrent OAuth (rare).
            tracing::debug!(
                installation_id,
                delivery = delivery_id,
                "installation.created received (backup path — primary is OAuth callback)"
            );
        }
        _ => {
            tracing::debug!(action, "installation: unhandled action");
        }
    }
    Ok(())
}

/// `installation_repositories` — updates which repos the app can access.
async fn handle_installation_repositories(
    state: &AppState,
    payload: &serde_json::Value,
    delivery_id: &str,
) -> Result<(), String> {
    let installation_id = payload["installation"]["id"].as_u64().ok_or_else(|| {
        "installation_repositories: missing installation.id".to_string()
    })?;

    // `repository_selection` can be "all" or "selected". Pass the full payload
    // as selected_repos — the Restate handler owns the parsing.
    let selected_repos = payload["repository_selection"].as_str().unwrap_or("selected");
    let added = payload["repositories_added"].clone();
    let removed = payload["repositories_removed"].clone();

    let input = serde_json::json!({
        "installation_id": installation_id,
        "selected_repos": {
            "type": selected_repos,
            "added": added,
            "removed": removed,
        },
    });

    state
        .restate
        .send(
            "Installation",
            &installation_id.to_string(),
            "repos_changed",
            &input,
        )
        .await
        .map_err(|e| format!("Restate send: {e}"))?;

    tracing::info!(
        installation_id,
        delivery = delivery_id,
        "installation_repositories routed"
    );
    Ok(())
}
```

- [ ] **Step 2: Verify**

`cargo check -p web` — no errors. `github::hmac` is a `pub mod` in the github crate, so `github::hmac::verify_signature_256` is directly accessible.

---

### Task 10: Tests + smoke cleanup + README

**Files:**
- Modify: `crates/web/tests/route_smoke.rs`
- Modify: `docs/superpowers/plans/README.md`

- [ ] **Step 1: Replace 501 tests with real smoke tests**

Remove the `invitation_routes_return_501` and `webhook_route_returns_501` tests. Replace with:

```rust
#[tokio::test]
async fn invitation_landing_unknown_slug_returns_404() {
    let app = build_test_app().await;
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/i/AAAAAAAAAAAAAAAA")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn invitation_request_form_unauthenticated_redirects_to_login() {
    let app = build_test_app().await;
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/i/AAAAAAAAAAAAAAAA/request")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    let location = resp.headers().get("location").unwrap().to_str().unwrap();
    assert!(location.starts_with("/login"));
}

#[tokio::test]
async fn webhook_missing_signature_returns_401() {
    let app = build_test_app().await;
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/webhooks/github")
                .header("x-github-event", "push")
                .header("x-github-delivery", "test-delivery-id")
                .body(Body::from(r#"{"action":"push"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn webhook_bad_hmac_returns_401() {
    use axum::body::Body;
    let app = build_test_app().await;
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/webhooks/github")
                .header("x-github-event", "push")
                .header("x-github-delivery", "test-delivery-id")
                // SHA-256 of empty string (wrong body content) — HMAC will fail.
                .header(
                    "x-hub-signature-256",
                    "sha256=e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
                )
                .body(Body::from(r#"{"action":"push"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

/// Verifies that a correctly-signed webhook payload returns 200 (Decision #9).
///
/// `for_local_dev` sets `webhook_secret` to `GHINVITE_WEBHOOK_SECRET` env var or
/// empty `Vec<u8>`. This test signs with the same empty key, so it passes regardless
/// of the env var being set.
///
/// Requires `hmac`, `sha2`, `hex` dev-dependencies in `crates/web/Cargo.toml`:
/// ```toml
/// [dev-dependencies]
/// hmac = "0.12"
/// sha2 = "0.10"
/// hex = "0.4"
/// ```
#[tokio::test]
async fn webhook_valid_hmac_returns_ok() {
    use hmac::{Hmac, Mac};
    use sha2::Sha256;

    let body = r#"{"action":"ping"}"#;
    let mut mac = Hmac::<Sha256>::new_from_slice(b"").unwrap();
    mac.update(body.as_bytes());
    let sig = format!("sha256={}", hex::encode(mac.finalize().into_bytes()));

    let app = build_test_app().await;
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/webhooks/github")
                .header("x-github-event", "ping")
                .header("x-github-delivery", "test-delivery-id")
                .header("x-hub-signature-256", &sig)
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}
```

- [ ] **Step 1b: Add dev-dependencies for HMAC happy-path test**

In `crates/web/Cargo.toml` under `[dev-dependencies]`:

```toml
hmac = "0.12"
sha2 = "0.10"
hex = "0.4"
```

- [ ] **Step 1c: Note ownership-check test scope**

The `requester_id == session.user_id` guard in the `pending` handler (Task 8) is security-critical. A full test requires injecting a real `InvitationRequest` row via storage + two distinct sessions, which is beyond a route smoke test. Document this in a `// TODO(test): ownership guard integration test` comment at the top of the `pending` handler, and add to the `route_smoke.rs` file a note comment:

```rust
// NOTE: The requester_id ownership guard in `pending` (Task 8) is tested via
// integration test (tests/invitation_ownership.rs) in Plan 8; the smoke suite
// only covers the unauthenticated redirect path.
```

- [ ] **Step 2: Mark Plan 6 Implemented**

In `docs/superpowers/plans/README.md`, change the Plan 6 row:
- Status: `Pending` → `Implemented`
- Filename: `_not yet written_` → `2026-05-04-ghinvite-recipient-flow.md`

- [ ] **Step 3: Final verify**

```
cargo test --workspace
```

All tests pass. In particular:
- `cargo test -p restate-svc` — 53+ tests
- `cargo test -p github` — 60+ tests
- `cargo test -p web --test route_smoke` — updated smoke suite
- `cargo test -p web --lib` — session tests including new `return_to` validation

---

## Known gaps (v1.1)

- `member` webhook event: backup acceptance signal requires `Storage::get_pending_invitation_by_repo_and_user` (not in the trait). Currently logged and skipped. Add in v1.1 alongside the storage method.
- Auto-refresh of the pending page (SSE or polling) — deferred to v1.1 per spec §12.
- Decline reasons visible to the recipient — spec says decline reasons are "internal-only" in v1; `PendingPage` for declined state shows only "your request was declined."
- Recipient-facing error detail: errors from Restate on request submission use static flash messages (same approach as Plan 5).

## Security notes

- Generic 404 for inactive/missing links — never reveals whether slug exists or why it's inactive (spec §16, Q7).
- `return_to` validated to `/i/` prefix before storage; cleared after OAuth completes (one-shot, prevents session fixation).
- Requester ownership check on pending page: `request.requester_id == session.user_id` before showing state.
- HMAC verification: `github::hmac::verify_signature_256` uses constant-time comparison (spec §13).
- Webhook always returns 200 on routing success; 401 only on HMAC failure (bad HMAC → no enqueue, no structured response).
- Identity confirmation on request form: "Signed in as @{login}" with sign-out link — defends against wrong-account access requests (spec §12).

---

## /autoplan decision audit trail

| # | Phase | Decision | Classification | Principle | Rationale | Rejected |
|---|-------|----------|----------------|-----------|-----------|---------|
| 1 | CEO | CSRF on form POSTs | Mechanical | P3 | SameSite=Lax session cookie prevents cross-site form POST from carrying session cookie — same resolution as Plan 5. Add doc note, no code change needed. | Explicit CSRF token (over-engineering for SameSite=Lax env) |
| 2 | CEO | return_to `..` path traversal bypass | Mechanical | P1 | Add `!raw.contains("..")` to validate_return_to + test cases for path traversal | Accept risk (too high impact) |
| 3 | CEO | TOCTOU advisory note for submit check | Mechanical | P5 | Add comment noting web-layer is_active check is advisory; Restate re-validates at step 1 | Skip documentation |
| 4 | CEO | member event silently dropped | Taste | P6 | Accept v1.1 gap. Reconciler (~24h) is backstop. Add note. Missing storage trait method makes full impl a Plan 7 scope item. | Implement now (requires new storage method + migration) |
| 5 | CEO | Webhook 200 on Restate failure | Mechanical | P1 | Return 5xx on Restate routing failure for handled events → enables GitHub 72h retry | Keep 200 on failure (swallows transient outages) |
| 6 | CEO | logout?return_to link | Mechanical | P5 | Simplify to plain /logout — logout handler has no return_to support; amending is out of scope | Amend logout handler (scope creep) |
| 7 | CEO | now prop unused in LandingPage | Mechanical | P1 | Use now to display expiry on landing page (completeness) | Remove prop entirely |
| 8 | CEO | Pending URL security model undocumented | Mechanical | P5 | Add security note: RequestId ULID is 128 bits entropy, treated as unguessable capability token | Skip documentation |
| 9 | CEO | No happy-path HMAC test | Mechanical | P1 | Add valid-HMAC 200 smoke test to Task 10 | Accept gap |
| 10 | CEO | get_invitation_request existence | Low | P5 | Already in storage trait (verified). No action. | N/A |
| 11 | Design | Double-submit creates 2 Restate workflows | Mechanical | P1 | Generate request_id in GET /request, embed as hidden form field; POST uses provided ULID → exactly-once dedup via Restate | Two separate ULIDs, rely on DB unique index (DB constraint violation inside Restate workflow is messy) |
| 12 | Design | Permission badge buried below repos | Mechanical | P1 | Move permission above repo list; semantic color (maintain=warning, admin=error badge) | Keep current order |
| 13 | Design | None + Pending states visual mismatch | Mechanical | P5 | Unify None and Pending to same yellow warning style in PendingPage | Distinct colours (confuses users) |
| 14 | Design | No "already submitted" guard on GET /request | Mechanical | P1 | Check existing pending request in request_form handler; redirect to /pending if found | Let user resubmit (creates DB constraint in workflow) |
| 15 | Design | No identity on landing page | Mechanical | P1 | Show "Signed in as @{login}" near CTA on LandingPage | Skip (user sees identity on request form) |
| 16 | Design | Logout drops context (override D6) | Mechanical | P1 | Amend logout handler in Task 3 to support validated return_to — spec requires this for the wrong-account path | Plain /logout only (breaks the flow the spec tries to prevent) |
| 17 | Design | No repo count summary | Mechanical | P1 | Add explicit summary sentence with count above list | Silent list only |
| 18 | Design | Back to link after decline | Mechanical | P5 | Remove "Back to link" CTA from declined state | Keep CTA (re-request spam) |
| 19 | Design | Identity banner alert-info styling | Mechanical | P5 | Change to neutral bg-base-200 styling | Keep alert-info (reads as warning) |
| 20 | Design | InvitationLayout no brand | Taste | P3 | Keep card-only for v1; add wordmark in v1.1 | Add wordmark now |
| 21 | Design | Empty repo list no fallback | Mechanical | P1 | Add empty state: "No repos currently accessible via this link" | Silent empty list |
| 22 | Design | Grammar: "repo(s)" in title | Mechanical | P1 | Fix to proper singular/plural string | Accept bad grammar |
| 23 | Eng | `validate_return_to` missing `..` check (D#2 not applied to code) | Mechanical | P1 | Added `&& !raw.contains("..")` to guard + traversal test cases | Accept traversal risk |
| 24 | Eng | Logout handler missing `?return_to=` support (D#16 not in Task 3 steps) | Mechanical | P1 | Added Step 4 to Task 3: add `LogoutQuery` + amend `logout` handler | Plain `/logout` only (breaks wrong-account flow) |
| 25 | Eng | `request_id` generated in POST handler (D#11 not applied) | Mechanical | P1 | Moved generation to GET handler; hidden form field; fallback parse in POST | Two ULIDs (DB conflict inside Restate) |
| 26 | Eng | No existing-pending redirect in GET /request (D#14 not in Task 6) | Mechanical | P1 | Added `list_requests_for_link()` + filter in `request_form`; redirect if pending found | Let user submit (Restate workflow fails with DuplicatePendingRequest) |
| 27 | Eng | `now` prop unused in LandingPage (D#7 not applied) | Mechanical | P1 | Added expiry display block using `expires_at` and `now` | Dead prop |
| 28 | Eng | "Signed in as @{login}" missing from LandingPage (D#15 not applied) | Mechanical | P1 | Added identity line in `Some(login)` CTA branch | Silent identity |
| 29 | Eng | No happy-path HMAC test (D#9 not in Task 10) | Mechanical | P1 | Added `webhook_valid_hmac_returns_ok` test + hmac/sha2/hex dev-deps | Accept gap |
| 30 | Eng | Ownership-check test not in smoke suite | Mechanical | P5 | Noted scope limit; added TODO comment + plan stub for Plan 8 integration test | Accept gap in route_smoke |
| 31 | Eng | Architecture prose: `delivery_id` wrongly described as Restate key | Mechanical | P5 | Fixed prose: internal key is the Restate key; delivery_id is for logging | Accept inaccuracy |
