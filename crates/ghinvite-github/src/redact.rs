//! Turning upstream responses into diagnostics that are safe to keep.
//!
//! Every GitHub response body is attacker-influenced to some degree: a token
//! endpoint's payload *is* a credential, and anything between us and
//! `api.github.com` can echo the `Authorization` header we sent into its own
//! error page. Error values outlive the request that produced them — they are
//! formatted into logs, into Restate terminal errors, and (historically) into
//! browser responses — so nothing in this crate may retain a raw body.
//!
//! This module is the single seam where a body becomes a diagnostic, and it
//! keeps **no upstream text at all**. That is deliberate, and it is the second
//! design here: the first tried to keep GitHub's `message` and scrub
//! credentials out of it by pattern. Pattern-matching cannot do that job. A
//! secret need not look like one (`client-secret-do-not-expose` is just a
//! lowercase word), an assignment can be split across a newline, and the key
//! naming it can be a word no list carries. Every such hole is invisible until
//! someone finds it.
//!
//! So what survives is only what this crate can enumerate:
//!
//! * that a body *was* a GitHub error envelope, and how many bytes its message
//!   ran to — never the message itself;
//! * the `errors[].code` sub-codes GitHub documents, matched against
//!   [`VALIDATION_SUB_CODES`], because a 422's sub-code is the whole reason a
//!   caller looks at a 422;
//! * the size and shape of anything unrecognised.
//!
//! Callers that need to classify a response — [`crate::Response::rate_limit`]
//! reads GitHub's rate-limit wording — do so against the *raw* body, before it
//! reaches here. Classification and retention are separate questions.

/// Longest diagnostic we keep. Everything below is enumerated rather than
/// copied, so this is a backstop that should never bind in practice.
const MAX_DIAGNOSTIC: usize = 512;

/// Most `errors[].code` sub-codes we keep from one envelope.
const MAX_CODES: usize = 4;

/// Summarise a non-2xx response body into a diagnostic safe to keep in an
/// error value. See the module docs for what "safe" is taken to mean.
pub(crate) fn upstream_diagnostic(body: &[u8]) -> String {
    // Parse at most once, and only when the body is small enough to be worth
    // parsing at all. Nothing upstream sends a megabyte of error envelope, and
    // the transport buffers whatever it is handed, so an oversized body is
    // described rather than walked.
    let parsed = (body.len() <= MAX_INSPECT)
        .then(|| serde_json::from_slice::<serde_json::Value>(body).ok())
        .flatten();
    let summary = parsed
        .as_ref()
        .and_then(envelope_summary)
        .unwrap_or_else(|| unrecognised_summary(body, parsed.is_some()));
    bound(&summary, MAX_DIAGNOSTIC)
}

/// Largest body worth parsing to look for an error envelope. GitHub's error
/// responses are a few hundred bytes; anything past this is described by size
/// rather than walked, so a gateway cannot make us parse what it likes.
const MAX_INSPECT: usize = 64 * 1024;

/// Describe a body we could not decode without quoting any of it.
///
/// `serde_json`'s own message is not safe to keep: a type mismatch renders the
/// offending value (`invalid type: string "ghs_..."`). The category and
/// position carry the same triage value and none of the payload.
pub(crate) fn decode_diagnostic(error: &serde_json::Error, body_len: usize) -> String {
    let category = match error.classify() {
        serde_json::error::Category::Io => "io",
        serde_json::error::Category::Syntax => "syntax",
        serde_json::error::Category::Data => "shape",
        serde_json::error::Category::Eof => "truncated",
    };
    format!(
        "{category} error at line {} column {} ({body_len}-byte body)",
        error.line(),
        error.column()
    )
}

/// Reduce an upstream-supplied code to one of the values `known` lists.
///
/// The slots this guards — an OAuth `error`, a validation sub-code, an
/// installation's account type, its repository selection — all take values from
/// a documented set, and every one of them arrives either on a
/// browser-controlled redirect or in a GitHub response. Matching against the
/// caller's allowlist is what makes them safe to keep; judging them by shape is
/// not, for the reasons in the module docs.
pub fn bounded_upstream_code(raw: &str, known: &[&str]) -> String {
    if known.contains(&raw) {
        raw.to_owned()
    } else {
        UNRECOGNIZED.to_owned()
    }
}

/// Stands in for any upstream value that is not one the caller knows.
pub const UNRECOGNIZED: &str = "unrecognized";

/// The `error` codes GitHub documents for the OAuth web flow.
pub const OAUTH_ERROR_CODES: &[&str] = &[
    "access_denied",
    "application_suspended",
    "bad_verification_code",
    "incorrect_client_credentials",
    "redirect_uri_mismatch",
    "unsupported_response_type",
    "unverified_user_email",
];

/// The `code` values GitHub documents on a validation error object, plus the
/// `already_exists` its collaborator-invitation endpoint returns. A sub-code
/// outside this set is not one any caller branches on.
pub const VALIDATION_SUB_CODES: &[&str] = &[
    "already_exists",
    "custom",
    "invalid",
    "missing",
    "missing_field",
    "unprocessable",
];

/// Pull the documented fields out of an already-parsed GitHub error envelope.
///
/// `message` is free upstream prose, so only its size is kept; `errors[].code`
/// is an identifier indistinguishable by shape from an opaque secret, so it
/// survives only by being one GitHub documents.
fn envelope_summary(value: &serde_json::Value) -> Option<String> {
    let object = value.as_object()?;
    let message = object.get("message")?.as_str()?;
    let mut summary = format!("message withheld ({} bytes)", message.len());
    let codes: Vec<String> = object
        .get("errors")
        .and_then(|errors| errors.as_array())
        .map(|errors| {
            errors
                .iter()
                .filter_map(|entry| entry.get("code")?.as_str())
                .take(MAX_CODES)
                .map(|code| bounded_upstream_code(code, VALIDATION_SUB_CODES))
                .collect()
        })
        .unwrap_or_default();
    if !codes.is_empty() {
        summary.push_str(&format!(" codes=[{}]", codes.join(",")));
    }
    Some(summary)
}

/// Describe a body we do not recognise by size alone. `parsed` says whether it
/// was valid JSON, so this never has to parse it a second time.
fn unrecognised_summary(body: &[u8], parsed: bool) -> String {
    let shape = if body.is_empty() {
        "empty"
    } else if body.len() > MAX_INSPECT {
        "oversized"
    } else if parsed {
        "unrecognized json"
    } else {
        "non-json"
    };
    format!("{shape} body ({} bytes)", body.len())
}

/// Truncate on a character boundary, marking that we did.
fn bound(text: &str, limit: usize) -> String {
    if text.len() <= limit {
        return text.to_owned();
    }
    let mut end = limit;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    const APP_JWT: &str = "eyJhbGciOiJSUzI1NiJ9.eyJpc3MiOiIxMjM0NSJ9.c2lnbmF0dXJlLXZhbHVl";
    const INSTALLATION_TOKEN: &str = "ghs_16C7e42F292c6912E7710c838347Ae178B4a";

    /// `message` is free upstream prose. These are three shapes no pattern
    /// scrubber catches: an opaque value with no key beside it, an assignment
    /// split by a newline, and a credential named by an unlisted word. None of
    /// it survives, because none of the text does.
    #[test]
    fn no_upstream_message_text_is_retained() {
        for body in [
            r#"{"message":"client-secret-do-not-expose"}"#,
            "{\"message\":\"client_secret:\\nopaque-value-here\"}",
            r#"{"message":"passphrase was opaque-value-here"}"#,
            r#"{"message":"Validation Failed"}"#,
        ] {
            let diagnostic = upstream_diagnostic(body.as_bytes());
            assert!(!diagnostic.contains("opaque-value-here"), "{diagnostic}");
            assert!(
                !diagnostic.contains("client-secret-do-not-expose"),
                "{diagnostic}"
            );
            assert!(!diagnostic.contains("Validation Failed"), "{diagnostic}");
            // The envelope stays recognisable as one, which is what tells a
            // GitHub refusal apart from a gateway's HTML.
            assert!(diagnostic.contains("message withheld"), "{diagnostic}");
        }
    }

    /// The sub-code is the whole reason a caller looks at a 422, so it has to
    /// survive — but only by being one GitHub documents.
    #[test]
    fn only_documented_validation_sub_codes_are_retained() {
        let body = r#"{"message":"Validation Failed","errors":[{"resource":"RepositoryInvitation","code":"already_exists","field":"invitee_id"},{"code":"client-secret-do-not-expose"}]}"#;
        let diagnostic = upstream_diagnostic(body.as_bytes());
        assert!(diagnostic.contains("already_exists"), "{diagnostic}");
        assert!(
            !diagnostic.contains("client-secret-do-not-expose"),
            "{diagnostic}"
        );
        assert!(diagnostic.contains(UNRECOGNIZED), "{diagnostic}");
        // Fields we never asked for are not carried either.
        assert!(!diagnostic.contains("invitee_id"), "{diagnostic}");
    }

    #[test]
    fn envelope_drops_undocumented_fields() {
        // A gateway that reflects the request it proxied must not smuggle the
        // credential through on a field we never asked for.
        let body = format!(
            r#"{{"message":"Bad gateway","request_headers":{{"authorization":"Bearer {APP_JWT}"}}}}"#
        );
        let diagnostic = upstream_diagnostic(body.as_bytes());
        assert!(!diagnostic.contains(APP_JWT), "{diagnostic}");
        assert!(!diagnostic.contains("request_headers"), "{diagnostic}");
        assert!(diagnostic.contains("message withheld"), "{diagnostic}");
    }

    #[test]
    fn unrecognised_body_is_reduced_to_its_size() {
        let body = format!("<html><body>proxy error: Bearer {APP_JWT}</body></html>");
        let diagnostic = upstream_diagnostic(body.as_bytes());
        assert!(!diagnostic.contains(APP_JWT), "{diagnostic}");
        assert!(!diagnostic.contains("proxy error"), "{diagnostic}");
        assert!(diagnostic.contains("non-json"), "{diagnostic}");
        assert!(diagnostic.contains(&body.len().to_string()), "{diagnostic}");
    }

    #[test]
    fn json_without_an_error_envelope_is_reduced_to_its_size() {
        // A token response is well-formed JSON with no `message` field. It must
        // never be echoed back, however it reached an error path.
        let body = format!(r#"{{"access_token":"{INSTALLATION_TOKEN}","token_type":"bearer"}}"#);
        let diagnostic = upstream_diagnostic(body.as_bytes());
        assert!(!diagnostic.contains(INSTALLATION_TOKEN), "{diagnostic}");
        assert!(!diagnostic.contains("access_token"), "{diagnostic}");
        assert!(diagnostic.contains("unrecognized json"), "{diagnostic}");
    }

    /// A gateway can hand the transport whatever it likes. Past the inspection
    /// cap the body is described, not parsed.
    #[test]
    fn an_oversized_body_is_described_rather_than_parsed() {
        let body = format!(r#"{{"message":"{}"}}"#, "a".repeat(MAX_INSPECT));
        let diagnostic = upstream_diagnostic(body.as_bytes());
        assert!(diagnostic.contains("oversized body"), "{diagnostic}");
        assert!(diagnostic.contains(&body.len().to_string()), "{diagnostic}");
        assert!(!diagnostic.contains("aaaa"), "{diagnostic}");
    }

    #[test]
    fn empty_body_says_so() {
        assert!(upstream_diagnostic(b"").contains("empty body (0 bytes)"));
    }

    /// Every diagnostic is now enumerated rather than copied, so the bound is a
    /// backstop. It still has to hold, and still has to land on a character
    /// boundary — the euro sign sits exactly where a naive byte slice would cut.
    #[test]
    fn the_diagnostic_bound_never_splits_a_codepoint() {
        let text = format!("{}€ tail", "a".repeat(MAX_DIAGNOSTIC));
        let bounded = bound(&text, MAX_DIAGNOSTIC);
        assert!(bounded.ends_with('…'), "{bounded}");
        assert!(bounded.len() <= MAX_DIAGNOSTIC + 4, "{}", bounded.len());
        assert_eq!(bound("short", MAX_DIAGNOSTIC), "short");
    }

    #[test]
    fn decode_diagnostic_keeps_position_not_payload() {
        let body = format!(r#"{{"access_token":"{INSTALLATION_TOKEN}"}}"#);
        let error = serde_json::from_str::<u64>(&body).unwrap_err();
        let diagnostic = decode_diagnostic(&error, body.len());
        assert!(!diagnostic.contains(INSTALLATION_TOKEN), "{diagnostic}");
        assert!(diagnostic.contains("line 1"), "{diagnostic}");
        assert!(
            diagnostic.contains(&format!("{}-byte body", body.len())),
            "{diagnostic}"
        );
    }

    #[test]
    fn decode_diagnostic_classifies_shape_mismatch_apart_from_garbage() {
        let shape = serde_json::from_str::<u64>(r#""a string""#).unwrap_err();
        assert!(decode_diagnostic(&shape, 10).contains("shape"));
        let syntax = serde_json::from_str::<u64>("not json").unwrap_err();
        assert!(decode_diagnostic(&syntax, 8).contains("syntax"));
    }

    #[test]
    fn bounded_code_passes_documented_codes_through() {
        for code in OAUTH_ERROR_CODES {
            assert_eq!(bounded_upstream_code(code, OAUTH_ERROR_CODES), *code);
        }
        for code in VALIDATION_SUB_CODES {
            assert_eq!(bounded_upstream_code(code, VALIDATION_SUB_CODES), *code);
        }
    }

    #[test]
    fn bounded_code_drops_anything_the_caller_does_not_know() {
        for raw in [
            // Prose, and an empty or overlong value.
            &format!("see {INSTALLATION_TOKEN}"),
            "",
            &"a".repeat(65),
            // A credential is shaped like an identifier, so a syntax rule
            // alone would wave these straight through into the logs.
            INSTALLATION_TOKEN,
            &INSTALLATION_TOKEN.to_ascii_lowercase(),
            APP_JWT,
            "ghp_0123456789abcdefghij",
            "github_pat_0123456789abcdefghij",
            // Lowercase, hyphenated and short: indistinguishable from a
            // documented code by shape, which is why shape is not the test.
            "client-secret-do-not-expose",
        ] {
            assert_eq!(
                bounded_upstream_code(raw, OAUTH_ERROR_CODES),
                UNRECOGNIZED,
                "{raw}"
            );
        }
    }

    /// Each slot brings its own documented set; a value from one is not
    /// automatically good in another.
    #[test]
    fn bounded_code_is_scoped_to_the_allowlist_it_is_given() {
        assert_eq!(bounded_upstream_code("all", &["all", "selected"]), "all");
        assert_eq!(
            bounded_upstream_code("all", OAUTH_ERROR_CODES),
            UNRECOGNIZED
        );
        assert_eq!(
            bounded_upstream_code("already_exists", OAUTH_ERROR_CODES),
            UNRECOGNIZED
        );
    }
}
