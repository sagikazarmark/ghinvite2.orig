//! Run the parameterized Storage suite against `SqlxStorage` (in-memory SQLite).
//!
//! `tests::run_suite` takes a factory so each scenario gets a fresh DB.

use storage::SqlxStorage;
use storage::tests::run_suite;

#[tokio::test]
async fn sqlx_storage_passes_full_suite() {
    run_suite(|| async { SqlxStorage::in_memory().await.unwrap() }).await;
}
