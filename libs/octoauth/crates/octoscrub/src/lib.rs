//! A last-resort scrubber for GitHub credentials in log lines.
//!
//! [`scrub`] replaces what looks like a GitHub credential with `<redacted>`:
//! tokens (`ghs_`, `ghu_`, `ghr_`, `gho_`, `ghp_`, `github_pat_`), JWT-shaped
//! values (an App's credential), `Bearer` and `Basic` authorization values
//! (the word, too, so prose like "basic auth failed" loses a word), the OAuth
//! query parameters `code`, `state`, `code_verifier`, `client_secret`,
//! `refresh_token` and `access_token`, and the JSON fields `code_verifier`,
//! `client_secret`, `refresh_token` and `access_token` (GitHub's JSON uses
//! `code` for error codes). Names are kept, so the line still says what was
//! there.
//!
//! Install it in a log layer or formatter as a **backstop**. It does not make
//! upstream text safe to keep: a secret need not look like one, and no
//! pattern list knows every shape. Errors that may end up in logs or durable
//! journals should carry no upstream text in the first place, like
//! `octoauth::ApiError`.
//!
//! ```
//! let line = "GET /orgs/acme?access_token=ghs_abc123 failed: Bearer eyJhbGciOiJSUzI1NiJ9.e30.c2ln";
//! assert_eq!(
//!     octoscrub::scrub(line),
//!     "GET /orgs/acme?access_token=<redacted> failed: <redacted>"
//! );
//! ```
#![forbid(unsafe_code)]
#![deny(missing_docs)]

use regex::Regex;
use std::borrow::Cow;
use std::sync::OnceLock;

/// Replace what looks like a GitHub credential in `input` with `<redacted>`.
///
/// Borrows `input` when nothing matched, so clean lines cost no allocation.
/// Idempotent: scrubbing a scrubbed line changes nothing.
pub fn scrub(input: &str) -> Cow<'_, str> {
    let mut output = Cow::Borrowed(input);
    for (pattern, replacement) in patterns() {
        if let Cow::Owned(replaced) = pattern.replace_all(&output, *replacement) {
            output = Cow::Owned(replaced);
        }
    }
    output
}

fn patterns() -> &'static [(Regex, &'static str)] {
    static PATTERNS: OnceLock<Vec<(Regex, &'static str)>> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        [
            // JSON values, keeping the key. Escapes are consumed, so an
            // escaped quote cannot leave a suffix behind.
            (
                r#"(?i)("(?:code_verifier|client_secret|refresh_token|access_token)"\s*:\s*")(?:\\.|[^"\\])*"#,
                "${1}<redacted>",
            ),
            // Query parameters, keeping the name.
            (
                r"(?i)([?&](?:code|state|code_verifier|client_secret|refresh_token|access_token)=)[^&\s]+",
                "${1}<redacted>",
            ),
            // Authorization values, whatever their shape.
            (r"(?i)\b(?:Bearer|Basic)\s+\S+", "<redacted>"),
            // GitHub tokens: installation, user-to-server, refresh, OAuth,
            // personal; fine-grained personal tokens.
            (
                r"(?:gh[surop]_[A-Za-z0-9._\-]{36,}|github_pat_[A-Za-z0-9._\-]+)",
                "<redacted>",
            ),
            // JWTs, e.g. an App's credential or a stateless installation token's body.
            (
                r"eyJ[A-Za-z0-9_\-]{10,}\.[A-Za-z0-9_\-]+\.[A-Za-z0-9_\-]*",
                "<redacted>",
            ),
        ]
        .into_iter()
        .map(|(pattern, replacement)| {
            (
                Regex::new(pattern).expect("static scrub pattern"),
                replacement,
            )
        })
        .collect()
    })
}
