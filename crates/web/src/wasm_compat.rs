//! Wasm32 compatibility helpers.
//!
//! On `wasm32-unknown-unknown` (Cloudflare Workers), `reqwest` dispatches to
//! `fetch` and its returned futures wrap `JsFuture`, which is `!Send`. axum's
//! `Handler` trait — and `async_trait` bodies on traits like `Storage` —
//! require `Future + Send`. Because Workers are single-threaded (no
//! `std::thread::spawn`, no preemptive threading on `wasm32-unknown-unknown`),
//! values never actually cross a thread boundary, so we can soundly assert
//! `Send` on these futures.
//!
//! On native targets `wasm_send` is a no-op pass-through, so the same call
//! sites compile under both targets.

#[cfg(target_arch = "wasm32")]
mod wasm {
    use std::future::Future;
    use std::pin::Pin;
    use std::task::{Context, Poll};

    /// Wraps a `!Send` future and unsafely implements `Send`.
    ///
    /// SAFETY: wasm32-unknown-unknown has no OS threads; futures are never
    /// moved across thread boundaries. This impl is sound only under the
    /// single-threaded Cloudflare Workers execution model. If
    /// SharedArrayBuffer-based threads are ever added to the build, this must
    /// be revisited.
    pub struct WasmSend<F>(pub F);

    unsafe impl<F> Send for WasmSend<F> {}

    impl<F: Future> Future for WasmSend<F> {
        type Output = F::Output;
        fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
            // SAFETY: structural pin projection to the inner field; the outer
            // pin guarantees the inner is also pinned.
            unsafe { self.map_unchecked_mut(|s| &mut s.0) }.poll(cx)
        }
    }

    /// Wraps a future in `WasmSend` so the resulting future is `Send`.
    pub fn wasm_send<F: Future>(f: F) -> WasmSend<F> {
        WasmSend(f)
    }
}

#[cfg(target_arch = "wasm32")]
pub use wasm::{WasmSend, wasm_send};

/// No-op on native: futures are already `Send` when their bodies don't touch
/// `JsValue`. The signature mirrors the wasm version so call sites compile
/// unchanged.
#[cfg(not(target_arch = "wasm32"))]
pub fn wasm_send<F: std::future::Future>(f: F) -> F {
    f
}
