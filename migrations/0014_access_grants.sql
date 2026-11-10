-- Read projection only. AccessGrant owns removal state; a projection cannot
-- authorize a DELETE or reconstruct a missing ownership receipt.
CREATE TABLE access_grants (
  id TEXT PRIMARY KEY,
  request_id TEXT NOT NULL REFERENCES invitation_requests(id),
  target_kind TEXT NOT NULL CHECK(target_kind IN ('repo','org','team')),
  target_id INTEGER NOT NULL CHECK(target_id BETWEEN 1 AND 9007199254740991),
  delivery_generation INTEGER NOT NULL CHECK(delivery_generation BETWEEN 1 AND 9007199254740991),
  account_id INTEGER NOT NULL CHECK(account_id BETWEEN 1 AND 9007199254740991),
  link_id TEXT REFERENCES invitation_links(id),
  remove_at TEXT,
  revision INTEGER NOT NULL CHECK(revision BETWEEN 1 AND 9007199254740991),
  receipt TEXT NOT NULL CHECK(json_valid(receipt)),
  UNIQUE(request_id,target_kind,target_id,delivery_generation)
);
CREATE INDEX access_grants_link ON access_grants(link_id,id);
CREATE TRIGGER access_grant_immutable BEFORE UPDATE ON access_grants
WHEN OLD.id!=NEW.id OR OLD.request_id!=NEW.request_id OR OLD.target_kind!=NEW.target_kind
 OR OLD.target_id!=NEW.target_id OR OLD.delivery_generation!=NEW.delivery_generation
 OR OLD.account_id!=NEW.account_id OR OLD.link_id IS NOT NEW.link_id OR OLD.remove_at IS NOT NEW.remove_at
 OR json_extract(OLD.receipt,'$.command')!=json_extract(NEW.receipt,'$.command')
 OR NEW.revision<OLD.revision OR (NEW.revision=OLD.revision AND NEW.receipt!=OLD.receipt)
 OR (json_extract(OLD.receipt,'$.outcome.kind') IN ('removed','removal_skipped') AND NEW.receipt!=OLD.receipt)
BEGIN SELECT RAISE(ABORT,'access grant immutable receipt conflict'); END;
