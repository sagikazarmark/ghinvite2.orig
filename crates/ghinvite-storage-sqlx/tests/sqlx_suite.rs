//! Run the parameterized Storage suite against `SqlxStorage` (in-memory SQLite).
//!
//! `test_suite::run_suite` takes a factory so each scenario gets a fresh DB.

use ghinvite_core::storage::test_suite::run_suite;
use ghinvite_storage_sqlx::SqlxStorage;

#[tokio::test]
async fn sqlx_storage_passes_full_suite() {
    run_suite(|| async { SqlxStorage::in_memory().await.unwrap() }).await;
}
