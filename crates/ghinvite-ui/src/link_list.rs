//! Pure invitation-link selection and its server-rendered Console page.

use chrono::{DateTime, Utc};
use dioxus::prelude::*;
use ghinvite_core::InvitationLink;

/// Normalized URL state. Construct from the route's raw query strings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinkListQuery {
    filter: &'static str,
    sort: &'static str,
    direction: &'static str,
    page: usize,
}

impl LinkListQuery {
    pub fn new(filter: &str, sort: &str, direction: &str, page: &str) -> Self {
        Self {
            filter: match filter {
                "inactive" => "inactive",
                "all" => "all",
                _ => "active",
            },
            sort: match sort {
                "description" => "description",
                "uses" => "uses",
                "expiration" => "expiration",
                _ => "created",
            },
            direction: if direction == "asc" { "asc" } else { "desc" },
            // Saturate digit-only page numbers so even oversized requests reach the last page.
            page: if !page.is_empty() && page.bytes().all(|b| b.is_ascii_digit()) {
                page.bytes()
                    .fold(0usize, |n, b| {
                        n.saturating_mul(10).saturating_add((b - b'0').into())
                    })
                    .max(1)
            } else {
                1
            },
        }
    }

    pub fn filter(&self) -> &'static str {
        self.filter
    }
    pub fn sort(&self) -> &'static str {
        self.sort
    }
    pub fn direction(&self) -> &'static str {
        self.direction
    }
    pub fn page(&self) -> usize {
        self.page
    }

    fn href(&self, base: &str, page: usize) -> String {
        format!(
            "{base}?filter={}&sort={}&direction={}&page={page}",
            self.filter, self.sort, self.direction
        )
    }
}

/// One page of borrowed rows; counts distinguish an empty account from no matches.
#[derive(Debug)]
pub struct LinkListSelection<'a> {
    pub query: LinkListQuery,
    pub rows: Vec<&'a InvitationLink>,
    pub total_links: usize,
    pub matching_links: usize,
    pub total_pages: usize,
}

/// Select from the complete, already account-scoped result at one supplied instant.
/// Descriptions use case-sensitive lexical order; ties use ID ascending in either direction.
pub fn select_links<'a>(
    all_links: &'a [InvitationLink],
    query: &LinkListQuery,
    now: DateTime<Utc>,
) -> LinkListSelection<'a> {
    let mut rows: Vec<_> = all_links
        .iter()
        .filter(|link| match query.filter {
            "all" => true,
            "inactive" => !link.is_active(now),
            _ => link.is_active(now),
        })
        .collect();
    rows.sort_by(|a, b| {
        let order = match query.sort {
            "description" => a.description.cmp(&b.description),
            "uses" => a.uses_count.cmp(&b.uses_count),
            "expiration" => match (a.expires_at, b.expires_at) {
                (Some(a), Some(b)) => a.cmp(&b),
                (None, Some(_)) => return std::cmp::Ordering::Greater,
                (Some(_), None) => return std::cmp::Ordering::Less,
                (None, None) => std::cmp::Ordering::Equal,
            },
            _ => a.created_at.cmp(&b.created_at),
        };
        let order = if query.direction == "desc" {
            order.reverse()
        } else {
            order
        };
        order.then_with(|| a.id.0.cmp(&b.id.0))
    });
    let matching_links = rows.len();
    let total_pages = matching_links.div_ceil(25).max(1);
    let mut query = query.clone();
    query.page = query.page.min(total_pages);
    let rows = rows
        .into_iter()
        .skip((query.page - 1) * 25)
        .take(25)
        .collect();
    LinkListSelection {
        query,
        matching_links,
        total_links: all_links.len(),
        total_pages,
        rows,
    }
}

#[derive(Clone, PartialEq, Props)]
pub struct LinkListPageProps {
    pub signed_in_login: Option<String>,
    pub flash: Option<crate::flash::Flash>,
    pub account_login: String,
    pub query: LinkListQuery,
    /// `None` means loading failed, never an empty account.
    pub all_links: Option<Vec<InvitationLink>>,
    pub now: DateTime<Utc>,
}

#[component]
pub fn LinkListPage(props: LinkListPageProps) -> Element {
    let base = format!("/console/accounts/{}/links", props.account_login);
    let selection = props
        .all_links
        .as_ref()
        .map(|links| select_links(links, &props.query, props.now));
    rsx! {
        crate::layouts::ConsoleLayout {
            signed_in_login: props.signed_in_login.clone(),
            title: "Invitation links - {props.account_login}",
            account_login: Some(props.account_login.clone()),
            active_nav: Some("links".to_string()),
            flash: props.flash.clone(),
            children: rsx! {
                header { class: "mb-4 flex flex-col gap-2 sm:flex-row sm:items-center sm:justify-between",
                    div {
                        h1 { class: "text-xl font-semibold tracking-tight", "Invitation links" }
                        p { class: "mt-0.5 text-sm text-base-content/60", "Manage invitation links for {props.account_login}." }
                    }
                    a { class: "btn btn-primary btn-sm", href: "{base}/new", "New invitation link" }
                }
                form { method: "get", action: "{base}", class: "mac-panel mb-4 flex flex-wrap items-end gap-3 p-4",
                    {[
                        ("filter", "Filter", props.query.filter(), vec![("active", "Active"), ("inactive", "Inactive"), ("all", "All")]),
                        ("sort", "Sort by", props.query.sort(), vec![("description", "Description"), ("uses", "Uses"), ("expiration", "Expiration"), ("created", "Created")]),
                        ("direction", "Direction", props.query.direction(), vec![("asc", "Ascending"), ("desc", "Descending")]),
                    ].into_iter().map(|(name, label, value, options)| rsx! {
                        div { class: "form-control gap-2",
                            label { class: "text-sm font-medium", r#for: "link-list-{name}", "{label}" }
                            select { id: "link-list-{name}", name: "{name}", class: "select select-bordered select-sm",
                                for (option, label) in options {
                                    option { value: "{option}", selected: value == option, "{label}" }
                                }
                            }
                        }
                    })}
                    input { r#type: "hidden", name: "page", value: "1" }
                    button { r#type: "submit", class: "btn btn-primary btn-sm", "Apply" }
                }
                if selection.is_none() {
                    div { class: "alert alert-error items-start", role: "alert",
                        div {
                            h2 { class: "font-semibold", "Invitation links could not be loaded" }
                            p { class: "mt-1 text-sm", "Try again to load this account's invitation links." }
                            a { class: "btn btn-ghost btn-sm mt-3", href: props.query.href(&base, props.query.page), "Try again" }
                        }
                    }
                }
                if let Some(selection) = &selection {
                    if selection.total_links == 0 {
                        section { class: "mac-panel p-5 text-sm text-base-content/70",
                            h2 { class: "font-medium text-base-content", "No invitation links yet" }
                            p { class: "mt-1", "Create an invitation link to let GitHub users request repository access." }
                            a { class: "btn btn-primary btn-sm mt-4", href: "{base}/new", "Create first invitation link" }
                        }
                    } else if selection.matching_links == 0 {
                        section { class: "mac-panel p-5 text-sm text-base-content/70",
                            h2 { class: "font-medium text-base-content", "No invitation links match this filter" }
                            p { class: "mt-1", "Choose another filter to see your invitation links." }
                            a { class: "btn btn-ghost btn-sm mt-4", href: LinkListQuery::new("all", props.query.sort(), props.query.direction(), "1").href(&base, 1), "Show all invitation links" }
                        }
                    } else {
                        section { class: "mac-panel compact-table overflow-hidden",
                            div { class: "overflow-x-auto", tabindex: "0", role: "region", aria_label: "Invitation links table",
                                table { class: "table compact-table table-sm",
                                    thead { tr {
                                        for column in ["Description", "Status", "Uses", "Expiration", "Created"] {
                                            th { scope: "col", "{column}" }
                                        }
                                    } }
                                    tbody {
                                        {selection.rows.iter().map(|link| {
                                            let active = link.is_active(props.now);
                                            let status = if active { "active" } else { "inactive" };
                                            let badge = if active { "badge badge-success" } else { "badge badge-ghost" };
                                            let max = link.max_uses.map(|max| max.to_string()).unwrap_or_else(|| "unlimited".into());
                                            let expiration = link.expires_at.map(|date| date.format("%Y-%m-%d").to_string()).unwrap_or_else(|| "No expiration".into());
                                            let created = link.created_at.format("%Y-%m-%d").to_string();
                                            rsx! {
                                                tr {
                                                    td {
                                                        a { class: "link link-primary font-medium", href: "{base}/{link.id}", "{link.description}" }
                                                        p { class: "mt-0.5 text-xs text-base-content/60", "Invitation code: ", span { class: "font-mono", "{link.slug}" } }
                                                    }
                                                    td { span { class: "{badge}", "{status}" } }
                                                    td { class: "whitespace-nowrap tabular-nums", "{link.uses_count} / {max}" }
                                                    td { class: "whitespace-nowrap", "{expiration}" }
                                                    td { class: "whitespace-nowrap", "{created}" }
                                                }
                                            }
                                        })}
                                    }
                                }
                            }
                            nav { class: "flex flex-wrap items-center justify-between gap-3 border-t border-base-300 px-4 py-3", aria_label: "Invitation links pagination",
                                p { class: "text-sm text-base-content/65", "Page {selection.query.page} of {selection.total_pages}" }
                                div { class: "flex gap-2",
                                    if selection.query.page > 1 {
                                        a { class: "btn btn-ghost btn-sm", href: selection.query.href(&base, selection.query.page - 1), "Previous" }
                                    }
                                    if selection.query.page < selection.total_pages {
                                        a { class: "btn btn-ghost btn-sm", href: selection.query.href(&base, selection.query.page + 1), "Next" }
                                    }
                                }
                            }
                        }
                    }
                }
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{DateTime, Utc};
    use ghinvite_core::{InvitationLink, Permission, Slug};

    fn now() -> DateTime<Utc> {
        "2026-05-04T12:00:00Z".parse().unwrap()
    }

    fn link(n: u32) -> InvitationLink {
        InvitationLink {
            id: format!("{n:026}").parse().unwrap(),
            slug: Slug::from_string(format!("code{n:012}")).unwrap(),
            installation_id: 1,
            account_id: 2,
            created_by: 3,
            created_at: now() + chrono::Duration::seconds(n.into()),
            expires_at: None,
            max_uses: None,
            uses_count: 0,
            permission: Permission::Pull,
            approval_required: false,
            description: format!("Workshop {n:03}"),
            internal_note: None,
            revoked_at: None,
            revoked_by: None,
            repos: vec![],
        }
    }

    fn descriptions<'a>(selection: &LinkListSelection<'a>) -> Vec<&'a str> {
        selection
            .rows
            .iter()
            .map(|link| link.description.as_str())
            .collect()
    }

    fn render(all_links: Option<Vec<InvitationLink>>, query: LinkListQuery) -> String {
        crate::testing::render(move || {
            rsx! {
                LinkListPage {
                    signed_in_login: Some("admin".to_string()),
                    flash: None,
                    account_login: "acme".to_string(),
                    query: query.clone(),
                    all_links: all_links.clone(),
                    now: now(),
                }
            }
        })
    }

    #[test]
    fn page_renders_console_table_with_primary_description_and_domain_values() {
        let mut limited = link(2);
        limited.uses_count = 5;
        limited.max_uses = Some(5);
        limited.expires_at = Some(now());
        let html = render(
            Some(vec![link(1), limited]),
            LinkListQuery::new("all", "created", "asc", ""),
        );
        assert!(html.contains("console-frame"));
        assert!(html.contains("<h1"));
        assert!(html.contains("Invitation links"));
        assert!(html.contains("href=\"/console/accounts/acme/links/new\""));
        for column in ["Description", "Status", "Uses", "Expiration", "Created"] {
            assert!(
                html.contains(&format!("<th scope=\"col\">{column}</th>")),
                "{column}"
            );
        }
        assert!(html.contains(
            "href=\"/console/accounts/acme/links/00000000000000000000000001\">Workshop 001</a>"
        ));
        assert!(html.find("Workshop 001").unwrap() < html.find("code000000000001").unwrap());
        for text in [
            "Invitation code:",
            "active</span>",
            "inactive</span>",
            "0 / unlimited",
            "5 / 5",
            "No expiration",
            "2026-05-04",
        ] {
            assert!(html.contains(text), "{text}");
        }
        assert_eq!(html.matches("<tbody>").count(), 1);
        assert!(html.contains("overflow-x-auto"));
        assert!(html.contains("tabindex=\"0\""));
        assert!(html.contains("role=\"region\""));
        assert!(html.contains("aria-label=\"Invitation links table\""));
        assert_eq!(
            html.matches("<script").count(),
            1,
            "only the shared layout script"
        );
        assert!(!html.contains("onclick"));
    }

    #[test]
    fn native_get_controls_show_normalized_state_and_reset_page_one() {
        for (query, selected) in [
            (
                LinkListQuery::new("inactive", "expiration", "asc", "3"),
                ["inactive", "expiration", "asc"],
            ),
            (
                LinkListQuery::new("invalid", "invalid", "invalid", "3"),
                ["active", "created", "desc"],
            ),
        ] {
            let html = render(Some(vec![link(1)]), query);
            assert!(html.contains("<form method=\"get\" action=\"/console/accounts/acme/links\""));
            for (name, label) in [
                ("filter", "Filter"),
                ("sort", "Sort by"),
                ("direction", "Direction"),
            ] {
                assert!(html.contains(&format!("for=\"link-list-{name}\">{label}</label>")));
                assert!(html.contains(&format!("<select id=\"link-list-{name}\" name=\"{name}\"")));
            }
            for (value, label) in [
                ("active", "Active"),
                ("inactive", "Inactive"),
                ("all", "All"),
                ("description", "Description"),
                ("uses", "Uses"),
                ("expiration", "Expiration"),
                ("created", "Created"),
                ("asc", "Ascending"),
                ("desc", "Descending"),
            ] {
                let selected_attr = if selected.contains(&value) {
                    " selected=true"
                } else {
                    ""
                };
                assert!(
                    html.contains(&format!(
                        "<option value=\"{value}\"{selected_attr}>{label}</option>"
                    )),
                    "{value}"
                );
            }
            assert!(html.contains("<input type=\"hidden\" name=\"page\" value=\"1\"/>"));
            assert!(
                html.contains("type=\"submit\" class=\"btn btn-primary btn-sm\">Apply</button>")
            );
            assert!(!html.contains("onchange"));
            assert!(!html.contains("onsubmit"));
        }
    }

    #[test]
    fn pagination_renders_selected_rows_and_only_existing_state_preserving_neighbors() {
        let mut links: Vec<_> = (1..=80).map(link).collect();
        for row in &mut links[..52] {
            row.revoked_at = Some(now());
        }
        for (page, normalized, previous, next, first, last, count) in [
            ("1", 1, None, Some(2), "Workshop 001", "Workshop 025", 25),
            ("2", 2, Some(1), Some(3), "Workshop 026", "Workshop 050", 25),
            (
                "999999999999999999999999999999",
                3,
                Some(2),
                None,
                "Workshop 051",
                "Workshop 052",
                2,
            ),
        ] {
            let html = render(
                Some(links.clone()),
                LinkListQuery::new("inactive", "description", "asc", page),
            );
            assert!(html.contains(&format!("Page {normalized} of 3")));
            let body = html
                .split("<tbody>")
                .nth(1)
                .unwrap()
                .split("</tbody>")
                .next()
                .unwrap();
            assert_eq!(body.matches("<tr>").count(), count);
            assert!(body.contains(first));
            assert!(body.contains(last));
            assert!(!body.contains("Workshop 053"));
            for (label, page) in [("Previous", previous), ("Next", next)] {
                if let Some(page) = page {
                    assert!(html.contains(&format!("href=\"/console/accounts/acme/links?filter=inactive&#38;sort=description&#38;direction=asc&#38;page={page}\">{label}</a>")), "{label}");
                } else {
                    assert!(!html.contains(&format!(">{label}</a>")));
                }
            }
        }
        let html = render(
            Some((1..=26).map(link).collect()),
            LinkListQuery::new("bad", "bad", "bad", "0"),
        );
        assert!(html.contains("href=\"/console/accounts/acme/links?filter=active&#38;sort=created&#38;direction=desc&#38;page=2\">Next</a>"));
    }

    #[test]
    fn empty_account_and_empty_filter_have_distinct_recovery_without_false_rows() {
        let mut revoked = link(1);
        revoked.revoked_at = Some(now());
        for (links, message, absent) in [
            (
                vec![],
                "No invitation links yet",
                "No invitation links match this filter",
            ),
            (
                vec![revoked],
                "No invitation links match this filter",
                "No invitation links yet",
            ),
        ] {
            let html = render(Some(links), LinkListQuery::new("", "", "", "99"));
            assert!(html.contains(message));
            assert!(!html.contains(absent));
            assert!(html.contains("href=\"/console/accounts/acme/links/new\""));
            assert!(html.contains("name=\"filter\""));
            assert!(!html.contains("<table"));
            assert!(!html.contains("Invitation links pagination"));
            if message == "No invitation links match this filter" {
                assert!(html.contains("href=\"/console/accounts/acme/links?filter=all&#38;sort=created&#38;direction=desc&#38;page=1\">Show all invitation links</a>"));
            }
        }
    }

    #[test]
    fn load_failure_is_an_explicit_error_with_native_retry_not_an_empty_account() {
        let html = render(None, LinkListQuery::new("inactive", "uses", "asc", "2"));
        assert!(html.contains("role=\"alert\""));
        assert!(html.contains("Invitation links could not be loaded"));
        assert!(html.contains("href=\"/console/accounts/acme/links?filter=inactive&#38;sort=uses&#38;direction=asc&#38;page=2\">Try again</a>"));
        for absent in [
            "No invitation links yet",
            "No invitation links match",
            "<table",
            "Invitation links pagination",
            "Page 1",
        ] {
            assert!(!html.contains(absent), "{absent}");
        }
        assert!(html.contains("href=\"/console/accounts/acme/links/new\""));
    }

    #[test]
    fn query_normalizes_each_field_independently_and_handles_page_overflow() {
        for (filter, sort, direction, page, expected) in [
            (
                "inactive",
                "uses",
                "asc",
                "2",
                ("inactive", "uses", "asc", 2),
            ),
            (
                "all",
                "expiration",
                "desc",
                "0003",
                ("all", "expiration", "desc", 3),
            ),
            (
                "bad",
                "description",
                "bad",
                "-1",
                ("active", "description", "desc", 1),
            ),
            ("active", "bad", "asc", "0", ("active", "created", "asc", 1)),
            ("", "", "", "abc", ("active", "created", "desc", 1)),
            (
                "",
                "",
                "",
                "9999999999999999999999999999999999999999",
                ("active", "created", "desc", usize::MAX),
            ),
        ] {
            let query = LinkListQuery::new(filter, sort, direction, page);
            assert_eq!(
                (
                    query.filter(),
                    query.sort(),
                    query.direction(),
                    query.page()
                ),
                expected
            );
        }
    }

    #[test]
    fn filters_use_the_injected_instant_for_expiration_revocation_and_exhaustion() {
        let mut links: Vec<_> = (1..=7).map(link).collect();
        links[0].expires_at = Some(now());
        links[1].expires_at = Some(now() - chrono::Duration::nanoseconds(1));
        links[2].expires_at = Some(now() + chrono::Duration::nanoseconds(1));
        links[3].revoked_at = Some(now());
        links[4].max_uses = Some(2);
        links[4].uses_count = 2;
        links[5].max_uses = Some(2);
        links[5].uses_count = 3;
        links[6].uses_count = u32::MAX;
        for (filter, expected) in [
            ("active", vec!["Workshop 007", "Workshop 003"]),
            (
                "inactive",
                vec![
                    "Workshop 006",
                    "Workshop 005",
                    "Workshop 004",
                    "Workshop 002",
                    "Workshop 001",
                ],
            ),
            (
                "all",
                vec![
                    "Workshop 007",
                    "Workshop 006",
                    "Workshop 005",
                    "Workshop 004",
                    "Workshop 003",
                    "Workshop 002",
                    "Workshop 001",
                ],
            ),
        ] {
            let selected = select_links(&links, &LinkListQuery::new(filter, "", "", ""), now());
            assert_eq!(descriptions(&selected), expected, "{filter}");
        }
        let selected = select_links(
            &links,
            &LinkListQuery::new("active", "", "", ""),
            now() + chrono::Duration::nanoseconds(1),
        );
        assert_eq!(descriptions(&selected), ["Workshop 007"]);
    }

    #[test]
    fn every_sort_and_direction_has_literal_order_with_no_expiration_last() {
        let mut links: Vec<_> = (1..=4).map(link).collect();
        for (row, (description, uses, max, expiration)) in links.iter_mut().zip([
            ("Delta", 2, Some(2), Some(3)),
            ("Alpha", 10, None, None),
            ("Charlie", 1, Some(100), Some(1)),
            ("Bravo", 0, Some(1), Some(2)),
        ]) {
            row.description = description.into();
            row.uses_count = uses;
            row.max_uses = max;
            row.expires_at = expiration.map(|days| now() + chrono::Duration::days(days));
        }
        for (sort, direction, expected) in [
            ("description", "asc", ["Alpha", "Bravo", "Charlie", "Delta"]),
            (
                "description",
                "desc",
                ["Delta", "Charlie", "Bravo", "Alpha"],
            ),
            ("uses", "asc", ["Bravo", "Charlie", "Delta", "Alpha"]),
            ("uses", "desc", ["Alpha", "Delta", "Charlie", "Bravo"]),
            ("expiration", "asc", ["Charlie", "Bravo", "Delta", "Alpha"]),
            ("expiration", "desc", ["Delta", "Bravo", "Charlie", "Alpha"]),
            ("created", "asc", ["Delta", "Alpha", "Charlie", "Bravo"]),
            ("created", "desc", ["Bravo", "Charlie", "Alpha", "Delta"]),
        ] {
            let selected = select_links(
                &links,
                &LinkListQuery::new("all", sort, direction, ""),
                now(),
            );
            assert_eq!(descriptions(&selected), expected, "{sort} {direction}");
        }
    }

    #[test]
    fn ties_always_use_id_ascending_even_when_input_is_reversed() {
        let mut links: Vec<_> = (1..=3).map(link).collect();
        for row in &mut links {
            row.description = "Same purpose".into();
            row.created_at = now();
        }
        for expiration in [None, Some(now())] {
            for row in &mut links {
                row.expires_at = expiration;
            }
            for sort in ["description", "uses", "expiration", "created"] {
                for direction in ["asc", "desc"] {
                    for _ in 0..2 {
                        links.reverse();
                        let selected = select_links(
                            &links,
                            &LinkListQuery::new("all", sort, direction, ""),
                            now(),
                        );
                        let ids: Vec<_> =
                            selected.rows.iter().map(|row| row.id.to_string()).collect();
                        assert_eq!(
                            ids,
                            [
                                "00000000000000000000000001",
                                "00000000000000000000000002",
                                "00000000000000000000000003"
                            ],
                            "{sort} {direction}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn full_set_is_filtered_and_sorted_before_twenty_five_row_pages_are_clamped() {
        let mut links: Vec<_> = (1..=80).map(link).collect();
        for row in &mut links[..28] {
            row.revoked_at = Some(now());
        }
        for (page, expected_page, count, first, last) in [
            ("1", 1, 25, "Workshop 080", "Workshop 056"),
            ("2", 2, 25, "Workshop 055", "Workshop 031"),
            ("3", 3, 2, "Workshop 030", "Workshop 029"),
            (
                "99999999999999999999999999999999999999",
                3,
                2,
                "Workshop 030",
                "Workshop 029",
            ),
            ("0", 1, 25, "Workshop 080", "Workshop 056"),
            ("-10", 1, 25, "Workshop 080", "Workshop 056"),
        ] {
            let selected = select_links(
                &links,
                &LinkListQuery::new("active", "created", "desc", page),
                now(),
            );
            assert_eq!(
                (
                    selected.total_links,
                    selected.matching_links,
                    selected.total_pages
                ),
                (80, 52, 3)
            );
            assert_eq!(
                (selected.query.page(), selected.rows.len()),
                (expected_page, count)
            );
            assert_eq!(selected.rows.first().unwrap().description, first);
            assert_eq!(selected.rows.last().unwrap().description, last);
        }
        let empty = select_links(&[], &LinkListQuery::new("", "", "", "99"), now());
        assert_eq!(
            (
                empty.total_links,
                empty.matching_links,
                empty.total_pages,
                empty.query.page()
            ),
            (0, 0, 1, 1)
        );
        let no_matches = select_links(&links[..28], &LinkListQuery::new("", "", "", "99"), now());
        assert_eq!(
            (
                no_matches.total_links,
                no_matches.matching_links,
                no_matches.total_pages,
                no_matches.query.page()
            ),
            (28, 0, 1, 1)
        );
        let exact = select_links(
            &links[..25],
            &LinkListQuery::new("all", "", "", "99"),
            now(),
        );
        assert_eq!(
            (exact.total_pages, exact.query.page(), exact.rows.len()),
            (1, 1, 25)
        );
    }

    #[test]
    fn default_selection_is_active_newest_first_page_one() {
        let mut revoked = link(3);
        revoked.revoked_at = Some(now());
        let links = vec![link(1), revoked, link(2)];
        let selected = select_links(&links, &LinkListQuery::new("", "", "", ""), now());
        assert_eq!(descriptions(&selected), ["Workshop 002", "Workshop 001"]);
        assert_eq!(selected.query.filter(), "active");
        assert_eq!(selected.query.sort(), "created");
        assert_eq!(selected.query.direction(), "desc");
        assert_eq!(selected.query.page(), 1);
        assert_eq!(
            (
                selected.total_links,
                selected.matching_links,
                selected.total_pages
            ),
            (3, 2, 1)
        );
    }
}
