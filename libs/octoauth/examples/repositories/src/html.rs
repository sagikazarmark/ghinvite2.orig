//! The pages, as plain HTML strings. Every value from GitHub or the request
//! goes through [`escape`].

use crate::Repository;
use crate::session::Account;
use std::fmt::Write as _;
use std::time::Duration;

/// `text` safe in HTML text and in a double-quoted attribute.
pub fn escape(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            c => escaped.push(c),
        }
    }
    escaped
}

fn layout(app: &str, body: &str) -> String {
    let app = escape(app);
    format!(
        "<!doctype html>\n<html lang=\"en\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
         <title>{app}</title><style>{STYLE}</style></head>\
         <body><main><h1>{app}</h1>{body}</main></body></html>\n"
    )
}

const STYLE: &str = "body{font:16px/1.5 system-ui,sans-serif;margin:0;padding:2rem}\
main{max-width:40rem;margin:auto}header{display:flex;gap:.75rem;align-items:center}\
header img{border-radius:50%}header form{margin-left:auto}\
.muted{color:#666}.error{color:#b00}li small{color:#666}";

/// The signed-out home page.
pub fn signed_out(app: &str) -> String {
    layout(
        app,
        "<p>See the repositories this GitHub App can reach that you can access too.</p>\
         <p><a href=\"/login\">Sign in with GitHub</a></p>",
    )
}

/// What [`signed_in`] shows for one installation.
pub enum Listing {
    /// The repositories the installation reaches that the user can access.
    Repositories(Vec<Repository>),
    /// Why they could not be listed (a fixed code).
    Failed(String),
}

/// The signed-in home page.
pub fn signed_in(
    app: &str,
    install_url: Option<&str>,
    account: &Account,
    installations: &[(String, Listing)],
) -> String {
    let mut body = String::from("<header>");
    if let Some(avatar) = &account.avatar_url {
        let _ = write!(
            body,
            "<img src=\"{}\" alt=\"\" width=\"40\" height=\"40\">",
            escape(avatar)
        );
    }
    let _ = write!(
        body,
        "<span>Signed in as <strong>{}</strong></span>\
         <form method=\"post\" action=\"/logout\"><button>Sign out</button></form></header>\
         <p class=\"muted\">Your access as of sign-in, {}. \
         <a href=\"/login\">Re-check access</a></p>",
        escape(&account.login),
        ago(account.checked.elapsed()),
    );
    if installations.is_empty() {
        body.push_str("<p>The App is not installed on any account you can access.</p>");
    }
    for (account, listing) in installations {
        let _ = write!(body, "<section><h2>{}</h2>", escape(account));
        match listing {
            Listing::Repositories(repositories) if repositories.is_empty() => {
                body.push_str("<p class=\"muted\">No repositories you can access.</p>");
            }
            Listing::Repositories(repositories) => {
                body.push_str("<ul>");
                for repository in repositories {
                    let name = escape(&repository.full_name);
                    match &repository.html_url {
                        Some(url) => {
                            let _ = write!(body, "<li><a href=\"{}\">{name}</a>", escape(url));
                        }
                        None => {
                            let _ = write!(body, "<li>{name}");
                        }
                    }
                    if repository.private {
                        body.push_str(" <small>private</small>");
                    }
                    body.push_str("</li>");
                }
                body.push_str("</ul>");
            }
            Listing::Failed(code) => {
                let _ = write!(
                    body,
                    "<p class=\"error\">Cannot list repositories: <code>{}</code></p>",
                    escape(code)
                );
            }
        }
        body.push_str("</section>");
    }
    if let Some(url) = install_url {
        let _ = write!(
            body,
            "<p><a href=\"{}\">Install the App on an account</a></p>",
            escape(url)
        );
    }
    layout(app, &body)
}

/// A page with a message and a way back.
pub fn message(app: &str, text: &str, code: Option<&str>) -> String {
    let mut body = format!("<p>{}</p>", escape(text));
    if let Some(code) = code {
        let _ = write!(body, "<p class=\"muted\"><code>{}</code></p>", escape(code));
    }
    body.push_str("<p><a href=\"/\">Back</a></p>");
    layout(app, &body)
}

fn ago(elapsed: Duration) -> String {
    match elapsed.as_secs() / 60 {
        0 => "just now".into(),
        1 => "a minute ago".into(),
        minutes if minutes < 120 => format!("{minutes} minutes ago"),
        minutes => format!("{} hours ago", minutes / 60),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_markup_and_quotes() {
        assert_eq!(
            escape(r#"<a href="x">'&'</a>"#),
            "&lt;a href=&quot;x&quot;&gt;&#39;&amp;&#39;&lt;/a&gt;"
        );
    }

    #[test]
    fn ages() {
        assert_eq!(ago(Duration::from_secs(30)), "just now");
        assert_eq!(ago(Duration::from_secs(90)), "a minute ago");
        assert_eq!(ago(Duration::from_secs(600)), "10 minutes ago");
        assert_eq!(ago(Duration::from_secs(3 * 3600)), "3 hours ago");
    }
}
