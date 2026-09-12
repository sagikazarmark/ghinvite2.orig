use super::*;

/// Native development/test storage. All payloads crossing this boundary are opaque.
#[derive(Clone, Debug)]
pub struct SqliteBackend {
    pool: sqlx::SqlitePool,
}

impl SqliteBackend {
    pub fn new(pool: sqlx::SqlitePool) -> Self {
        Self { pool }
    }

    pub async fn migrate(&self) -> Result<()> {
        sqlx::query("CREATE TABLE IF NOT EXISTS protected_sessions (id TEXT PRIMARY KEY, payload BLOB NOT NULL, expires_at INTEGER NOT NULL)")
            .execute(&self.pool).await.map_err(sql_error)?;
        sqlx::query("CREATE TABLE IF NOT EXISTS session_revocations (id TEXT PRIMARY KEY, expires_at INTEGER NOT NULL)")
            .execute(&self.pool).await.map_err(sql_error)?;
        Ok(())
    }
}

fn sql_error(_: sqlx::Error) -> Error {
    backend_error("session database unavailable")
}

#[async_trait::async_trait]
impl Backend for SqliteBackend {
    async fn get(&self, id: &Id) -> Result<Option<Vec<u8>>> {
        sqlx::query_scalar("SELECT payload FROM protected_sessions WHERE id = ? AND expires_at > ?")
            .bind(id.to_string())
            .bind(now().unix_timestamp())
            .fetch_optional(&self.pool)
            .await
            .map_err(sql_error)
    }
    async fn insert(&self, id: &Id, payload: &[u8], expires_at: i64) -> Result<bool> {
        Ok(sqlx::query("INSERT INTO protected_sessions(id, payload, expires_at) VALUES (?, ?, ?) ON CONFLICT(id) DO NOTHING")
            .bind(id.to_string()).bind(payload).bind(expires_at).execute(&self.pool).await.map_err(sql_error)?.rows_affected() == 1)
    }
    async fn put(&self, id: &Id, payload: &[u8], expires_at: i64) -> Result<()> {
        sqlx::query("INSERT INTO protected_sessions(id, payload, expires_at) VALUES (?, ?, ?) ON CONFLICT(id) DO UPDATE SET payload = excluded.payload, expires_at = excluded.expires_at")
            .bind(id.to_string()).bind(payload).bind(expires_at).execute(&self.pool).await.map_err(sql_error)?;
        Ok(())
    }
    async fn is_revoked(&self, id: &Id) -> Result<bool> {
        let found: Option<i64> =
            sqlx::query_scalar("SELECT 1 FROM session_revocations WHERE id = ? AND expires_at > ?")
                .bind(id.to_string())
                .bind(now().unix_timestamp())
                .fetch_optional(&self.pool)
                .await
                .map_err(sql_error)?;
        Ok(found.is_some())
    }
    async fn revoke(&self, id: &Id, retention: Duration) -> Result<()> {
        sqlx::query("INSERT INTO session_revocations(id, expires_at) VALUES (?, ?) ON CONFLICT(id) DO UPDATE SET expires_at = MAX(expires_at, excluded.expires_at)")
            .bind(id.to_string()).bind((now() + retention).unix_timestamp()).execute(&self.pool).await.map_err(sql_error)?;
        Ok(())
    }
    async fn delete(&self, id: &Id) -> Result<()> {
        sqlx::query("DELETE FROM protected_sessions WHERE id = ?")
            .bind(id.to_string())
            .execute(&self.pool)
            .await
            .map_err(sql_error)?;
        Ok(())
    }
}
