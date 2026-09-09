//! Dioxus view components for ghinvite.
//!
//! Every page is a plain `rsx!` component that takes its data as props and
//! returns markup; nothing here touches a request, a session, storage, or
//! GitHub. Today `ghinvite-web` renders these on the server with `dioxus_ssr`;
//! per ADR 0001 this is also the only workspace crate a browser-side Dioxus
//! island may depend on.
//!
//! Elsewhere in the workspace `cfg(target_arch = "wasm32")` means "Cloudflare
//! Workers", so this crate is built for the browser on its own:
//! `cargo check -p ghinvite-ui --target wasm32-unknown-unknown`, never through
//! `--workspace --target wasm32-unknown-unknown` (feature unification would
//! drag Worker-only dependencies into the browser build and vice versa).

pub mod audit;
pub mod components;
pub mod console;
pub mod flash;
pub mod forms;
pub mod home;
pub mod invitation;
pub mod layouts;
pub mod links;
pub mod not_found;
pub mod requests;
pub mod settings;

/// Test-only SSR helper so the rendered-markup tests in this crate can assert
/// on HTML. The production renderer is `ghinvite_web::views::render`; keeping
/// `dioxus-ssr` a dev-dependency keeps it out of the browser build.
#[cfg(test)]
pub(crate) mod testing {
    use dioxus::prelude::*;

    pub fn render<F>(component: F) -> String
    where
        F: 'static + Clone + Fn() -> Element + Send,
    {
        let mut vdom = VirtualDom::new_with_props(component, ());
        vdom.rebuild_in_place();
        dioxus_ssr::render(&vdom)
    }
}
