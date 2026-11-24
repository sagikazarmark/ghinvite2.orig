//! `SqlxStorage` — the native (SQLite via sqlx) implementation of the
//! [`ghinvite_core::storage::Storage`] port. Used by the dev binaries and
//! tests; production (Cloudflare Workers) uses `ghinvite-storage-d1`.

mod access_grants;
mod blocked_digest;
mod bulk;
mod invite_budget;
mod org_delivery;
mod org_webhooks;
mod pending_signin;
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

/// A statement built at runtime by one of ghinvite-core's shared SQL builders
/// (the text the D1 storage prepares too). Those builders only join their own
/// constant fragments, picked by enum and flag arguments, and every value is a
/// bound parameter; this is the one place that audit is asserted to sqlx's
/// `SqlSafeStr` check. Literal statements go straight to `sqlx::query`.
pub(crate) fn shared_query(
    sql: String,
) -> sqlx::query::Query<'static, sqlx::Sqlite, sqlx::sqlite::SqliteArguments> {
    sqlx::query(sqlx::AssertSqlSafe(sql))
}

/// Use the same exact integer range as D1, including native development.
#[inline]
pub(crate) fn u64_to_i64(v: u64) -> Result<i64> {
    ghinvite_core::storage::integer::unsigned(v)
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
        assert_eq!(super::u64_to_i64(0).unwrap(), 0);
        assert_eq!(super::u64_to_i64(42).unwrap(), 42);
        assert!(super::u64_to_i64(1 << 53).is_err());
    }
}

mod email_review;
mod email_vault;

mod email_delivery;

#[cfg(test)]
mod invitation_timing_tests;
