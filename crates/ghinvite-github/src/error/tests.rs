//! gh-toolkit's classification tests, unchanged in their expectations. The
//! rate-limit cases were `Response::rate_limit` tests; they now run through
//! [`ApiError::from_response`], which takes the rules from octoauth.
use super::*;
use serde_json::json;

fn classify(status: u16, body: serde_json::Value) -> ApiError {
    ApiError::from_response(&Response {
        status,
        headers: Default::default(),
        body: serde_json::to_vec(&body).unwrap(),
    })
}

/// A response with the given headers and `{"message": ...}` body, the shape
/// GitHub returns for every refusal.
fn refusal(status: u16, headers: &[(&str, &str)], message: &str) -> Response {
    Response {
        status,
        headers: headers
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect(),
        body: json!({ "message": message }).to_string().into(),
    }
}

fn limit(response: &Response) -> Option<(RateLimitScope, Option<Duration>)> {
    match ApiError::from_response(response) {
        ApiError::RateLimited {
            scope, retry_after, ..
        } => Some((scope, retry_after)),
        _ => None,
    }
}

#[test]
fn policy_denials_are_bounded_positive_and_secret_free() {
    let outside = "Inviting outside collaborators is restricted by organization policy.";
    for status in [403, 422] {
        let result = classify(
            status,
            json!({"message":outside,"private":"POLICY_SECRET_MARKER"}),
        );
        if status == 403 {
            assert_eq!(
                result,
                ApiError::Forbidden {
                    reason: ForbiddenReason::OutsideCollaboratorsRestricted
                }
            );
        } else {
            assert!(
                matches!(&result, ApiError::Validation { codes, .. } if codes.contains(&ValidationCode::OutsideCollaboratorsRestricted))
            );
        }
        assert!(!format!("{result:?} {result}").contains("POLICY_SECRET_MARKER"));
        assert_eq!(
            classify(
                status,
                json!({"message":outside,"errors":[{"code":"spammed"}]})
            ),
            ApiError::Spammed
        );
        for near_miss in [
            "Outside collaborators may be restricted by organization policy.",
            "Outside collaborators are allowed in this organization.",
            "Outside collaborators are not restricted by organization policy.",
            "Resource not accessible by integration",
            "IP allow list enabled",
        ] {
            let result = classify(status, json!({"message":near_miss}));
            assert!(matches!(
                result,
                ApiError::Forbidden {
                    reason: ForbiddenReason::Other
                } | ApiError::Validation { .. }
            ));
            if let ApiError::Validation { codes, .. } = result {
                assert!(codes.is_empty());
            }
        }
        for message in [
            format!("Example: {outside}"),
            format!("{outside} Or another problem."),
            format!("{outside} POLICY_SECRET_MARKER"),
        ] {
            let result = classify(status, json!({"message":message}));
            assert!(!format!("{result:?} {result}").contains("POLICY_SECRET_MARKER"));
            match result {
                ApiError::Forbidden {
                    reason: ForbiddenReason::Other,
                } => {}
                ApiError::Validation { codes, .. } => assert!(codes.is_empty()),
                other => panic!("unexpected classification: {other:?}"),
            }
        }
    }
    let ip = "Although you appear to have the correct authorization credentials, the `policy-secret-marker` organization has an IP allow list enabled, and your IP address is not permitted to access this resource.";
    let result = classify(403, json!({"message":ip}));
    assert_eq!(
        result,
        ApiError::Forbidden {
            reason: ForbiddenReason::IpAllowList
        }
    );
    assert!(!format!("{result:?} {result}").contains("policy-secret-marker"));
    assert_eq!(
        classify(
            403,
            json!({"message":ip.replace("not permitted", "permitted")})
        ),
        ApiError::Forbidden {
            reason: ForbiddenReason::Other
        }
    );
    let mut response = Response {
        status: 403,
        headers: Default::default(),
        body: serde_json::to_vec(&json!({"message":outside})).unwrap(),
    };
    response.headers.insert("retry-after".into(), "30".into());
    assert!(matches!(
        ApiError::from_response(&response),
        ApiError::RateLimited { .. }
    ));
    assert_eq!(
        classify(403, json!({"message":outside,"padding":"x".repeat(65536)})),
        ApiError::Forbidden {
            reason: ForbiddenReason::Other
        }
    );
    let mut errors = vec![json!({"message":"other"}); 32];
    errors.push(json!({"message":outside}));
    assert_eq!(
        classify(403, json!({"errors":errors})),
        ApiError::Forbidden {
            reason: ForbiddenReason::Other
        }
    );
    assert_eq!(
        classify(403, json!({"errors":[{"code":"custom","message":outside}]})),
        ApiError::Forbidden {
            reason: ForbiddenReason::OutsideCollaboratorsRestricted
        }
    );
}

#[test]
fn sso_header_names_the_refusal() {
    let mut response = refusal(
        403,
        &[],
        "Resource protected by organization SAML enforcement",
    );
    response.headers.insert(
        "x-github-sso".into(),
        "required; url=https://private.test".into(),
    );
    let error = ApiError::from_response(&response);
    assert_eq!(
        error,
        ApiError::Forbidden {
            reason: ForbiddenReason::SsoRequired
        }
    );
    assert!(!format!("{error:?} {error}").contains("private.test"));
}

#[test]
fn retryability_respects_ambiguity_and_rate_limits() {
    let cases = [
        (ApiError::Server { status: 503 }, true, true),
        (ApiError::NotSent, true, true),
        (ApiError::Ambiguous, false, true),
        (ApiError::Unauthorized, false, false),
        (ApiError::NotFound, false, false),
        (ApiError::Moved { status: 302 }, false, false),
        (
            ApiError::RateLimited {
                scope: RateLimitScope::Secondary,
                retry_after: None,
                resource: None,
            },
            true,
            true,
        ),
    ];
    for (error, unsafe_retry, safe_retry) in cases {
        assert_eq!(error.is_transient_for_retry_safe(false), unsafe_retry);
        assert_eq!(error.is_transient_for_retry_safe(true), safe_retry);
        assert!(error.code().starts_with("gh:api:"));
    }
    assert_eq!(
        ApiError::from(TransportError::SentNoResponse { timed_out: false }),
        ApiError::Ambiguous
    );
    assert_eq!(ApiError::from(TransportError::NotSent), ApiError::NotSent);
    assert_eq!(
        ApiError::from(TransportError::Oversize),
        ApiError::Decode { what: "oversize" }
    );
}

#[test]
fn typed_errors_retain_codes_lengths_and_clone_no_upstream_text() {
    let secret = "upstream-secret-marker";
    let response = Response {
        status: 422,
        headers: Default::default(),
        body: serde_json::to_vec(
            &json!({"message":secret,"errors":[{"code":"invalid","message":secret},{"code":secret}]}),
        )
        .unwrap(),
    };
    let error = ApiError::from_response(&response);
    assert_eq!(
        error,
        ApiError::Validation {
            codes: vec![ValidationCode::Invalid, ValidationCode::Other],
            message_len: secret.len()
        }
    );
    assert!(!format!("{error} {error:?}").contains(secret));
    let call: CallError<u8> = CallError::Api(error);
    let cloned = call.clone();
    assert_eq!(cloned.code(), "gh:api:validation");
    assert_eq!(cloned.to_string(), "gh:api:validation");
    assert_eq!(CallError::<u8>::App(7).to_string(), "gh:call:app");
}

#[test]
fn invitation_refusal_categories_discard_text_and_require_positive_evidence() {
    let classify = |body| classify(422, body);
    assert_eq!(
        classify(json!({"errors":[{"code":"spammed","message":"secret-marker"}]})),
        ApiError::Spammed
    );
    assert_eq!(classify(json!({"message":"Spammed"})), ApiError::Spammed);
    for message in [
        "Validation failed",
        "Validation failed, or the endpoint has been spammed.",
        "not spammed",
    ] {
        assert!(matches!(
            classify(json!({"message":message})),
            ApiError::Validation { .. }
        ));
    }
    let result = classify(
        json!({"errors":[{"code":"custom","message":"Cannot assign secret-marker permission of read"}]}),
    );
    assert!(
        matches!(&result,ApiError::Validation{codes,..} if codes.contains(&ValidationCode::CannotAssignBelowBase))
    );
    assert!(!format!("{result:?} {result}").contains("secret-marker"));
    for message in [
        "Cannot assign permission",
        "Cannot assign  permission of read",
        "Cannot assign member permission of ",
    ] {
        assert!(
            matches!(classify(json!({"message":message})),ApiError::Validation{codes,..} if codes.is_empty())
        );
    }
}

/// The statuses octoauth leaves as `Status` that this crate names.
#[test]
fn redirects_method_and_gone_statuses_are_named_and_others_keep_their_status() {
    for (status, expected) in [
        (301, ApiError::Moved { status: 301 }),
        (302, ApiError::Moved { status: 302 }),
        (307, ApiError::Moved { status: 307 }),
        (308, ApiError::Moved { status: 308 }),
        (405, ApiError::NotAllowed),
        (410, ApiError::Gone),
        (401, ApiError::Unauthorized),
        (404, ApiError::NotFound),
        (409, ApiError::Conflict),
        (500, ApiError::Server { status: 500 }),
        (502, ApiError::Server { status: 502 }),
        (400, ApiError::Status { status: 400 }),
        (418, ApiError::Status { status: 418 }),
        (200, ApiError::Status { status: 200 }),
    ] {
        let mut response = refusal(status, &[], "anything");
        response
            .headers
            .insert("location".into(), "https://private-location.test".into());
        let error = ApiError::from_response(&response);
        assert_eq!(error, expected, "{status}");
        assert!(!format!("{error} {error:?}").contains("private-location"));
    }
    assert_eq!(
        ApiError::Moved { status: 302 }.to_string(),
        "gh:api:moved:302"
    );
    assert_eq!(
        ApiError::Server { status: 503 }.to_string(),
        "gh:api:server:503"
    );
    assert_eq!(
        ApiError::Status { status: 400 }.to_string(),
        "gh:api:status:400"
    );
}

#[test]
fn rate_limit_resource_is_bounded() {
    for (header, expected) in [
        ("core", Resource::Core),
        ("search", Resource::Search),
        ("graphql", Resource::Graphql),
        ("integration_manifest", Resource::IntegrationManifest),
        ("PRIVATE_RESOURCE", Resource::Other),
    ] {
        let error = ApiError::from_response(&refusal(
            429,
            &[("x-ratelimit-resource", header)],
            "Too Many Requests",
        ));
        assert!(
            matches!(error, ApiError::RateLimited { resource: Some(r), .. } if r == expected),
            "{header}"
        );
        assert!(!format!("{error:?}").contains("PRIVATE"));
    }
    assert!(matches!(
        ApiError::from_response(&refusal(429, &[], "Too Many Requests")),
        ApiError::RateLimited { resource: None, .. }
    ));
}

#[test]
fn retry_after_classifies_a_secondary_limit_on_403() {
    let limit = limit(&refusal(
        403,
        &[("retry-after", "60"), ("x-ratelimit-remaining", "4998")],
        "You have exceeded a secondary rate limit",
    ))
    .expect("retry-after is documented secondary-limit evidence");
    assert_eq!(
        limit,
        (RateLimitScope::Secondary, Some(Duration::from_secs(60)))
    );
}

#[test]
fn exhausted_quota_takes_its_wait_from_the_reset_header() {
    let limit = limit(&refusal(
        403,
        &[
            ("x-ratelimit-remaining", "0"),
            ("x-ratelimit-reset", "1893457800"),
            ("date", "Tue, 01 Jan 2030 00:00:00 GMT"),
        ],
        "API rate limit exceeded for installation ID 9.",
    ))
    .expect("an exhausted quota is documented primary-limit evidence");
    // 1893457800 is 1800s after the `date` above.
    assert_eq!(
        limit,
        (RateLimitScope::Primary, Some(Duration::from_secs(1800)))
    );
}

#[test]
fn reset_is_measured_against_githubs_clock_not_ours() {
    let limit = limit(&refusal(
        403,
        &[
            ("x-ratelimit-remaining", "0"),
            ("x-ratelimit-reset", "1893454200"),
            ("date", "Tue, 01 Jan 2030 00:00:00 GMT"),
        ],
        "API rate limit exceeded",
    ))
    .unwrap();
    assert_eq!(limit, (RateLimitScope::Primary, None));
}

#[test]
fn body_wording_classifies_a_limit_that_sent_no_headers() {
    let limit = limit(&refusal(
        403,
        &[],
        "You have exceeded a secondary rate limit",
    ))
    .expect("the documented body wording is evidence on its own");
    assert_eq!(limit, (RateLimitScope::Secondary, None));
}

#[test]
fn malformed_or_absent_headers_fall_back_to_the_other_evidence() {
    let limit_seen = limit(&refusal(
        403,
        &[
            ("retry-after", "in a little while"),
            ("x-ratelimit-remaining", "plenty"),
            ("x-ratelimit-reset", "soon"),
            ("date", "whenever"),
        ],
        "API rate limit exceeded for installation ID 9.",
    ))
    .expect("body wording survives unparseable headers");
    assert_eq!(limit_seen, (RateLimitScope::Secondary, None));
    let limit_seen = limit(&refusal(429, &[("retry-after", "?")], "Too Many Requests"))
        .expect("429 is a secondary limit by definition");
    assert_eq!(limit_seen, (RateLimitScope::Secondary, None));
}

#[test]
fn genuine_forbidden_response_is_not_throttling() {
    let response = refusal(
        403,
        &[
            ("x-ratelimit-remaining", "4998"),
            ("x-ratelimit-reset", "1893457800"),
            ("date", "Tue, 01 Jan 2030 00:00:00 GMT"),
        ],
        "Resource not accessible by integration",
    );
    assert_eq!(
        ApiError::from_response(&response),
        ApiError::Forbidden {
            reason: ForbiddenReason::Other
        }
    );
}

#[test]
fn genuine_forbidden_response_is_not_throttling_on_the_hours_last_request() {
    let response = refusal(
        403,
        &[
            ("x-ratelimit-remaining", "0"),
            ("x-ratelimit-reset", "1893457800"),
            ("date", "Tue, 01 Jan 2030 00:00:00 GMT"),
        ],
        "Resource not accessible by integration",
    );
    assert_eq!(limit(&response), None);
    assert!(matches!(
        ApiError::from_response(&response),
        ApiError::Forbidden { .. }
    ));
}

#[test]
fn retry_after_accepts_the_http_date_form() {
    let dated = refusal(
        403,
        &[
            ("retry-after", "Tue, 01 Jan 2030 00:02:00 GMT"),
            ("date", "Tue, 01 Jan 2030 00:00:00 GMT"),
        ],
        "You have exceeded a secondary rate limit",
    );
    assert_eq!(
        limit(&dated),
        Some((RateLimitScope::Secondary, Some(Duration::from_secs(120))))
    );
    let past = refusal(
        403,
        &[
            ("retry-after", "Mon, 31 Dec 2029 23:58:00 GMT"),
            ("date", "Tue, 01 Jan 2030 00:00:00 GMT"),
        ],
        "You have exceeded a secondary rate limit",
    );
    assert_eq!(limit(&past), Some((RateLimitScope::Secondary, None)));
    let undated = refusal(
        403,
        &[("retry-after", "Tue, 01 Jan 2030 00:02:00 GMT")],
        "You have exceeded a secondary rate limit",
    );
    assert_eq!(limit(&undated), Some((RateLimitScope::Secondary, None)));
}

#[test]
fn an_unusable_retry_after_still_cites_a_limit() {
    let limit = limit(&refusal(
        403,
        &[("retry-after", "Tue, 01 Jan 2030 00:02:00 GMT")],
        "Something went wrong",
    ))
    .expect("a retry-after GitHub sent cites a limit even when unreadable");
    assert_eq!(limit, (RateLimitScope::Secondary, None));
}

#[test]
fn a_cited_secondary_limit_stays_secondary_on_an_exhausted_quota() {
    let limit = limit(&refusal(
        429,
        &[
            ("x-ratelimit-remaining", "0"),
            ("x-ratelimit-reset", "1893457800"),
            ("date", "Tue, 01 Jan 2030 00:00:00 GMT"),
        ],
        "Too Many Requests",
    ))
    .unwrap();
    assert_eq!(
        limit,
        (RateLimitScope::Secondary, Some(Duration::from_secs(1800)))
    );
}

#[test]
fn retry_after_names_the_wait_even_on_an_exhausted_quota() {
    let limit = limit(&refusal(
        429,
        &[
            ("retry-after", "30"),
            ("x-ratelimit-remaining", "0"),
            ("x-ratelimit-reset", "1893457800"),
            ("date", "Tue, 01 Jan 2030 00:00:00 GMT"),
        ],
        "You have exceeded a secondary rate limit",
    ))
    .unwrap();
    assert_eq!(
        limit,
        (RateLimitScope::Secondary, Some(Duration::from_secs(30)))
    );
}

#[test]
fn a_healthy_quotas_reset_is_never_mistaken_for_a_secondary_wait() {
    let limit = limit(&refusal(
        403,
        &[
            ("x-ratelimit-remaining", "4998"),
            ("x-ratelimit-reset", "1893457800"),
            ("date", "Tue, 01 Jan 2030 00:00:00 GMT"),
        ],
        "You have exceeded a secondary rate limit",
    ))
    .unwrap();
    assert_eq!(limit, (RateLimitScope::Secondary, None));
}

#[test]
fn exhausted_quota_is_evidence_when_github_names_no_other_reason() {
    let response = Response {
        status: 403,
        headers: [("x-ratelimit-remaining".to_owned(), "0".to_owned())]
            .into_iter()
            .collect(),
        body: b"<html>upstream error</html>".to_vec(),
    };
    let limit = limit(&response).expect("headers are the only evidence");
    assert_eq!(limit.0, RateLimitScope::Primary);
}

#[test]
fn non_throttling_status_never_classifies_as_a_limit() {
    assert_eq!(
        ApiError::from_response(&refusal(
            404,
            &[("x-ratelimit-remaining", "0"), ("retry-after", "60")],
            "Not Found"
        )),
        ApiError::NotFound
    );
}

#[test]
fn suggested_wait_defaults_to_a_minute() {
    let limited = |retry_after| ApiError::RateLimited {
        scope: RateLimitScope::Secondary,
        retry_after,
        resource: None,
    };
    assert_eq!(
        limited(None).suggested_wait(),
        Some(Duration::from_secs(60))
    );
    assert_eq!(
        limited(Some(Duration::from_secs(5))).suggested_wait(),
        Some(Duration::from_secs(5))
    );
    assert_eq!(ApiError::NotFound.suggested_wait(), None);
}

/// Deviation from gh-toolkit, from octoauth's classifier: unrecognised codes
/// sort after recognised ones. gh-toolkit kept GitHub's order
/// (`[Other, Invalid]` here). Callers test membership, never position.
#[test]
fn unrecognised_validation_codes_sort_last() {
    assert_eq!(
        classify(
            422,
            json!({"errors":[{"code":"future_code"},{"code":"invalid"},{"code":"other_future"}]})
        ),
        ApiError::Validation {
            codes: vec![ValidationCode::Invalid, ValidationCode::Other],
            message_len: 0
        }
    );
}
