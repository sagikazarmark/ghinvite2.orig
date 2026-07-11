//! `SqlxStorage` — the native (SQLite via sqlx) implementation of the
//! [`ghinvite_core::storage::Storage`] port. Used by the dev binaries and
//! tests; production (Cloudflare Workers) uses `ghinvite-storage-d1`.

pub mod records;
pub mod sqlx_impl;

pub use sqlx_impl::SqlxStorage;

pub(crate) fn to_db_err(e: sqlx::Error) -> ghinvite_core::storage::Error {
    ghinvite_core::storage::Error::Database(e.to_string())
}
