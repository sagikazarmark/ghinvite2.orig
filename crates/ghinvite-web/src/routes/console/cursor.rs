//! Console paging cursors: an opaque URL token carrying a page's storage
//! boundary.
//!
//! Each page keeps its own cursor shape, what it binds the cursor to, and what
//! an unreadable cursor means (the queue refuses it; history and audit fall
//! back to the first page). This module owns only the token itself.
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, Datelike, Utc};
use serde::{Serialize, de::DeserializeOwned};

/// Longest token accepted. Every cursor the Console issues is far shorter;
/// the cap bounds the work an arbitrary query string can ask for.
const MAX_TOKEN_BYTES: usize = 512;

/// The URL token for `cursor`.
pub(super) fn encode<T: Serialize>(cursor: &T) -> String {
    URL_SAFE_NO_PAD.encode(serde_json::to_vec(cursor).expect("console cursor serializes"))
}

/// The cursor `token` carries, or `None` when it is not one this Console
/// issued: over the size cap, not base64 or JSON of `T`, or positioned at a
/// time (`at`) outside years 1 to 9999.
pub(super) fn decode<T: DeserializeOwned>(
    token: &str,
    at: impl FnOnce(&T) -> DateTime<Utc>,
) -> Option<T> {
    if token.len() > MAX_TOKEN_BYTES {
        return None;
    }
    let bytes = URL_SAFE_NO_PAD.decode(token).ok()?;
    let cursor: T = serde_json::from_slice(&bytes).ok()?;
    (1..=9999).contains(&at(&cursor).year()).then_some(cursor)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Cursor {
        at: DateTime<Utc>,
        id: u64,
    }

    fn cursor(at: &str) -> Cursor {
        Cursor {
            at: at.parse().unwrap(),
            id: 7,
        }
    }

    fn decoded(token: &str) -> Option<Cursor> {
        decode(token, |c: &Cursor| c.at)
    }

    #[test]
    fn an_issued_cursor_decodes_to_itself() {
        let issued = cursor("2026-05-04T12:00:00Z");

        assert_eq!(decoded(&encode(&issued)), Some(issued));
    }

    #[test]
    fn a_token_that_is_not_an_issued_cursor_is_refused() {
        for token in [
            "",
            "broken",
            "not base64!",
            &URL_SAFE_NO_PAD.encode(b"{\"at\":\"2026-05-04T12:00:00Z\"}"),
            &URL_SAFE_NO_PAD.encode(b"{\"at\":\"2026-05-04T12:00:00Z\",\"id\":7,\"x\":1}"),
        ] {
            assert_eq!(decoded(token), None, "token={token:?}");
        }
    }

    #[test]
    fn a_token_over_the_size_cap_is_refused_before_decoding() {
        let padded = format!(
            "{}{}",
            encode(&cursor("2026-05-04T12:00:00Z")),
            "A".repeat(MAX_TOKEN_BYTES)
        );

        assert_eq!(decoded(&padded), None);
    }

    #[test]
    fn a_cursor_outside_four_digit_years_is_refused() {
        let far = Cursor {
            at: DateTime::<Utc>::MAX_UTC,
            id: 7,
        };

        assert_eq!(decoded(&encode(&far)), None);
        assert!(decoded(&encode(&cursor("9999-12-31T23:59:59Z"))).is_some());
    }
}
