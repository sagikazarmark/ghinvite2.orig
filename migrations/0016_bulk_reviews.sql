-- Exact retained review and submission identity. Requests remain separate owners.
CREATE TABLE bulk_batches (
  id TEXT PRIMARY KEY,
  account_id INTEGER NOT NULL CHECK(account_id>0 AND account_id<=9007199254740991),
  created_by INTEGER NOT NULL REFERENCES users(user_id),
  revision INTEGER NOT NULL CHECK(revision>0 AND revision<=9007199254740991),
  manifest TEXT NOT NULL CHECK(json_valid(manifest)),
  snapshot TEXT NOT NULL CHECK(json_valid(snapshot)),
  CHECK(json_extract(manifest,'$.batch_id') IS id),
  CHECK(json_extract(manifest,'$.admin.account_id') IS account_id),
  CHECK(json_extract(manifest,'$.admin.user_id') IS created_by),
  CHECK(json_extract(snapshot,'$.manifest') IS manifest),
  CHECK(json_extract(snapshot,'$.revision') IS revision)
);
CREATE INDEX bulk_batches_account ON bulk_batches(account_id,id);
CREATE TRIGGER bulk_batches_immutable BEFORE UPDATE ON bulk_batches WHEN
 NEW.id IS NOT OLD.id OR NEW.account_id IS NOT OLD.account_id OR NEW.created_by IS NOT OLD.created_by OR NEW.manifest IS NOT OLD.manifest
 OR NEW.revision<=OLD.revision
 OR (json_extract(OLD.snapshot,'$.submitted') IS NOT NULL AND json_extract(NEW.snapshot,'$.submitted') IS NOT json_extract(OLD.snapshot,'$.submitted'))
 BEGIN SELECT RAISE(ABORT,'immutable bulk batch'); END;
CREATE TRIGGER bulk_batches_retained BEFORE DELETE ON bulk_batches
 BEGIN SELECT RAISE(ABORT,'retained bulk batch'); END;
CREATE TABLE bulk_batch_requests (
 batch_id TEXT NOT NULL REFERENCES bulk_batches(id),
 row_number INTEGER NOT NULL CHECK(row_number BETWEEN 1 AND 200),
 request_id TEXT NOT NULL UNIQUE,
 requester_id INTEGER NOT NULL CHECK(requester_id>0 AND requester_id<=9007199254740991),
 PRIMARY KEY(batch_id,row_number)
);
CREATE TRIGGER bulk_batch_request_binding BEFORE INSERT ON bulk_batch_requests WHEN NOT EXISTS(
 SELECT 1 FROM bulk_batches b, json_each(b.snapshot,'$.submitted.requests') r WHERE b.id=NEW.batch_id
 AND json_extract(r.value,'$.source.batch_id') IS NEW.batch_id
 AND json_extract(r.value,'$.source.row') IS NEW.row_number
 AND json_extract(r.value,'$.request_id') IS NEW.request_id
 AND json_extract(r.value,'$.requester_id') IS NEW.requester_id)
 BEGIN SELECT RAISE(ABORT,'invalid bulk request binding'); END;
CREATE TRIGGER bulk_batch_request_immutable BEFORE UPDATE ON bulk_batch_requests
 BEGIN SELECT RAISE(ABORT,'immutable bulk request binding'); END;
CREATE TRIGGER bulk_batch_request_retained BEFORE DELETE ON bulk_batch_requests
 BEGIN SELECT RAISE(ABORT,'retained bulk request binding'); END;

-- Fresh-schema baseline: 0001 permits a nullable admission link. These guards
-- bind every linkless request to one exact, already retained batch submission.
ALTER TABLE invitation_requests ADD COLUMN batch_row INTEGER;
CREATE TRIGGER request_origin_insert BEFORE INSERT ON invitation_requests WHEN NOT (
 (NEW.source='link' AND NEW.invitation_link_id IS NOT NULL AND NEW.batch_id IS NULL AND NEW.batch_row IS NULL)
 OR (NEW.source='batch' AND NEW.invitation_link_id IS NULL AND NEW.domain_evidence_json IS NULL
     AND NEW.state IN ('approved','lapsed') AND NEW.decision_deadline IS NULL
     AND EXISTS(SELECT 1 FROM bulk_batch_requests r JOIN bulk_batches b ON b.id=r.batch_id
       WHERE r.batch_id=NEW.batch_id AND r.row_number=NEW.batch_row AND r.request_id=NEW.id
       AND r.requester_id=NEW.requester_id AND json_extract(b.snapshot,'$.submitted.submitted_by')=NEW.decided_by)))
 BEGIN SELECT RAISE(ABORT,'invalid request origin'); END;
CREATE TRIGGER request_origin_update BEFORE UPDATE ON invitation_requests WHEN
 NEW.source IS NOT OLD.source OR NEW.invitation_link_id IS NOT OLD.invitation_link_id
 OR NEW.batch_id IS NOT OLD.batch_id OR NEW.batch_row IS NOT OLD.batch_row
 OR (NEW.source='batch' AND (NEW.domain_evidence_json IS NOT NULL OR NEW.decision_deadline IS NOT NULL OR NEW.decided_by IS NOT OLD.decided_by))
 BEGIN SELECT RAISE(ABORT,'immutable request origin'); END;
DROP TRIGGER requests_queue_insert;
CREATE TRIGGER requests_queue_insert AFTER INSERT ON invitation_requests BEGIN
 UPDATE invitation_requests SET queue_account_id=COALESCE(
   (SELECT account_id FROM invitation_links WHERE id=NEW.invitation_link_id),
   (SELECT account_id FROM bulk_batches WHERE id=NEW.batch_id)) WHERE id=NEW.id;
END;
