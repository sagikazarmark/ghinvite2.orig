//! GitHub pagination semantics: the `Link` response header.
//!
//! A listing that stops at the first page is not a shorter listing — it is an
//! incomplete observation. Callers that read absence as evidence (an invitation
//! that is no longer pending) must be able to tell "walked every page" from
//! "stopped early", so this module only ever reports a next page or nothing,
//! and refuses to follow a link that leaves the configured API base.
//! [`walk_link_pages_with`] is the walk every `Link`-paged listing shares: it
//! reads to the last page or fails, never returning what it read so far.

use crate::transport::Response;
use std::collections::HashSet;

/// Backstop on one `Link` walk. At 100 per page this is far past any real
/// listing; exceeding it is a paging fault, not a complete list.
pub(crate) const MAX_PAGES: usize = 1000;

/// A `Link` header that cannot be trusted to mean "no next page". Carries no
/// part of the header: it is upstream text and may hold a credential.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PagingFault;

/// The walk every `Link`-paged listing shares. Typed errors from fetch/absorb are
/// preserved; pagination faults use the caller's fixed, secret-free error.
pub(crate) async fn walk_link_pages_with<Fut, E>(
    first: String,
    base_url: &str,
    max_pages: usize,
    mut fetch: impl FnMut(String) -> Fut,
    mut absorb: impl FnMut(Response) -> std::result::Result<(), E>,
    paging_error: impl Fn() -> E,
) -> std::result::Result<(), E>
where
    Fut: Future<Output = std::result::Result<Response, E>>,
{
    let mut path = first;
    // Every page already walked. A cycle of any length — not just a link back
    // to the page in hand — is a paging fault, and catching it here spends one
    // request on it instead of the whole page budget.
    let mut walked = HashSet::new();
    for _ in 0..max_pages {
        if !walked.insert(path.clone()) {
            // `path` came out of an upstream `Link` header; the fault is the
            // revisit, which needs none of its text to state.
            return Err(paging_error());
        }
        let resp = fetch(path).await?;
        let next = next_page_path(&resp, base_url).map_err(|_| paging_error())?;
        absorb(resp)?;
        match next {
            Some(next) => path = next,
            None => return Ok(()),
        }
    }
    Err(paging_error())
}

/// The `rel="next"` target of a `Link` header, as a path under `base_url`.
///
/// Returns `Ok(None)` on the last page: no `Link` header, or one that parses
/// and carries no `next` relation. Errors when the header cannot be trusted to
/// mean "no next page" — unparseable, or pointing off the API base — because
/// silently stopping there would fabricate absence. Whether the target advances
/// is the caller's business: only it knows which pages it has already walked.
pub(crate) fn next_page_path(
    response: &Response,
    base_url: &str,
) -> Result<Option<String>, PagingFault> {
    let Some(link) = response.headers.get("link") else {
        return Ok(None);
    };
    let Some(next) = next_relation(link)? else {
        return Ok(None);
    };
    let Some(path) = next.strip_prefix(base_url).filter(|p| p.starts_with('/')) else {
        // The rejected target is whatever answered us, so none of it is kept:
        // a gateway can put a credential in a query string as readily as in a
        // body, and this error reaches logs and Restate terminal errors.
        return Err(PagingFault);
    };
    Ok(Some(path.to_owned()))
}

/// Pull the `rel="next"` URL out of an RFC 8288 `Link` field value, e.g.
/// `<https://api.github.com/…&page=2>; rel="next", <…&page=9>; rel="last"`.
/// Splitting on the angle brackets rather than on commas keeps URLs that
/// themselves contain commas intact.
///
/// A header that carries no parseable link at all — no brackets, or one left
/// unclosed — is an error rather than `None`: it may well have announced a next
/// page we could not read, and reading it as the last page would turn a
/// truncated walk into confirmed absence.
fn next_relation(header: &str) -> Result<Option<&str>, PagingFault> {
    // Same reasoning as the off-base case: the header is upstream text.
    let malformed = || PagingFault;
    let mut rest = header.trim();
    let mut next = None;
    loop {
        let after = rest.strip_prefix('<').ok_or_else(malformed)?;
        let close = after.find('>').ok_or_else(malformed)?;
        let url = &after[..close];
        let tail = &after[close + 1..];
        let mut quoted = false;
        let mut split = tail.len();
        for (index, ch) in tail.char_indices() {
            if ch == '"' {
                quoted = !quoted;
            }
            if ch == ',' && !quoted {
                split = index;
                break;
            }
            if ch == '<' && !quoted {
                return Err(malformed());
            }
        }
        if quoted {
            return Err(malformed());
        }
        let params = &tail[..split];
        if !params.trim().is_empty() && !params.trim_start().starts_with(';') {
            return Err(malformed());
        }
        // Reject malformed parameter values rather than accidentally treating
        // an unreadable `rel=next` as the end of an authoritative listing.
        for parameter in params.split(';').skip(1) {
            let parameter = parameter.trim();
            if parameter.is_empty() {
                return Err(malformed());
            }
            let Some((name, value)) = parameter.split_once('=') else {
                if parameter
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
                {
                    continue;
                }
                return Err(malformed());
            };
            if name.trim().is_empty() {
                return Err(malformed());
            }
            let value = value.trim();
            if value.starts_with('"') {
                if value.len() < 2
                    || !value.ends_with('"')
                    || value[1..value.len() - 1].contains('"')
                {
                    return Err(malformed());
                }
            } else if value.is_empty()
                || value.chars().any(char::is_whitespace)
                || value.contains('"')
            {
                return Err(malformed());
            }
        }
        if is_next(params) && next.replace(url).is_some() {
            return Err(malformed());
        }
        if split == tail.len() {
            return Ok(next);
        }
        rest = tail[split + 1..].trim();
        if rest.is_empty() {
            return Err(malformed());
        }
    }
}

/// Is `next` among this link's relations? Parameter names and relation types
/// are case-insensitive, and `rel` may hold a space-separated list (RFC 8288
/// §3.3), so `rel="NEXT last"` names a next page just as `rel="next"` does.
/// Reading either as "no next page" would truncate the walk.
fn is_next(params: &str) -> bool {
    params.split(';').any(|param| {
        let Some((name, value)) = param.split_once('=') else {
            return false;
        };
        if !name.trim().eq_ignore_ascii_case("rel") {
            return false;
        }
        // The trailing `,` belongs to the comma separating this link from the
        // next one in the header, not to the parameter value.
        value
            .trim()
            .trim_end_matches(',')
            .trim()
            .trim_matches('"')
            .split_whitespace()
            .any(|relation| relation.eq_ignore_ascii_case("next"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn response(link: Option<&str>) -> Response {
        let mut headers = BTreeMap::new();
        if let Some(link) = link {
            headers.insert("link".into(), link.into());
        }
        Response {
            status: 200,
            headers,
            body: b"[]".to_vec(),
        }
    }

    #[test]
    fn no_link_header_is_the_last_page() {
        assert_eq!(
            next_page_path(&response(None), "https://api.github.test").unwrap(),
            None
        );
    }

    #[test]
    fn link_without_a_next_relation_is_the_last_page() {
        let link = "<https://api.github.test/x?page=1>; rel=\"prev\", \
                    <https://api.github.test/x?page=1>; rel=\"first\"";
        assert_eq!(
            next_page_path(&response(Some(link)), "https://api.github.test").unwrap(),
            None
        );
    }

    #[test]
    fn next_relation_wins_over_other_relations() {
        let link = "<https://api.github.test/x?page=1>; rel=\"prev\", \
                    <https://api.github.test/x?page=3>; rel=\"next\", \
                    <https://api.github.test/x?page=9>; rel=\"last\"";
        assert_eq!(
            next_page_path(&response(Some(link)), "https://api.github.test").unwrap(),
            Some("/x?page=3".into())
        );
    }

    #[test]
    fn unquoted_rel_is_accepted() {
        let link = "<https://api.github.test/x?page=3>; rel=next";
        assert_eq!(
            next_page_path(&response(Some(link)), "https://api.github.test").unwrap(),
            Some("/x?page=3".into())
        );
    }

    #[test]
    fn a_url_containing_a_comma_survives_parsing() {
        let link = "<https://api.github.test/x?ids=1,2&page=3>; rel=\"next\"";
        assert_eq!(
            next_page_path(&response(Some(link)), "https://api.github.test").unwrap(),
            Some("/x?ids=1,2&page=3".into())
        );
    }

    /// A `Link` header is upstream text exactly like a body is, and whatever
    /// answers our request chooses it. Neither the rejected target nor the
    /// header it came in may survive into an error that reaches logs or a
    /// Restate terminal error.
    #[test]
    fn a_credential_bearing_link_header_never_reaches_the_error() {
        let token = "installation-token-test-only-not-a-credential";
        let cases = [
            // Off-base target: the URL is the attacker's.
            format!("<https://evil.test/x?token={token}>; rel=\"next\""),
            // Unparseable header: no brackets to cut it down to a URL.
            format!("rel=next token={token}"),
            // Unclosed link: the whole header is suspect.
            format!("<https://api.github.test/x?token={token}; rel=\"next\""),
        ];
        for link in cases {
            let err =
                next_page_path(&response(Some(&link)), "https://api.github.test").unwrap_err();
            let rendered = format!("{err:?}");
            assert!(!rendered.contains(token), "{rendered}");
            assert!(!rendered.contains("evil.test"), "{rendered}");
        }
    }

    #[test]
    fn off_base_next_link_is_an_error() {
        let link = "<https://evil.test/x?page=3>; rel=\"next\"";
        let err = next_page_path(&response(Some(link)), "https://api.github.test").unwrap_err();
        assert_eq!(err, PagingFault);
    }

    #[test]
    fn host_prefix_alone_does_not_make_a_link_on_base() {
        let link = "<https://api.github.test.evil.test/x?page=3>; rel=\"next\"";
        let err = next_page_path(&response(Some(link)), "https://api.github.test").unwrap_err();
        assert_eq!(err, PagingFault);
    }

    #[test]
    fn a_rel_list_containing_next_is_a_next_link() {
        let link = "<https://api.github.test/x?page=3>; rel=\"next last\"";
        assert_eq!(
            next_page_path(&response(Some(link)), "https://api.github.test").unwrap(),
            Some("/x?page=3".into())
        );
    }

    #[test]
    fn relation_and_parameter_names_are_case_insensitive() {
        let link = "<https://api.github.test/x?page=3>; REL=\"NEXT\"";
        assert_eq!(
            next_page_path(&response(Some(link)), "https://api.github.test").unwrap(),
            Some("/x?page=3".into())
        );
    }

    #[test]
    fn a_parameter_that_merely_starts_with_rel_is_not_a_relation() {
        let link = "<https://api.github.test/x?page=3>; relation=\"next\"";
        assert_eq!(
            next_page_path(&response(Some(link)), "https://api.github.test").unwrap(),
            None
        );
    }

    #[test]
    fn an_unclosed_link_header_is_an_error_not_the_last_page() {
        let link = "<https://api.github.test/x?page=3; rel=\"next\"";
        let err = next_page_path(&response(Some(link)), "https://api.github.test").unwrap_err();
        assert_eq!(err, PagingFault);
    }

    #[test]
    fn a_link_header_carrying_no_link_at_all_is_an_error() {
        let err =
            next_page_path(&response(Some("rel=next")), "https://api.github.test").unwrap_err();
        assert_eq!(err, PagingFault);
    }

    #[test]
    fn an_unclosed_link_after_a_complete_one_is_an_error() {
        let link = "<https://api.github.test/x?page=1>; rel=\"prev\", <https://api.github.test/x";
        let err = next_page_path(&response(Some(link)), "https://api.github.test").unwrap_err();
        assert_eq!(err, PagingFault);
    }

    /// Walk a listing whose page `n` links to `links[n]`, recording every
    /// path fetched and every page absorbed.
    async fn walk(
        links: &[Option<&str>],
        max_pages: usize,
    ) -> (Result<(), PagingFault>, Vec<String>, usize) {
        let mut fetched = Vec::new();
        let mut absorbed = 0;
        let result = walk_link_pages_with(
            "/x?page=0".into(),
            "https://api.github.test",
            max_pages,
            |path| {
                let page: usize = path.rsplit('=').next().unwrap().parse().unwrap();
                fetched.push(path);
                let link = links[page]
                    .map(|next| format!("<https://api.github.test{next}>; rel=\"next\""));
                async move { Ok(response(link.as_deref())) }
            },
            |_| {
                absorbed += 1;
                Ok(())
            },
            || PagingFault,
        )
        .await;
        (result, fetched, absorbed)
    }

    #[tokio::test]
    async fn a_walk_reads_every_page_to_the_last() {
        let (result, fetched, absorbed) =
            walk(&[Some("/x?page=1"), Some("/x?page=2"), None], MAX_PAGES).await;
        result.unwrap();
        assert_eq!(fetched, ["/x?page=0", "/x?page=1", "/x?page=2"]);
        assert_eq!(absorbed, 3);
    }

    #[tokio::test]
    async fn a_walk_refuses_a_cycle_on_the_first_revisit() {
        let (result, fetched, _) = walk(
            &[Some("/x?page=1"), Some("/x?page=2"), Some("/x?page=0")],
            MAX_PAGES,
        )
        .await;
        let err = result.unwrap_err();
        assert_eq!(err, PagingFault);
        assert_eq!(fetched.len(), 3);
    }

    #[tokio::test]
    async fn a_walk_past_its_page_bound_is_an_error_not_a_partial_list() {
        let (result, fetched, _) = walk(&[Some("/x?page=1"), Some("/x?page=2"), None], 2).await;
        let err = result.unwrap_err();
        assert_eq!(err, PagingFault);
        assert_eq!(fetched, ["/x?page=0", "/x?page=1"]);
    }

    #[tokio::test]
    async fn a_walk_that_ends_exactly_at_its_page_bound_is_complete() {
        let (result, _, absorbed) = walk(&[Some("/x?page=1"), None], 2).await;
        result.unwrap();
        assert_eq!(absorbed, 2);
    }
}
