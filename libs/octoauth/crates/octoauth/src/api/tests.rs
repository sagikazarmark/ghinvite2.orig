use super::*;
use RateLimitScope::{Primary, Secondary};
use serde_json::json;

const DATE: &str = "Tue, 01 Jan 2030 00:00:00 GMT";
/// 1800 s after [`DATE`].
const RESET: &str = "1893457800";
const OUTSIDE: &str = "Inviting outside collaborators is restricted by organization policy.";
/// Not GitHub's refusal templates; `{}` stands for the outside-collaborator one.
const NEAR_MISSES: [&str; 7] = [
    "Outside collaborators may be restricted by organization policy.",
    "Outside collaborators are allowed in this organization.",
    "Outside collaborators are not restricted by organization policy.",
    "Resource not accessible by integration",
    "IP allow list enabled",
    "Example: {}",
    "{} Or another problem.",
];
const IP: &str = "Although you appear to have the correct authorization credentials, the `policy-secret-marker` organization has an IP allow list enabled, and your IP address is not permitted to access this resource.";

fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
    pairs
        .iter()
        .map(|(name, value)| {
            (
                http::HeaderName::from_bytes(name.as_bytes()).unwrap(),
                http::HeaderValue::from_str(value).unwrap(),
            )
        })
        .collect()
}

fn status(code: u16) -> StatusCode {
    StatusCode::from_u16(code).unwrap()
}

/// Classify raw bytes.
fn bytes(code: u16, pairs: &[(&str, &str)], body: &[u8]) -> ApiError {
    ApiError::classify(status(code), &headers(pairs), ErrorBody::Bytes(body))
}

/// Classify a JSON body both as raw bytes and as parsed fields, which must
/// agree.
fn classify(code: u16, pairs: &[(&str, &str)], body: Value) -> ApiError {
    let raw = serde_json::to_vec(&body).unwrap();
    let from_bytes = bytes(code, pairs, &raw);
    let errors = body
        .get("errors")
        .and_then(Value::as_array)
        .map_or(&[][..], Vec::as_slice);
    let from_parsed = ApiError::classify(
        status(code),
        &headers(pairs),
        ErrorBody::Parsed {
            message: body.get("message").and_then(Value::as_str),
            errors,
        },
    );
    assert_eq!(from_bytes, from_parsed, "Bytes and Parsed disagree");
    from_bytes
}

fn message(text: &str) -> Value {
    json!({ "message": text })
}

fn limited(scope: RateLimitScope, seconds: Option<u64>) -> ApiError {
    ApiError::RateLimited {
        scope,
        retry_after: seconds.map(Duration::from_secs),
    }
}

#[test]
fn statuses() {
    assert_eq!(
        classify(401, &[], message("Bad credentials")),
        ApiError::Unauthorized
    );
    assert_eq!(classify(404, &[], message("Not Found")), ApiError::NotFound);
    assert_eq!(classify(409, &[], message("Conflict")), ApiError::Conflict);
    assert_eq!(
        classify(422, &[], message("Validation Failed")),
        ApiError::Validation { codes: vec![] }
    );
    assert_eq!(
        classify(503, &[], json!({})),
        ApiError::Server { status: 503 }
    );
    assert_eq!(
        classify(302, &[], json!({})),
        ApiError::Status { status: 302 }
    );
    assert_eq!(
        classify(410, &[], json!({})),
        ApiError::Status { status: 410 }
    );
    assert_eq!(
        ApiError::classify(status(403), &HeaderMap::new(), ErrorBody::Empty),
        ApiError::Forbidden {
            reason: ForbiddenReason::Other
        }
    );
    // A non-JSON body names nothing.
    assert_eq!(
        bytes(403, &[], b"<html>Forbidden</html>"),
        ApiError::Forbidden {
            reason: ForbiddenReason::Other
        }
    );
}

#[test]
fn not_a_rate_limit() {
    // Throttling headers on a status that is not 403/429 mean nothing.
    assert_eq!(
        classify(422, &[("retry-after", "5")], json!({})),
        ApiError::Validation { codes: vec![] }
    );
    // A refusal with quota to spare.
    assert_eq!(
        classify(
            403,
            &[
                ("x-ratelimit-remaining", "4998"),
                ("x-ratelimit-reset", RESET),
                ("date", DATE)
            ],
            message("Resource not accessible by integration"),
        ),
        ApiError::Forbidden {
            reason: ForbiddenReason::Other
        }
    );
    // A refusal served as the hour's last request still names its reason.
    assert_eq!(
        classify(
            403,
            &[("x-ratelimit-remaining", "0"), ("x-ratelimit-reset", RESET)],
            message("Resource not accessible by integration"),
        ),
        ApiError::Forbidden {
            reason: ForbiddenReason::Other
        }
    );
}

#[test]
fn secondary_limits() {
    assert_eq!(classify(429, &[], json!({})), limited(Secondary, None));
    assert_eq!(
        classify(
            403,
            &[("retry-after", "60"), ("x-ratelimit-remaining", "4998")],
            message("You have exceeded a secondary rate limit"),
        ),
        limited(Secondary, Some(60))
    );
    // The wording alone is evidence.
    assert_eq!(
        classify(
            403,
            &[],
            message("You have exceeded a secondary rate limit")
        ),
        limited(Secondary, None)
    );
    assert_eq!(
        classify(
            403,
            &[],
            message("You have triggered an abuse detection mechanism")
        ),
        limited(Secondary, None)
    );
    // `retry-after` as an HTTP date, measured against GitHub's `date`.
    assert_eq!(
        classify(
            403,
            &[
                ("retry-after", "Tue, 01 Jan 2030 00:02:00 GMT"),
                ("date", DATE)
            ],
            json!({}),
        ),
        limited(Secondary, Some(120))
    );
    // A date already past, or one with no `date` to measure against, is no
    // wait, but still cites a limit.
    assert_eq!(
        classify(
            403,
            &[
                ("retry-after", "Mon, 31 Dec 2029 23:58:00 GMT"),
                ("date", DATE)
            ],
            message("Something went wrong"),
        ),
        limited(Secondary, None)
    );
    assert_eq!(
        classify(
            403,
            &[("retry-after", "Tue, 01 Jan 2030 00:02:00 GMT")],
            message("Something went wrong"),
        ),
        limited(Secondary, None)
    );
    // A healthy quota's reset is never a secondary limit's wait.
    assert_eq!(
        classify(
            403,
            &[
                ("x-ratelimit-remaining", "4998"),
                ("x-ratelimit-reset", RESET),
                ("date", DATE)
            ],
            message("You have exceeded a secondary rate limit"),
        ),
        limited(Secondary, None)
    );
    // Unreadable headers fall back to the other evidence.
    assert_eq!(
        classify(
            403,
            &[
                ("retry-after", "in a little while"),
                ("x-ratelimit-remaining", "plenty"),
                ("x-ratelimit-reset", "soon"),
                ("date", "whenever")
            ],
            message("API rate limit exceeded for installation ID 9."),
        ),
        limited(Secondary, None)
    );
    assert_eq!(
        classify(429, &[("retry-after", "?")], message("Too Many Requests")),
        limited(Secondary, None)
    );
}

#[test]
fn primary_limits() {
    assert_eq!(
        classify(
            403,
            &[
                ("x-ratelimit-remaining", "0"),
                ("x-ratelimit-reset", RESET),
                ("date", DATE)
            ],
            message("API rate limit exceeded for installation ID 9."),
        ),
        limited(Primary, Some(1800))
    );
    // A reset already past by GitHub's clock is no wait.
    assert_eq!(
        classify(
            403,
            &[
                ("x-ratelimit-remaining", "0"),
                ("x-ratelimit-reset", "1893454200"),
                ("date", DATE)
            ],
            message("API rate limit exceeded"),
        ),
        limited(Primary, None)
    );
    // Without a body naming a reason, the headers stand.
    assert_eq!(
        classify(403, &[("x-ratelimit-remaining", "0")], json!({})),
        limited(Primary, None)
    );
    assert_eq!(
        bytes(
            403,
            &[("x-ratelimit-remaining", "0")],
            b"<html>upstream error</html>"
        ),
        limited(Primary, None)
    );
    // A cited secondary limit stays secondary on an exhausted quota; the
    // reset is still when the request can next succeed.
    assert_eq!(
        classify(
            429,
            &[
                ("x-ratelimit-remaining", "0"),
                ("x-ratelimit-reset", RESET),
                ("date", DATE)
            ],
            message("Too Many Requests"),
        ),
        limited(Secondary, Some(1800))
    );
    // `retry-after` names the wait even on an exhausted quota.
    assert_eq!(
        classify(
            429,
            &[
                ("retry-after", "30"),
                ("x-ratelimit-remaining", "0"),
                ("x-ratelimit-reset", RESET),
                ("date", DATE)
            ],
            message("You have exceeded a secondary rate limit"),
        ),
        limited(Secondary, Some(30))
    );
}

#[test]
fn rate_limits_take_precedence_over_refusals() {
    assert!(matches!(
        classify(403, &[("retry-after", "30")], message(OUTSIDE)),
        ApiError::RateLimited { .. }
    ));
    assert!(matches!(
        classify(
            403,
            &[("retry-after", "30")],
            json!({"errors": [{"code": "spammed"}]})
        ),
        ApiError::RateLimited { .. }
    ));
}

#[test]
fn forbidden_reasons() {
    let forbidden = |reason| ApiError::Forbidden { reason };
    assert_eq!(
        classify(
            403,
            &[("x-github-sso", "required; url=https://github.com/x")],
            message(OUTSIDE)
        ),
        forbidden(ForbiddenReason::SsoRequired)
    );
    assert_eq!(
        classify(403, &[], message(OUTSIDE)),
        forbidden(ForbiddenReason::OutsideCollaboratorsRestricted)
    );
    assert_eq!(
        classify(
            403,
            &[],
            json!({"errors": [{"code": "custom", "message": OUTSIDE}]})
        ),
        forbidden(ForbiddenReason::OutsideCollaboratorsRestricted)
    );
    assert_eq!(
        classify(403, &[], message(IP)),
        forbidden(ForbiddenReason::IpAllowList)
    );
    assert_eq!(
        classify(403, &[], message(&IP.replace("not permitted", "permitted"))),
        forbidden(ForbiddenReason::Other)
    );
    for near_miss in NEAR_MISSES {
        assert_eq!(
            classify(403, &[], message(&near_miss.replace("{}", OUTSIDE))),
            forbidden(ForbiddenReason::Other),
            "{near_miss}"
        );
    }
    // Only 32 `errors` entries are inspected.
    let mut errors = vec![json!({"message": "other"}); 32];
    errors.push(json!({"message": OUTSIDE}));
    assert_eq!(
        classify(403, &[], json!({"errors": errors})),
        forbidden(ForbiddenReason::Other)
    );
    // A body over 64 KiB is not inspected.
    assert_eq!(
        bytes(
            403,
            &[],
            &serde_json::to_vec(&json!({"message": OUTSIDE, "padding": "x".repeat(65536)}))
                .unwrap()
        ),
        forbidden(ForbiddenReason::Other)
    );
}

#[test]
fn ip_allow_list_text() {
    let text = IP.replace("policy-secret-marker", "acme-1");
    assert!(ip_allow_list_denial(&text));
    assert!(!ip_allow_list_denial(&text.replace("acme-1", "")));
    assert!(!ip_allow_list_denial(&text.replace("acme-1", "acme 1")));
    assert!(!ip_allow_list_denial("IP allow list"));
}

#[test]
fn spam_needs_githubs_explicit_word() {
    for code in [403, 422] {
        assert_eq!(
            classify(
                code,
                &[],
                json!({"errors": [{"code": "spammed", "message": "secret-marker"}]})
            ),
            ApiError::Spammed
        );
        assert_eq!(classify(code, &[], message("Spammed")), ApiError::Spammed);
    }
    for text in [
        "Validation failed",
        "Validation failed, or the endpoint has been spammed.",
        "not spammed",
    ] {
        assert!(matches!(
            classify(422, &[], message(text)),
            ApiError::Validation { .. }
        ));
    }
}

#[test]
fn validation_codes() {
    let codes = |body| match classify(422, &[], body) {
        ApiError::Validation { codes } => codes,
        other => panic!("expected Validation, got {other:?}"),
    };
    use ValidationCode::*;
    assert_eq!(
        codes(json!({"message": "Validation Failed", "errors": [
            {"code": "already_exists", "field": "login"},
            {"code": "invalid"},
            {"code": "already_exists"},
            {"code": "secret-marker"},
            {"code": "other-unknown"},
            {"code": "missing"}, {"code": "missing_field"}, {"code": "unprocessable"}, {"code": "custom"}
        ]})),
        vec![
            AlreadyExists,
            Invalid,
            Missing,
            MissingField,
            Unprocessable,
            Custom,
            Unrecognized
        ]
    );
    assert_eq!(
        codes(
            json!({"errors": [{"code": "custom", "message": "Cannot assign secret-marker permission of read"}]})
        ),
        vec![CannotAssignBelowBase, Custom]
    );
    for text in [
        "Cannot assign permission",
        "Cannot assign  permission of read",
        "Cannot assign member permission of ",
    ] {
        assert_eq!(codes(message(text)), vec![], "{text}");
    }
    assert_eq!(
        codes(message(OUTSIDE)),
        vec![OutsideCollaboratorsRestricted]
    );
    for near_miss in NEAR_MISSES {
        let text = near_miss.replace("{}", OUTSIDE);
        assert_eq!(codes(message(&text)), vec![], "{text}");
    }
    assert_eq!(codes(message(&format!("{OUTSIDE} secret-marker"))), vec![]);
}

#[test]
fn no_upstream_text_survives() {
    let marker = "secret-marker";
    let cases = [
        classify(
            422,
            &[],
            json!({"message": marker, "errors": [{"code": "invalid", "message": marker}, {"code": marker}]}),
        ),
        classify(403, &[], json!({"message": format!("{OUTSIDE} {marker}")})),
        classify(403, &[], message(IP)),
        classify(404, &[], message(marker)),
        classify(500, &[], message(marker)),
        classify(403, &[("retry-after", "1")], message(marker)),
        classify(
            422,
            &[],
            json!({"errors": [{"code": "custom", "message": format!("Cannot assign {marker} permission of read")}]}),
        ),
    ];
    for error in cases {
        let rendered = format!("{error} {error:?}");
        assert!(!rendered.contains(marker), "{rendered}");
        assert!(!rendered.contains("policy-secret-marker"), "{rendered}");
    }
    let body = ErrorBody::Parsed {
        message: Some(marker),
        errors: &[json!({"message": marker})],
    };
    assert_eq!(
        format!("{body:?}"),
        "Parsed { message_len: Some(13), errors: 1 }"
    );
    assert_eq!(
        format!("{:?}", ErrorBody::Bytes(marker.as_bytes())),
        "Bytes(13 bytes)"
    );
}

/// A gateway in front of GitHub that reflects the request it proxied, or a
/// body quoting a token: classification still works, and nothing of the
/// credential survives.
#[test]
fn reflected_credentials_never_survive() {
    let jwt = "eyJhbGciOiJSUzI1NiJ9.eyJpc3MiOjd9.c2lnbmF0dXJl";
    let token = format!("ghs_{}", "A".repeat(36));
    let html = format!("<html>502 Bad Gateway<pre>Authorization: Bearer {jwt}</pre></html>");
    let cases = [
        (
            bytes(502, &[], html.as_bytes()),
            ApiError::Server { status: 502 },
        ),
        (
            classify(
                429,
                &[],
                json!({"message": "You have exceeded a secondary rate limit", "seen": token}),
            ),
            limited(Secondary, None),
        ),
        (
            classify(403, &[], message(&format!("Denied for Bearer {jwt}"))),
            ApiError::Forbidden {
                reason: ForbiddenReason::Other,
            },
        ),
        (
            classify(
                422,
                &[],
                json!({"message": token, "errors": [{"code": token, "message": jwt}, {"code": "already_exists"}]}),
            ),
            ApiError::Validation {
                codes: vec![ValidationCode::AlreadyExists, ValidationCode::Unrecognized],
            },
        ),
        (classify(401, &[], message(&token)), ApiError::Unauthorized),
    ];
    for (error, expected) in cases {
        let rendered = format!("{error} {error:?}");
        for secret in [jwt, token.as_str(), "Bearer", "rate limit"] {
            assert!(!rendered.contains(secret), "leaked {secret}: {rendered}");
        }
        assert_eq!(error, expected);
    }
}

#[test]
fn codes_and_guidance() {
    let all = [
        ApiError::Unauthorized,
        ApiError::Forbidden {
            reason: ForbiddenReason::Other,
        },
        ApiError::NotFound,
        ApiError::Conflict,
        ApiError::Validation { codes: vec![] },
        ApiError::Spammed,
        limited(Primary, None),
        limited(Secondary, Some(3)),
    ];
    for error in all {
        assert_eq!(error.to_string(), error.code());
        assert!(error.code().starts_with("gh:api:"));
    }
    assert_eq!(
        ApiError::Server { status: 502 }.to_string(),
        "gh:api:server:502"
    );
    assert_eq!(
        ApiError::Status { status: 302 }.to_string(),
        "gh:api:status:302"
    );

    assert!(ApiError::Unauthorized.is_unauthorized());
    assert!(!ApiError::NotFound.is_unauthorized());
    assert!(ApiError::Server { status: 503 }.is_transient());
    assert!(limited(Primary, None).is_transient());
    assert!(!ApiError::Conflict.is_transient());
    assert_eq!(
        limited(Secondary, None).suggested_wait(),
        Some(Duration::from_secs(60))
    );
    assert_eq!(
        limited(Secondary, Some(5)).suggested_wait(),
        Some(Duration::from_secs(5))
    );
    assert_eq!(ApiError::NotFound.suggested_wait(), None);
}
