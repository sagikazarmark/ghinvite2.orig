INSERT INTO users(user_id,login,last_seen_at) VALUES(1,'owner','2026-10-17T12:00:00.123456789Z'),(2,'requester','2026-10-17T12:00:00.123456789Z');
INSERT INTO installations(app,installation_id,account_id,account_login,account_type,installed_at,selected_repos) VALUES('repo',77,42,'acme','Organization','2026-10-17T12:00:00.123456789Z','All'),('org',88,42,'acme','Organization','2026-10-17T12:00:00.123456789Z','All');
INSERT INTO invitation_links(id,installation_id,organization_installation_id,account_id,created_by,created_at,approval_required,secret_generation,secret_sha256,projection_revision,projection_content) VALUES('01K7P000000000000000000001',77,88,42,1,'2026-10-17T12:00:00.123456789Z',1,1,'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',1,'{}');
INSERT INTO invitation_requests(id,invitation_link_id,requester_id,state,created_at,projection_revision,projection_content) VALUES('01K7P000000000000000000002','01K7P000000000000000000001',2,'approved','2026-10-17T12:00:00.123456789Z',1,'{}');
INSERT INTO deliveries(id,invitation_id,request_id,kind,target_id,delivery_generation,create_outcome,outcome_reason,effective_role_before,upstream_invitation_id,tracked_foreign,tracking_stopped_reason,settlement,settlement_reason,final_result,cascade_intent,held,sent_at,own_expires_at,unknown_since,last_observed_at,created_at,updated_at,projection_revision) VALUES('01K7P000000000000000000002:10:1','01K7P000000000000000000003','01K7P000000000000000000002','repo',10,1,'created','retained-reason','read',910,1,'own_expiry','accepted','admin_reissue','satisfied','{"retained":true}',1,'2026-10-17T12:00:00.123456789Z','2026-10-18T12:00:00.123456789Z','2026-10-17T12:00:00.123456789Z','2026-10-17T12:01:00.987654321Z','2026-10-17T12:00:00.123456789Z','2026-10-17T12:02:00.111111111Z',7);
INSERT INTO delivery_outcomes(delivery_id,request_id,receipt) VALUES('01K7P000000000000000000002:10:1','01K7P000000000000000000002','{
 "create": {
  "audit_version": "canonical_v1",
  "command": {
   "invitation_id": "01K7P000000000000000000003",
   "delivery_generation": 1,
   "link_id": "01K7P000000000000000000001",
   "request_id": "01K7P000000000000000000002",
   "approval_id": "retained approval",
   "account_id": 42,
   "installation_id": 77,
   "requester_id": 2,
   "repo_id": 10,
   "repo_full_name": "acme/api",
   "permission": "push",
   "approved_at": "2026-10-17T12:00:00.123456789Z"
  },
  "outcome": {
   "kind": "created",
   "upstream_id": 910
  },
  "revision": 2,
  "confirmed_at": "2026-10-17T12:00:00.123456789Z"
 },
 "revision": 3,
 "settlement": null
}');
INSERT INTO delivery_attempts(delivery_key,step,command,attempt_generation,retryable) VALUES('01K7P000000000000000000002:10:1','repo','{
 "invitation_id": "01K7P000000000000000000003",
 "delivery_generation": 1,
 "link_id": "01K7P000000000000000000001",
 "request_id": "01K7P000000000000000000002",
 "approval_id": "retained approval",
 "account_id": 42,
 "installation_id": 77,
 "requester_id": 2,
 "repo_id": 10,
 "repo_full_name": "acme/api",
 "permission": "push",
 "approved_at": "2026-10-17T12:00:00.123456789Z"
}',4,0);
INSERT INTO audit_events(id,account_id,occurred_at,event_type,actor_kind,target_kind,target_id,metadata,projection_content) VALUES('audit:01K7P000000000000000000003',42,'2026-10-17T12:00:00.123456789Z','invitation.sent','system','github_invitation','01K7P000000000000000000003','{ "raw": "retained audit" }','{ "retained": true }');
INSERT INTO audit_events(id,account_id,occurred_at,event_type,actor_kind,target_kind,target_id,metadata,projection_content) VALUES('audit:01K7P000000000000000000002:10:1',42,'2026-10-17T12:00:00.123456789Z','invitation.sent','system','github_invitation','01K7P000000000000000000002:10:1','{ "raw": "retained audit" }','{ "retained": true }');
INSERT INTO deliveries(id,invitation_id,request_id,kind,target_id,delivery_generation,create_outcome,outcome_reason,effective_role_before,upstream_invitation_id,tracked_foreign,tracking_stopped_reason,settlement,settlement_reason,final_result,cascade_intent,held,sent_at,own_expires_at,unknown_since,last_observed_at,created_at,updated_at,projection_revision) VALUES('org:01K7P000000000000000000002:42:1','01K7P000000000000000000004','01K7P000000000000000000002','org',42,1,'created','retained-reason','read',942,1,'own_expiry','accepted','admin_reissue','satisfied','{"retained":true}',1,'2026-10-17T12:00:00.123456789Z','2026-10-18T12:00:00.123456789Z','2026-10-17T12:00:00.123456789Z','2026-10-17T12:01:00.987654321Z','2026-10-17T12:00:00.123456789Z','2026-10-17T12:02:00.111111111Z',7);
INSERT INTO delivery_outcomes(delivery_id,request_id,receipt) VALUES('org:01K7P000000000000000000002:42:1','01K7P000000000000000000002','{
 "create": {
  "audit_version": "canonical_v1",
  "command": {
   "invitation_id": "01K7P000000000000000000004",
   "delivery_generation": 1,
   "link_id": "01K7P000000000000000000001",
   "request_id": "01K7P000000000000000000002",
   "approval_id": "retained approval",
   "account_id": 42,
   "installation_id": 88,
   "requester_id": 2,
   "repo_id": 42,
   "repo_full_name": "acme/api",
   "permission": "push",
   "approved_at": "2026-10-17T12:00:00.123456789Z"
  },
  "outcome": {
   "kind": "created",
   "upstream_id": 942
  },
  "revision": 2,
  "confirmed_at": "2026-10-17T12:00:00.123456789Z"
 },
 "revision": 3,
 "settlement": null
}');
INSERT INTO delivery_attempts(delivery_key,step,command,attempt_generation,retryable) VALUES('org:01K7P000000000000000000002:42:1','org','{
 "invitation_id": "01K7P000000000000000000004",
 "delivery_generation": 1,
 "link_id": "01K7P000000000000000000001",
 "request_id": "01K7P000000000000000000002",
 "approval_id": "retained approval",
 "account_id": 42,
 "installation_id": 88,
 "requester_id": 2,
 "repo_id": 42,
 "repo_full_name": "acme/api",
 "permission": "push",
 "approved_at": "2026-10-17T12:00:00.123456789Z"
}',4,0);
INSERT INTO audit_events(id,account_id,occurred_at,event_type,actor_kind,target_kind,target_id,metadata,projection_content) VALUES('audit:01K7P000000000000000000004',42,'2026-10-17T12:00:00.123456789Z','invitation.sent','system','github_invitation','01K7P000000000000000000004','{ "raw": "retained audit" }','{ "retained": true }');
INSERT INTO audit_events(id,account_id,occurred_at,event_type,actor_kind,target_kind,target_id,metadata,projection_content) VALUES('audit:org:01K7P000000000000000000002:42:1',42,'2026-10-17T12:00:00.123456789Z','invitation.sent','system','github_invitation','org:01K7P000000000000000000002:42:1','{ "raw": "retained audit" }','{ "retained": true }');
INSERT INTO delivery_steps VALUES('org:01K7P000000000000000000002:42:1','team:5','{ "create": {"team_id":5,"outcome":{"kind":"created"},"confirmed_at":"2026-10-17T12:00:00.123456789Z"}, "revision":2 }','created',NULL,'2026-10-17T12:00:00.123456789Z',2);
INSERT INTO organization_step_telemetry VALUES('org:01K7P000000000000000000002:42:1','org','2026-10-17T12:00:00.123456789Z','2026-10-17T12:00:00.123456789Z'),('org:01K7P000000000000000000002:42:1','team:5','2026-10-17T12:00:00.123456789Z','2026-10-17T12:00:00.123456789Z');

-- Historical incomplete projection: an owned Created row has no sent_at.
-- Migration preserves this unknown clock rather than inferring it from JSON.
INSERT INTO deliveries(id,invitation_id,request_id,kind,target_id,delivery_generation,create_outcome,upstream_invitation_id,created_at,updated_at,projection_revision)
 VALUES('01K7P000000000000000000002:11:1','01K7P000000000000000000005','01K7P000000000000000000002','repo',11,1,'created',911,'2026-10-17T12:00:00.123456789Z','2026-10-17T12:02:00.111111111Z',2);
INSERT INTO delivery_outcomes(delivery_id,request_id,receipt)
 SELECT '01K7P000000000000000000002:11:1',request_id,
 json_set(receipt,'$.create.command.invitation_id','01K7P000000000000000000005','$.create.command.repo_id',11,'$.create.outcome.upstream_id',911)
 FROM delivery_outcomes WHERE delivery_id='01K7P000000000000000000002:10:1';
