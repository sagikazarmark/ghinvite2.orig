-- M2 email recovery: encrypted mailbox only, immutable opaque reference/binding.
-- Ciphertext is cleared after the staged/terminal horizon; references remain
-- tombstoned so retries cannot assign another mailbox to an old delivery.
CREATE TABLE email_vault (
    email_ref TEXT PRIMARY KEY CHECK(length(email_ref)=64 AND email_ref NOT GLOB '*[^0-9a-f]*'),
    batch_id TEXT NOT NULL CHECK(length(batch_id)=26),
    row_number INTEGER NOT NULL CHECK(row_number BETWEEN 1 AND 200),
    account_id INTEGER NOT NULL CHECK(account_id BETWEEN 1 AND 9007199254740991),
    installation_id INTEGER NOT NULL CHECK(installation_id BETWEEN 1 AND 9007199254740991),
    ciphertext TEXT CHECK(ciphertext IS NULL OR length(ciphertext) BETWEEN 1 AND 8192),
    created_at INTEGER NOT NULL CHECK(created_at BETWEEN 1 AND 9007199254654591),
    activated_at INTEGER CHECK(activated_at>=created_at AND activated_at<=9007199254654591),
    retired_at INTEGER CHECK(retired_at>=COALESCE(activated_at,created_at) AND retired_at<=9007199254654591),
    purge_after INTEGER CHECK(purge_after BETWEEN 1 AND 9007199254740991),
    purged_at INTEGER CHECK(purged_at IS NULL OR (purge_after IS NOT NULL AND purged_at>=purge_after AND purged_at<=9007199254740991)),
    CHECK((ciphertext IS NULL) = (purged_at IS NOT NULL)),
    CHECK(purge_after IS CASE WHEN retired_at IS NOT NULL THEN retired_at+86400
        WHEN activated_at IS NOT NULL THEN NULL ELSE created_at+86400 END)
);
CREATE INDEX email_vault_expiry ON email_vault(purge_after) WHERE ciphertext IS NOT NULL;
CREATE TRIGGER email_vault_no_reassignment BEFORE UPDATE ON email_vault
WHEN NEW.email_ref!=OLD.email_ref OR NEW.batch_id!=OLD.batch_id
 OR NEW.row_number!=OLD.row_number OR NEW.account_id!=OLD.account_id
 OR NEW.installation_id!=OLD.installation_id OR NEW.created_at!=OLD.created_at
 OR (NEW.ciphertext IS NOT OLD.ciphertext AND NEW.ciphertext IS NOT NULL)
 OR (OLD.activated_at IS NOT NULL AND NEW.activated_at IS NOT OLD.activated_at)
 OR (OLD.retired_at IS NOT NULL AND NEW.retired_at IS NOT OLD.retired_at)
 OR (OLD.purged_at IS NOT NULL AND NEW.purged_at IS NOT OLD.purged_at)
 OR (OLD.purged_at IS NOT NULL AND (NEW.activated_at IS NOT OLD.activated_at OR NEW.retired_at IS NOT OLD.retired_at))
BEGIN SELECT RAISE(ABORT,'email vault immutable binding'); END;
CREATE TRIGGER email_vault_no_delete BEFORE DELETE ON email_vault
BEGIN SELECT RAISE(ABORT,'email vault tombstone required'); END;
