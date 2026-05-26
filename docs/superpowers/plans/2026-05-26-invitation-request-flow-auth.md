# Invitation Request Flow Auth Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Require sign-in before invitation-flow details are revealed and make `/i/{code}` the single requester-facing page for review, submission, and current request status.

**Architecture:** Move invitation-code lookup into an authenticated route path, then resolve the signed-in requester's existing requests before deciding whether to show status, a retryable form, or a generic invitation-flow 404. Keep request-state selection centralized in `invitation_link_resolution.rs`, render one merged Dioxus page in `views/invitation.rs`, and simplify `routes/invitation.rs` to one canonical `GET` and `POST` route plus authenticated nested 404s.

**Tech Stack:** Rust 2024, Axum routes, tower-sessions, Dioxus SSR, sqlx-backed test storage, GitHub mock OAuth transport, `cargo test`.

---

## File Structure

Modify:

- `crates/web/src/invitation_link_resolution.rs`: replace active-only public-link resolution with an authenticated-flow context loader and requester request-state selector.
- `crates/web/src/views/invitation.rs`: replace split landing, request form, and pending page components with one merged `RequestPage` component plus focused view tests.
- `crates/web/src/routes/invitation.rs`: register only `/i/{slug}` as the supported requester route, authenticate before resolution, submit from `POST /i/{slug}`, and make nested `/i/{slug}/...` paths authenticate before 404.
- `crates/web/tests/invitation_resolution.rs`: update recipient-flow route tests for pre-auth redirects, merged page behavior, request-state selection, inactive-link concealment, and submit behavior.
- `crates/web/tests/route_smoke.rs`: update smoke expectations so invitation-flow routes redirect to login before showing any 404.

No new production files are needed.

---

### Task 1: Public Invitation Context And Request Selection

**Files:**

- Modify: `crates/web/src/invitation_link_resolution.rs`

- [ ] **Step 1: Write failing resolver and selector tests**

Replace the old tests that mention `PendingRequestPolicy` with tests for context loading and request-state selection. Keep the existing `FakeStorage`, `sample_link`, and `sample_request` helpers, then use these tests in the module:

```rust
#[tokio::test]
async fn malformed_slug_returns_invalid_slug_without_storage_lookup() {
    let storage = FakeStorage::default();

    let err = resolve_public_invitation_link_context(&storage, "not-a-valid-slug")
        .await
        .unwrap_err();

    assert!(matches!(err, ResolutionError::InvalidSlug));
    assert_eq!(*storage.slug_lookup_count.lock().unwrap(), 0);
}

#[tokio::test]
async fn unknown_slug_returns_unknown_slug_after_storage_lookup() {
    let storage = FakeStorage::default();

    let err = resolve_public_invitation_link_context(&storage, "abcdEFGH01234567")
        .await
        .unwrap_err();

    assert!(matches!(err, ResolutionError::UnknownSlug));
    assert_eq!(*storage.slug_lookup_count.lock().unwrap(), 1);
}

#[tokio::test]
async fn stored_slug_mismatch_returns_slug_mismatch() {
    let storage = FakeStorage {
        link: Some(sample_link("ZZZZZZZZZZZZZZZZ")),
        ..FakeStorage::default()
    };

    let err = resolve_public_invitation_link_context(&storage, "abcdEFGH01234567")
        .await
        .unwrap_err();

    assert!(matches!(err, ResolutionError::SlugMismatch));
}

#[tokio::test]
async fn inactive_link_still_resolves_with_requests() {
    let mut link = sample_link("abcdEFGH01234567");
    link.revoked_at = Some(dt("2026-05-20T11:00:00Z"));
    link.revoked_by = Some(701);
    let request = sample_request(RequestId::new(), link.id, 802, RequestState::Pending);
    let storage = FakeStorage {
        requests: vec![request.clone()],
        link: Some(link),
        ..FakeStorage::default()
    };

    let context = resolve_public_invitation_link_context(&storage, "abcdEFGH01234567")
        .await
        .unwrap();

    assert_eq!(context.slug.as_str(), "abcdEFGH01234567");
    assert_eq!(context.requests, vec![request]);
    assert!(!context.link.is_active(dt("2026-05-20T12:00:00Z")));
}

#[test]
fn requester_selection_prefers_pending_over_other_states() {
    let link = sample_link("abcdEFGH01234567");
    let requests = vec![
        sample_request(RequestId::new(), link.id, 802, RequestState::Declined),
        sample_request(RequestId::new(), link.id, 802, RequestState::Approved),
        sample_request(RequestId::new(), link.id, 802, RequestState::Pending),
        sample_request(RequestId::new(), link.id, 999, RequestState::Pending),
    ];

    let selection = select_requester_request_state(&requests, 802);

    assert_eq!(selection.current_status, Some(RequestState::Pending));
    assert_eq!(selection.retry_notice, None);
}

#[test]
fn requester_selection_prefers_approved_before_retryable_states() {
    let link = sample_link("abcdEFGH01234567");
    let requests = vec![
        sample_request(RequestId::new(), link.id, 802, RequestState::Expired),
        sample_request(RequestId::new(), link.id, 802, RequestState::Approved),
    ];

    let selection = select_requester_request_state(&requests, 802);

    assert_eq!(selection.current_status, Some(RequestState::Approved));
    assert_eq!(selection.retry_notice, None);
}

#[test]
fn requester_selection_keeps_newest_retryable_state() {
    let link = sample_link("abcdEFGH01234567");
    let requests = vec![
        sample_request(RequestId::new(), link.id, 802, RequestState::Cancelled),
        sample_request(RequestId::new(), link.id, 802, RequestState::Declined),
        sample_request(RequestId::new(), link.id, 999, RequestState::Approved),
    ];

    let selection = select_requester_request_state(&requests, 802);

    assert_eq!(selection.current_status, None);
    assert_eq!(selection.retry_notice, Some(RequestState::Cancelled));
}
```

- [ ] **Step 2: Run resolver tests to verify they fail**

Run: `cargo test -p web invitation_link_resolution`

Expected: FAIL with unresolved names `resolve_public_invitation_link_context`, `PublicInvitationLinkContext`, and `select_requester_request_state`.

- [ ] **Step 3: Replace the resolver API with context loading and selection**

In `crates/web/src/invitation_link_resolution.rs`, replace `PendingRequestPolicy`, `PublicInvitationLinkResolution`, and `resolve_public_invitation_link` with this API:

```rust
use crate::error::WebError;
use domain::{InvitationLink, InvitationRequest, RequestState, Slug};
use storage::Storage;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PublicInvitationLinkContext {
    pub(crate) slug: Slug,
    pub(crate) link: InvitationLink,
    pub(crate) requests: Vec<InvitationRequest>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct RequesterRequestSelection {
    pub(crate) current_status: Option<RequestState>,
    pub(crate) retry_notice: Option<RequestState>,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum ResolutionError {
    #[error("invalid slug")]
    InvalidSlug,
    #[error("unknown slug")]
    UnknownSlug,
    #[error("stored slug mismatch")]
    SlugMismatch,
    #[error("storage error: {0}")]
    Storage(#[from] storage::Error),
}

impl ResolutionError {
    pub(crate) fn into_public_error(self) -> WebError {
        WebError::NotFound
    }
}

pub(crate) async fn resolve_public_invitation_link_context(
    storage: &dyn Storage,
    raw_slug: &str,
) -> Result<PublicInvitationLinkContext, ResolutionError> {
    let slug = Slug::from_string(raw_slug.to_string()).map_err(|_| ResolutionError::InvalidSlug)?;
    let link = storage
        .get_invitation_link_by_slug(slug.as_str())
        .await
        .map_err(ResolutionError::Storage)?
        .ok_or(ResolutionError::UnknownSlug)?;

    if !link.slug.ct_eq(&slug) {
        return Err(ResolutionError::SlugMismatch);
    }

    let requests = storage
        .list_requests_for_link(link.id)
        .await
        .map_err(ResolutionError::Storage)?;

    Ok(PublicInvitationLinkContext {
        slug,
        link,
        requests,
    })
}

pub(crate) fn select_requester_request_state(
    requests: &[InvitationRequest],
    requester_id: u64,
) -> RequesterRequestSelection {
    let mut approved = None;
    let mut retry_notice = None;

    for request in requests.iter().filter(|request| request.requester_id == requester_id) {
        match request.state {
            RequestState::Pending => {
                return RequesterRequestSelection {
                    current_status: Some(RequestState::Pending),
                    retry_notice: None,
                };
            }
            RequestState::Approved => approved = Some(RequestState::Approved),
            RequestState::Declined | RequestState::Expired | RequestState::Cancelled => {
                if retry_notice.is_none() {
                    retry_notice = Some(request.state);
                }
            }
        }
    }

    if approved.is_some() {
        RequesterRequestSelection {
            current_status: approved,
            retry_notice: None,
        }
    } else {
        RequesterRequestSelection {
            current_status: None,
            retry_notice,
        }
    }
}
```

Keep the fake storage and domain fixture helpers in the test module, but remove tests that assert inactive links fail or that pending requests are returned as a resolution outcome. Inactive links now resolve so the route can show an existing signed-in status.

- [ ] **Step 4: Run resolver tests to verify they pass**

Run: `cargo test -p web invitation_link_resolution`

Expected: PASS for all `invitation_link_resolution` tests.

- [ ] **Step 5: Commit resolver work**

```bash
git add crates/web/src/invitation_link_resolution.rs
git commit -m "refactor(web): resolve invitation flow context"
```

---

### Task 2: Merged Requester Page View

**Files:**

- Modify: `crates/web/src/views/invitation.rs`

- [ ] **Step 1: Write failing merged view tests**

Replace the old component tests for `LandingPage`, `RequestFormPage`, and `PendingPage` with these tests:

```rust
#[test]
fn request_page_renders_merged_form() {
    let link = sample_link();
    let html = crate::views::render::render(move || {
        rsx! {
            RequestPage {
                slug: "abcdEFGH01234567".to_string(),
                link: link.clone(),
                signed_in_login: "octocat".to_string(),
                flash: None,
                request_id: "01ARZ3NDEKTSV4RRFFQ69G5FAV".to_string(),
                current_status: None,
                retry_notice: None,
            }
        }
    });

    assert!(html.contains("Request repository access"));
    assert!(html.contains("Review the repositories and submit a request as @octocat."));
    assert!(html.contains("acme/api"));
    assert!(html.contains("acme/web"));
    assert!(html.contains("Permission: Write (push)"));
    assert!(html.contains("Signed in as"));
    assert!(html.contains("Not you?"));
    assert!(html.contains("href=\"/logout\""));
    assert!(html.contains("name=\"request_id\""));
    assert!(html.contains("action=\"/i/abcdEFGH01234567\""));
    assert!(html.contains("Submit request"));
    assert!(!html.contains("AI coding workshop"));
    assert!(!html.contains("Only admins should see this note"));
    assert!(!html.contains("collaborator access"));
}

#[test]
fn request_page_renders_pending_status_without_form() {
    let link = sample_link();
    let html = crate::views::render::render(move || {
        rsx! {
            RequestPage {
                slug: "abcdEFGH01234567".to_string(),
                link: link.clone(),
                signed_in_login: "octocat".to_string(),
                flash: None,
                request_id: "01ARZ3NDEKTSV4RRFFQ69G5FAV".to_string(),
                current_status: Some(RequestState::Pending),
                retry_notice: None,
            }
        }
    });

    assert!(html.contains("Awaiting review"));
    assert!(html.contains("account admins have your request"));
    assert!(html.contains("href=\"/i/abcdEFGH01234567\""));
    assert!(!html.contains("textarea"));
    assert!(!html.contains("Submit request"));
    assert!(!html.contains("Create your own invitation link"));
}

#[test]
fn request_page_renders_approved_status_without_form() {
    let link = sample_link();
    let html = crate::views::render::render(move || {
        rsx! {
            RequestPage {
                slug: "abcdEFGH01234567".to_string(),
                link: link.clone(),
                signed_in_login: "octocat".to_string(),
                flash: None,
                request_id: "01ARZ3NDEKTSV4RRFFQ69G5FAV".to_string(),
                current_status: Some(RequestState::Approved),
                retry_notice: None,
            }
        }
    });

    assert!(html.contains("Approved"));
    assert!(html.contains("GitHub notifications and email"));
    assert!(!html.contains("textarea"));
    assert!(!html.contains("Submit request"));
}

#[test]
fn request_page_renders_retry_notice_with_form() {
    let link = sample_link();
    let html = crate::views::render::render(move || {
        rsx! {
            RequestPage {
                slug: "abcdEFGH01234567".to_string(),
                link: link.clone(),
                signed_in_login: "octocat".to_string(),
                flash: None,
                request_id: "01ARZ3NDEKTSV4RRFFQ69G5FAV".to_string(),
                current_status: None,
                retry_notice: Some(RequestState::Declined),
            }
        }
    });

    assert!(html.contains("Your previous request was declined."));
    assert!(html.contains("You can submit a new request."));
    assert!(html.contains("Submit request"));
    assert!(html.contains("Justification"));
}
```

- [ ] **Step 2: Run view tests to verify they fail**

Run: `cargo test -p web views::invitation`

Expected: FAIL because `RequestPage` does not exist and old tests still reference removed components until the view is replaced.

- [ ] **Step 3: Replace split components with one merged `RequestPage`**

In `crates/web/src/views/invitation.rs`, keep `permission_label`, remove `LandingPage`, `RequestFormPage`, and `PendingPage`, then add this component API and helpers:

```rust
#[derive(Clone, PartialEq, Props)]
pub struct RequestPageProps {
    pub slug: String,
    pub link: InvitationLink,
    pub signed_in_login: String,
    pub flash: Option<crate::session::Flash>,
    pub request_id: String,
    pub current_status: Option<RequestState>,
    pub retry_notice: Option<RequestState>,
}

fn retry_notice_copy(state: RequestState) -> Option<&'static str> {
    match state {
        RequestState::Declined => Some("Your previous request was declined. You can submit a new request."),
        RequestState::Expired => Some("Your previous request expired. You can submit a new request."),
        RequestState::Cancelled => Some("Your previous request was cancelled. You can submit a new request."),
        RequestState::Pending | RequestState::Approved => None,
    }
}

#[component]
pub fn RequestPage(props: RequestPageProps) -> Element {
    let slug = props.slug.clone();
    let login = props.signed_in_login.clone();
    let perm_label = permission_label(props.link.permission);
    let repo_count = props.link.repos.len();
    let repo_word = if repo_count == 1 { "repository" } else { "repositories" };
    let action = format!("/i/{slug}");
    let refresh_href = format!("/i/{slug}");
    let request_id = props.request_id.clone();

    let repos_view = props.link.repos.iter().map(|repo| {
        let name = repo.repo_full_name.clone();
        rsx! { li { class: "break-words", "{name}" } }
    });

    let flash_view = match &props.flash {
        None => rsx! {},
        Some(f) => {
            let alert_class = match f.level {
                crate::session::FlashLevel::Error => "alert alert-error mb-4 shadow-sm",
                crate::session::FlashLevel::Success => "alert alert-success mb-4 shadow-sm",
                crate::session::FlashLevel::Info => "alert alert-info mb-4 shadow-sm",
            };
            let message = f.message.clone();
            rsx! { div { class: "{alert_class}", span { "{message}" } } }
        }
    };

    let retry_notice = props.retry_notice.and_then(retry_notice_copy).map(|copy| {
        rsx! {
            div { class: "alert alert-warning mb-4 shadow-sm",
                span { "{copy}" }
            }
        }
    });

    let content = match props.current_status {
        Some(RequestState::Pending) => rsx! {
            h1 { class: "text-xl font-semibold tracking-tight", "Awaiting review" }
            p { class: "mt-2 text-sm leading-6 text-base-content/70",
                "Account admins have your request. Check again later for a decision."
            }
            div { class: "alert alert-warning mt-5 shadow-sm",
                span { "Your invitation request is awaiting account admin review." }
            }
            div { class: "mt-6 flex flex-col gap-2 sm:flex-row sm:justify-end",
                a { class: "btn btn-primary", href: "{refresh_href}", "Check again" }
                a { class: "btn btn-ghost", href: "/", "Go home" }
            }
        },
        Some(RequestState::Approved) => rsx! {
            h1 { class: "text-xl font-semibold tracking-tight", "Approved" }
            p { class: "mt-2 text-sm leading-6 text-base-content/70",
                "Check your GitHub notifications and email for GitHub invitations."
            }
            div { class: "alert alert-success mt-5 shadow-sm",
                span { "Your invitation request was approved." }
            }
            div { class: "mt-6 flex justify-end",
                a { class: "btn btn-ghost", href: "/", "Go home" }
            }
        },
        _ => rsx! {
            h1 { class: "text-xl font-semibold tracking-tight", "Request repository access" }
            p { class: "mt-2 text-sm leading-6 text-base-content/70",
                "Review the repositories and submit a request as @{login}."
            }
            {flash_view}
            {retry_notice.unwrap_or_else(|| rsx! {})}
            div { class: "alert alert-info mt-5 shadow-sm",
                span {
                    "Signed in as "
                    strong { "@{login}" }
                    ". Not you? "
                    a { href: "/logout", class: "link", "Sign out and sign in again." }
                }
            }
            section { class: "mt-5 space-y-3",
                p { class: "text-sm font-medium", "Repository access" }
                ul { class: "list-inside list-disc space-y-1 rounded-box border border-base-300 bg-base-200 p-4 text-sm", {repos_view} }
                p { class: "text-sm text-base-content/70",
                    "Requesting access to {repo_count} {repo_word}. "
                    span { class: "badge badge-neutral", "Permission: {perm_label}" }
                }
            }
            form { method: "post", action: "{action}", class: "mt-5 space-y-4",
                input { r#type: "hidden", name: "request_id", value: "{request_id}" }
                div { class: "form-control gap-2",
                    label { class: "label", r#for: "justification", span { class: "label-text font-medium", "Justification" } }
                    textarea {
                        id: "justification",
                        name: "justification",
                        class: "textarea textarea-bordered w-full",
                        placeholder: "Why do you need access?",
                        rows: "3",
                    }
                    p { class: "text-sm text-base-content/70", "Optional, visible to account admins." }
                }
                div { class: "flex justify-end",
                    button { r#type: "submit", class: "btn btn-primary w-full sm:w-auto", "Submit request" }
                }
            }
        },
    };

    rsx! {
        InvitationLayout {
            signed_in_login: Some(props.signed_in_login.clone()),
            title: "Request repository access · ghinvite".to_string(),
            account_login: None,
            active_nav: None,
            flash: None,
            children: rsx! { {content} },
        }
    }
}
```

- [ ] **Step 4: Run view tests to verify they pass**

Run: `cargo test -p web views::invitation`

Expected: PASS for the `views::invitation` tests.

- [ ] **Step 5: Commit view work**

```bash
git add crates/web/src/views/invitation.rs
git commit -m "refactor(web): merge invitation request page"
```

---

### Task 3: Authenticate Before Invitation Resolution

**Files:**

- Modify: `crates/web/src/routes/invitation.rs`
- Modify: `crates/web/tests/invitation_resolution.rs`
- Modify: `crates/web/tests/route_smoke.rs`

- [ ] **Step 1: Write failing pre-auth route tests**

In `crates/web/tests/invitation_resolution.rs`, replace the old unauthenticated landing 404 and active landing tests with these tests:

```rust
#[tokio::test]
async fn landing_unauthenticated_redirects_to_login_for_active_slug() {
    let (app, _calls) = build_test_app(
        active_link(ACTIVE_SLUG),
        None,
        MockTransport::scripted(vec![]),
    )
    .await;

    let resp = app
        .oneshot(
            Request::builder()
                .uri(format!("/i/{ACTIVE_SLUG}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    let location = resp.headers().get("location").unwrap().to_str().unwrap();
    assert_eq!(location, format!("/login?return_to=/i/{ACTIVE_SLUG}"));
}

#[tokio::test]
async fn landing_unauthenticated_redirects_to_login_for_bad_or_inactive_slugs() {
    let mut revoked = active_link(ACTIVE_SLUG);
    revoked.revoked_at = Some(dt("2026-05-05T12:00:00Z"));
    revoked.revoked_by = Some(CREATOR_ID);

    let mut expired = active_link(ACTIVE_SLUG);
    expired.expires_at = Some(Utc.with_ymd_and_hms(2020, 1, 1, 0, 0, 0).unwrap());

    let mut exhausted = active_link(ACTIVE_SLUG);
    exhausted.max_uses = Some(1);
    exhausted.uses_count = 1;

    for (link, uri, expected_return_to) in [
        (active_link(ACTIVE_SLUG), "/i/not-a-valid-slug".to_string(), "/i/not-a-valid-slug".to_string()),
        (active_link(ACTIVE_SLUG), format!("/i/{UNKNOWN_SLUG}"), format!("/i/{UNKNOWN_SLUG}")),
        (revoked, format!("/i/{ACTIVE_SLUG}"), format!("/i/{ACTIVE_SLUG}")),
        (expired, format!("/i/{ACTIVE_SLUG}"), format!("/i/{ACTIVE_SLUG}")),
        (exhausted, format!("/i/{ACTIVE_SLUG}"), format!("/i/{ACTIVE_SLUG}")),
    ] {
        let (app, _calls) = build_test_app(link, None, MockTransport::scripted(vec![])).await;
        let resp = app
            .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::SEE_OTHER);
        let location = resp.headers().get("location").unwrap().to_str().unwrap();
        assert_eq!(location, format!("/login?return_to={expected_return_to}"));
    }
}

#[tokio::test]
async fn submit_unauthenticated_redirects_to_login_for_canonical_page() {
    let (app, _calls) = build_test_app(
        active_link(ACTIVE_SLUG),
        None,
        MockTransport::scripted(vec![]),
    )
    .await;

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/i/{ACTIVE_SLUG}"))
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from("request_id=01ARZ3NDEKTSV4RRFFQ69G5FAV&justification=ship"))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    let location = resp.headers().get("location").unwrap().to_str().unwrap();
    assert_eq!(location, format!("/login?return_to=/i/{ACTIVE_SLUG}"));
    assert!(calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn nested_invitation_routes_authenticate_before_404() {
    let (app, _calls) = build_test_app(
        active_link(ACTIVE_SLUG),
        None,
        MockTransport::scripted(oauth_expectations("octocat", REQUESTER_ID)),
    )
    .await;

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/i/{ACTIVE_SLUG}/request"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    let location = resp.headers().get("location").unwrap().to_str().unwrap();
    assert_eq!(location, format!("/login?return_to=/i/{ACTIVE_SLUG}/request"));

    let cookie = sign_in(app.clone()).await;
    let resp = app
        .oneshot(
            Request::builder()
                .uri(format!("/i/{ACTIVE_SLUG}/request"))
                .header("cookie", cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8_lossy(&body);
    assert!(text.contains("Page not found"));
    assert!(text.contains("The link may be incorrect or no longer available."));
}
```

In `crates/web/tests/route_smoke.rs`, update the invitation smoke expectations so unauthenticated `GET /i/AAAAAAAAAAAAAAAA` and `GET /i/AAAAAAAAAAAAAAAA/anything` expect `StatusCode::SEE_OTHER` and a `/login?return_to=` location instead of an HTML 404.

- [ ] **Step 2: Run route tests to verify they fail**

Run: `cargo test -p web landing_unauthenticated_redirects_to_login_for_active_slug && cargo test -p web nested_invitation_routes_authenticate_before_404`

Expected: FAIL because `GET /i/{slug}` still resolves before auth and `/i/{slug}/request` still exists.

- [ ] **Step 3: Consolidate route registration and pre-auth redirects**

In `crates/web/src/routes/invitation.rs`, update imports and route registration:

```rust
use crate::commands::SubmitInvitationRequest;
use crate::invitation_link_resolution::{
    resolve_public_invitation_link_context, select_requester_request_state,
};
use crate::session;
use crate::state::AppState;
use crate::views::render::render;
use axum::Router;
use axum::extract::State;
use axum::http::Uri;
use axum::response::{Html, IntoResponse, Redirect};
use axum::routing::{any, get};
use chrono::Utc;
use dioxus::prelude::*;
use tower_sessions::Session as TowerSession;
```

```rust
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/i/{slug}", get(invitation_page).post(submit_request))
        .route("/i/{slug}/{*rest}", any(unknown_nested))
}
```

Add these small helpers near `signed_in_login_from_session`:

```rust
fn redirect_to_login(return_to: &str) -> axum::response::Response {
    Redirect::to(&format!("/login?return_to={return_to}")).into_response()
}

fn canonical_invitation_path(slug: &str) -> String {
    format!("/i/{slug}")
}
```

Replace `landing` with `invitation_page`, which should authenticate before resolution:

```rust
async fn invitation_page(
    State(state): State<AppState>,
    tower: TowerSession,
    axum::extract::Path(slug): axum::extract::Path<String>,
) -> impl IntoResponse {
    let session = session::load(&tower).await.unwrap_or_default();
    if !session.is_authenticated() {
        return redirect_to_login(&canonical_invitation_path(&slug));
    }

    let now = Utc::now();
    let context = match resolve_public_invitation_link_context(state.storage.as_ref(), &slug).await {
        Ok(context) => context,
        Err(_) => return invitation_not_found_response(Some(session.login.clone())),
    };
    let selection = select_requester_request_state(&context.requests, session.user_id);
    if !context.link.is_active(now) && selection.current_status.is_none() {
        return invitation_not_found_response(Some(session.login.clone()));
    }

    let slug = context.slug.as_str().to_string();
    let request_id = domain::RequestId::new().to_string();
    let flash = session::take_flash(&tower).await.unwrap_or(None);
    let signed_in_login = session.login.clone();
    let html = render(move || {
        rsx! {
            crate::views::invitation::RequestPage {
                slug: slug.clone(),
                link: context.link.clone(),
                signed_in_login: signed_in_login.clone(),
                flash: flash.clone(),
                request_id: request_id.clone(),
                current_status: selection.current_status,
                retry_notice: selection.retry_notice,
            }
        }
    });
    Html(html).into_response()
}
```

Replace `unknown_nested` with an authenticated 404 for every method:

```rust
async fn unknown_nested(tower: TowerSession, uri: Uri) -> impl IntoResponse {
    let session = session::load(&tower).await.unwrap_or_default();
    if !session.is_authenticated() {
        let return_to = uri
            .path_and_query()
            .map(|value| value.as_str())
            .unwrap_or("/");
        return redirect_to_login(return_to);
    }
    invitation_not_found_response(Some(session.login.clone()))
}
```

Remove `request_form`, `pending`, and `plain_not_found` from this file.

- [ ] **Step 4: Run pre-auth route tests to verify they pass**

Run: `cargo test -p web landing_unauthenticated_redirects_to_login_for_active_slug && cargo test -p web nested_invitation_routes_authenticate_before_404`

Expected: PASS for those tests. Other invitation route tests may still fail until Task 4 updates state and submit expectations.

- [ ] **Step 5: Commit route auth work**

```bash
git add crates/web/src/routes/invitation.rs crates/web/tests/invitation_resolution.rs crates/web/tests/route_smoke.rs
git commit -m "fix(web): authenticate invitation flow before resolution"
```

---

### Task 4: State-Aware Form, Status, And Submission Behavior

**Files:**

- Modify: `crates/web/src/routes/invitation.rs`
- Modify: `crates/web/tests/invitation_resolution.rs`

- [ ] **Step 1: Upgrade route-test fixtures for multiple requests**

In `crates/web/tests/invitation_resolution.rs`, replace `build_test_app` with this wrapper plus vector-based helper:

```rust
async fn build_test_app(
    link: InvitationLink,
    pending: Option<InvitationRequest>,
    mock: MockTransport,
) -> (axum::Router, Arc<Mutex<Vec<RecordedCommand>>>) {
    build_test_app_with_requests(link, pending.into_iter().collect(), mock).await
}

async fn build_test_app_with_requests(
    link: InvitationLink,
    requests: Vec<InvitationRequest>,
    mock: MockTransport,
) -> (axum::Router, Arc<Mutex<Vec<RecordedCommand>>>) {
    let storage = storage::SqlxStorage::in_memory().await.unwrap();
    storage
        .insert_installation(&sample_account())
        .await
        .unwrap();
    storage
        .upsert_user(&sample_user(CREATOR_ID, "creator"))
        .await
        .unwrap();
    storage
        .upsert_user(&sample_user(REQUESTER_ID, "octocat"))
        .await
        .unwrap();
    storage.insert_invitation_link(&link).await.unwrap();
    for request in requests {
        storage
            .insert_invitation_request_and_increment_uses(&request)
            .await
            .unwrap();
    }

    let storage: Arc<dyn storage::Storage> = Arc::new(storage);
    let transport: Arc<dyn github::HttpTransport> = Arc::new(mock);
    let commands = Arc::new(RecordingCommands::default());
    let calls = commands.calls.clone();
    let state = AppState::new(storage, transport, commands, WebConfig::for_local_dev());
    let session_store = tower_sessions::MemoryStore::default();
    (build_app(state, session_store), calls)
}
```

Add a timestamped request helper for newest-first behavior tests:

```rust
fn request_with_state_at(
    id: RequestId,
    link: InvitationLinkId,
    requester_id: u64,
    state: RequestState,
    created_at: &str,
) -> InvitationRequest {
    InvitationRequest {
        id,
        invitation_link_id: link,
        requester_id,
        justification: None,
        state,
        decided_by: None,
        decided_at: None,
        decline_reason: None,
        created_at: dt(created_at),
    }
}
```

- [ ] **Step 2: Write failing signed-in GET and POST behavior tests**

Replace the old request-form and pending-route tests with these canonical `/i/{slug}` tests:

```rust
#[tokio::test]
async fn signed_in_landing_renders_merged_request_form() {
    let (app, _calls) = build_test_app(
        active_link(ACTIVE_SLUG),
        None,
        MockTransport::scripted(oauth_expectations("octocat", REQUESTER_ID)),
    )
    .await;
    let cookie = sign_in(app.clone()).await;

    let resp = app
        .oneshot(
            Request::builder()
                .uri(format!("/i/{ACTIVE_SLUG}"))
                .header("cookie", cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8_lossy(&body);
    assert!(text.contains("Request repository access"));
    assert!(text.contains("Review the repositories and submit a request as @octocat."));
    assert!(text.contains("acme/api"));
    assert!(text.contains("Permission: Read (pull)"));
    assert!(text.contains("Justification"));
    assert!(text.contains("action=\"/i/abcdEFGH01234567\""));
    assert!(!text.contains("AI coding workshop"));
}

#[tokio::test]
async fn signed_in_landing_shows_pending_status_instead_of_form() {
    let link = active_link(ACTIVE_SLUG);
    let pending = pending_request(RequestId::new(), link.id, REQUESTER_ID);
    let (app, _calls) = build_test_app(
        link,
        Some(pending),
        MockTransport::scripted(oauth_expectations("octocat", REQUESTER_ID)),
    )
    .await;
    let cookie = sign_in(app.clone()).await;

    let resp = app
        .oneshot(
            Request::builder()
                .uri(format!("/i/{ACTIVE_SLUG}"))
                .header("cookie", cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8_lossy(&body);
    assert!(text.contains("Awaiting review"));
    assert!(text.contains("account admins have your request"));
    assert!(!text.contains("Submit request"));
    assert!(!text.contains("textarea"));
}

#[tokio::test]
async fn signed_in_landing_shows_approved_status_instead_of_form() {
    let link = active_link(ACTIVE_SLUG);
    let approved = request_with_state(RequestId::new(), link.id, REQUESTER_ID, RequestState::Approved);
    let (app, _calls) = build_test_app(
        link,
        Some(approved),
        MockTransport::scripted(oauth_expectations("octocat", REQUESTER_ID)),
    )
    .await;
    let cookie = sign_in(app.clone()).await;

    let resp = app
        .oneshot(
            Request::builder()
                .uri(format!("/i/{ACTIVE_SLUG}"))
                .header("cookie", cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8_lossy(&body);
    assert!(text.contains("Approved"));
    assert!(text.contains("GitHub notifications and email"));
    assert!(!text.contains("Submit request"));
}

#[tokio::test]
async fn signed_in_landing_shows_newest_retry_notice_with_form() {
    let link = active_link(ACTIVE_SLUG);
    let requests = vec![
        request_with_state_at(RequestId::new(), link.id, REQUESTER_ID, RequestState::Cancelled, "2026-05-04T12:40:00Z"),
        request_with_state_at(RequestId::new(), link.id, REQUESTER_ID, RequestState::Declined, "2026-05-04T12:30:00Z"),
    ];
    let (app, _calls) = build_test_app_with_requests(
        link,
        requests,
        MockTransport::scripted(oauth_expectations("octocat", REQUESTER_ID)),
    )
    .await;
    let cookie = sign_in(app.clone()).await;

    let resp = app
        .oneshot(
            Request::builder()
                .uri(format!("/i/{ACTIVE_SLUG}"))
                .header("cookie", cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8_lossy(&body);
    assert!(text.contains("Your previous request was cancelled."));
    assert!(!text.contains("Your previous request was declined."));
    assert!(text.contains("Submit request"));
}

#[tokio::test]
async fn inactive_link_with_existing_pending_request_shows_status() {
    let mut link = active_link(ACTIVE_SLUG);
    link.revoked_at = Some(dt("2026-05-05T12:00:00Z"));
    link.revoked_by = Some(CREATOR_ID);
    let pending = pending_request(RequestId::new(), link.id, REQUESTER_ID);
    let (app, _calls) = build_test_app(
        link,
        Some(pending),
        MockTransport::scripted(oauth_expectations("octocat", REQUESTER_ID)),
    )
    .await;
    let cookie = sign_in(app.clone()).await;

    let resp = app
        .oneshot(
            Request::builder()
                .uri(format!("/i/{ACTIVE_SLUG}"))
                .header("cookie", cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8_lossy(&body);
    assert!(text.contains("Awaiting review"));
    assert!(!text.contains("revoked"));
}

#[tokio::test]
async fn inactive_link_without_non_repeatable_request_returns_generic_404() {
    let mut link = active_link(ACTIVE_SLUG);
    link.expires_at = Some(Utc.with_ymd_and_hms(2020, 1, 1, 0, 0, 0).unwrap());
    let declined = request_with_state(RequestId::new(), link.id, REQUESTER_ID, RequestState::Declined);
    let (app, _calls) = build_test_app(
        link,
        Some(declined),
        MockTransport::scripted(oauth_expectations("octocat", REQUESTER_ID)),
    )
    .await;
    let cookie = sign_in(app.clone()).await;

    let resp = app
        .oneshot(
            Request::builder()
                .uri(format!("/i/{ACTIVE_SLUG}"))
                .header("cookie", cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8_lossy(&body);
    assert!(text.contains("Page not found"));
    assert!(!text.contains("expired"));
}

#[tokio::test]
async fn submit_creates_request_and_redirects_to_canonical_page() {
    let link = active_link(ACTIVE_SLUG);
    let link_id = link.id;
    let (app, calls) = build_test_app(
        link,
        None,
        MockTransport::scripted(oauth_expectations("octocat", REQUESTER_ID)),
    )
    .await;
    let cookie = sign_in(app.clone()).await;

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/i/{ACTIVE_SLUG}"))
                .header("cookie", cookie)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from("request_id=01ARZ3NDEKTSV4RRFFQ69G5FAV&justification=ship-it"))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    let location = resp.headers().get("location").unwrap().to_str().unwrap();
    assert_eq!(location, format!("/i/{ACTIVE_SLUG}"));
    assert_eq!(
        calls.lock().unwrap().as_slice(),
        &[RecordedCommand::SubmitInvitationRequest {
            invitation_link_id: link_id,
            requester_id: REQUESTER_ID,
            justification: Some("ship-it".into()),
        }]
    );
}

#[tokio::test]
async fn submit_with_existing_approved_request_redirects_without_command() {
    let link = active_link(ACTIVE_SLUG);
    let approved = request_with_state(RequestId::new(), link.id, REQUESTER_ID, RequestState::Approved);
    let (app, calls) = build_test_app(
        link,
        Some(approved),
        MockTransport::scripted(oauth_expectations("octocat", REQUESTER_ID)),
    )
    .await;
    let cookie = sign_in(app.clone()).await;

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/i/{ACTIVE_SLUG}"))
                .header("cookie", cookie)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from("request_id=01ARZ3NDEKTSV4RRFFQ69G5FAV&justification=again"))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    let location = resp.headers().get("location").unwrap().to_str().unwrap();
    assert_eq!(location, format!("/i/{ACTIVE_SLUG}"));
    assert!(calls.lock().unwrap().is_empty());
}
```

- [ ] **Step 3: Run canonical route tests to verify they fail**

Run: `cargo test -p web signed_in_landing_renders_merged_request_form && cargo test -p web submit_creates_request_and_redirects_to_canonical_page`

Expected: FAIL until `submit_request` uses `/i/{slug}` and status-aware duplicate handling.

- [ ] **Step 4: Finish canonical `POST /i/{slug}` behavior**

In `crates/web/src/routes/invitation.rs`, replace `submit_request` with this shape:

```rust
async fn submit_request(
    State(state): State<AppState>,
    tower: TowerSession,
    axum::extract::Path(slug): axum::extract::Path<String>,
    serde_qs::axum::QsForm(form): serde_qs::axum::QsForm<SubmitForm>,
) -> impl IntoResponse {
    let session = session::load(&tower).await.unwrap_or_default();
    let canonical_path = canonical_invitation_path(&slug);
    if !session.is_authenticated() {
        return redirect_to_login(&canonical_path);
    }

    let now = Utc::now();
    let context = match resolve_public_invitation_link_context(state.storage.as_ref(), &slug).await {
        Ok(context) => context,
        Err(error) => return error.into_public_error().into_response(),
    };
    let slug = context.slug.as_str().to_string();
    let canonical_path = canonical_invitation_path(&slug);
    let selection = select_requester_request_state(&context.requests, session.user_id);
    if selection.current_status.is_some() {
        return Redirect::to(&canonical_path).into_response();
    }
    if !context.link.is_active(now) {
        return invitation_not_found_response(Some(session.login.clone()));
    }

    let justification = form
        .justification
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    use std::str::FromStr;
    let request_id =
        domain::RequestId::from_str(&form.request_id).unwrap_or_else(|_| domain::RequestId::new());

    if let Err(error) = state
        .commands
        .submit_invitation_request(SubmitInvitationRequest::new(
            request_id,
            context.link.id,
            session.user_id,
            justification,
            now,
        ))
        .await
    {
        tracing::warn!(error = ?error, "submit invitation request command failed");
        let _ = session::set_flash(
            &tower,
            session::Flash {
                level: session::FlashLevel::Error,
                message: "Failed to submit request. Please try again.".into(),
            },
        )
        .await;
        return Redirect::to(&canonical_path).into_response();
    }

    Redirect::to(&canonical_path).into_response()
}
```

Make sure `SubmitForm` comment says `GET /i/{slug}` instead of `GET /request`:

```rust
/// Pre-generated ULID from GET /i/{slug} handler for double-submit dedup.
```

- [ ] **Step 5: Run full invitation route tests**

Run: `cargo test -p web invitation_resolution`

Expected: PASS for `tests/invitation_resolution.rs` after removing or updating old assertions that mention `/request` and `/pending` as supported pages.

- [ ] **Step 6: Commit state-aware route work**

```bash
git add crates/web/src/routes/invitation.rs crates/web/tests/invitation_resolution.rs
git commit -m "feat(web): show invitation request status on canonical page"
```

---

### Task 5: Smoke Tests And Final Verification

**Files:**

- Modify: `crates/web/tests/route_smoke.rs`

- [ ] **Step 1: Finalize route smoke expectations**

Ensure the invitation smoke tests assert these behaviors:

```rust
#[tokio::test]
async fn invitation_landing_unknown_slug_redirects_to_login() {
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

    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    let location = resp.headers().get("location").unwrap().to_str().unwrap();
    assert_eq!(location, "/login?return_to=/i/AAAAAAAAAAAAAAAA");
}

#[tokio::test]
async fn invitation_unknown_nested_route_redirects_to_login() {
    let app = build_test_app().await;
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/i/AAAAAAAAAAAAAAAA/anything")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    let location = resp.headers().get("location").unwrap().to_str().unwrap();
    assert_eq!(location, "/login?return_to=/i/AAAAAAAAAAAAAAAA/anything");
}

#[tokio::test]
async fn invitation_request_form_unauthenticated_redirects_to_login_before_404() {
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
    assert_eq!(location, "/login?return_to=/i/AAAAAAAAAAAAAAAA/request");
}

#[tokio::test]
async fn invitation_pending_unauthenticated_redirects_to_login_before_404() {
    let app = build_test_app().await;
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/i/AAAAAAAAAAAAAAAA/pending/01ARZ3NDEKTSV4RRFFQ69G5FAV")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    let location = resp.headers().get("location").unwrap().to_str().unwrap();
    assert_eq!(
        location,
        "/login?return_to=/i/AAAAAAAAAAAAAAAA/pending/01ARZ3NDEKTSV4RRFFQ69G5FAV"
    );
}
```

- [ ] **Step 2: Run smoke tests**

Run: `cargo test -p web route_smoke`

Expected: PASS for all route smoke tests.

- [ ] **Step 3: Run web crate tests**

Run: `cargo test -p web`

Expected: PASS for all web crate tests.

- [ ] **Step 4: Run workspace formatting check**

Run: `cargo fmt --all --check`

Expected: PASS with no formatting diff.

- [ ] **Step 5: Run focused spec verification commands**

Run: `cargo test -p web invitation_resolution && cargo test -p web route_smoke && cargo test -p web`

Expected: all three commands PASS.

- [ ] **Step 6: Inspect final diff**

Run: `git diff --stat && git diff -- crates/web/src/invitation_link_resolution.rs crates/web/src/routes/invitation.rs crates/web/src/views/invitation.rs crates/web/tests/invitation_resolution.rs crates/web/tests/route_smoke.rs`

Expected: diff only touches the five planned files and reflects the canonical `/i/{slug}` flow.

- [ ] **Step 7: Commit final verification cleanup**

```bash
git add crates/web/src/invitation_link_resolution.rs crates/web/src/routes/invitation.rs crates/web/src/views/invitation.rs crates/web/tests/invitation_resolution.rs crates/web/tests/route_smoke.rs
git commit -m "test(web): cover canonical invitation request flow"
```

---

## Self-Review

Spec coverage:

- Pre-auth privacy is covered by Task 3 tests and route implementation.
- Single canonical `/i/{code}` page is covered by Task 2 view work and Task 3 route registration.
- Pending and approved non-repeatable states are covered by Task 1 selector tests, Task 2 status rendering tests, and Task 4 route tests.
- Retryable declined, expired, and cancelled states are covered by Task 1 selector tests, Task 2 retry-notice tests, and Task 4 newest-notice route tests.
- Inactive link concealment with existing status exception is covered by Task 4 inactive-link tests.
- Nested `/i/{code}/...` authenticate-before-404 behavior is covered by Task 3 and Task 5 tests.
- UI direction is covered by Task 2 component structure, copy assertions, no nested cards, no requester-primary `Create your own invitation link`, and semantic status notices.

Placeholder scan: the plan contains concrete file paths, code snippets, commands, expected failures, expected passes, and commit checkpoints.

Type consistency: route code uses `RequestState`, `PublicInvitationLinkContext`, `RequesterRequestSelection`, `RequestPage`, `SubmitForm`, and `SubmitInvitationRequest` consistently with the definitions introduced in earlier tasks.
