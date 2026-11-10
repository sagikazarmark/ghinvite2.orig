//! One D1 SQL/mapping implementation, with Worker and native bridge transports.
#[cfg(all(not(target_arch = "wasm32"), feature = "native-bridge"))]
pub mod native;
#[cfg(any(target_arch = "wasm32", feature = "native-bridge"))]
mod wasm_impl;
#[cfg(any(target_arch = "wasm32", feature = "native-bridge"))]
pub use wasm_impl::D1Storage;
