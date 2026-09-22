//! Small helpers shared across modules in this crate.

use crate::transport::{Method, Request};

/// A GitHub REST API request carrying `bearer` — an App JWT, an installation
/// token, or a user access token — and the headers every such call sends.
pub(crate) fn github_request(method: Method, url: impl Into<String>, bearer: &str) -> Request {
    Request::new(method, url)
        .header("accept", "application/vnd.github+json")
        .header("authorization", format!("Bearer {bearer}"))
        .header("user-agent", "ghinvite")
        .header("x-github-api-version", "2022-11-28")
}

/// Percent-encode a single URL path segment. Allows only RFC 3986 unreserved
/// characters; everything else is hex-encoded uppercase. Used to build
/// `/repos/{owner}/{repo}/...` paths defensively against logins or repo
/// names that might contain spaces, dots, or other non-ASCII characters.
pub(crate) fn path_segment(s: &str) -> String {
    const SAFE: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789-_.~";
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if SAFE.contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passes_through_safe_chars() {
        assert_eq!(path_segment("octocat"), "octocat");
        assert_eq!(path_segment("acme-api"), "acme-api");
        assert_eq!(path_segment("abc.def_~"), "abc.def_~");
    }

    #[test]
    fn percent_encodes_space() {
        assert_eq!(path_segment("acme corp"), "acme%20corp");
    }

    #[test]
    fn percent_encodes_slash() {
        assert_eq!(path_segment("a/b"), "a%2Fb");
    }
}
