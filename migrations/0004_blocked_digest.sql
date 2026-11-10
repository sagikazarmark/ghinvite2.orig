-- Atomic daily observations of projected facts, never delivery authority.
CREATE TABLE blocked_digest_days (
    day TEXT PRIMARY KEY NOT NULL CHECK(length(day)=10),
    observed_at TEXT NOT NULL,
    complete INTEGER NOT NULL DEFAULT 0 CHECK(complete IN (0,1))
);
CREATE TABLE blocked_digest_items (
    day TEXT NOT NULL REFERENCES blocked_digest_days(day) ON DELETE CASCADE,
    delivery_key TEXT NOT NULL,
    link_id TEXT NOT NULL,
    account_id INTEGER NOT NULL CHECK(account_id BETWEEN 1 AND 9007199254740991),
    reason TEXT NOT NULL CHECK(reason IN ('installation_unavailable','installation_permissions','requester_identity','outside_collaborators_restricted','ip_allow_list','target_unavailable','mint_forbidden')),
    PRIMARY KEY(day,delivery_key)
);
CREATE INDEX blocked_digest_account ON blocked_digest_items(day,account_id,link_id);
