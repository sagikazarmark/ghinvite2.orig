//! Response basics every route uses: JSON bodies, GitHub's error shapes,
//! rate-limit headers and the request ID.

use axum::http::header::{CONTENT_TYPE, LINK};
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use chrono::{DateTime, Utc};
use serde::Serialize;

/// GitHub's JSON content type.
pub(crate) const JSON: &str = "application/json; charset=utf-8";

/// `documentation_url` of octoemu's own errors (501, admin API).
pub(crate) const OCTOEMU_DOCS: &str = "https://github.com/sagikazarmark/ghinvite2.orig/blob/main/libs/octoauth/crates/octoemu/README.md";

/// GitHub's REST documentation root, the default `documentation_url`.
pub(crate) const REST_DOCS: &str = "https://docs.github.com/rest";

/// `X-GitHub-Request-Id`.
pub(crate) static REQUEST_ID: HeaderName = HeaderName::from_static("x-github-request-id");

/// `status` with `body` as JSON.
pub(crate) fn json<T: Serialize + ?Sized>(status: StatusCode, body: &T) -> Response {
    match serde_json::to_vec(body) {
        Ok(bytes) => (
            status,
            [(CONTENT_TYPE, HeaderValue::from_static(JSON))],
            bytes,
        )
            .into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

/// `status` with `body` as JSON and, for a list spanning several pages, a
/// `Link` header (see `paginate`).
pub(crate) fn json_with_link<T: Serialize + ?Sized>(
    status: StatusCode,
    body: &T,
    link: Option<String>,
) -> Response {
    let mut response = json(status, body);
    if let Some(value) = link.and_then(|link| HeaderValue::from_str(&link).ok()) {
        response.headers_mut().insert(LINK, value);
    }
    response
}

/// The HTTP status as GitHub repeats it in an error body's `status`: the
/// code as a string (`"404"`; measured 2026-11-24 on every REST error the
/// fidelity suite records).
pub(crate) fn status_text(status: StatusCode) -> String {
    status.as_u16().to_string()
}

/// GitHub's error body: `{"message", "documentation_url", "status"}`.
#[derive(Debug, Serialize)]
pub(crate) struct ErrorBody<'a> {
    pub(crate) message: &'a str,
    pub(crate) documentation_url: &'a str,
    pub(crate) status: String,
}

/// A GitHub-style error.
pub(crate) fn error(status: StatusCode, message: &str, documentation_url: &str) -> Response {
    json(
        status,
        &ErrorBody {
            message,
            documentation_url,
            status: status_text(status),
        },
    )
}

/// One entry of a 422's `errors`.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct FieldError {
    pub(crate) resource: String,
    pub(crate) field: String,
    /// `missing`, `missing_field`, `invalid`, `already_exists`,
    /// `unprocessable` or `custom` (with `message`).
    pub(crate) code: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) message: Option<String>,
}

/// A 422 `Validation Failed` with `errors`, and `status` as on every other
/// error: the token route's measured 422s (its description names
/// `validation-error`) carry it, and that schema leaves it open.
pub(crate) fn validation_failed(errors: &[FieldError], documentation_url: &str) -> Response {
    #[derive(Serialize)]
    struct Body<'a> {
        message: &'static str,
        errors: &'a [FieldError],
        documentation_url: &'a str,
        status: String,
    }
    json(
        StatusCode::UNPROCESSABLE_ENTITY,
        &Body {
            message: "Validation Failed",
            errors,
            documentation_url,
            status: status_text(StatusCode::UNPROCESSABLE_ENTITY),
        },
    )
}

/// The 501 every unbuilt API or web route answers. Never a 404, which a
/// client could take for GitHub's answer.
pub(crate) fn not_implemented(method: &str, path: &str) -> Response {
    #[derive(Serialize)]
    struct Body<'a> {
        message: &'static str,
        route: String,
        documentation_url: &'a str,
    }
    json(
        StatusCode::NOT_IMPLEMENTED,
        &Body {
            message: "octoemu: not implemented",
            route: format!("{method} {path}"),
            documentation_url: OCTOEMU_DOCS,
        },
    )
}

/// `x-ratelimit-*` values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RateLimit {
    pub(crate) limit: u32,
    pub(crate) remaining: u32,
    pub(crate) used: u32,
    /// When the window resets, in Unix seconds.
    pub(crate) reset: i64,
    /// `core`, `search`, `graphql`, …
    pub(crate) resource: &'static str,
}

impl RateLimit {
    /// The fixed stand-in until budgets are modelled: a fresh
    /// 5000-request `core` window resetting in an hour.
    pub(crate) fn stub(now: DateTime<Utc>) -> Self {
        Self {
            limit: 5000,
            remaining: 5000,
            used: 0,
            reset: now.timestamp() + 3600,
            resource: "core",
        }
    }

    /// Sets the five `x-ratelimit-*` headers.
    pub(crate) fn apply(&self, headers: &mut HeaderMap) {
        let mut set = |name: &'static str, value: String| {
            if let Ok(value) = HeaderValue::from_str(&value) {
                headers.insert(HeaderName::from_static(name), value);
            }
        };
        set("x-ratelimit-limit", self.limit.to_string());
        set("x-ratelimit-remaining", self.remaining.to_string());
        set("x-ratelimit-used", self.used.to_string());
        set("x-ratelimit-reset", self.reset.to_string());
        set("x-ratelimit-resource", self.resource.to_owned());
    }
}

/// A request ID in GitHub's shape (colon-separated hex), unique per
/// emulator instance: `<port>:<pid>:<sequence>`.
pub(crate) fn request_id(port: u16, sequence: u64) -> String {
    format!(
        "{port:04X}:{:06X}:{:08X}",
        std::process::id() & 0xFF_FFFF,
        sequence
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;

    async fn body(response: Response) -> serde_json::Value {
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn error_bodies_have_githubs_shape() {
        let response = error(StatusCode::NOT_FOUND, "Not Found", REST_DOCS);
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(response.headers()[CONTENT_TYPE], JSON);
        assert_eq!(
            body(response).await,
            serde_json::json!({"message": "Not Found", "documentation_url": REST_DOCS, "status": "404"})
        );

        let response = validation_failed(
            &[FieldError {
                resource: "Invitation".into(),
                field: "invitee_id".into(),
                code: "missing_field".into(),
                message: None,
            }],
            REST_DOCS,
        );
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(
            body(response).await,
            serde_json::json!({
                "message": "Validation Failed",
                "errors": [{"resource": "Invitation", "field": "invitee_id", "code": "missing_field"}],
                "documentation_url": REST_DOCS,
                "status": "422",
            })
        );
    }

    #[test]
    fn rate_limit_headers() {
        let now = DateTime::from_timestamp(1_900_000_000, 0).unwrap();
        let mut headers = HeaderMap::new();
        RateLimit::stub(now).apply(&mut headers);
        assert_eq!(headers["x-ratelimit-limit"], "5000");
        assert_eq!(headers["x-ratelimit-remaining"], "5000");
        assert_eq!(headers["x-ratelimit-used"], "0");
        assert_eq!(headers["x-ratelimit-reset"], "1900003600");
        assert_eq!(headers["x-ratelimit-resource"], "core");
    }

    #[test]
    fn request_ids_are_distinct() {
        assert_ne!(request_id(8080, 1), request_id(8080, 2));
        assert_eq!(request_id(1, 1).split(':').count(), 3);
    }
}
