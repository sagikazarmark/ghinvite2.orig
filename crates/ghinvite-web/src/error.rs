//! Web-binary error type with axum `IntoResponse` mapping.
//!
//! Two rules hold across this module:
//!
//! * **Nothing a browser sees is quoted from upstream.** Every failure renders
//!   ghinvite's own copy plus a link out, so a visitor is never left on a dead
//!   page holding a gateway's diagnostic.
//! * **Nothing an upstream service said can be put into these variants in the
//!   first place.** The ingress and OAuth failures are typed rather than
//!   stringly, so a future caller cannot reintroduce the leak by formatting a
//!   response body into one.

use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Response};
use dioxus::prelude::*;
use thiserror::Error;

pub type Result<T> = std::result::Result<T, WebError>;

/// Why a Restate ingress call did not produce a usable answer.
///
/// Every detail string here is a literal owned by this crate. That is the
/// point of the type: an ingress response body has no slot to live in.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum IngressFailure {
    /// The ingress client could not be built from configuration. Operator
    /// facing — raised at startup, not per request.
    #[error("{0}")]
    Config(&'static str),

    /// No usable answer arrived, so whether the command took effect is
    /// unknown. Callers must offer recovery, never a plain retry.
    ///
    /// A rejected fire-and-forget send belongs here too: the ingress may have
    /// persisted the invocation before the response went wrong.
    #[error("{detail}; outcome unknown")]
    OutcomeUnknown {
        detail: &'static str,
        /// The status the ingress answered with, when it answered at all.
        status: Option<u16>,
    },

    /// The ingress answered with a status ghinvite does not act on.
    #[error("ingress returned HTTP {status}")]
    Rejected { status: u16 },
}

impl IngressFailure {
    /// Shorthand for the common unknown-outcome case where the ingress never
    /// answered at all.
    pub fn unreachable(detail: &'static str) -> Self {
        Self::OutcomeUnknown {
            detail,
            status: None,
        }
    }

    /// Treat this failure as leaving the command's effect in doubt.
    ///
    /// Routes that change state and cannot cheaply re-read the result use
    /// this: a refused request-response call proves only that *this* request
    /// failed, not that the handler never ran.
    pub fn into_outcome_unknown(self) -> Self {
        match self {
            Self::Rejected { status } => Self::OutcomeUnknown {
                detail: "ingress rejected the command",
                status: Some(status),
            },
            Self::Config(detail) => Self::OutcomeUnknown {
                detail,
                status: None,
            },
            unknown => unknown,
        }
    }
}

/// A GitHub OAuth error code, bounded to the documented shape.
///
/// GitHub's `error` parameter arrives on a redirect the browser controls, so
/// it is attacker-supplied text until it has been through here. The field is
/// private: [`OAuthErrorCode::new`] is the only way in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OAuthErrorCode(String);

impl OAuthErrorCode {
    pub fn new(raw: &str) -> Self {
        Self(ghinvite_github::bounded_upstream_code(raw))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for OAuthErrorCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Why a GitHub sign-in or setup return could not be completed.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum OAuthFailure {
    /// The GitHub user declined authorization at GitHub's consent screen.
    #[error("authorization declined")]
    Declined,

    /// GitHub reported some other authorization error.
    #[error("github reported {0}")]
    Provider(OAuthErrorCode),

    /// The callback did not match this browser's pending sign-in: no stored
    /// CSRF state, or a state that does not match.
    #[error("callback does not match a pending sign-in")]
    State,

    /// The installation the setup return named is not visible to the
    /// signed-in user.
    #[error("installation is not visible to the signed-in user")]
    InstallationNotVisible,

    /// GitHub described the installation with a value ghinvite cannot act on.
    /// `field` names which one, as a literal — the value itself is upstream
    /// text and stays in the (bounded) log at the call site.
    #[error("installation has an unsupported {field}")]
    UnsupportedInstallation { field: &'static str },
}

impl OAuthFailure {
    /// Classify the `error` parameter GitHub put on the callback URL.
    pub fn from_callback(error: &str) -> Self {
        match error {
            "access_denied" => Self::Declined,
            other => Self::Provider(OAuthErrorCode::new(other)),
        }
    }
}

#[derive(Debug, Error)]
pub enum WebError {
    #[error("operation conflict")]
    Conflict,
    #[error("storage error: {0}")]
    Storage(#[from] ghinvite_core::storage::Error),

    #[error("github api error: {0}")]
    Github(#[from] ghinvite_github::Error),

    #[error("session error: {0}")]
    Session(String),

    #[error("oauth error: {0}")]
    OAuth(OAuthFailure),

    #[error("restate ingress error: {0}")]
    Restate(IngressFailure),

    /// Resource not found / not authorized — surfaced as a generic 404 so we
    /// don't leak whether the resource exists. Used for invitation-link routes,
    /// account routes the user isn't an admin of, etc.
    #[error("not found")]
    NotFound,

    /// Caller is not authenticated. Surfaces as a 302 redirect to /login.
    #[error("unauthenticated")]
    Unauthenticated,

    /// Caller is authenticated but lacks permission for the resource.
    /// Per spec §10.3: surfaces as 404 to avoid leaking existence.
    #[error("forbidden")]
    Forbidden,

    /// Some input from the client was malformed (e.g. missing query param).
    #[error("bad request: {0}")]
    BadRequest(String),

    /// Catch-all for unexpected internal errors.
    #[error("internal error: {0}")]
    Internal(String),
}

impl WebError {
    /// A stable, payload-free label for logs and metrics.
    ///
    /// Log this — with [`WebError::upstream_status`] where there is one —
    /// rather than the error itself: a `tracing` field holding the whole error
    /// drags its diagnostic string into every sink that sees the event.
    pub fn kind(&self) -> &'static str {
        match self {
            WebError::Conflict => "conflict",
            WebError::Storage(_) => "storage",
            WebError::Github(_) => "github",
            WebError::Session(_) => "session",
            WebError::OAuth(_) => "oauth",
            WebError::Restate(_) => "ingress",
            WebError::NotFound => "not_found",
            WebError::Unauthenticated => "unauthenticated",
            WebError::Forbidden => "forbidden",
            WebError::BadRequest(_) => "bad_request",
            WebError::Internal(_) => "internal",
        }
    }

    /// The status an upstream answered with, where one exists. Safe to log:
    /// it is a number, not a body.
    pub fn upstream_status(&self) -> Option<u16> {
        match self {
            WebError::Github(error) => error.status(),
            WebError::Restate(IngressFailure::Rejected { status }) => Some(*status),
            WebError::Restate(IngressFailure::OutcomeUnknown { status, .. }) => *status,
            _ => None,
        }
    }
}

/// Copy for a rendered failure page: what happened, and where to go next.
///
/// Only `primary_href` is owned, because it is the one piece a route may
/// replace with a destination it knows is better than the generic fallback.
struct Problem {
    status: StatusCode,
    eyebrow: &'static str,
    heading: &'static str,
    message: &'static str,
    primary_href: String,
    primary_label: &'static str,
    secondary: Option<(&'static str, &'static str)>,
}

impl Problem {
    fn render(self) -> Response {
        let Problem {
            status,
            eyebrow,
            heading,
            message,
            primary_href,
            primary_label,
            secondary,
        } = self;
        let html = crate::views::render::render(move || {
            rsx! {
                crate::views::problem::ProblemPage {
                    // The session is not reachable from here, so the page
                    // renders signed-out chrome. Its links go to public routes
                    // that work either way.
                    signed_in_login: None::<String>,
                    eyebrow: eyebrow.to_string(),
                    heading: heading.to_string(),
                    message: message.to_string(),
                    primary_href: primary_href.clone(),
                    primary_label: primary_label.to_string(),
                    secondary_href: secondary.map(|(href, _)| href.to_string()),
                    secondary_label: secondary.map(|(_, label)| label.to_string()),
                }
            }
        });
        (status, Html(html)).into_response()
    }
}

fn ingress_problem(failure: &IngressFailure) -> Problem {
    let (heading, message) = match failure {
        // Whether the command took effect is genuinely unknown, so the copy
        // must not invite a blind retry.
        IngressFailure::OutcomeUnknown { .. } => (
            "This did not finish",
            "ghinvite could not confirm whether the request took effect. Open the page you came from to check before trying again.",
        ),
        IngressFailure::Config(_) | IngressFailure::Rejected { .. } => (
            "Service temporarily unavailable",
            "ghinvite could not reach the service that handles this request. Please try again in a moment.",
        ),
    };
    Problem {
        status: StatusCode::BAD_GATEWAY,
        eyebrow: "Service",
        heading,
        message,
        primary_href: "/".into(),
        primary_label: "Go home",
        secondary: None,
    }
}

fn oauth_problem(failure: &OAuthFailure) -> Problem {
    let (heading, message) = match failure {
        OAuthFailure::Declined => (
            "Sign-in was not completed",
            "GitHub did not authorize ghinvite. You can start the sign-in again whenever you are ready.",
        ),
        OAuthFailure::Provider(_) => (
            "Sign-in could not be completed",
            "GitHub could not complete this sign-in. Please start the sign-in again.",
        ),
        OAuthFailure::State => (
            "This sign-in link is no longer valid",
            "The link did not match a sign-in started in this browser. Please start the sign-in again.",
        ),
        OAuthFailure::InstallationNotVisible => (
            "That installation is not available",
            "This GitHub App installation is not visible to your GitHub account. Sign in as an account admin, then open it again.",
        ),
        OAuthFailure::UnsupportedInstallation { .. } => (
            "This installation is not supported",
            "ghinvite cannot manage this GitHub App installation. Check the installation's account and repository settings on GitHub, then open it again.",
        ),
    };
    Problem {
        status: StatusCode::BAD_REQUEST,
        eyebrow: "Sign-in",
        heading,
        message,
        primary_href: "/login".into(),
        primary_label: "Start sign-in again",
        secondary: Some(("/", "Go home")),
    }
}

fn github_problem(error: &ghinvite_github::Error) -> Problem {
    let (status, heading, message) = match error {
        // Every throttled response arrives here, a 429 and a rate-limited 403
        // alike: GitHub declined to answer, so the read is unavailable rather
        // than refused.
        ghinvite_github::Error::RateLimited { .. } => (
            StatusCode::SERVICE_UNAVAILABLE,
            "GitHub is limiting requests",
            "GitHub is limiting requests. Wait a moment, then try again.",
        ),
        ghinvite_github::Error::Status { status: 504, .. } => (
            StatusCode::GATEWAY_TIMEOUT,
            "GitHub did not respond in time",
            "GitHub did not respond in time. Please try again.",
        ),
        // Transport errors do not carry a typed timeout distinction;
        // DNS/TLS failures must not be reported as known timeouts.
        _ => (
            StatusCode::BAD_GATEWAY,
            "GitHub access could not be verified",
            "GitHub access could not be verified. Please try again.",
        ),
    };
    Problem {
        status,
        eyebrow: "GitHub",
        heading,
        message,
        primary_href: "/".into(),
        primary_label: "Go home",
        secondary: None,
    }
}

/// The failure page for errors that get one, or `None` for the variants with
/// their own response shape (404, the login redirect, a form's own message).
fn problem_for(error: &WebError) -> Option<Problem> {
    Some(match error {
        WebError::Conflict => Problem {
            status: StatusCode::CONFLICT,
            eyebrow: "Conflict",
            heading: "This attempt conflicts with another",
            message: "Recover the original attempt to see its result, or explicitly start a fresh attempt.",
            primary_href: "/".into(),
            primary_label: "Go home",
            secondary: None,
        },
        WebError::OAuth(failure) => oauth_problem(failure),
        WebError::Restate(failure) => ingress_problem(failure),
        WebError::Github(error) => github_problem(error),
        WebError::Session(_) => Problem {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            eyebrow: "Session",
            heading: "Session temporarily unavailable",
            message: "ghinvite could not read this browser's session. Please try again in a moment.",
            primary_href: "/".into(),
            primary_label: "Go home",
            secondary: None,
        },
        WebError::Storage(_) | WebError::Internal(_) => Problem {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            eyebrow: "Error",
            heading: "Something went wrong",
            message: "ghinvite could not complete this request. Please try again in a moment.",
            primary_href: "/".into(),
            primary_label: "Go home",
            secondary: None,
        },
        WebError::NotFound
        | WebError::Forbidden
        | WebError::Unauthenticated
        | WebError::BadRequest(_) => return None,
    })
}

impl WebError {
    /// Record the failure for whoever is on call, with fields that carry no
    /// payload. Called once per rendered response.
    fn log(&self) {
        match self {
            WebError::OAuth(failure) => {
                // The code is bounded by construction; the prose GitHub sent
                // alongside it never reached this value.
                tracing::warn!(oauth_failure = %failure, "oauth flow failed");
            }
            WebError::Restate(failure) => tracing::warn!(
                ingress_failure = %failure,
                upstream_status = ?self.upstream_status(),
                "restate ingress call failed"
            ),
            WebError::Github(error) => tracing::warn!(
                upstream_status = ?error.status(),
                kind = error.kind(),
                "GitHub read failed"
            ),
            WebError::Storage(_) | WebError::Internal(_) => {
                // Neither variant can reach a GitHub or ingress response body.
                // `Storage` does carry the storage driver's own message, which
                // is why this stays at error level rather than being widened
                // to anything a request can influence.
                tracing::error!(error = ?self, "internal error rendering response");
            }
            _ => {}
        }
    }

    /// Render this failure with a destination the route knows is better than
    /// the generic one — usually the page the visitor was already on.
    ///
    /// Errors that do not render a failure page (404, the login redirect, a
    /// form's own message) ignore the destination and respond as usual.
    pub fn into_response_with_recovery(
        self,
        href: impl Into<String>,
        label: &'static str,
    ) -> Response {
        match problem_for(&self) {
            Some(problem) => {
                self.log();
                Problem {
                    primary_href: href.into(),
                    primary_label: label,
                    ..problem
                }
                .render()
            }
            None => self.into_response(),
        }
    }
}

impl IntoResponse for WebError {
    fn into_response(self) -> Response {
        match problem_for(&self) {
            Some(problem) => {
                self.log();
                problem.render()
            }
            None => match self {
                WebError::Unauthenticated => {
                    // Redirect to /login. axum's redirect helper makes this clean.
                    axum::response::Redirect::to("/login").into_response()
                }
                WebError::BadRequest(msg) => (StatusCode::BAD_REQUEST, msg).into_response(),
                // NotFound and Forbidden alike: a generic 404 that does not say
                // whether the resource exists.
                _ => (StatusCode::NOT_FOUND, "Not Found".to_string()).into_response(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::BodyExt;

    async fn body_text(resp: Response) -> String {
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    #[tokio::test]
    async fn not_found_renders_404() {
        let resp = WebError::NotFound.into_response();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn forbidden_renders_404() {
        // Spec §10.3: forbidden → 404 to avoid information leak.
        let resp = WebError::Forbidden.into_response();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn unauthenticated_redirects_to_login() {
        let resp = WebError::Unauthenticated.into_response();
        assert_eq!(resp.status(), StatusCode::SEE_OTHER);
        let location = resp.headers().get("location").unwrap();
        assert_eq!(location.to_str().unwrap(), "/login");
    }

    #[tokio::test]
    async fn bad_request_passes_message() {
        let resp = WebError::BadRequest("missing 'code' param".into()).into_response();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let body = body_text(resp).await;
        assert!(body.contains("missing 'code' param"));
    }

    #[tokio::test]
    async fn restate_error_renders_502_with_a_way_out() {
        let resp = WebError::Restate(IngressFailure::Rejected { status: 500 }).into_response();
        assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);
        let body = body_text(resp).await;
        assert!(body.contains("href=\"/\""), "{body}");
    }

    /// The operational detail tells an outcome-unknown failure from a plain
    /// one, but only the classification reaches the visitor.
    #[tokio::test]
    async fn ingress_failures_keep_the_outcome_unknown_distinction() {
        let unknown = body_text(
            WebError::Restate(IngressFailure::unreachable("ingress send unavailable"))
                .into_response(),
        )
        .await;
        let rejected =
            body_text(WebError::Restate(IngressFailure::Rejected { status: 503 }).into_response())
                .await;
        assert!(unknown.contains("could not confirm whether"), "{unknown}");
        assert!(
            !rejected.contains("could not confirm whether"),
            "{rejected}"
        );
        assert!(!unknown.contains("ingress send unavailable"), "{unknown}");
        assert!(!rejected.contains("503"), "{rejected}");
    }

    #[tokio::test]
    async fn oauth_failures_render_recovery_navigation() {
        for failure in [
            OAuthFailure::Declined,
            OAuthFailure::Provider(OAuthErrorCode::new("bad_verification_code")),
            OAuthFailure::State,
            OAuthFailure::InstallationNotVisible,
        ] {
            let resp = WebError::OAuth(failure.clone()).into_response();
            assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "{failure:?}");
            let body = body_text(resp).await;
            assert!(body.contains("href=\"/login\""), "{failure:?}: {body}");
            assert!(body.contains("href=\"/\""), "{failure:?}: {body}");
        }
    }

    /// GitHub's `error` parameter rides in on a browser redirect, so it is
    /// attacker-supplied until `OAuthErrorCode` bounds it.
    #[test]
    fn oauth_error_code_drops_anything_that_is_not_a_documented_code() {
        assert_eq!(
            OAuthErrorCode::new("bad_verification_code").as_str(),
            "bad_verification_code"
        );
        assert_eq!(
            OAuthErrorCode::new("<script>alert(1)</script>").as_str(),
            "unrecognized_error"
        );
        assert_eq!(
            OAuthFailure::from_callback("access_denied"),
            OAuthFailure::Declined
        );
    }

    /// Routes that know where the visitor was send them back there instead of
    /// to the home page, without changing what the page is allowed to say.
    #[tokio::test]
    async fn recovery_destination_replaces_the_generic_link() {
        let resp = WebError::Restate(IngressFailure::unreachable("ingress unreachable"))
            .into_response_with_recovery("/i/abc123", "Back to the invitation link");
        assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);
        let body = body_text(resp).await;
        assert!(body.contains("href=\"/i/abc123\""), "{body}");
        assert!(body.contains("Back to the invitation link"), "{body}");
        assert!(body.contains("could not confirm whether"), "{body}");
        assert!(!body.contains("ingress unreachable"), "{body}");
    }

    /// A 404 stays a bare 404 whatever destination the caller offers: saying
    /// more would say whether the resource exists.
    #[tokio::test]
    async fn recovery_destination_is_ignored_where_there_is_no_failure_page() {
        let resp = WebError::NotFound.into_response_with_recovery("/console", "Back to console");
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        assert_eq!(body_text(resp).await, "Not Found");
    }

    #[test]
    fn kind_labels_every_variant_without_its_payload() {
        let cases: [(WebError, &str); 6] = [
            (WebError::Conflict, "conflict"),
            (WebError::Session("secret".into()), "session"),
            (WebError::OAuth(OAuthFailure::State), "oauth"),
            (
                WebError::Restate(IngressFailure::Rejected { status: 500 }),
                "ingress",
            ),
            (WebError::BadRequest("secret".into()), "bad_request"),
            (WebError::Internal("secret".into()), "internal"),
        ];
        for (error, expected) in cases {
            assert_eq!(error.kind(), expected);
        }
    }

    #[test]
    fn upstream_status_exposes_the_number_only() {
        assert_eq!(
            WebError::Restate(IngressFailure::Rejected { status: 503 }).upstream_status(),
            Some(503)
        );
        assert_eq!(
            WebError::Github(ghinvite_github::Error::Status {
                status: 429,
                body: "secret".into(),
            })
            .upstream_status(),
            Some(429)
        );
        assert_eq!(WebError::Conflict.upstream_status(), None);
    }

    #[tokio::test]
    async fn throttled_github_reads_are_unavailable_and_denied_ones_are_not() {
        // The status alone no longer decides: a 403 that carried a limit is as
        // unavailable as a 429, and one that carried none is still a refusal.
        for (headers, message, expected) in [
            (
                vec![("retry-after", "60")],
                "You have exceeded a secondary rate limit",
                StatusCode::SERVICE_UNAVAILABLE,
            ),
            (
                vec![],
                "Resource not accessible by integration",
                StatusCode::BAD_GATEWAY,
            ),
        ] {
            let error = ghinvite_github::Response {
                status: 403,
                headers: headers
                    .into_iter()
                    .map(|(k, v)| (k.to_owned(), v.to_owned()))
                    .collect(),
                body: serde_json::json!({ "message": message }).to_string().into(),
            }
            .status_error();
            let resp = WebError::Github(error).into_response();
            assert_eq!(resp.status(), expected, "for {message:?}");
            // Neither discloses the upstream diagnostic.
            assert!(!body_text(resp).await.contains(message));
        }
    }
}
