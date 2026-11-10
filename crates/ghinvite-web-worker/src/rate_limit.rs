//! Bindings are required in production. They are platform-managed, per-location
//! approximate limits, as specified in P1 §11.2.
use ghinvite_web::rate_limit::{Bucket, EdgeRateLimiter, TrustedClientIp, Unavailable};
use worker::{Env, RateLimiter};

pub struct WorkerRateLimiter {
    ip: RateLimiter,
    user: RateLimiter,
    authority: RateLimiter,
}
impl WorkerRateLimiter {
    pub fn from_env(env: &Env) -> worker::Result<Self> {
        Ok(Self {
            ip: env.rate_limiter("RL_IP")?,
            user: env.rate_limiter("RL_USER")?,
            authority: env.rate_limiter("AUTHZ_RL")?,
        })
    }
}
#[async_trait::async_trait]
impl EdgeRateLimiter for WorkerRateLimiter {
    async fn check(&self, bucket: Bucket, key: &str) -> Result<bool, Unavailable> {
        let limiter = match bucket {
            Bucket::Ip => &self.ip,
            Bucket::User => &self.user,
            Bucket::Authority => &self.authority,
        };
        limiter
            .limit(key.to_owned())
            .await
            .map(|outcome| outcome.success)
            .map_err(|_| Unavailable)
    }
}
pub fn client_ip(req: &worker::HttpRequest) -> Option<TrustedClientIp> {
    // Cloudflare populates/overwrites this header at the public ingress. We do
    // not accept X-Forwarded-For; native ingress uses socket peer identity.
    let value = req.headers().get("CF-Connecting-IP")?.to_str().ok()?;
    value.parse().ok().map(TrustedClientIp::new)
}

#[cfg(feature = "runtime-tests")]
pub async fn fixture(req: worker::HttpRequest, env: &Env) -> axum::response::Response {
    use axum::response::IntoResponse;
    let limiter = match WorkerRateLimiter::from_env(env) {
        Ok(limiter) => limiter,
        Err(_) => {
            return (
                axum::http::StatusCode::SERVICE_UNAVAILABLE,
                "Rate limiter unavailable",
            )
                .into_response();
        }
    };
    let limiter = ghinvite_web::rate_limit::ObservedRateLimiter::new(std::sync::Arc::new(limiter));
    let (bucket, key) = if req.uri().path().ends_with("/ip") {
        let Some(raw) = req
            .headers()
            .get("CF-Connecting-IP")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<std::net::IpAddr>().ok())
        else {
            return (
                axum::http::StatusCode::SERVICE_UNAVAILABLE,
                "Trusted IP unavailable",
            )
                .into_response();
        };
        (Bucket::Ip, raw.to_string())
    } else if req.uri().path().ends_with("/authority") {
        let Some(key) = req.uri().query().filter(|key| {
            key.len() <= 128 && (key.starts_with("repo:") || key.starts_with("org:"))
        }) else {
            return axum::http::StatusCode::BAD_REQUEST.into_response();
        };
        (Bucket::Authority, key.to_owned())
    } else if req.uri().path().ends_with("/user") {
        // Test-only driver; production user keys come from the signed-in session.
        let Some(id) = req
            .uri()
            .query()
            .and_then(|s| s.parse::<u64>().ok())
            .filter(|id| *id > 0)
        else {
            return axum::http::StatusCode::BAD_REQUEST.into_response();
        };
        (Bucket::User, id.to_string())
    } else {
        return axum::http::StatusCode::NOT_FOUND.into_response();
    };
    match limiter.check(bucket, &key).await {
        Ok(true) => axum::http::StatusCode::NO_CONTENT.into_response(),
        Ok(false) => axum::http::StatusCode::TOO_MANY_REQUESTS.into_response(),
        Err(_) => axum::http::StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}
