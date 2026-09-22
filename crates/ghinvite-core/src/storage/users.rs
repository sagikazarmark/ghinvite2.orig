//! Cached GitHub users, shared by SQLite and D1. Rows decode as [`crate::User`].

/// Bind user ID, login, nullable avatar URL and last-seen time.
pub const UPSERT: &str = "INSERT INTO users (user_id, login, avatar_url, last_seen_at) VALUES (?1, ?2, ?3, ?4)
    ON CONFLICT(user_id) DO UPDATE SET login = excluded.login, avatar_url = excluded.avatar_url, last_seen_at = excluded.last_seen_at";
pub const GET: &str =
    "SELECT user_id, login, avatar_url, last_seen_at FROM users WHERE user_id = ?1";
