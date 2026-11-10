-- Organization/team telemetry is independent of immutable create/settlement receipts.
CREATE TABLE organization_step_telemetry (
 delivery_id TEXT NOT NULL REFERENCES deliveries(id),
 step TEXT NOT NULL CHECK(step='org' OR (step GLOB 'team:[1-9]*' AND substr(step,6)=CAST(CAST(substr(step,6) AS INTEGER) AS TEXT) AND CAST(substr(step,6) AS INTEGER) BETWEEN 1 AND 9007199254740991)),
 unknown_since TEXT,
 last_observed_at TEXT,
 PRIMARY KEY(delivery_id,step)
);
CREATE VIEW operational_delivery_steps AS
 SELECT d.id,d.request_id,l.id AS link_id,l.account_id,d.kind,
 CASE d.kind WHEN 'repo' THEN 'repo' ELSE 'org' END AS step,
 d.target_id,d.delivery_generation,d.create_outcome,d.outcome_reason,
 d.settlement,d.tracking_stopped_reason,d.sent_at,
 CASE d.kind WHEN 'repo' THEN d.unknown_since ELSE t.unknown_since END AS unknown_since,
 CASE d.kind WHEN 'repo' THEN d.last_observed_at ELSE t.last_observed_at END AS last_observed_at
 FROM deliveries d LEFT JOIN invitation_requests r ON r.id=d.request_id
 LEFT JOIN invitation_links l ON l.id=r.invitation_link_id
 LEFT JOIN organization_step_telemetry t ON t.delivery_id=d.id AND t.step='org'
 WHERE d.kind IN ('repo','org')
 UNION ALL
 SELECT d.id,d.request_id,l.id,l.account_id,'team',s.step,
 json_extract(s.receipt,'$.create.team_id'),d.delivery_generation,s.outcome,s.reason,
 json_extract(s.receipt,'$.settlement.state'),json_extract(s.receipt,'$.tracking_stopped'),
 CASE WHEN s.outcome='created' THEN json_extract(s.receipt,'$.create.confirmed_at') ELSE NULL END,
 t.unknown_since,t.last_observed_at
 FROM delivery_steps s JOIN deliveries d ON d.id=s.delivery_id
 LEFT JOIN invitation_requests r ON r.id=d.request_id LEFT JOIN invitation_links l ON l.id=r.invitation_link_id
 LEFT JOIN organization_step_telemetry t ON t.delivery_id=d.id AND t.step=s.step
 WHERE d.kind='org';

-- Preserve old daily snapshots exactly; new items identify each independent step.
ALTER TABLE blocked_digest_items RENAME TO blocked_digest_items_legacy;
DROP INDEX blocked_digest_account;
CREATE TABLE blocked_digest_items (
 day TEXT NOT NULL REFERENCES blocked_digest_days(day) ON DELETE CASCADE,
 delivery_key TEXT NOT NULL,
 step TEXT NOT NULL,
 kind TEXT NOT NULL CHECK(kind IN ('repo','org','team')),
 link_id TEXT NOT NULL,
 account_id INTEGER NOT NULL CHECK(account_id BETWEEN 1 AND 9007199254740991),
 reason TEXT NOT NULL CHECK(reason IN ('installation_unavailable','installation_permissions','requester_identity','outside_collaborators_restricted','ip_allow_list','target_unavailable','mint_forbidden','no_seat','idp_synced_team','managed_enterprise')),
 PRIMARY KEY(day,delivery_key,step),
 CHECK((kind='repo' AND step='repo') OR (kind='org' AND step='org') OR (kind='team' AND step GLOB 'team:[1-9]*')),
 CHECK(kind!='repo' OR reason NOT IN ('no_seat','idp_synced_team','managed_enterprise')),
 CHECK(kind='repo' OR reason!='outside_collaborators_restricted')
);
INSERT INTO blocked_digest_items(day,delivery_key,step,kind,link_id,account_id,reason)
 SELECT day,delivery_key,'repo','repo',link_id,account_id,reason FROM blocked_digest_items_legacy;
DROP TABLE blocked_digest_items_legacy;
CREATE INDEX blocked_digest_account ON blocked_digest_items(day,account_id,link_id);
