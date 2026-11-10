-- Review escrow is separate from delivery recovery; opaque identities cannot be reused.
CREATE TABLE email_review (
 email_ref TEXT PRIMARY KEY CHECK(length(email_ref)=64 AND email_ref NOT GLOB '*[^0-9a-f]*'),
 batch_id TEXT NOT NULL CHECK(length(batch_id)=26),
 row_number INTEGER NOT NULL CHECK(row_number BETWEEN 1 AND 200),
 account_id INTEGER NOT NULL CHECK(account_id BETWEEN 1 AND 9007199254740991),
 installation_id INTEGER NOT NULL CHECK(installation_id BETWEEN 1 AND 9007199254740991),
 ciphertext TEXT CHECK(ciphertext IS NULL OR length(ciphertext) BETWEEN 1 AND 8192),
 created_at INTEGER NOT NULL CHECK(created_at BETWEEN 1 AND 9007199254654591),
 expires_at INTEGER NOT NULL CHECK(expires_at=created_at+3600),
 cleared_at INTEGER CHECK(cleared_at>=created_at AND cleared_at<=9007199254654591),
 CHECK((ciphertext IS NULL)=(cleared_at IS NOT NULL)),
 UNIQUE(batch_id,row_number)
);
CREATE INDEX email_review_expiry ON email_review(expires_at) WHERE ciphertext IS NOT NULL;
CREATE TRIGGER email_review_immutable BEFORE UPDATE ON email_review
WHEN NEW.email_ref!=OLD.email_ref OR NEW.batch_id!=OLD.batch_id OR NEW.row_number!=OLD.row_number
 OR NEW.account_id!=OLD.account_id OR NEW.installation_id!=OLD.installation_id
 OR NEW.created_at!=OLD.created_at OR NEW.expires_at!=OLD.expires_at
 OR (NEW.ciphertext IS NOT OLD.ciphertext AND NEW.ciphertext IS NOT NULL)
 OR (OLD.cleared_at IS NOT NULL AND NEW.cleared_at IS NOT OLD.cleared_at)
BEGIN SELECT RAISE(ABORT,'email review immutable binding'); END;
CREATE TRIGGER email_review_no_delete BEFORE DELETE ON email_review
BEGIN SELECT RAISE(ABORT,'email review tombstone required'); END;
