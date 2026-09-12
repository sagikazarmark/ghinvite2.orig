//! Request-local form authority supplied by SSR, or the island's server props.
use dioxus::prelude::*;

#[derive(Clone, Default)]
pub struct CsrfToken(pub Option<String>);

#[component]
pub fn CsrfField() -> Element {
    let token = try_consume_context::<CsrfToken>().unwrap_or_default();
    rsx! { input { r#type: "hidden", name: "csrf_token", value: token.0.unwrap_or_default() } }
}
