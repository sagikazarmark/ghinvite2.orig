//! Turning upstream responses into diagnostics that are safe to keep.
//!
//! Every GitHub response body is attacker-influenced to some degree: a token
//! endpoint's payload *is* a credential, and anything between us and
//! `api.github.com` can echo the `Authorization` header we sent into its own
//! error page. Error values outlive the request that produced them — they are
//! formatted into logs, into Restate terminal errors, and (historically) into
//! browser responses — so nothing in this crate may retain a raw body.
//!
//! This module is the single seam where a body becomes a diagnostic. It keeps
//! only what triages an incident:
//!
//! * the fields GitHub documents on its error envelope (`message`, and the
//!   `errors[].code` sub-codes that distinguish validation failures), and
//! * the size and shape of anything it does not recognise.
//!
//! Both paths are bounded and run through [`scrub_credentials`] as a backstop.

/// Longest diagnostic we keep. Bodies are summarised, not truncated, so this
/// is a backstop rather than the usual outcome.
const MAX_DIAGNOSTIC: usize = 512;

/// Largest body worth parsing to look for an error envelope. GitHub's error
/// responses are a few hundred bytes; anything past this is described by size
/// rather than walked, so a gateway cannot make us parse what it likes.
const MAX_INSPECT: usize = 64 * 1024;

/// Longest `message` we echo from a recognised GitHub error envelope.
const MAX_MESSAGE: usize = 160;

/// Most `errors[].code` sub-codes we keep from one envelope.
const MAX_CODES: usize = 4;

/// Longest single sub-code we keep.
const MAX_CODE: usize = 40;

/// Credential prefixes GitHub issues. Any word starting with one of these is a
/// token, whatever else it looks like.
const TOKEN_PREFIXES: &[&str] = &[
    "github_pat_",
    "ghp_",
    "gho_",
    "ghu_",
    "ghs_",
    "ghr_",
    "ghg_",
];

/// Placeholder left where a credential was removed.
const REDACTED: &str = "[redacted]";

/// Summarise a non-2xx response body into a diagnostic safe to keep in an
/// error value.
///
/// A recognised GitHub error envelope keeps its `message` and validation
/// sub-codes, because those are what tell a 422 "already a collaborator" apart
/// from a 422 "permission not valid". Anything else is reduced to its size and
/// media shape — the body itself never survives.
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
    bound(&scrub_credentials(&summary), MAX_DIAGNOSTIC)
}

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
/// The slots this guards — an OAuth `error`, an installation's account type,
/// its repository selection — all take values from a documented set, and every
/// one of them arrives either on a browser-controlled redirect or in a GitHub
/// response. A syntax rule alone is not enough: a secret such as
/// `client-secret-do-not-expose` is lowercase, hyphenated and short, so it
/// would pass as an identifier and land in the logs verbatim. Matching against
/// the caller's allowlist removes that whole class — anything unrecognised
/// becomes [`UNRECOGNIZED`], and the status or field name still says what
/// happened.
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

/// Field and parameter names whose value is a credential wherever it appears —
/// in a reflected JSON body, a form encoding, or a header dump.
const CREDENTIAL_KEYS: &[&str] = &[
    "access_token",
    "api_key",
    "apikey",
    "auth",
    "authorization",
    "client_secret",
    "credential",
    "credentials",
    "passwd",
    "password",
    "private_key",
    "refresh_token",
    "secret",
    "token",
];

/// Authentication schemes that introduce a credential as the next word, with
/// no `=` or `:` in between.
const CREDENTIAL_SCHEMES: &[&str] = &["basic", "bearer", "token"];

/// What the scrubber expects of the next word it sees.
#[derive(Clone, Copy, PartialEq)]
enum Expect {
    /// Nothing in particular; judge the word on its own shape.
    Anything,
    /// A credential named by the preceding scheme (`Bearer <token>`).
    Credential,
    /// A credential, but only once an `=` or `:` confirms this is an
    /// assignment rather than prose that happens to contain the word.
    AssignedCredential { assigned: bool },
}

/// Replace credential-shaped runs with [`REDACTED`].
///
/// Defence in depth: the callers above already drop everything they do not
/// recognise, so this only has to cover text that survived as a documented
/// field. It walks the text as runs of token characters separated by anything
/// else, and judges each run by its own shape and by what introduced it.
pub(crate) fn scrub_credentials(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut word = String::new();
    let mut expect = Expect::Anything;

    for ch in text.chars() {
        if is_token_char(ch) {
            word.push(ch);
            continue;
        }
        expect = push_word(&mut out, &word, expect);
        word.clear();
        expect = match ch {
            '=' | ':' => match expect {
                Expect::AssignedCredential { .. } => Expect::AssignedCredential { assigned: true },
                other => other,
            },
            // Quotes and spaces sit between a key and its value; anything else
            // ends the assignment.
            ' ' | '\t' | '"' | '\'' => expect,
            _ => Expect::Anything,
        };
        out.push(ch);
    }
    push_word(&mut out, &word, expect);
    out
}

/// Emit one word, redacted or not, and report what the word implies about the
/// next one.
fn push_word(out: &mut String, word: &str, expect: Expect) -> Expect {
    if word.is_empty() {
        return expect;
    }
    let lowered = word.to_ascii_lowercase();
    let introduced = matches!(
        expect,
        Expect::Credential | Expect::AssignedCredential { assigned: true }
    );
    if introduced {
        // `Authorization: Bearer <token>` — a scheme in the value slot names
        // the credential after it and is not itself secret. Reading schemes
        // only here keeps prose ("the request had no token and was rejected")
        // from dragging the next word in.
        if CREDENTIAL_SCHEMES.contains(&lowered.as_str()) {
            out.push_str(word);
            return Expect::Credential;
        }
        out.push_str(REDACTED);
        return Expect::Anything;
    }
    if looks_like_credential(word) {
        out.push_str(REDACTED);
        return Expect::Anything;
    }
    out.push_str(word);
    if CREDENTIAL_KEYS.contains(&lowered.as_str()) {
        Expect::AssignedCredential { assigned: false }
    } else {
        Expect::Anything
    }
}

/// `=` is deliberately absent: it separates a key from its value far more
/// often than it pads a token, and GitHub's tokens and compact JWTs carry no
/// base64 padding.
fn is_token_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.' | '+' | '/' | '~')
}

fn looks_like_credential(word: &str) -> bool {
    if TOKEN_PREFIXES
        .iter()
        .any(|prefix| word.len() >= prefix.len() + 8 && word.starts_with(prefix))
    {
        return true;
    }
    // A compact JWT: three base64url segments, the first of which is the
    // `{"alg":` header every GitHub App JWT starts with.
    word.starts_with("eyJ") && word.matches('.').count() == 2 && word.len() >= 20
}

/// Pull the documented fields out of an already-parsed GitHub error envelope.
fn envelope_summary(value: &serde_json::Value) -> Option<String> {
    let object = value.as_object()?;
    let message = object.get("message")?.as_str()?;
    // Scrub before `{:?}` escapes the text, not after. Escaping inserts
    // backslashes, and a backslash ends the `key = value` run the scanner
    // follows — so `client_secret=\"…\"` would walk straight past it.
    let mut summary = format!(
        "message={:?}",
        scrub_credentials(&bound(message, MAX_MESSAGE))
    );
    let codes: Vec<&str> = object
        .get("errors")
        .and_then(|errors| errors.as_array())
        .map(|errors| {
            errors
                .iter()
                .filter_map(|entry| entry.get("code")?.as_str())
                .filter(|code| is_sub_code(code))
                .take(MAX_CODES)
                .collect()
        })
        .unwrap_or_default();
    if !codes.is_empty() {
        summary.push_str(&format!(" codes=[{}]", codes.join(",")));
    }
    Some(summary)
}

/// GitHub's validation sub-codes are short snake_case identifiers. Anything
/// else in that slot is not a code we know how to act on.
fn is_sub_code(code: &str) -> bool {
    !code.is_empty()
        && code.len() <= MAX_CODE
        && code
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
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

    #[test]
    fn envelope_keeps_message_and_validation_sub_codes() {
        let body = br#"{"message":"Validation Failed","errors":[{"resource":"RepositoryInvitation","code":"already_exists","field":"invitee_id"}]}"#;
        let diagnostic = upstream_diagnostic(body);
        assert!(diagnostic.contains("Validation Failed"), "{diagnostic}");
        assert!(diagnostic.contains("already_exists"), "{diagnostic}");
    }

    #[test]
    fn envelope_drops_undocumented_fields() {
        // A gateway that reflects the request it proxied must not smuggle the
        // credential through on a field we never asked for.
        let body = format!(
            r#"{{"message":"Bad gateway","request_headers":{{"authorization":"Bearer {APP_JWT}"}}}}"#
        );
        let diagnostic = upstream_diagnostic(body.as_bytes());
        assert!(diagnostic.contains("Bad gateway"), "{diagnostic}");
        assert!(!diagnostic.contains(APP_JWT), "{diagnostic}");
        assert!(!diagnostic.contains("request_headers"), "{diagnostic}");
    }

    #[test]
    fn credential_in_a_documented_field_is_scrubbed() {
        let body =
            format!(r#"{{"message":"upstream rejected token {INSTALLATION_TOKEN} for install"}}"#);
        let diagnostic = upstream_diagnostic(body.as_bytes());
        assert!(!diagnostic.contains(INSTALLATION_TOKEN), "{diagnostic}");
        assert!(diagnostic.contains(REDACTED), "{diagnostic}");
        assert!(
            diagnostic.contains("upstream rejected token"),
            "{diagnostic}"
        );
    }

    /// The summary renders `message` with `{:?}`, which escapes any quotes
    /// inside it. Scrubbing the escaped form would walk past a credential:
    /// the backslash breaks the `key = value` run the scanner follows.
    #[test]
    fn a_quoted_assignment_inside_message_is_still_scrubbed() {
        let body = r#"{"message":"upstream rejected client_secret=\"opaque-secret-value\" today"}"#;
        let diagnostic = upstream_diagnostic(body.as_bytes());
        assert!(!diagnostic.contains("opaque-secret-value"), "{diagnostic}");
        assert!(diagnostic.contains(REDACTED), "{diagnostic}");
        assert!(diagnostic.contains("upstream rejected"), "{diagnostic}");
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

    #[test]
    fn diagnostics_are_bounded() {
        let body = format!(r#"{{"message":"{}"}}"#, "a".repeat(8192));
        let diagnostic = upstream_diagnostic(body.as_bytes());
        assert!(
            diagnostic.len() <= MAX_DIAGNOSTIC + 4,
            "{}",
            diagnostic.len()
        );
        assert!(diagnostic.contains('…'), "{diagnostic}");
    }

    #[test]
    fn bounding_never_splits_a_codepoint() {
        let body = format!(r#"{{"message":"{}€ tail"}}"#, "a".repeat(MAX_MESSAGE));
        // Reaching here without a panic is the assertion; the euro sign sits
        // exactly where the naive byte slice would land.
        assert!(upstream_diagnostic(body.as_bytes()).contains('…'));
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
    }

    #[test]
    fn scrub_leaves_ordinary_prose_alone() {
        let text = "Validation Failed: invitee_id already_exists on repo owner/name.";
        assert_eq!(scrub_credentials(text), text);
    }

    #[test]
    fn scrub_redacts_the_value_of_a_credential_key() {
        // A reflected form body or header dump names the credential next to it,
        // so the name is enough even when the value has no recognisable shape.
        for text in [
            "client_secret=s3cr3t-value-here",
            r#""access_token": "opaque-value-here""#,
            "Authorization: Bearer opaque-value-here",
            "api_key=s3cr3t-value-here",
        ] {
            let scrubbed = scrub_credentials(text);
            assert!(!scrubbed.contains("value-here"), "{text} -> {scrubbed}");
            assert!(scrubbed.contains(REDACTED), "{text} -> {scrubbed}");
        }
    }

    #[test]
    fn scrub_does_not_swallow_a_key_name_mentioned_in_prose() {
        let text = "the request had no token and was rejected";
        assert_eq!(scrub_credentials(text), text);
    }

    #[test]
    fn scrub_stops_at_the_end_of_an_assignment() {
        let scrubbed = scrub_credentials(r#"{"token":"abcdefgh","repo":"owner/name"}"#);
        assert!(scrubbed.contains("owner/name"), "{scrubbed}");
        assert!(!scrubbed.contains("abcdefgh"), "{scrubbed}");
    }

    #[test]
    fn scrub_catches_every_github_token_prefix() {
        for prefix in TOKEN_PREFIXES {
            let token = format!("{prefix}0123456789abcdef");
            assert_eq!(
                scrub_credentials(&format!("got {token} back")),
                format!("got {REDACTED} back"),
            );
        }
    }
}
