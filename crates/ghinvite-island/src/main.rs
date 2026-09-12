//! Browser entrypoint of the island: mounts [`ghinvite_island::LinkFormIsland`]
//! onto the server-rendered container.
//!
//! Loaded by the page as `<script type="module" src="/assets/ghinvite-island.js">`
//! (a stable loader written by `scripts/build-island.sh` that imports the
//! hashed `dx` bundle). The generated JS initialises the wasm and calls
//! `main`, which:
//!
//! 1. reads the props blob (`<script type="application/json"
//!    id="link-form-props">`) and deserialises it into `LinkFormIslandProps`;
//!    if the block is missing or malformed the island logs to the console and
//!    returns — the server-rendered form stays and works;
//! 2. empties `<div id="link-form-island">`. `dioxus-web`'s non-hydrating mount
//!    **appends** to the root element (verified in the #33 spike: without this
//!    the page ends up with two forms), so the island clears the SSR markup
//!    itself. The first frame the island renders is byte-identical to what it
//!    removed (`tests/parity.rs`), so nothing visibly changes;
//! 3. marks the root `data-island="mounted"` so headless tests can wait for
//!    `#link-form-island[data-island="mounted"]` — on the container, not the
//!    form, which must stay identical to the server's;
//! 4. launches Dioxus on that root with the props as context.
//!
//! Anything typed before the wasm loads is replaced by the props values; this
//! fresh-render trade-off is recorded in ADR 0001.
//!
//! Native builds get an empty `main` so `cargo test --workspace` compiles the
//! crate; the browser code is wasm32-only.

#[cfg(not(target_arch = "wasm32"))]
fn main() {}

#[cfg(target_arch = "wasm32")]
fn main() {
    browser::mount();
}

#[cfg(target_arch = "wasm32")]
mod browser {
    use dioxus::prelude::*;
    use ghinvite_island::LinkFormIsland;
    use ghinvite_ui::link_form::{
        LINK_FORM_ISLAND_PROPS_ID, LINK_FORM_ISLAND_ROOT_ID, LinkFormIslandProps,
    };

    /// Value of `data-island` on the root once the island has taken over.
    const MOUNTED: &str = "mounted";

    pub fn mount() {
        let Some(document) = web_sys::window().and_then(|window| window.document()) else {
            return;
        };

        let props = match read_props(&document) {
            Ok(props) => props,
            Err(reason) => {
                web_sys::console::error_1(
                    &format!("ghinvite-island: not mounting, {reason}; the server-rendered form stays in place").into(),
                );
                return;
            }
        };

        let Some(root) = document.get_element_by_id(LINK_FORM_ISLAND_ROOT_ID) else {
            web_sys::console::error_1(
                &format!("ghinvite-island: not mounting, no #{LINK_FORM_ISLAND_ROOT_ID}").into(),
            );
            return;
        };

        // See the module docs: dioxus-web appends, so remove the SSR form first.
        root.set_inner_html("");
        if let Err(error) = root.set_attribute("data-island", MOUNTED) {
            web_sys::console::warn_1(&error);
        }

        dioxus::LaunchBuilder::web()
            .with_cfg(dioxus::web::Config::new().rootname(LINK_FORM_ISLAND_ROOT_ID))
            .with_context(props)
            .launch(Root);
    }

    /// The page's props blob, or why it cannot be used.
    fn read_props(document: &web_sys::Document) -> Result<LinkFormIslandProps, String> {
        let blob = document
            .get_element_by_id(LINK_FORM_ISLAND_PROPS_ID)
            .ok_or_else(|| format!("no #{LINK_FORM_ISLAND_PROPS_ID} props block"))?;
        let text = blob
            .text_content()
            .ok_or_else(|| format!("#{LINK_FORM_ISLAND_PROPS_ID} is empty"))?;
        serde_json::from_str(&text).map_err(|error| format!("props blob is not valid: {error}"))
    }

    #[component]
    fn Root() -> Element {
        let props = use_context::<LinkFormIslandProps>();
        rsx! {
            LinkFormIsland {
                csrf_token: props.csrf_token,
                action: props.action,
                values: props.values,
                repos: props.repos,
                now: props.now,
            }
        }
    }
}
