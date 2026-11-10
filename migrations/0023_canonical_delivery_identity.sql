-- ADR0011 / ID23–24. Activate only with canonical command/fence/projection
-- consumers and after every old shared-schema user has drained or moved away.
-- SQLite rebuild ordering: https://www.sqlite.org/lang_altertable.html
-- D1 FK deferral: https://developers.cloudflare.com/d1/sql-api/foreign-keys/
-- Original receipts, audit facts, fences and every retained column stay intact.
PRAGMA defer_foreign_keys = ON;
CREATE TABLE delivery_identity_cutover_guard (assertion INTEGER NOT NULL CHECK(assertion=1));
-- Refuse to discard a bridge-only historical audit binding. Restore the exact
-- retained receipt through its compatible owner before retrying this migration.
INSERT INTO delivery_identity_cutover_guard SELECT NOT EXISTS(
 SELECT 1 FROM deliveries d JOIN audit_events a
 ON a.target_kind='github_invitation' AND a.target_id=d.invitation_id
 WHERE NOT EXISTS(SELECT 1 FROM delivery_outcomes o
 WHERE o.delivery_id=d.id AND o.request_id=d.request_id
 AND json_extract(o.receipt,'$.create.command.invitation_id')=d.invitation_id)
);
DROP VIEW operational_delivery_steps;
DROP TRIGGER organization_step_parent;


CREATE TABLE deliveries_canonical (
  id TEXT PRIMARY KEY NOT NULL,
  request_id TEXT NOT NULL REFERENCES invitation_requests(id),
  batch_id TEXT,
  kind TEXT NOT NULL DEFAULT 'repo' CHECK(kind IN ('repo','org','email')),
  target_id INTEGER NOT NULL CHECK(target_id BETWEEN 1 AND 9007199254740991),
  delivery_generation INTEGER NOT NULL CHECK(delivery_generation BETWEEN 1 AND 9007199254740991),
  create_outcome TEXT NOT NULL DEFAULT 'planned',
  outcome_reason TEXT,
  effective_role_before TEXT,
  upstream_invitation_id INTEGER CHECK(upstream_invitation_id BETWEEN 1 AND 9007199254740991),
  tracked_foreign INTEGER NOT NULL DEFAULT 0 CHECK(tracked_foreign IN (0,1)),
  tracking_stopped_reason TEXT,
  settlement TEXT,
  settlement_reason TEXT,
  final_result TEXT,
  cascade_intent TEXT,
  held INTEGER NOT NULL DEFAULT 0 CHECK(held IN (0,1)),
  sent_at TEXT,
  own_expires_at TEXT,
  unknown_since TEXT,
  last_observed_at TEXT,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  projection_revision INTEGER NOT NULL DEFAULT 0,
  CHECK(id = CASE kind WHEN 'org' THEN 'org:' ELSE '' END || request_id || ':' || target_id || ':' || delivery_generation)
);

INSERT INTO deliveries_canonical(id,request_id,batch_id,kind,target_id,delivery_generation,create_outcome,outcome_reason,effective_role_before,upstream_invitation_id,tracked_foreign,tracking_stopped_reason,settlement,settlement_reason,final_result,cascade_intent,held,sent_at,own_expires_at,unknown_since,last_observed_at,created_at,updated_at,projection_revision) SELECT id,request_id,batch_id,kind,target_id,delivery_generation,create_outcome,outcome_reason,effective_role_before,upstream_invitation_id,tracked_foreign,tracking_stopped_reason,settlement,settlement_reason,final_result,cascade_intent,held,sent_at,own_expires_at,unknown_since,last_observed_at,created_at,updated_at,projection_revision FROM deliveries;

DROP TABLE deliveries;

ALTER TABLE deliveries_canonical RENAME TO deliveries;

CREATE INDEX idx_deliveries_request ON deliveries(request_id);

CREATE INDEX idx_deliveries_route ON deliveries(kind,target_id,settlement);

CREATE INDEX idx_deliveries_upstream ON deliveries(upstream_invitation_id);

CREATE TRIGGER delivery_settlement_fence BEFORE UPDATE ON deliveries
WHEN OLD.settlement IN ('accepted','declined','expired','cancelled') AND (
 NEW.settlement IS NOT OLD.settlement OR NEW.upstream_invitation_id IS NOT OLD.upstream_invitation_id
 OR NEW.request_id IS NOT OLD.request_id OR NEW.target_id IS NOT OLD.target_id OR NEW.delivery_generation IS NOT OLD.delivery_generation)
BEGIN SELECT RAISE(ABORT,'settled invitation writer conflict'); END;

CREATE TRIGGER delivery_tracking_stop_fence BEFORE UPDATE ON deliveries
WHEN OLD.tracking_stopped_reason IS NOT NULL AND NEW.tracking_stopped_reason IS NOT OLD.tracking_stopped_reason
BEGIN SELECT RAISE(ABORT,'foreign tracking stop conflict'); END;

CREATE TRIGGER organization_step_parent BEFORE INSERT ON delivery_steps
WHEN NOT EXISTS(SELECT 1 FROM deliveries WHERE id=NEW.delivery_id AND kind='org')
BEGIN SELECT RAISE(ABORT,'organization step parent conflict'); END;

CREATE VIEW operational_delivery_steps AS
 SELECT d.id,d.request_id,l.id AS link_id,COALESCE(l.account_id,b.account_id) AS account_id,
 r.batch_id,r.batch_row,d.kind,CASE d.kind WHEN 'repo' THEN 'repo' ELSE 'org' END AS step,
 d.target_id,d.delivery_generation,d.create_outcome,d.outcome_reason,
 d.settlement,d.tracking_stopped_reason,d.sent_at,
 CASE d.kind WHEN 'repo' THEN d.unknown_since ELSE t.unknown_since END AS unknown_since,
 CASE d.kind WHEN 'repo' THEN d.last_observed_at ELSE t.last_observed_at END AS last_observed_at
 FROM deliveries d LEFT JOIN invitation_requests r ON r.id=d.request_id
 LEFT JOIN invitation_links l ON l.id=r.invitation_link_id
 LEFT JOIN bulk_batches b ON b.id=r.batch_id
 LEFT JOIN organization_step_telemetry t ON t.delivery_id=d.id AND t.step='org'
 WHERE d.kind IN ('repo','org')
 UNION ALL
 SELECT d.id,d.request_id,l.id,COALESCE(l.account_id,b.account_id),r.batch_id,r.batch_row,'team',s.step,
 json_extract(s.receipt,'$.create.team_id'),d.delivery_generation,s.outcome,s.reason,
 json_extract(s.receipt,'$.settlement.state'),json_extract(s.receipt,'$.tracking_stopped'),
 CASE WHEN s.outcome='created' THEN json_extract(s.receipt,'$.create.confirmed_at') ELSE NULL END,
 t.unknown_since,t.last_observed_at
 FROM delivery_steps s JOIN deliveries d ON d.id=s.delivery_id
 LEFT JOIN invitation_requests r ON r.id=d.request_id
 LEFT JOIN invitation_links l ON l.id=r.invitation_link_id
 LEFT JOIN bulk_batches b ON b.id=r.batch_id
 LEFT JOIN organization_step_telemetry t ON t.delivery_id=d.id AND t.step=s.step
 WHERE d.kind='org'
 UNION ALL
 SELECT e.id,NULL,NULL,e.account_id,e.batch_id,e.row_number,'email','org-email',
 json_extract(e.receipt,'$.command.organization.org_id'),1,
 json_extract(e.receipt,'$.outcome.kind'),json_extract(e.receipt,'$.outcome.reason'),
 json_extract(e.receipt,'$.settlement.kind'),NULL,
 CASE WHEN json_extract(e.receipt,'$.outcome.kind')='created' THEN json_extract(e.receipt,'$.confirmed_at') ELSE NULL END,
 json_extract(e.receipt,'$.unknown_since'),json_extract(e.receipt,'$.last_observed_at')
 FROM email_deliveries e;

-- These expression indexes keyed fences by the obsolete ULID. The immutable
-- fence primary key remains the kind-qualified physical delivery key and step.
DROP INDEX delivery_attempt_identity;
DROP INDEX organization_attempt_identity;
INSERT INTO delivery_identity_cutover_guard SELECT NOT EXISTS(SELECT 1 FROM pragma_foreign_key_check);
DROP TABLE delivery_identity_cutover_guard;
PRAGMA defer_foreign_keys = OFF;
