//! Native deployment shape for the web app: SqlxStorage, a SQLite session
//! backend, a reqwest GitHub transport, and `/assets` served off the local
//! filesystem, wired into `ghinvite_web::build_app`.
//!
//! The Cloudflare Workers shape is `ghinvite-web-worker`, which wires D1
//! storage and a KV session backend into the same call. Keeping both out of
//! `ghinvite-web` is what lets that crate stay free of tokio, sqlx and
//! `tower-http/fs`.

mod session_sqlite;

pub use session_sqlite::SqliteBackend;

use ghinvite_web::{AppState, WebConfig};

/// Serve the Dioxus island bundle under `/assets/*` from
/// [`WebConfig::island_assets_dir`]. A missing file is a plain 404, which is
/// what the new-link page's `<script type="module">` relies on to degrade to
/// the server-rendered form when no bundle is built.
///
/// On Workers, Cloudflare Static Assets answer `/assets/*` before the Worker
/// is invoked (`[assets]` in `wrangler/web.toml`), so no equivalent exists
/// there and `tower-http/fs` is never compiled in.
pub fn island_assets(config: &WebConfig) -> Option<axum::Router<AppState>> {
    let dir = config.island_assets_dir.as_ref()?;
    Some(axum::Router::new().nest_service("/assets", tower_http::services::ServeDir::new(dir)))
}
