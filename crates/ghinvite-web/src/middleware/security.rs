//! Public capability surfaces must not leak through referrers, caches or indexes.
use axum::{
    extract::Request,
    http::{HeaderValue, header},
    middleware::Next,
    response::{IntoResponse, Response},
};

pub async fn headers(request: Request, next: Next) -> Response {
    let sensitive = capability_path(request.uri().path());
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    headers.insert(
        header::STRICT_TRANSPORT_SECURITY,
        HeaderValue::from_static("max-age=31536000"),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(super::csp::CONTENT_SECURITY_POLICY),
    );
    if sensitive {
        headers.insert("x-robots-tag", HeaderValue::from_static("noindex"));
        headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    }
    response
}

fn capability_path(path: &str) -> bool {
    ["/i", "/code", "/join", "/requests"].iter().any(|prefix| {
        path == *prefix
            || path
                .strip_prefix(prefix)
                .is_some_and(|tail| tail.starts_with('/'))
    })
}

pub async fn robots() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        "User-agent: *\nDisallow: /i\nDisallow: /code\nDisallow: /join\nDisallow: /requests\n",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Router,
        body::Body,
        http::{Request, StatusCode},
        routing::any,
    };
    use tower::ServiceExt;

    #[tokio::test]
    async fn privacy_headers_cover_success_redirect_error_and_legacy_paths() {
        let app = Router::new()
            .fallback(any(|| async {
                (
                    StatusCode::SEE_OTHER,
                    [
                        (header::LOCATION, "/join"),
                        (header::CACHE_CONTROL, "public, max-age=600"),
                    ],
                )
            }))
            .layer(axum::middleware::from_fn(headers));
        for path in [
            "/i",
            "/i/SECRET_MARKER",
            "/i/SECRET_MARKER/submit",
            "/code",
            "/join",
            "/requests/123",
        ] {
            let response = app
                .clone()
                .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.headers()[header::REFERRER_POLICY], "no-referrer");
            assert_eq!(
                response.headers()[header::STRICT_TRANSPORT_SECURITY],
                "max-age=31536000"
            );
            assert_eq!(
                response.headers()[header::X_CONTENT_TYPE_OPTIONS],
                "nosniff"
            );
            assert_eq!(
                response.headers()[header::CONTENT_SECURITY_POLICY],
                super::super::csp::CONTENT_SECURITY_POLICY
            );
            assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
            assert_eq!(response.headers()["x-robots-tag"], "noindex");
            assert!(!format!("{:?}", response.headers()).contains("SECRET_MARKER"));
        }
        let error = Router::new()
            .fallback(any(|| async { StatusCode::SERVICE_UNAVAILABLE }))
            .layer(axum::middleware::from_fn(headers));
        let response = error
            .oneshot(Request::builder().uri("/join").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        assert_eq!(response.headers()["x-robots-tag"], "noindex");
    }

    #[test]
    fn path_boundaries_do_not_disable_unrelated_asset_caching() {
        for path in [
            "/",
            "/index",
            "/static/i.js",
            "/codecs",
            "/joining",
            "/console/requests",
        ] {
            assert!(!capability_path(path));
        }
    }

    #[tokio::test]
    async fn robots_disallows_public_capability_routes() {
        let response = robots().await.into_response();
        let bytes = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .unwrap();
        assert_eq!(
            &bytes[..],
            b"User-agent: *\nDisallow: /i\nDisallow: /code\nDisallow: /join\nDisallow: /requests\n"
        );
    }
}
