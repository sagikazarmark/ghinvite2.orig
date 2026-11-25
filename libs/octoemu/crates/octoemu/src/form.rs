//! `application/x-www-form-urlencoded`: query strings and form bodies, read
//! and written (the OAuth pages take and answer forms, and build redirect
//! URLs).

/// The `name=value` pairs of a query string or form body, decoded (`+` is a
/// space, `%XX` a byte). Invalid UTF-8 becomes U+FFFD.
pub(crate) fn parse(input: &str) -> Vec<(String, String)> {
    input
        .split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| {
            let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
            (decode(name), decode(value))
        })
        .collect()
}

/// The first value of `name` in `pairs`.
pub(crate) fn get<'a>(pairs: &'a [(String, String)], name: &str) -> Option<&'a str> {
    pairs
        .iter()
        .find(|(n, _)| n == name)
        .map(|(_, v)| v.as_str())
}

fn decode(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' => match (
                bytes.get(i + 1).and_then(|b| (*b as char).to_digit(16)),
                bytes.get(i + 2).and_then(|b| (*b as char).to_digit(16)),
            ) {
                (Some(high), Some(low)) => {
                    out.push((high * 16 + low) as u8);
                    i += 2;
                }
                _ => out.push(b'%'),
            },
            byte => out.push(byte),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `value` percent-encoded for a query string or form body: everything
/// but unreserved characters (RFC 3986) is escaped, spaces as `%20`.
pub(crate) fn encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// `pairs` as a form body or query string.
pub(crate) fn serialize<'a>(pairs: impl IntoIterator<Item = (&'a str, &'a str)>) -> String {
    pairs
        .into_iter()
        .map(|(name, value)| format!("{}={}", encode(name), encode(value)))
        .collect::<Vec<_>>()
        .join("&")
}

/// `url` with `pairs` appended to its query.
pub(crate) fn with_query<'a>(
    url: &str,
    pairs: impl IntoIterator<Item = (&'a str, &'a str)>,
) -> String {
    let query = serialize(pairs);
    if query.is_empty() {
        return url.to_owned();
    }
    let separator = if url.contains('?') { '&' } else { '?' };
    format!("{url}{separator}{query}")
}

/// `text` safe inside HTML text and double-quoted attributes.
pub(crate) fn html_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        let pairs = parse("a=1&b=x+y%2Bz&c=%E2%9C%93&d&e=%zz");
        assert_eq!(
            pairs,
            vec![
                ("a".into(), "1".into()),
                ("b".into(), "x y+z".into()),
                ("c".into(), "\u{2713}".into()),
                ("d".into(), "".into()),
                ("e".into(), "%zz".into()),
            ]
        );
        let body = serialize([("redirect_uri", "http://h/cb?x=1 2"), ("s", "a&b")]);
        assert_eq!(body, "redirect_uri=http%3A%2F%2Fh%2Fcb%3Fx%3D1%202&s=a%26b");
        assert_eq!(parse(&body)[0].1, "http://h/cb?x=1 2");
        assert_eq!(
            with_query("http://h/cb?x=1", [("a", "b")]),
            "http://h/cb?x=1&a=b"
        );
        assert_eq!(with_query("http://h/cb", [("a", "b")]), "http://h/cb?a=b");
        assert_eq!(
            html_escape("<a href=\"x\">&'"),
            "&lt;a href=&quot;x&quot;&gt;&amp;&#39;"
        );
    }
}
