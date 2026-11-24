//! Snapshot normalisation: what differs between two runs of the same
//! scenario (IDs, timestamps, tokens, GUIDs, origins, the world's names)
//! becomes a placeholder, so a live recording and an emulator run compare.

use regex::{Captures, Regex};
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::sync::LazyLock;

/// The response headers the fidelity suite relies on, and so records.
pub const RELIED_HEADERS: &[&str] = &[
    "content-type",
    "link",
    "location",
    "retry-after",
    "x-accepted-github-permissions",
    "x-github-sso",
    "x-ratelimit-remaining",
    "x-ratelimit-resource",
];

/// Fields describing an account's, repository's or App's profile and
/// counts: whatever the live world happens to hold, not behaviour (the
/// emulator's world installs the fidelity App on three organizations, the
/// live one on as many as the operator set up). Their value becomes
/// `<varies>`; whether they are there is still compared.
pub const VARIES: &[&str] = &[
    "bio",
    "blog",
    "collaborators",
    "company",
    "description",
    "disk_usage",
    "email",
    "followers",
    "following",
    "forks",
    "forks_count",
    "hireable",
    "homepage",
    "installations_count",
    "language",
    "location",
    "members_count",
    "name",
    "network_count",
    "open_issues",
    "open_issues_count",
    "owned_private_repos",
    "private_gists",
    "public_gists",
    "public_repos",
    "repos_count",
    "size",
    "stargazers_count",
    "subscribers_count",
    "topics",
    "total_private_repos",
    "twitter_username",
    "watchers",
    "watchers_count",
];

static JWT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"eyJ[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+\.[A-Za-z0-9_-]*").expect("a valid regex")
});
static TOKEN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b(gh[pousr])_[A-Za-z0-9_.-]+").expect("a valid regex"));
static FINE_GRAINED: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\bgithub_pat_[A-Za-z0-9_]+").expect("a valid regex"));
static GUID: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}\b")
        .expect("a valid regex")
});
static TIMESTAMP: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(\.\d+)?(Z|[+-]\d{2}:?\d{2})?")
        .expect("a valid regex")
});
static ORIGIN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"https?://(127\.0\.0\.1|localhost|\[::1\])(:\d+)?|https://(api\.|uploads\.|avatars\.)?github\.com|https://avatars\.githubusercontent\.com",
    )
    .expect("a valid regex")
});
static GIT_ORIGIN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"git://[^/\s]+|git@[^:\s]+:").expect("a valid regex"));
static EMAIL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}").expect("a valid regex")
});

/// Turns run-specific values into placeholders.
#[derive(Clone, Debug, Default)]
pub struct Normaliser {
    /// `(name, placeholder)`, longest name first; matched ignoring case, as
    /// a whole login or slug.
    names: Vec<(Regex, String)>,
    /// Known numeric IDs and their placeholders (`<repo-id>`).
    ids: BTreeMap<u64, String>,
    /// Extra origins (the emulator's own) that become `<origin>`.
    origins: Vec<String>,
}

impl Normaliser {
    /// A normaliser with nothing known but the standard patterns.
    pub fn new() -> Self {
        Self::default()
    }

    /// `name` (a login, slug or repository name) becomes `<placeholder>`.
    #[must_use]
    pub fn name(mut self, name: &str, placeholder: &str) -> Self {
        if name.is_empty() {
            return self;
        }
        let pattern = format!(
            r"(?i)(^|[^A-Za-z0-9_.-]){}($|[^A-Za-z0-9_-])",
            regex::escape(name)
        );
        let regex = Regex::new(&pattern).expect("an escaped name is a valid regex");
        self.names.push((regex, format!("<{placeholder}>")));
        // Longest name first, so `acme-team` is not caught by `acme`.
        self.names
            .sort_by_key(|(regex, _)| std::cmp::Reverse(regex.as_str().len()));
        self
    }

    /// The numeric ID `id` becomes `<placeholder>` under ID keys and in URL
    /// paths.
    #[must_use]
    pub fn id(mut self, id: u64, placeholder: &str) -> Self {
        if id != 0 {
            self.ids.insert(id, format!("<{placeholder}>"));
        }
        self
    }

    /// `origin` (e.g. the emulator's `http://127.0.0.1:41234`) becomes
    /// `<origin>`.
    #[must_use]
    pub fn origin(mut self, origin: &str) -> Self {
        self.origins.push(origin.trim_end_matches('/').to_owned());
        self
    }

    /// A normalised copy of a JSON body.
    pub fn value(&self, value: &Value) -> Value {
        self.walk(None, value)
    }

    fn id_placeholder(&self, number: u64) -> Value {
        Value::String(
            self.ids
                .get(&number)
                .cloned()
                .unwrap_or_else(|| "<id>".to_owned()),
        )
    }

    fn walk(&self, key: Option<&str>, value: &Value) -> Value {
        let key_is = |predicate: &dyn Fn(&str) -> bool| key.is_some_and(predicate);
        if key_is(&|k| VARIES.contains(&k)) {
            return Value::String("<varies>".into());
        }
        match value {
            Value::Object(object) => {
                let mut out = Map::new();
                for (name, field) in object {
                    out.insert(name.clone(), self.walk(Some(name), field));
                }
                Value::Object(out)
            }
            Value::Array(items) => Value::Array(
                items
                    .iter()
                    // Array items inherit their key (`repository_ids`).
                    .map(|item| self.walk(key, item))
                    .collect(),
            ),
            Value::Number(number) => {
                if key_is(&|k| k == "id" || k.ends_with("_id") || k.ends_with("_ids")) {
                    match number.as_u64() {
                        Some(id) => self.id_placeholder(id),
                        None => Value::String("<id>".into()),
                    }
                } else if key_is(&|k| k.ends_with("_at")) {
                    Value::String("<timestamp>".into())
                } else if key_is(&|k| k == "duration") {
                    Value::String("<duration>".into())
                } else {
                    value.clone()
                }
            }
            Value::String(text) => {
                if key_is(&|k| k == "node_id") {
                    Value::String("<node-id>".into())
                } else if key_is(&|k| k == "avatar_url") {
                    Value::String("<avatar-url>".into())
                } else if key_is(&|k| k == "id" || k.ends_with("_id"))
                    && !text.is_empty()
                    && text.bytes().all(|b| b.is_ascii_digit())
                {
                    text.parse().map_or_else(
                        |_| Value::String("<id>".into()),
                        |id| self.id_placeholder(id),
                    )
                } else {
                    Value::String(self.text(text))
                }
            }
            Value::Bool(_) | Value::Null => value.clone(),
        }
    }

    /// A normalised copy of free text (a string field, a header, a request
    /// line).
    pub fn text(&self, text: &str) -> String {
        let mut out = JWT.replace_all(text, "<jwt>").into_owned();
        out = TOKEN
            .replace_all(&out, |caps: &Captures<'_>| format!("<{}-token>", &caps[1]))
            .into_owned();
        out = FINE_GRAINED.replace_all(&out, "<github-pat>").into_owned();
        out = GUID.replace_all(&out, "<guid>").into_owned();
        out = TIMESTAMP.replace_all(&out, "<timestamp>").into_owned();
        for origin in &self.origins {
            out = out.replace(origin.as_str(), "<origin>");
        }
        out = ORIGIN.replace_all(&out, "<origin>").into_owned();
        out = GIT_ORIGIN
            .replace_all(&out, |caps: &Captures<'_>| {
                if caps[0].starts_with("git@") {
                    "<ssh-origin>:".to_owned()
                } else {
                    "<git-origin>".to_owned()
                }
            })
            .into_owned();
        // Before the names, whose placeholders would end a URL early.
        out = self.url_ids(&out);
        out = EMAIL.replace_all(&out, "<email>").into_owned();
        for (regex, placeholder) in &self.names {
            // Twice, so adjacent matches sharing a separator both go.
            for _ in 0..2 {
                out = regex
                    .replace_all(&out, |caps: &Captures<'_>| {
                        format!("{}{placeholder}{}", &caps[1], &caps[2])
                    })
                    .into_owned();
            }
        }
        out
    }

    /// Numeric path segments of `<origin>` URLs: a known ID's placeholder,
    /// else `<n>`.
    fn url_ids(&self, text: &str) -> String {
        if !text.contains("<origin>/") {
            return text.to_owned();
        }
        let mut out = String::with_capacity(text.len());
        for (index, part) in text.split("<origin>").enumerate() {
            if index > 0 {
                out.push_str("<origin>");
            }
            if index == 0 {
                out.push_str(part);
                continue;
            }
            // The URL runs to the first character that cannot be in one.
            let end = part
                .find(|c: char| c.is_whitespace() || matches!(c, '>' | '"' | ',' | ';'))
                .unwrap_or(part.len());
            let (url, rest) = part.split_at(end);
            let (path, query) = match url.find(['?', '#']) {
                Some(at) => url.split_at(at),
                None => (url, ""),
            };
            let path: Vec<String> = path
                .split('/')
                .map(|segment| {
                    if !segment.is_empty() && segment.bytes().all(|b| b.is_ascii_digit()) {
                        segment
                            .parse::<u64>()
                            .ok()
                            .and_then(|id| self.ids.get(&id).cloned())
                            .unwrap_or_else(|| "<n>".to_owned())
                    } else {
                        segment.to_owned()
                    }
                })
                .collect();
            out.push_str(&path.join("/"));
            out.push_str(query);
            out.push_str(rest);
        }
        out
    }

    /// A normalised request path (with its query): the names, and numeric
    /// segments as known IDs or `<n>`.
    pub fn path(&self, path: &str) -> String {
        let text = self.text(&format!("<origin>{path}"));
        text.strip_prefix("<origin>").unwrap_or(&text).to_owned()
    }

    /// A relied-on header's normalised value. The rate-limit budget left
    /// varies with every run, except when it is spent.
    pub fn header(&self, name: &str, value: &str) -> String {
        match name {
            "x-ratelimit-remaining" if value.trim() != "0" => "<n>".into(),
            _ => self.text(value),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn normaliser() -> Normaliser {
        Normaliser::new()
            .name("acme", "org")
            .name("acme-widgets", "repo")
            .name("octocat", "invitee")
            .id(42, "repo-id")
            .origin("http://127.0.0.1:41234")
    }

    #[test]
    fn ids_timestamps_tokens_and_names_become_placeholders() {
        let body = json!({
            "id": 42,
            "node_id": "R_kgDOabc",
            "installation_id": 7,
            "repository_ids": [42, 43],
            "created_at": "2026-11-18T10:00:00Z",
            "delivered_at": 1_763_000_000,
            "token": format!("ghs_{}", "a".repeat(40)),
            "jwt": "eyJhbGciOiJSUzI1NiJ9.eyJpc3MiOjF9.c2ln",
            "guid": "0b989ba4-242f-11e5-81e1-c7b6966d2516",
            "html_url": "https://github.com/acme/acme-widgets",
            "url": "http://127.0.0.1:41234/repos/acme/acme-widgets/invitations/42/x/99?page=2",
            "avatar_url": "https://avatars.githubusercontent.com/u/1?v=4",
            "login": "OctoCat",
            "bot": "acme[bot]",
            "email": "octocat@example.com",
            "contact": "write to octocat@example.com",
            "count": 3,
            "followers": 12,
            "name": null,
        });
        assert_eq!(
            normaliser().value(&body),
            json!({
                "id": "<repo-id>",
                "node_id": "<node-id>",
                "installation_id": "<id>",
                "repository_ids": ["<repo-id>", "<id>"],
                "created_at": "<timestamp>",
                "delivered_at": "<timestamp>",
                "token": "<ghs-token>",
                "jwt": "<jwt>",
                "guid": "<guid>",
                "html_url": "<origin>/<org>/<repo>",
                "url": "<origin>/repos/<org>/<repo>/invitations/<repo-id>/x/<n>?page=2",
                "avatar_url": "<avatar-url>",
                "login": "<invitee>",
                "bot": "<org>[bot]",
                "email": "<varies>",
                "contact": "write to <email>",
                "count": 3,
                "followers": "<varies>",
                "name": "<varies>",
            })
        );
    }

    #[test]
    fn link_headers_and_the_rate_limit_budget() {
        let normaliser = normaliser();
        assert_eq!(
            normaliser.header(
                "link",
                "<http://127.0.0.1:41234/repositories/42/invitations?page=2>; rel=\"next\""
            ),
            "<<origin>/repositories/<repo-id>/invitations?page=2>; rel=\"next\""
        );
        assert_eq!(normaliser.header("x-ratelimit-remaining", "4999"), "<n>");
        assert_eq!(normaliser.header("x-ratelimit-remaining", "0"), "0");
        assert_eq!(
            normaliser.path("/repos/acme/acme-widgets/invitations/7?per_page=100"),
            "/repos/<org>/<repo>/invitations/<n>?per_page=100"
        );
        assert_eq!(
            normaliser.path("/repositories/42"),
            "/repositories/<repo-id>"
        );
        // A name inside a longer word stays.
        assert_eq!(normaliser.text("acmeish acme"), "acmeish <org>");
    }
}
