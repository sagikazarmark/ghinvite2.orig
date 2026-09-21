//! `SqlxStorage` — the native (SQLite via sqlx) implementation of the
//! [`ghinvite_core::storage::Storage`] port. Used by the dev binaries and
//! tests; production (Cloudflare Workers) uses `ghinvite-storage-d1`.

mod projection;
pub mod sqlx_impl;

pub use sqlx_impl::SqlxStorage;

use ghinvite_core::storage::{Error, Result};
use serde::de::DeserializeOwned;
use sqlx::sqlite::SqliteRow;
use sqlx::{Column, Row, TypeInfo, ValueRef};

pub(crate) fn to_db_err(e: sqlx::Error) -> Error {
    Error::Database(e.to_string())
}

/// Centralized `u64 → i64` cast for SQLite bindings. SQLite has no unsigned
/// integers, so every `u64` we store rides as `i64`. The `debug_assert!` is the
/// safety net for development; production never sees `u64` values above
/// `i64::MAX` from GitHub's id space.
#[inline]
pub(crate) fn u64_to_i64(v: u64) -> i64 {
    debug_assert!(v <= i64::MAX as u64, "u64 value {v} exceeds i64::MAX");
    v as i64
}

/// Decode a row into one of core's shared row types. Each column becomes a
/// JSON value of its SQLite storage class, as D1 presents its rows.
pub(crate) fn decode<T: DeserializeOwned>(row: &SqliteRow) -> Result<T> {
    let mut object = serde_json::Map::new();
    for column in row.columns() {
        let i = column.ordinal();
        let raw = row.try_get_raw(i).map_err(to_db_err)?;
        let value = if raw.is_null() {
            serde_json::Value::Null
        } else {
            match raw.type_info().name() {
                "INTEGER" => row
                    .try_get_unchecked::<i64, _>(i)
                    .map_err(to_db_err)?
                    .into(),
                "REAL" => row
                    .try_get_unchecked::<f64, _>(i)
                    .map_err(to_db_err)?
                    .into(),
                "TEXT" => row
                    .try_get_unchecked::<String, _>(i)
                    .map_err(to_db_err)?
                    .into(),
                other => return Err(Error::Corrupt(format!("unexpected {other} column"))),
            }
        };
        object.insert(column.name().to_owned(), value);
    }
    ghinvite_core::storage::decode_row(serde_json::Value::Object(object))
}

#[cfg(test)]
mod tests {
    #[test]
    fn u64_to_i64_round_trips_normal_values() {
        assert_eq!(super::u64_to_i64(0), 0);
        assert_eq!(super::u64_to_i64(42), 42);
        assert_eq!(super::u64_to_i64(i64::MAX as u64), i64::MAX);
    }
}
