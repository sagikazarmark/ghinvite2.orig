//! Dioxus 0.7 SSR renderer.

use dioxus::prelude::*;

/// Render a Dioxus component to a complete HTML string. Uses dioxus-ssr's
/// pre-rendering — no client-side hydration is wired in Plan 4.
///
/// The component receives no props; pass shared state through the closure.
pub fn render<F>(component: F) -> String
where
    F: 'static + Clone + Fn() -> Element + Send,
{
    let mut vdom = VirtualDom::new_with_props(component, ());
    vdom.rebuild_in_place();
    dioxus_ssr::render(&vdom)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hello() -> Element {
        rsx! { div { "hello world" } }
    }

    #[test]
    fn render_produces_html() {
        let html = render(hello);
        assert!(html.contains("<div"));
        assert!(html.contains("hello world"));
    }
}
