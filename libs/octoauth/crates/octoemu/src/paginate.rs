//! GitHub's REST pagination, for every list route: `per_page` (default 30,
//! silently capped at 100) and `page` (from 1) in the query, and a `Link`
//! header with `prev`, `next`, `last` and `first`.
//!
//! Documented: <https://docs.github.com/en/rest/using-the-rest-api/using-pagination-in-the-rest-api>.
//!
//! ```text
//! let page = Pagination::of(&uri);
//! let paged = page.apply(items);           // this page's items, plus totals
//! let link = paged.link(&origin, &uri);    // None when everything fits one page
//! response::json_with_link(StatusCode::OK, &paged.items, link)
//! ```

use axum::http::Uri;

/// `per_page` when the query has none.
pub(crate) const DEFAULT_PER_PAGE: usize = 30;

/// The largest `per_page`; larger values silently become this.
pub(crate) const MAX_PER_PAGE: usize = 100;

/// The page a request asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Pagination {
    /// From 1.
    pub(crate) page: usize,
    /// 1 to [`MAX_PER_PAGE`].
    pub(crate) per_page: usize,
}

impl Pagination {
    /// Reads `page` and `per_page` from `uri`'s query. Missing, zero or
    /// malformed values fall back to the defaults (page 1, 30 per page);
    /// `per_page` above 100 becomes 100.
    pub(crate) fn of(uri: &Uri) -> Self {
        let mut page = 1;
        let mut per_page = DEFAULT_PER_PAGE;
        for (name, value) in query_pairs(uri) {
            let Ok(number) = value.parse::<usize>() else {
                continue;
            };
            match name {
                "page" if number > 0 => page = number,
                "per_page" if number > 0 => per_page = number.min(MAX_PER_PAGE),
                _ => {}
            }
        }
        Self { page, per_page }
    }

    /// This page of `items`.
    pub(crate) fn apply<T>(self, items: Vec<T>) -> Paged<T> {
        let total = items.len();
        let items = items
            .into_iter()
            .skip((self.page - 1).saturating_mul(self.per_page))
            .take(self.per_page)
            .collect();
        Paged {
            items,
            total,
            pagination: self,
        }
    }
}

/// One page of a list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Paged<T> {
    /// The page's items.
    pub(crate) items: Vec<T>,
    /// Items across all pages.
    pub(crate) total: usize,
    pub(crate) pagination: Pagination,
}

impl<T> Paged<T> {
    /// The last page's number (1 for an empty list).
    pub(crate) fn last_page(&self) -> usize {
        self.total.div_ceil(self.pagination.per_page).max(1)
    }

    /// The `Link` header value, in GitHub's order (`prev`, `next`, `last`,
    /// `first`), with absolute URLs on `origin` (`http://host:port`) that
    /// keep `uri`'s other query parameters. `None` when the list fits on the
    /// first page.
    pub(crate) fn link(&self, origin: &str, uri: &Uri) -> Option<String> {
        let page = self.pagination.page;
        let last = self.last_page();
        if page == 1 && last == 1 {
            return None;
        }
        let url = |number: usize| {
            let mut query: Vec<String> = query_pairs(uri)
                .filter(|(name, _)| *name != "page")
                .map(|(name, value)| format!("{name}={value}"))
                .collect();
            query.push(format!("page={number}"));
            format!("<{origin}{}?{}>", uri.path(), query.join("&"))
        };
        let mut rels = Vec::new();
        if page > 1 {
            rels.push(format!("{}; rel=\"prev\"", url((page - 1).min(last))));
        }
        if page < last {
            rels.push(format!("{}; rel=\"next\"", url(page + 1)));
            rels.push(format!("{}; rel=\"last\"", url(last)));
        }
        if page > 1 {
            rels.push(format!("{}; rel=\"first\"", url(1)));
        }
        Some(rels.join(", "))
    }
}

/// `name=value` pairs of `uri`'s query, undecoded.
fn query_pairs(uri: &Uri) -> impl Iterator<Item = (&str, &str)> {
    uri.query()
        .unwrap_or_default()
        .split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| pair.split_once('=').unwrap_or((pair, "")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn uri(s: &str) -> Uri {
        s.parse().unwrap()
    }

    #[test]
    fn reads_defaults_and_caps() {
        let of = |s: &str| Pagination::of(&uri(s));
        assert_eq!(
            of("/x"),
            Pagination {
                page: 1,
                per_page: 30
            }
        );
        assert_eq!(
            of("/x?per_page=500&page=3"),
            Pagination {
                page: 3,
                per_page: 100
            }
        );
        assert_eq!(
            of("/x?per_page=0&page=abc"),
            Pagination {
                page: 1,
                per_page: 30
            }
        );
    }

    #[test]
    fn pages_and_links() {
        let request = uri("/installation/repositories?per_page=2&page=2&sort=x");
        let paged = Pagination::of(&request).apply((1..=5).collect::<Vec<_>>());
        assert_eq!(paged.items, vec![3, 4]);
        assert_eq!(paged.total, 5);
        assert_eq!(paged.last_page(), 3);
        let o = "http://127.0.0.1:1/installation/repositories?per_page=2&sort=x";
        assert_eq!(
            paged.link("http://127.0.0.1:1", &request).unwrap(),
            format!(
                "<{o}&page=1>; rel=\"prev\", <{o}&page=3>; rel=\"next\", \
                 <{o}&page=3>; rel=\"last\", <{o}&page=1>; rel=\"first\""
            )
        );

        let first = Pagination::of(&uri("/r?per_page=2")).apply(vec![1, 2, 3]);
        assert_eq!(
            first.link("http://h", &uri("/r?per_page=2")).unwrap(),
            "<http://h/r?per_page=2&page=2>; rel=\"next\", <http://h/r?per_page=2&page=2>; rel=\"last\""
        );
        let last = Pagination::of(&uri("/r?per_page=2&page=2")).apply(vec![1, 2, 3]);
        assert_eq!(last.items, vec![3]);
        assert_eq!(
            last.link("http://h", &uri("/r?per_page=2&page=2")).unwrap(),
            "<http://h/r?per_page=2&page=1>; rel=\"prev\", <http://h/r?per_page=2&page=1>; rel=\"first\""
        );
        let single = Pagination::of(&uri("/r")).apply(vec![1, 2, 3]);
        assert_eq!(single.link("http://h", &uri("/r")), None);
        let beyond = Pagination::of(&uri("/r?page=9")).apply(vec![1]);
        assert!(beyond.items.is_empty());
        assert!(beyond.link("http://h", &uri("/r?page=9")).is_some());
    }
}
