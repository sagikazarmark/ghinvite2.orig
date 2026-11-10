-- Authenticated ingress routing only. Settlement remains a delivery-owner decision.
CREATE TABLE organization_webhook_receipts (
  app TEXT NOT NULL CHECK(app='org'),
  payload_sha256 TEXT NOT NULL CHECK(length(payload_sha256)=64 AND payload_sha256 NOT GLOB '*[^0-9a-f]*'),
  received_at INTEGER NOT NULL CHECK(received_at BETWEEN -9007199254740991 AND 9007199254740991),
  expires_at INTEGER NOT NULL CHECK(expires_at=received_at+604800),
  receipt TEXT CHECK(receipt IS NULL OR json_valid(receipt)),
  PRIMARY KEY(app,payload_sha256)
);
CREATE INDEX organization_webhook_receipts_expiry ON organization_webhook_receipts(expires_at,payload_sha256);
CREATE TRIGGER organization_webhook_receipt_binding BEFORE UPDATE ON organization_webhook_receipts
WHEN NEW.app!=OLD.app OR NEW.payload_sha256!=OLD.payload_sha256 OR NEW.received_at<OLD.received_at
 OR (NEW.received_at<OLD.expires_at AND (NEW.receipt IS NOT OLD.receipt OR NEW.received_at!=OLD.received_at OR NEW.expires_at!=OLD.expires_at))
BEGIN SELECT RAISE(ABORT,'organization webhook receipt conflict'); END;
