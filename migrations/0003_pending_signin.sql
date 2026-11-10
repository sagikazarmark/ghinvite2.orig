-- One opaque pending OAuth record per hashed trusted session identifier.
-- The state and PKCE verifier exist only inside the keyring envelope. Neither
-- the routing hash nor the expiry grants authority to decrypt or authenticate.
CREATE TABLE pending_signins (
  session_key TEXT PRIMARY KEY NOT NULL CHECK(length(session_key)=64 AND session_key NOT GLOB '*[^0-9a-f]*'),
  ciphertext TEXT NOT NULL CHECK(length(ciphertext) BETWEEN 1 AND 8192),
  expires_at INTEGER NOT NULL CHECK(expires_at BETWEEN 1 AND 9007199254740991)
);
CREATE INDEX idx_pending_signins_expiry ON pending_signins(expires_at);
