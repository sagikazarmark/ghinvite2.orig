//! Cached GitHub users, shared by SQLite and D1. Rows decode as [`crate::User`].

/// Bind user ID, login, nullable avatar URL, last-seen time and optional GitHub account creation time.
pub const UPSERT: &str = "INSERT INTO users (user_id, login, avatar_url, last_seen_at, github_created_at) VALUES (?1, ?2, ?3, ?4, ?5)
    ON CONFLICT(user_id) DO UPDATE SET login = excluded.login, avatar_url = COALESCE(excluded.avatar_url, users.avatar_url), last_seen_at = excluded.last_seen_at, github_created_at = COALESCE(excluded.github_created_at, users.github_created_at)";
pub const GET: &str = "SELECT user_id, login, avatar_url, last_seen_at, github_created_at FROM users WHERE user_id = ?1";
