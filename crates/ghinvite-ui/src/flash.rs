//! One-shot status message shown to the user after a redirect.
//!
//! Set by a route handler in `ghinvite-web` (stored in the session), read on
//! the next request, and rendered by the layouts in this crate. The types live
//! here so the views can render a flash without depending on the session
//! store; `ghinvite_web::session` re-exports them.

use serde::{Deserialize, Serialize};

/// One-shot status message shown to the user after a redirect.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct Flash {
    pub level: FlashLevel,
    pub message: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum FlashLevel {
    Success,
    Error,
    Info,
}
