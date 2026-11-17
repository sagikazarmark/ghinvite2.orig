//! Content-Security-Policy for every HTML response.
//!
//! One policy covers the home page, the Console, the public invitation
//! request flow, and HTML error pages: none of them ship inline scripts or
//! styles, so there is nothing zone-specific left to relax. Client behaviour
//! lives in `/static/app.js` (see `assets/app.js`) or, later, a Dioxus island
//! (ADR 0001) — both are same-origin and covered by `script-src 'self'`.
//!
//! The header is only attached to `text/html` responses. Static assets, the
//! JSON webhook receiver, plain-text errors, and redirects are left alone.

use axum::http::{HeaderValue, Response, header};
use tower_http::set_header::SetResponseHeaderLayer;

/// The enforced policy, directive by directive:
///
/// - `default-src 'self'` — everything not listed below (fonts, connect,
///   media, workers, …) is same-origin only. The stylesheet uses system font
///   stacks and nothing fetches from third parties.
/// - `script-src 'self' 'wasm-unsafe-eval'` — only `/static/app.js` (and a
///   future island bundle) may run; `'wasm-unsafe-eval'` lets `dioxus-web`
///   compile its `.wasm` module without opening up `eval`. Inline `<script>`
///   blocks are blocked.
/// - `style-src 'self'` — the built Tailwind/DaisyUI stylesheet only. No
///   `style=""` attributes are rendered anywhere, so `'unsafe-inline'` is not
///   needed.
/// - `img-src 'self' data:` — DaisyUI v5 components (`.btn`, `.badge`,
///   `.alert`, `.toggle`, `.loading`) reference `data:image/svg+xml` URIs from
///   the stylesheet (`--fx-noise`, spinner masks). Without `data:` every page
///   would log a violation. No avatars are rendered yet; add
///   `https://avatars.githubusercontent.com` here when they are.
/// - `object-src 'none'` — no plugins.
/// - `base-uri 'none'` — no `<base>` rewriting of relative URLs.
/// - `frame-ancestors 'none'` — the app is never embedded.
/// - `form-action 'self' https://github.com` — every form posts to this
///   origin, but a POST from an expired session is answered with
///   `303 → /login → 303 → https://github.com/login/oauth/authorize`. Browsers
///   that enforce `form-action` across redirects (Chromium) need the final
///   hop listed or the sign-in bounce is silently blocked. The listed origin
///   is GitHub's web origin as configured ([`policy`]): `https://github.com`
///   in production, a stand-in's (`GITHUB_WEB_BASE`) outside it.
pub const CONTENT_SECURITY_POLICY: &str = "default-src 'self'; \
     script-src 'self' 'wasm-unsafe-eval'; \
     style-src 'self'; \
     img-src 'self' data:; \
     object-src 'none'; \
     base-uri 'none'; \
     frame-ancestors 'none'; \
     form-action 'self' https://github.com";

/// [`CONTENT_SECURITY_POLICY`] for a deployment whose GitHub web origin is
/// `github`'s web base: the sign-in bounce's final hop is that origin. With
/// GitHub's own bases (always, in production) this is exactly
/// [`CONTENT_SECURITY_POLICY`]; a non-production `GITHUB_WEB_BASE` (such as
/// the `octoemu` emulator) replaces `https://github.com` with its origin.
pub fn policy(github: &ghinvite_config::github::GithubBases) -> HeaderValue {
    // The base is validated at configuration (https, or http on loopback;
    // no credentials, query or fragment), so its origin is a plain
    // `scheme://host[:port]`: nothing in it can end the directive.
    let origin = url::Url::parse(github.web())
        .map(|url| url.origin().ascii_serialization())
        .ok()
        .filter(|origin| origin != "null");
    match origin {
        Some(origin) if origin != "https://github.com" => HeaderValue::from_str(&format!(
            "{} {origin}",
            CONTENT_SECURITY_POLICY
                .strip_suffix(" https://github.com")
                .expect("the policy ends with GitHub's origin")
        ))
        .unwrap_or_else(|_| HeaderValue::from_static(CONTENT_SECURITY_POLICY)),
        _ => HeaderValue::from_static(CONTENT_SECURITY_POLICY),
    }
}

/// Tower layer that sets `policy` ([`policy`]) on every `text/html` response
/// that does not already carry a policy. Mounted once in
/// [`crate::build_app`] so it covers every route and the HTML fallback.
pub fn layer<B>(
    policy: HeaderValue,
) -> SetResponseHeaderLayer<impl Fn(&Response<B>) -> Option<HeaderValue> + Clone> {
    SetResponseHeaderLayer::if_not_present(
        header::CONTENT_SECURITY_POLICY,
        move |response: &Response<B>| is_html(response).then(|| policy.clone()),
    )
}

fn is_html<B>(response: &Response<B>) -> bool {
    response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.trim_start().starts_with("text/html"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_meets_adr_minimum() {
        for directive in [
            "default-src 'self'",
            "script-src 'self' 'wasm-unsafe-eval'",
            "form-action 'self'",
            "base-uri 'none'",
            "frame-ancestors 'none'",
        ] {
            assert!(
                CONTENT_SECURITY_POLICY.contains(directive),
                "missing {directive}"
            );
        }
        assert!(!CONTENT_SECURITY_POLICY.contains("'unsafe-inline'"));
        assert!(!CONTENT_SECURITY_POLICY.contains("'unsafe-eval'"));
        HeaderValue::from_static(CONTENT_SECURITY_POLICY);
    }

    fn bases(web: Option<&str>) -> ghinvite_config::github::GithubBases {
        ghinvite_config::github::GithubBases::from_config(
            ghinvite_config::github::Deployment::Development,
            |field| match field {
                "GITHUB_WEB_BASE" => web.map(str::to_owned),
                _ => None,
            },
        )
        .unwrap()
    }

    #[test]
    fn github_bases_keep_the_production_policy() {
        assert_eq!(policy(&bases(None)), CONTENT_SECURITY_POLICY);
        assert_eq!(
            policy(&ghinvite_config::github::GithubBases::github()),
            CONTENT_SECURITY_POLICY
        );
    }

    #[test]
    fn form_action_names_the_configured_web_origin() {
        for (web, origin) in [
            ("http://127.0.0.1:41234", "http://127.0.0.1:41234"),
            ("http://127.0.0.1:41234/", "http://127.0.0.1:41234"),
            ("https://ghe.example.test/", "https://ghe.example.test"),
            ("https://github.com:443/", "https://github.com"),
        ] {
            let value = policy(&bases(Some(web)));
            let value = value.to_str().unwrap();
            assert!(
                value.ends_with(&format!("; form-action 'self' {origin}")),
                "{web}: {value}"
            );
            assert_eq!(value.matches("form-action").count(), 1);
            assert!(!value.contains("https://github.com") || origin == "https://github.com");
            assert_eq!(
                value.strip_suffix(origin),
                CONTENT_SECURITY_POLICY.strip_suffix("https://github.com"),
                "only the form-action origin changes"
            );
        }
    }

    #[test]
    fn only_html_responses_are_recognised() {
        fn with_content_type(value: Option<&'static str>) -> Response<()> {
            let mut response = Response::new(());
            if let Some(value) = value {
                response
                    .headers_mut()
                    .insert(header::CONTENT_TYPE, HeaderValue::from_static(value));
            }
            response
        }

        assert!(is_html(&with_content_type(Some(
            "text/html; charset=utf-8"
        ))));
        assert!(is_html(&with_content_type(Some("text/html"))));
        assert!(!is_html(&with_content_type(Some("text/css"))));
        assert!(!is_html(&with_content_type(Some(
            "text/javascript; charset=utf-8"
        ))));
        assert!(!is_html(&with_content_type(Some("application/json"))));
        assert!(!is_html(&with_content_type(None)));
    }
}
