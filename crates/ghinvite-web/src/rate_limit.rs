//! Edge throttles run before authority calls. No bearer secret or forwarding
//! header is a bucket key; runtime entry points provide the trusted peer IP.
use axum::{
    extract::{Request, State},
    http::{Method, StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
use std::{
    collections::{HashMap, VecDeque},
    net::IpAddr,
    sync::Mutex,
};
use tower_sessions::Session;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Bucket {
    Ip,
    User,
    Authority,
}
impl Bucket {
    const fn limit(self) -> usize {
        match self {
            Self::Ip => 30,
            Self::User | Self::Authority => 10,
        }
    }
}
#[derive(Clone, Copy, Debug)]
pub struct Unavailable;

#[async_trait::async_trait]
pub trait EdgeRateLimiter: Send + Sync {
    async fn check(&self, bucket: Bucket, key: &str) -> Result<bool, Unavailable>;
}
/// Central composition wrapper: one diagnostic per actual rejected/unavailable
/// binding check. Never includes the opaque key, IP, user, URL or request body.
pub struct ObservedRateLimiter {
    inner: std::sync::Arc<dyn EdgeRateLimiter>,
}
impl ObservedRateLimiter {
    pub fn new(inner: std::sync::Arc<dyn EdgeRateLimiter>) -> Self {
        Self { inner }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BindingRejection {
    Limited,
    Unavailable,
}
impl BindingRejection {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Limited => "limited",
            Self::Unavailable => "unavailable",
        }
    }
}
impl Bucket {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ip => "ip",
            Self::User => "user",
            Self::Authority => "authority",
        }
    }
}
#[async_trait::async_trait]
impl EdgeRateLimiter for ObservedRateLimiter {
    async fn check(&self, bucket: Bucket, key: &str) -> Result<bool, Unavailable> {
        let result = self.inner.check(bucket, key).await;
        let rejected = match result {
            Ok(true) => None,
            Ok(false) => Some(BindingRejection::Limited),
            Err(_) => Some(BindingRejection::Unavailable),
        };
        if let Some(reason) = rejected {
            tracing::warn!(
                event = "edge_binding_rejection",
                bucket = bucket.as_str(),
                reason = reason.as_str(),
                occurred_at = chrono::Utc::now().timestamp()
            );
        }
        result
    }
}

/// Constructor default: missing deployment wiring cannot disable a throttle.
pub struct Unconfigured;
#[async_trait::async_trait]
impl EdgeRateLimiter for Unconfigured {
    async fn check(&self, _: Bucket, _: &str) -> Result<bool, Unavailable> {
        Err(Unavailable)
    }
}

/// Only trusted runtime ingress and explicit router tests construct this.
/// Native uses the socket peer, Worker uses its platform-provided client IP.
/// X-Forwarded-For and request body identity never supply this extension.
#[derive(Clone, Copy, Debug)]
pub struct TrustedClientIp(IpAddr);
impl TrustedClientIp {
    pub const fn new(ip: IpAddr) -> Self {
        Self(ip)
    }
}

/// Process-local native equivalent. The mutex makes admission atomic across
/// concurrent requests; active key capacity is bounded and exhaustion refuses
/// new keys. Multi-process deployments require a shared limiter adapter.
#[derive(Default)]
pub struct MemoryRateLimiter {
    windows: Mutex<HashMap<(Bucket, String), VecDeque<i64>>>,
}
impl MemoryRateLimiter {
    fn check_at(&self, bucket: Bucket, key: &str, now_ms: i64) -> Result<bool, Unavailable> {
        const MAX_KEYS: usize = 10_000;
        if key.is_empty() || key.len() > 128 {
            return Err(Unavailable);
        }
        let cutoff = now_ms.saturating_sub(60_000);
        let mut windows = self.windows.lock().map_err(|_| Unavailable)?;
        windows.retain(|_, times| {
            while times.front().is_some_and(|time| *time <= cutoff) {
                times.pop_front();
            }
            !times.is_empty()
        });
        let id = (bucket, key.to_owned());
        if !windows.contains_key(&id) && windows.len() >= MAX_KEYS {
            return Err(Unavailable);
        }
        let times = windows.entry(id).or_default();
        if times.len() >= bucket.limit() {
            return Ok(false);
        }
        times.push_back(now_ms);
        Ok(true)
    }
}
#[async_trait::async_trait]
impl EdgeRateLimiter for MemoryRateLimiter {
    async fn check(&self, bucket: Bucket, key: &str) -> Result<bool, Unavailable> {
        self.check_at(bucket, key, chrono::Utc::now().timestamp_millis())
    }
}

const SIGNIN_STARTS: &str = "signin_starts";
const SIGNIN_BUDGET: &str = "signin_budget";
const SIGNIN_WINDOW_MS: i64 = 600_000;
#[derive(Clone, Debug, Default)]
pub struct SigninStarts {
    times: Vec<i64>,
    budget_key: Option<ghinvite_core::storage::signin_budget::SigninBudgetKey>,
}
pub async fn signin_starts(
    tower: &Session,
) -> Result<SigninStarts, tower_sessions::session::Error> {
    Ok(SigninStarts {
        times: tower.get(SIGNIN_STARTS).await?.unwrap_or_default(),
        budget_key: tower.get(SIGNIN_BUDGET).await?,
    })
}
/// Preserve both the bounded display list and stable, irreversible budget key
/// when OAuth rotates and clears identity state. Reservation authority is SQL.
pub async fn restore_signin_starts(
    tower: &Session,
    starts: &SigninStarts,
) -> Result<(), tower_sessions::session::Error> {
    tower.insert(SIGNIN_STARTS, &starts.times).await?;
    if let Some(key) = starts.budget_key {
        tower.insert(SIGNIN_BUDGET, key).await?;
    }
    Ok(())
}
async fn ensure_lineage(
    tower: &Session,
    starts: &mut SigninStarts,
) -> Result<(), tower_sessions::session::Error> {
    if starts.budget_key.is_none() {
        // A new browser receives a persisted random ID first. Existing parallel
        // requests derive the same lineage from the already-issued session ID.
        if tower.id().is_none() {
            restore_signin_starts(tower, starts).await?;
            crate::session_store::apply_lifetime(tower).await?;
            tower.save().await?;
        }
        let id = tower.id().ok_or_else(|| {
            tower_sessions::session::Error::Store(tower_sessions::session_store::Error::Backend(
                "session identity unavailable".into(),
            ))
        })?;
        use sha2::{Digest, Sha256};
        let mut digest = Sha256::new();
        digest.update(b"ghinvite/signin-budget/v1\0");
        digest.update(id.to_string().as_bytes());
        starts.budget_key = Some(
            ghinvite_core::storage::signin_budget::SigninBudgetKey::from_digest(
                digest.finalize().into(),
            ),
        );
    }
    restore_signin_starts(tower, starts).await?;
    tower.save().await
}
fn record_signin(starts: &mut Vec<i64>, now_ms: i64) -> bool {
    starts.retain(|at| *at > now_ms.saturating_sub(SIGNIN_WINDOW_MS));
    if starts.len() >= 5 {
        return false;
    }
    starts.push(now_ms);
    true
}
fn denied(seconds: u32) -> Response {
    (
        StatusCode::TOO_MANY_REQUESTS,
        [
            (header::RETRY_AFTER, seconds.to_string()),
            (header::CACHE_CONTROL, "no-store".into()),
        ],
        "Too many requests; try again later.",
    )
        .into_response()
}
fn unavailable() -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        [(header::CACHE_CONTROL, "no-store")],
        "Service temporarily unavailable; try again later.",
    )
        .into_response()
}

/// Place inside the session layer and outside all endpoint handlers.
pub async fn enforce(
    State(state): State<crate::AppState>,
    tower: Session,
    req: Request,
    next: Next,
) -> Response {
    let path = req.uri().path();
    let ip_limited = (req.method() == Method::POST && matches!(path, "/i" | "/code"))
        || path == "/oauth/callback";
    if ip_limited {
        let Some(ip) = req.extensions().get::<TrustedClientIp>() else {
            return unavailable();
        };
        match state.edge_limits.check(Bucket::Ip, &ip.0.to_string()).await {
            Ok(true) => {}
            Ok(false) => return denied(60),
            Err(_) => return unavailable(),
        }
    }
    let requester_refresh = path
        .strip_prefix("/requests/")
        .and_then(|p| p.strip_suffix("/refresh"))
        // Axum decodes the path parameter later. Charge the route shape here
        // so percent-encoded IDs cannot bypass the shared user bucket.
        .is_some_and(|id| !id.is_empty() && !id.contains('/'));
    if req.method() == Method::POST
        && (matches!(path, "/join" | "/join/organization/refresh") || requester_refresh)
    {
        let session = match crate::session::load(&tower).await {
            Ok(session) => session,
            Err(_) => return unavailable(),
        };
        // The endpoint handles unauthenticated callers. Never use caller input
        // as a substitute for a trusted GitHub user identity.
        if session.is_authenticated() && session.user_id != 0 {
            match state
                .edge_limits
                .check(Bucket::User, &session.user_id.to_string())
                .await
            {
                Ok(true) => {}
                Ok(false) => return denied(60),
                Err(_) => return unavailable(),
            }
        }
    }
    if req.method() == Method::GET && path == "/login" {
        let mut starts = match signin_starts(&tower).await {
            Ok(starts) => starts,
            Err(_) => return unavailable(),
        };
        let now = chrono::Utc::now().timestamp_millis();
        if !record_signin(&mut starts.times, now) {
            return denied(600);
        }
        if ensure_lineage(&tower, &mut starts).await.is_err() {
            return unavailable();
        }
        match state
            .storage
            .reserve_signin(
                starts.budget_key.expect("lineage initialized"),
                ghinvite_core::RequestId::new(),
                now,
            )
            .await
        {
            Ok(true) => {}
            Ok(false) => return denied(600),
            Err(_) => return unavailable(),
        }
        if restore_signin_starts(&tower, &starts).await.is_err()
            || crate::session_store::apply_lifetime(&tower).await.is_err()
            || tower.save().await.is_err()
        {
            return unavailable();
        }
    }
    next.run(req).await
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn observed_rejections_preserve_decisions_and_hide_keys() {
        use std::sync::Arc;
        #[derive(Clone, Default)]
        struct Capture(Arc<Mutex<Vec<u8>>>);
        impl std::io::Write for Capture {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let capture = Capture::default();
        let writer = capture.clone();
        let subscriber = tracing_subscriber::fmt()
            .without_time()
            .with_ansi(false)
            .with_writer(move || writer.clone())
            .finish();
        let _guard = tracing::subscriber::set_default(subscriber);
        let limits = ObservedRateLimiter::new(Arc::new(MemoryRateLimiter::default()));
        for _ in 0..10 {
            assert!(
                limits
                    .check(Bucket::User, "PRIVATE_USER_KEY")
                    .await
                    .unwrap()
            );
        }
        assert!(
            !limits
                .check(Bucket::User, "PRIVATE_USER_KEY")
                .await
                .unwrap()
        );
        let absent = ObservedRateLimiter::new(Arc::new(Unconfigured));
        assert!(
            absent
                .check(Bucket::Authority, "PRIVATE_AUTHORITY_KEY")
                .await
                .is_err()
        );
        let bytes = capture.0.lock().unwrap();
        let text = std::str::from_utf8(&bytes).unwrap();
        assert_eq!(text.matches("event=\"edge_binding_rejection\"").count(), 2);
        assert!(text.contains("reason=\"limited\""));
        assert!(text.contains("reason=\"unavailable\""));
        assert_eq!(text.matches("occurred_at=").count(), 2);
        assert!(!text.contains("PRIVATE_"));
    }
    #[test]
    fn windows_bound_each_identity_and_release_at_exact_boundary() {
        let limits = MemoryRateLimiter::default();
        for _ in 0..30 {
            assert!(limits.check_at(Bucket::Ip, "127.0.0.1", 0).unwrap());
        }
        assert!(!limits.check_at(Bucket::Ip, "127.0.0.1", 59_999).unwrap());
        assert!(limits.check_at(Bucket::Ip, "127.0.0.2", 59_999).unwrap());
        assert!(limits.check_at(Bucket::Ip, "127.0.0.1", 60_000).unwrap());
        for _ in 0..10 {
            assert!(limits.check_at(Bucket::User, "7", 0).unwrap());
        }
        assert!(!limits.check_at(Bucket::User, "7", 1).unwrap());
        assert!(limits.check_at(Bucket::User, "8", 1).unwrap());
    }
    #[test]
    fn signins_use_ten_minutes_and_do_not_extend_window_on_rejection() {
        let mut starts = vec![];
        for now in 0..5 {
            assert!(record_signin(&mut starts, now));
        }
        assert!(!record_signin(&mut starts, 599_999));
        assert_eq!(starts, vec![0, 1, 2, 3, 4]);
        assert!(record_signin(&mut starts, 600_000));
        assert_eq!(starts, vec![1, 2, 3, 4, 600_000]);
    }
    #[test]
    fn concurrent_native_callers_cannot_overrun_user_bucket() {
        let limiter = std::sync::Arc::new(MemoryRateLimiter::default());
        let allowed = std::thread::scope(|scope| {
            (0..100)
                .map(|_| {
                    let limiter = limiter.clone();
                    scope.spawn(move || limiter.check_at(Bucket::User, "42", 0).unwrap())
                })
                .collect::<Vec<_>>()
                .into_iter()
                .map(|thread| thread.join().unwrap() as usize)
                .sum::<usize>()
        });
        assert_eq!(allowed, 10);
    }
    async fn app(
        configured: bool,
    ) -> (axum::Router, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        let state = crate::AppState::new(
            Arc::new(
                ghinvite_storage_sqlx::SqlxStorage::in_memory()
                    .await
                    .unwrap(),
            ),
            crate::WebGithub::new(
                Arc::new(ghinvite_github::mocks::MockTransport::scripted(vec![])),
                &Default::default(),
                vec![],
            )
            .unwrap(),
            Arc::new(crate::RestateClient::new("http://127.0.0.1:9").unwrap()),
            crate::WebConfig::for_local_dev_with_secret([7; 32]),
        );
        let state = if configured {
            state.with_edge_limits(Arc::new(MemoryRateLimiter::default()))
        } else {
            state
        };
        let effects = Arc::new(AtomicUsize::new(0));
        let handler = {
            let effects = effects.clone();
            move || {
                effects.fetch_add(1, Ordering::SeqCst);
                async { StatusCode::NO_CONTENT }
            }
        };
        let router = axum::Router::new()
            .route("/i", axum::routing::post(handler.clone()))
            .route("/code", axum::routing::post(handler.clone()))
            .route("/oauth/callback", axum::routing::get(handler.clone()))
            .route("/join", axum::routing::post(handler.clone()))
            .route(
                "/join/organization/refresh",
                axum::routing::post(handler.clone()),
            )
            .route(
                "/requests/{request_id}/refresh",
                axum::routing::post(handler.clone()),
            )
            .route("/login", axum::routing::get(handler))
            .route(
                "/test-signin",
                axum::routing::get(|tower: Session| async move {
                    crate::session::save(
                        &tower,
                        &crate::session::Session {
                            user_id: 42,
                            login: "user".into(),
                            csrf_token: Some("test".into()),
                            ..Default::default()
                        },
                    )
                    .await
                    .unwrap();
                    StatusCode::NO_CONTENT
                }),
            )
            .route(
                "/test-rotate",
                axum::routing::get(|tower: Session| async move {
                    let starts = signin_starts(&tower).await.unwrap();
                    tower.cycle_id().await.unwrap();
                    tower.clear().await;
                    restore_signin_starts(&tower, &starts).await.unwrap();
                    StatusCode::NO_CONTENT
                }),
            )
            .layer(axum::middleware::from_fn_with_state(state.clone(), enforce))
            .layer(
                tower_sessions::SessionManagerLayer::new(tower_sessions::MemoryStore::default())
                    .with_secure(false),
            )
            .with_state(state);
        (router, effects)
    }
    async fn call(
        app: &axum::Router,
        method: Method,
        path: &str,
        cookie: Option<&str>,
        trusted_ip: bool,
    ) -> Response {
        use tower::ServiceExt;
        let mut req = axum::http::Request::builder()
            .method(method)
            .uri(path)
            .header("x-forwarded-for", "198.51.100.1")
            .header("cf-connecting-ip", "198.51.100.1");
        if let Some(cookie) = cookie {
            req = req.header(header::COOKIE, cookie);
        }
        let mut req = req.body(axum::body::Body::empty()).unwrap();
        if trusted_ip {
            req.extensions_mut()
                .insert(TrustedClientIp::new("127.0.0.1".parse().unwrap()));
        }
        app.clone().oneshot(req).await.unwrap()
    }
    fn cookie(response: &Response) -> String {
        response.headers()[header::SET_COOKIE]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned()
    }
    #[tokio::test]
    async fn missing_wiring_and_spoofed_headers_fail_before_side_effects() {
        use std::sync::atomic::Ordering;
        let (configured, effects) = app(true).await;
        assert_eq!(
            call(&configured, Method::POST, "/i", None, false)
                .await
                .status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert_eq!(effects.load(Ordering::SeqCst), 0);
        let (missing, effects) = app(false).await;
        assert_eq!(
            call(&missing, Method::POST, "/code", None, true)
                .await
                .status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert_eq!(effects.load(Ordering::SeqCst), 0);
    }
    #[tokio::test]
    async fn ip_and_user_routes_share_exact_buckets_and_refuse_before_handlers() {
        use std::sync::atomic::Ordering;
        let (app, effects) = app(true).await;
        for index in 0..30 {
            let (method, path) = match index % 3 {
                0 => (Method::POST, "/i"),
                1 => (Method::POST, "/code"),
                _ => (Method::GET, "/oauth/callback"),
            };
            assert_eq!(
                call(&app, method, path, None, true).await.status(),
                StatusCode::NO_CONTENT
            );
        }
        let response = call(&app, Method::POST, "/i", None, true).await;
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(response.headers()[header::RETRY_AFTER], "60");
        assert_eq!(effects.load(Ordering::SeqCst), 30);
        let first = cookie(&call(&app, Method::GET, "/test-signin", None, false).await);
        let second = cookie(&call(&app, Method::GET, "/test-signin", None, false).await);
        for index in 0..10 {
            assert_eq!(
                call(
                    &app,
                    Method::POST,
                    match index % 4 {
                        0 => "/join",
                        1 => "/join/organization/refresh",
                        2 => "/requests/01ARZ3NDEKTSV4RRFFQ69G5FAV/refresh",
                        _ => "/requests/%301ARZ3NDEKTSV4RRFFQ69G5FAV/refresh",
                    },
                    Some(if index % 2 == 0 { &first } else { &second }),
                    false
                )
                .await
                .status(),
                StatusCode::NO_CONTENT
            );
        }
        assert_eq!(
            call(
                &app,
                Method::POST,
                "/requests/%301ARZ3NDEKTSV4RRFFQ69G5FAV/refresh",
                Some(&second),
                false
            )
            .await
            .status(),
            StatusCode::TOO_MANY_REQUESTS
        );
        assert_eq!(effects.load(Ordering::SeqCst), 40);
    }
    #[tokio::test]
    async fn signin_starts_are_persisted_before_login_and_bounded_per_session() {
        use std::sync::atomic::Ordering;
        let (app, effects) = app(true).await;
        let first = call(&app, Method::GET, "/login", None, false).await;
        assert_eq!(first.status(), StatusCode::NO_CONTENT);
        let cookie = cookie(&first);
        for _ in 0..4 {
            assert_eq!(
                call(&app, Method::GET, "/login", Some(&cookie), false)
                    .await
                    .status(),
                StatusCode::NO_CONTENT
            );
        }
        assert_eq!(
            call(&app, Method::GET, "/login", Some(&cookie), false)
                .await
                .status(),
            StatusCode::TOO_MANY_REQUESTS
        );
        assert_eq!(effects.load(Ordering::SeqCst), 5);
        assert_eq!(
            call(&app, Method::GET, "/login", None, false)
                .await
                .status(),
            StatusCode::NO_CONTENT
        );
    }
    #[tokio::test]
    async fn concurrent_session_snapshots_and_id_rotation_cannot_reset_sql_budget() {
        use std::sync::atomic::Ordering;
        let (app, effects) = app(true).await;
        let cookie = cookie(&call(&app, Method::GET, "/test-signin", None, false).await);
        let mut tasks = tokio::task::JoinSet::new();
        for _ in 0..20 {
            let app = app.clone();
            let cookie = cookie.clone();
            tasks.spawn(async move {
                call(&app, Method::GET, "/login", Some(&cookie), false)
                    .await
                    .status()
            });
        }
        let mut allowed = 0;
        while let Some(result) = tasks.join_next().await {
            match result.unwrap() {
                StatusCode::NO_CONTENT => allowed += 1,
                StatusCode::TOO_MANY_REQUESTS => {}
                other => panic!("unexpected response {other}"),
            }
        }
        assert_eq!(allowed, 5);
        assert_eq!(effects.load(Ordering::SeqCst), 5);
        let rotated = call(&app, Method::GET, "/test-rotate", Some(&cookie), false).await;
        let new_cookie = super::tests::cookie(&rotated);
        assert_ne!(new_cookie, cookie);
        assert_eq!(
            call(&app, Method::GET, "/login", Some(&new_cookie), false)
                .await
                .status(),
            StatusCode::TOO_MANY_REQUESTS
        );
        assert_eq!(effects.load(Ordering::SeqCst), 5);
    }
}
