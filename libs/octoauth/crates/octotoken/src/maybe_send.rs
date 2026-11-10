//! `Send` bounds that apply natively but not on wasm32.

/// `Send` natively; nothing on wasm32, where JavaScript-backed futures (e.g.
/// reqwest's `fetch`) are not `Send`. Implemented for every type that
/// qualifies, so implementors never name it.
#[cfg(not(target_arch = "wasm32"))]
pub trait MaybeSend: Send {}

#[cfg(not(target_arch = "wasm32"))]
impl<T: Send + ?Sized> MaybeSend for T {}

/// `Send` natively; nothing on wasm32, where JavaScript-backed futures (e.g.
/// reqwest's `fetch`) are not `Send`. Implemented for every type that
/// qualifies, so implementors never name it.
#[cfg(target_arch = "wasm32")]
pub trait MaybeSend {}

#[cfg(target_arch = "wasm32")]
impl<T: ?Sized> MaybeSend for T {}

/// `Send + Sync` natively; nothing on wasm32. See [`MaybeSend`].
#[cfg(not(target_arch = "wasm32"))]
pub trait MaybeSendSync: Send + Sync {}

#[cfg(not(target_arch = "wasm32"))]
impl<T: Send + Sync + ?Sized> MaybeSendSync for T {}

/// `Send + Sync` natively; nothing on wasm32. See [`MaybeSend`].
#[cfg(target_arch = "wasm32")]
pub trait MaybeSendSync {}

#[cfg(target_arch = "wasm32")]
impl<T: ?Sized> MaybeSendSync for T {}
