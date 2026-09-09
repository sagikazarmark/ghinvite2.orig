//! Views: the Dioxus components from `ghinvite-ui`, re-exported under their
//! historical path, plus the server-side renderer.
//!
//! The components themselves live in `crates/ghinvite-ui` (ADR 0001) so a
//! browser-side island can build them without this crate's server
//! dependencies. `crate::views::links::LinkCreateFormPage` and friends resolve
//! through the glob below; only [`render`] is defined here, because it needs
//! `dioxus-ssr`, which stays out of the UI crate.

pub use ghinvite_ui::*;

pub mod render;
