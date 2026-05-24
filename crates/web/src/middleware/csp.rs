//! Content-Security-Policy header layer.
//!
//! Applied to console responses. For Plan 4 we ship the
//! constant + a helper layer; the home page (Task 22) sets a more permissive
//! policy because the marketing page may load external assets.

use axum::http::HeaderValue;
use tower_http::set_header::SetResponseHeaderLayer;

/// Strict CSP for console pages. Adjust if future changes introduce external
/// dependencies (CDN scripts, third-party fonts).
pub const CONSOLE_CSP: &str = "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; \
     img-src 'self' https://avatars.githubusercontent.com data:; \
     connect-src 'self'; \
     font-src 'self'; \
     frame-ancestors 'none'; \
     form-action 'self' https://github.com; \
     base-uri 'self'";

pub fn console_csp_layer() -> SetResponseHeaderLayer<HeaderValue> {
    SetResponseHeaderLayer::if_not_present(
        axum::http::header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(CONSOLE_CSP),
    )
}
