-- Opaque, encrypted browser continuations. Never session authentication records.
CREATE TABLE admin_attempts (
    scope TEXT NOT NULL,
    id TEXT NOT NULL,
    binding TEXT NOT NULL,
    payload TEXT NOT NULL,
    expires_at INTEGER NOT NULL,
    PRIMARY KEY (scope, id),
    UNIQUE (scope, binding)
);
