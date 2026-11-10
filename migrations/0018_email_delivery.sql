-- Email invitations have no manufactured requester/request/link parent.
CREATE TABLE email_delivery_attempts (
 id TEXT PRIMARY KEY CHECK(id LIKE 'email:%'),
 batch_id TEXT NOT NULL CHECK(length(batch_id)=26),
 row_number INTEGER NOT NULL CHECK(row_number BETWEEN 1 AND 200),
 command TEXT NOT NULL CHECK(json_valid(command) AND json_type(command)='object'),
 generation INTEGER NOT NULL DEFAULT 1 CHECK(generation BETWEEN 1 AND 9007199254740991),
 retryable INTEGER NOT NULL DEFAULT 0 CHECK(retryable IN (0,1)),
 UNIQUE(batch_id,row_number),
 CHECK(json_type(command,'$.batch.batch_id') IS 'text' AND json_type(command,'$.batch.row') IS 'integer' AND json_type(command,'$.organization.org_id') IS 'integer' AND json_type(command,'$.account_id') IS 'integer' AND json_type(command,'$.installation_id') IS 'integer' AND json_type(command,'$.email_ref') IS 'text'),
 CHECK(json_extract(command,'$.account_id') BETWEEN 1 AND 9007199254740991 AND json_extract(command,'$.installation_id') BETWEEN 1 AND 9007199254740991 AND json_extract(command,'$.organization.org_id')=json_extract(command,'$.account_id')),
 CHECK(batch_id IS json_extract(command,'$.batch.batch_id') AND row_number IS json_extract(command,'$.batch.row')),
 CHECK(id IS 'email:'||batch_id||':'||row_number||':'||json_extract(command,'$.organization.org_id'))
);
CREATE TRIGGER email_attempt_immutable BEFORE UPDATE ON email_delivery_attempts
WHEN NEW.id!=OLD.id OR NEW.batch_id!=OLD.batch_id OR NEW.row_number!=OLD.row_number OR NEW.command!=OLD.command
 OR NEW.generation<OLD.generation OR NEW.generation>OLD.generation+1
 OR (NEW.generation>OLD.generation AND (OLD.retryable!=1 OR NEW.retryable!=0))
BEGIN SELECT RAISE(ABORT,'email attempt binding conflict'); END;
CREATE TRIGGER email_attempt_no_delete BEFORE DELETE ON email_delivery_attempts
BEGIN SELECT RAISE(ABORT,'email attempt retained'); END;
CREATE TABLE email_deliveries (
 id TEXT PRIMARY KEY CHECK(id LIKE 'email:%'),
 batch_id TEXT NOT NULL CHECK(length(batch_id)=26),
 row_number INTEGER NOT NULL CHECK(row_number BETWEEN 1 AND 200),
 account_id INTEGER NOT NULL CHECK(account_id BETWEEN 1 AND 9007199254740991),
 receipt TEXT NOT NULL CHECK(json_valid(receipt) AND json_type(receipt)='object'),
 UNIQUE(batch_id,row_number),
 CHECK(json_type(receipt,'$.revision') IS 'integer' AND json_type(receipt,'$.command.batch.batch_id') IS 'text' AND json_type(receipt,'$.command.batch.row') IS 'integer' AND json_type(receipt,'$.command.account_id') IS 'integer' AND json_type(receipt,'$.outcome.kind') IS 'text'),
 CHECK(json_extract(receipt,'$.revision') BETWEEN 1 AND 9007199254740991),
 CHECK(batch_id IS json_extract(receipt,'$.command.batch.batch_id') AND row_number IS json_extract(receipt,'$.command.batch.row') AND account_id IS json_extract(receipt,'$.command.account_id')),
 CHECK(id IS 'email:'||batch_id||':'||row_number||':'||json_extract(receipt,'$.command.organization.org_id'))
);
CREATE INDEX email_delivery_batch_account ON email_deliveries(account_id,batch_id,row_number);
CREATE TRIGGER email_delivery_immutable BEFORE UPDATE ON email_deliveries
WHEN NEW.id!=OLD.id OR NEW.batch_id!=OLD.batch_id OR NEW.row_number!=OLD.row_number OR NEW.account_id!=OLD.account_id
 OR json_extract(NEW.receipt,'$.command') IS NOT json_extract(OLD.receipt,'$.command')
 OR json_extract(NEW.receipt,'$.revision')<=json_extract(OLD.receipt,'$.revision')
 OR (json_extract(OLD.receipt,'$.outcome.kind') IN ('created','failed','aborted') AND (json_extract(NEW.receipt,'$.outcome') IS NOT json_extract(OLD.receipt,'$.outcome') OR json_extract(NEW.receipt,'$.confirmed_at') IS NOT json_extract(OLD.receipt,'$.confirmed_at') OR json_extract(NEW.receipt,'$.completed_at') IS NOT json_extract(OLD.receipt,'$.completed_at')))
 OR (json_extract(OLD.receipt,'$.settlement') IS NOT NULL AND (json_extract(NEW.receipt,'$.settlement') IS NOT json_extract(OLD.receipt,'$.settlement') OR json_extract(NEW.receipt,'$.settled_at') IS NOT json_extract(OLD.receipt,'$.settled_at')))
 OR (json_extract(OLD.receipt,'$.outcome.kind')='outcome_unknown' AND json_extract(NEW.receipt,'$.outcome.kind') IN ('planned','blocked','throttled'))
BEGIN SELECT RAISE(ABORT,'email delivery binding conflict'); END;
