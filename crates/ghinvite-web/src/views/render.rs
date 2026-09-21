//! Dioxus 0.7 SSR renderer.

use dioxus::prelude::*;

/// Render a Dioxus component to a complete HTML document. Uses dioxus-ssr's
/// pre-rendering — no client-side hydration.
///
/// The component is a layout from `ghinvite_ui::layouts`, which renders a
/// `<head>` and a `<body>`; the doctype and the `<html>` root around them come
/// from [`ghinvite_ui::document::document_shell`].
///
/// The component receives no props; pass shared state through the closure.
pub fn render<F>(component: F) -> String
where
    F: 'static + Clone + Fn() -> Element + Send,
{
    render_with_csrf(None, component)
}

pub fn render_with_csrf<F>(token: Option<String>, component: F) -> String
where
    F: 'static + Clone + Fn() -> Element + Send,
{
    let mut vdom = VirtualDom::new_with_props(component, ());
    vdom.provide_root_context(ghinvite_ui::csrf::CsrfToken(token));
    vdom.rebuild_in_place();
    ghinvite_ui::document::document_shell(&dioxus_ssr::render(&vdom))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hello() -> Element {
        rsx! {
            head { title { "hello" } }
            body { div { "hello world" } }
        }
    }

    #[test]
    fn render_produces_html() {
        let html = render(hello);
        assert!(html.contains("<div"));
        assert!(html.contains("hello world"));
    }

    /// Browsers that see neither a doctype nor an `<html>` root fall back to
    /// quirks mode, where the layouts' viewport meta is ignored.
    #[test]
    fn render_wraps_the_page_in_the_standards_mode_document_shell() {
        let html = render(hello);

        assert!(html.starts_with("<!DOCTYPE html><html lang=\"en\">"));
        assert!(html.ends_with("</html>"));
        assert_eq!(html.matches("<html").count(), 1);
        assert_eq!(html.matches("<head>").count(), 1);
        assert_eq!(html.matches("<body>").count(), 1);
    }
}
