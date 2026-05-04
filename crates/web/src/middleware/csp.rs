//! Content-Security-Policy header layer.
//!
//! Applied to dashboard responses (set by Plan 5+). For Plan 4 we ship the
//! constant + a helper layer; the home page (Task 22) sets a more permissive
//! policy because the marketing page may load external assets.

use axum::http::HeaderValue;
use tower_http::set_header::SetResponseHeaderLayer;

/// Strict CSP for dashboard pages. Adjust if Plan 5 introduces external
/// dependencies (CDN scripts, third-party fonts).
pub const DASHBOARD_CSP: &str = "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; \
     img-src 'self' https://avatars.githubusercontent.com data:; \
     connect-src 'self'; \
     font-src 'self'; \
     frame-ancestors 'none'; \
     form-action 'self' https://github.com; \
     base-uri 'self'";

pub fn dashboard_csp_layer() -> SetResponseHeaderLayer<HeaderValue> {
    SetResponseHeaderLayer::if_not_present(
        axum::http::header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(DASHBOARD_CSP),
    )
}
