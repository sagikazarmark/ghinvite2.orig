//! Web binary core. axum app builder + tower-sessions + OAuth flow + Dioxus
//! layouts. Plan 7 wraps `build_app()` in a Workers `#[event(fetch)]`; Plans 5
//! and 6 fill in the dashboard and recipient routes.

pub mod config;
pub mod error;
pub mod middleware;
pub mod restate_client;
pub mod routes;
pub mod session;
pub mod state;
pub mod views;

// Re-exports filled in as types appear:
// pub use config::WebConfig;
// pub use error::{Result, WebError};
// pub use restate_client::RestateClient;
// pub use state::AppState;
