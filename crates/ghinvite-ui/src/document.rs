//! The HTML document shell every full page render is wrapped in.
//!
//! The layouts render a `<head>` and a `<body>` (see [`crate::layouts`]); the
//! root element and the doctype are not expressible in `rsx!`, so they are
//! joined on here. Server renderers (`ghinvite_web::views::render` and the
//! island's SSR fixture) call [`document_shell`] so every full HTML response
//! parses in standards mode and carries an explicit document language.

/// The language of every ghinvite document. All UI copy is English.
const DOCUMENT_LANGUAGE: &str = "en";

/// The standards-mode doctype. Without it browsers fall back to quirks mode,
/// where the viewport meta is ignored and box sizing changes.
const DOCTYPE: &str = "<!DOCTYPE html>";

/// Wrap server-rendered `<head>`/`<body>` markup in the document shell.
pub fn document_shell(rendered: &str) -> String {
    format!("{DOCTYPE}<html lang=\"{DOCUMENT_LANGUAGE}\">{rendered}</html>")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_opens_with_the_standards_mode_doctype() {
        let html = document_shell("<head></head><body></body>");

        assert!(html.starts_with("<!DOCTYPE html><html lang=\"en\">"));
    }

    #[test]
    fn shell_closes_a_single_root_element_around_the_rendered_markup() {
        let html = document_shell("<head><title>ghinvite</title></head><body><p>Hi</p></body>");

        assert_eq!(html.matches("<html").count(), 1);
        assert_eq!(html.matches("</html>").count(), 1);
        assert!(html.ends_with("<body><p>Hi</p></body></html>"));
        assert!(html.contains("<title>ghinvite</title>"));
    }
}
