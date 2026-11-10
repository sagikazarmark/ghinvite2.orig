//! Ported from gh-toolkit's `tests/redaction_backstop.rs`.

use octoscrub::scrub;
use std::borrow::Cow;

#[test]
fn long_tokens_jwts_and_authorization_values_leave_no_suffix() {
    for prefix in ["ghs_", "ghu_", "ghr_", "gho_", "ghp_", "github_pat_"] {
        let token = format!("{prefix}{}", "a.B-c_d".repeat(100));
        assert_eq!(
            scrub(&format!("before {token} after")),
            "before <redacted> after"
        );
    }
    // A stateless installation token's size.
    let jwt = format!(
        "eyJ{}.{}.{}",
        "a".repeat(40),
        "b".repeat(640),
        "c".repeat(100)
    );
    assert_eq!(scrub(&jwt), "<redacted>");
    for scheme in ["Bearer", "Basic", "bearer", "BASIC"] {
        assert_eq!(
            scrub(&format!("header {scheme}\tno-pattern-secret end")),
            "header <redacted> end"
        );
    }
}

#[test]
fn oauth_parameters_and_escaped_json_values_lose_values_not_names() {
    for name in [
        "code",
        "state",
        "code_verifier",
        "client_secret",
        "refresh_token",
        "access_token",
    ] {
        let input = format!("https://example.test/?{name}=private%20material&keep=value");
        assert_eq!(
            scrub(&input),
            format!("https://example.test/?{name}=<redacted>&keep=value")
        );
    }
    for name in [
        "code_verifier",
        "client_secret",
        "refresh_token",
        "access_token",
    ] {
        let input = format!(r#"{{"{name}":"private\"suffix","safe":true}}"#);
        assert_eq!(
            scrub(&input),
            format!(r#"{{"{name}":"<redacted>","safe":true}}"#)
        );
    }
    assert_eq!(scrub("?STATE=秘密&ok=1"), "?STATE=<redacted>&ok=1");
}

#[test]
fn clean_lines_are_borrowed_and_scrubbing_is_idempotent() {
    assert!(matches!(scrub("status=503 retry=60"), Cow::Borrowed(_)));
    // Short values without a credential shape are left alone: this is a
    // backstop for GitHub credentials, not a general secret detector.
    assert_eq!(
        scrub("/i/product-specific-secret ghs_short"),
        "/i/product-specific-secret ghs_short"
    );
    for line in [
        "?code=hidden&message=ok",
        "Authorization: Bearer abc",
        r#"{"access_token":"x"}"#,
    ] {
        let once = scrub(line).into_owned();
        assert_eq!(scrub(&once), once);
    }
}
