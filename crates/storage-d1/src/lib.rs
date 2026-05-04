//! Cloudflare D1 implementation of the Storage trait.
//! Only compiles on wasm32-unknown-unknown.

#[cfg(target_arch = "wasm32")]
mod wasm_impl;

#[cfg(target_arch = "wasm32")]
pub use wasm_impl::D1Storage;

pub mod bind;
