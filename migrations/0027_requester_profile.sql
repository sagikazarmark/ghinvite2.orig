-- GitHub's account creation time is advisory display metadata, distinct from
-- the local cache's last_seen_at. Existing profiles remain honestly unknown.
ALTER TABLE users ADD COLUMN github_created_at TEXT
    CHECK(github_created_at IS NULL OR julianday(github_created_at) IS NOT NULL);
