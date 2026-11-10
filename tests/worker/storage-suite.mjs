import assert from 'node:assert/strict';
import { readFileSync, readdirSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { Miniflare, Response, Log, LogLevel } from 'miniflare';
import './canonical-delivery-migration.mjs';

// Runs every shared Storage conformance scenario (ghinvite-core test_suite)
// against the real D1Storage inside workerd. Scenarios use fixed IDs, so each
// gets its own Miniflare instance: an in-memory, freshly migrated D1 database
// that is discarded with the instance. A failed assertion panics and aborts
// that instance's Wasm, so no poisoned module is reused by the next scenario.
const migrations = readdirSync(new URL('../../migrations/', import.meta.url))
  .filter(file => file.endsWith('.sql')).sort()
  .map(file => ({name:file,sql:readFileSync(new URL(`../../migrations/${file}`, import.meta.url), 'utf8')}));

async function withWorker(run, beforeMigration) {
  const logs = [];
  const mf = new Miniflare({
    log: new Log(LogLevel.ERROR),
    handleRuntimeStdio(stdout, stderr) {
      for (const stream of [stdout, stderr]) stream.on('data', chunk => {
        logs.push(chunk.toString()); if (logs.length > 50) logs.shift();
      });
    },
    modules: true,
    scriptPath: fileURLToPath(new URL('./workflows-worker.mjs', import.meta.url)),
    modulesRules: [{ type: 'CompiledWasm', include: ['**/*.wasm'] }],
    compatibilityDate: '2024-09-23',
    compatibilityFlags: ['nodejs_compat'],
    d1Databases: ['DB'],
    outboundService() { return new Response('Outbound network disabled', { status: 502 }); },
  });
  try {
    const db = await mf.getD1Database('DB');
    for (const {name,sql} of migrations) {
      if (beforeMigration) await beforeMigration(db,name);
      await db.exec(sql.replace(/^[ \t]*--.*$/gm, '').replaceAll('\n', ' '));
    }
    const fixture = async (path, input = null, expectedStatus = 200) => {
      const response = await mf.dispatchFetch(`http://worker.test/__fixture/${path}`, {
        method: 'POST', body: JSON.stringify(input), headers: { 'content-type': 'application/json' },
      });
      const text = await response.text();
      assert.equal(response.status, expectedStatus, `${path}: ${response.status} ${text}\n${logs.join('')}`);
      if (expectedStatus !== 200) return text;
      return JSON.parse(text);
    };
    return await run({ db, fixture });
  } finally {
    await mf.dispose();
  }
}

const scenarios = await withWorker(async ({ fixture, db }) => {
  assert.equal(await db.prepare("SELECT name FROM sqlite_master WHERE type='table' AND name='webhook_receipts'").first(), null);
  assert.deepEqual(await db.prepare('SELECT * FROM organization_webhook_receipts').first(), {
    app:'org',payload_sha256:'c'.repeat(64),received_at:1,expires_at:604801,receipt:'{"retained":"unchanged"}',
  });
  assert.deepEqual(await db.prepare('SELECT * FROM audit_events').first(), {
    id:'kept-audit',account_id:9010,occurred_at:'2026-10-17T12:00:00Z',event_type:'invitation.accepted',actor_kind:'system',actor_id:null,
    target_kind:'github_invitation',target_id:'original-id',metadata:'{"retained":"unchanged"}',request_id:null,
    projection_event_id:null,projection_content:'{"retained":"unchanged"}',evaluated_at:null,
  });
  assert.equal((await db.prepare('PRAGMA foreign_key_check').all()).results.length,0);
  return fixture('storage-suite');
}, async (db,name) => {
  if(name!=='0022_remove_member_webhook_facade.sql') return;
  await db.prepare("INSERT INTO webhook_receipts(payload_sha256,app,event,action,matched_delivery_id) VALUES(?,'repo','member','added',NULL)").bind('b'.repeat(64)).run();
  await db.prepare("INSERT INTO organization_webhook_receipts VALUES('org',?,1,604801,?)").bind('c'.repeat(64),'{"retained":"unchanged"}').run();
  await db.prepare("INSERT INTO audit_events(id,account_id,occurred_at,event_type,actor_kind,target_kind,target_id,metadata,projection_content) VALUES('kept-audit',9010,'2026-10-17T12:00:00Z','invitation.accepted','system','github_invitation','original-id',?,?)")
    .bind('{"retained":"unchanged"}','{"retained":"unchanged"}').run();
});
console.log('PASS additive member-webhook removal preserves organization receipt bytes, audit history and foreign keys');
await withWorker(async ({ fixture }) => {
  const report = await fixture('delivery-signals', '2026-10-17T13:00:00Z');
  assert.equal(report.pending.missing_timestamp, 1);
  assert.ok(report.steps.some(row => row.id === '01K7P000000000000000000002:11:1' && row.sent_at === null));
}, async (db, name) => {
  if (name !== '0023_canonical_delivery_identity.sql') return;
  const seed = readFileSync(new URL('../fixtures/canonical-delivery-migration.sql', import.meta.url), 'utf8');
  await db.exec(seed.replace(/^[ \t]*--.*$/gm, '').replaceAll('\n', ' '));
});
console.log('PASS canonical migration keeps historical missing-send-time delivery signal incomplete');
assert.ok(scenarios.length >= 9, 'scenario inventory must not be empty');
for (const scenario of scenarios) {
  await withWorker(async ({ fixture, db }) => {
    await fixture(`storage-suite/${scenario}`);
    if (scenario === 'scenario_bulk_review') {
      const report = await fixture('delivery-signals', '2026-10-17T12:00:00Z');
      assert.equal(report.repository_deliveries, 3);
      assert.equal(report.organization_deliveries, 1);
      assert.equal(report.team_steps, 1);
      assert.equal(report.email_deliveries, 1);
      assert.equal(report.steps.length, 6);
      assert.equal(report.unknown.total, 2);
      assert.equal(report.unknown.over_threshold, 2);
      for (const row of report.steps) {
        assert.equal(row.link_id, null);
        assert.equal(typeof row.batch_id, 'string');
        assert.equal(row.request_id === null, row.kind === 'email');
      }
      assert.ok(!JSON.stringify(report).includes('email_ref'));
      assert.ok(!JSON.stringify(report).includes('ed'.repeat(32)));
      const before = (await db.prepare('SELECT * FROM blocked_digest_items ORDER BY day,delivery_key,step').all()).results;
      const sample = before[0];
      for (const [batch, row] of [[null,1],[sample.batch_id,null],[sample.batch_id,0],[sample.batch_id,201]]) {
        await assert.rejects(db.prepare('INSERT INTO blocked_digest_items(day,delivery_key,step,kind,link_id,batch_id,batch_row,account_id,reason) VALUES(?,\'invalid-origin\',\'repo\',\'repo\',NULL,?,?,100,\'mint_forbidden\')')
          .bind(sample.day, batch, row).run(), /constraint/i);
      }
      assert.deepEqual((await db.prepare('SELECT * FROM blocked_digest_items ORDER BY day,delivery_key,step').all()).results, before);
    }
    if (scenario === 'scenario_canonical_delivery_audit') {
      const before=(await db.prepare('SELECT delivery_id,request_id,receipt FROM delivery_outcomes ORDER BY delivery_id').all()).results;
      assert.ok(before.length >= 6);
      for (const row of before) {
        const current=JSON.parse(row.receipt).create.audit_version ?? 'legacy';
        const opposite=current==='legacy'?'canonical_v1':'legacy';
        for (const version of [opposite,null,123,'future']) {
          await assert.rejects(db.prepare("UPDATE delivery_outcomes SET receipt=json_set(receipt,'$.create.audit_version',json(?),'$.revision',json_extract(receipt,'$.revision')+1) WHERE delivery_id=?").bind(JSON.stringify(version),row.delivery_id).run(),/delivery audit version/);
          assert.deepEqual((await db.prepare('SELECT delivery_id,request_id,receipt FROM delivery_outcomes ORDER BY delivery_id').all()).results,before);
        }
        for (const version of [null,123,'future']) {
          await assert.rejects(db.prepare("INSERT INTO delivery_outcomes(delivery_id,request_id,receipt) VALUES ('raw-audit-control',?,json_set(?,'$.create.audit_version',json(?)))").bind(row.request_id,row.receipt,JSON.stringify(version)).run(),/delivery audit version\/identity conflict/);
        }
      }
      assert.deepEqual((await db.prepare('SELECT delivery_id,request_id,receipt FROM delivery_outcomes ORDER BY delivery_id').all()).results,before);
    }
    if (scenario === 'scenario_link_installation_bindings') {
      const before=(await db.prepare('SELECT * FROM invitation_links ORDER BY id').all()).results;
      assert.equal(before.length,2);
      const org=before.find(row=>row.installation_app==='org');
      const mixed=before.find(row=>row.organization_installation_id!==null);
      assert.equal(org.installation_id,501);
      assert.equal(org.account_id,9018);
      assert.equal(org.organization_installation_id,null);
      assert.equal(mixed.installation_app,'repo');
      assert.equal(mixed.installation_id,501);
      assert.equal(mixed.organization_installation_id,502);
      assert.equal(mixed.account_id,9017);
      for (const [sql,args] of [
        ["UPDATE invitation_links SET installation_app='repo' WHERE id=?",[org.id]],
        ['UPDATE invitation_links SET organization_installation_id=501 WHERE id=?',[mixed.id]],
        ["DELETE FROM installations WHERE app='org' AND installation_id=502",[]],
        ["DELETE FROM installations WHERE app='org' AND installation_id=501",[]],
        ["UPDATE invitation_links SET organization_installation_app='repo' WHERE id=?",[mixed.id]],
      ]) {
        await assert.rejects(db.prepare(sql).bind(...args).run(),/FOREIGN KEY|constraint/i);
        assert.deepEqual((await db.prepare('SELECT * FROM invitation_links ORDER BY id').all()).results,before);
      }
    }
    if (scenario === 'scenario_email_domains') {
      const links=(await db.prepare('SELECT * FROM invitation_links ORDER BY id').all()).results;
      const requests=(await db.prepare('SELECT * FROM invitation_requests ORDER BY id').all()).results;
      const restricted=links.find(row=>JSON.parse(row.email_domains_json??'[]').length>0);
      const evidenced=requests.find(row=>row.domain_evidence_json!==null);
      assert.equal(evidenced.state,'lapsed');
      assert.deepEqual(JSON.parse(evidenced.domain_evidence_json).matched_domains,['example.com','a.xn--bcher-kva.example']);
      const insertLink="INSERT INTO invitation_links(id,installation_id,account_id,created_by,created_at,approval_required,projection_revision,projection_content,secret_generation,secret_sha256,email_domains_json) SELECT 'domain-raw-control',installation_id,account_id,created_by,created_at,approval_required,projection_revision,projection_content,secret_generation,secret_sha256,? FROM invitation_links WHERE id=?";
      await db.prepare(insertLink).bind(restricted.email_domains_json,restricted.id).run();
      await db.prepare("DELETE FROM invitation_links WHERE id='domain-raw-control'").run();
      for (const rules of ['null','{}','not json','[{}]',JSON.stringify([{exact:'a@example.com'}]),JSON.stringify([{exact:'UPPER.EXAMPLE'}]),JSON.stringify([{exact:'a.example'},{exact:'a.example'}]),JSON.stringify(Array.from({length:33},(_,i)=>({exact:`n${i}.example` })))]) {
        await assert.rejects(db.prepare(insertLink).bind(rules,restricted.id).run());
      }
      const insertRequest="INSERT INTO invitation_requests(id,invitation_link_id,requester_id,state,created_at,projection_revision,domain_evidence_json) SELECT 'domain-proof-control',invitation_link_id,requester_id,'pending',created_at,1,? FROM invitation_requests WHERE id=?";
      await db.prepare(insertRequest).bind(evidenced.domain_evidence_json,evidenced.id).run();
      await db.prepare("DELETE FROM invitation_requests WHERE id='domain-proof-control'").run();
      const invalidProofs=[null,'null','{}','not json',...['evil.example','xn--bcher-kva.example','users.noreply.github.com','a@example.com'].map(domain=>JSON.stringify({...JSON.parse(evidenced.domain_evidence_json),matched_domains:[domain]}))];
      for (const proof of invalidProofs) await assert.rejects(db.prepare(insertRequest).bind(proof,evidenced.id).run());
      await assert.rejects(db.prepare("UPDATE invitation_links SET email_domains_json='[]' WHERE id=?").bind(restricted.id).run());
      await assert.rejects(db.prepare('UPDATE invitation_requests SET domain_evidence_json=NULL WHERE id=?').bind(evidenced.id).run());
      assert.deepEqual((await db.prepare('SELECT * FROM invitation_links ORDER BY id').all()).results,links);
      assert.deepEqual((await db.prepare('SELECT * FROM invitation_requests ORDER BY id').all()).results,requests);
    }
    if (scenario === 'scenario_final_result_projection') {
      const rows=(await db.prepare('SELECT r.justification,d.kind,d.final_result FROM deliveries d JOIN invitation_requests r ON r.id=d.request_id ORDER BY r.justification,d.kind').all()).results;
      assert.equal(rows.length,18);
      const expected={accepted:'satisfied',owned_declined:'declined',foreign_disappeared:'lapsed_invite',own_expiry:'lapsed_invite',expired:'lapsed_invite',link_revoked:'unfulfillable',foreign_stopped:'unfulfillable',failed:'unfulfillable',unknown:null};
      for (const row of rows) {
        assert.ok(['repo','org'].includes(row.kind));
        assert.ok(Object.hasOwn(expected,row.justification));
        assert.equal(row.final_result,expected[row.justification],`${row.kind}/${row.justification}`);
      }
    }
    if (scenario === 'scenario_request_lapsed') {
      const before=(await db.prepare('SELECT * FROM invitation_requests ORDER BY id').all()).results;
      const lapsed=before.find(row=>row.state==='lapsed');
      const declined=before.find(row=>row.state==='declined');
      assert.ok(lapsed && declined);
      assert.equal(before.filter(row=>row.state==='lapsed').length,2);
      for (const update of ["state='approved'", "state='pending'", 'decided_by=701',
        "decided_at='2099-01-01T00:00:00Z'", 'projection_revision=projection_revision-1',
        "projection_content=json_set(projection_content,'$.decision.decision_id','replacement')"]) {
        await assert.rejects(db.prepare(`UPDATE invitation_requests SET ${update} WHERE id=?`).bind(lapsed.id).run(),/constraint|invalid request lapse/i);
        assert.deepEqual((await db.prepare('SELECT * FROM invitation_requests ORDER BY id').all()).results,before);
      }
      await assert.rejects(db.prepare("UPDATE invitation_requests SET state='lapsed',projection_revision=projection_revision+1 WHERE id=?").bind(declined.id).run(),/constraint|invalid request lapse/i);
      await assert.rejects(db.prepare("INSERT INTO invitation_requests(id,invitation_link_id,requester_id,state,created_at,projection_revision) VALUES('unapproved-lapse',?,701,'lapsed',?,3)").bind(lapsed.invitation_link_id,lapsed.created_at).run(),/constraint|invalid request lapse/i);
      assert.deepEqual((await db.prepare('SELECT * FROM invitation_requests ORDER BY id').all()).results,before);
    }
    if (scenario === 'scenario_organization_webhook_bindings') {
      const before=(await db.prepare('SELECT * FROM organization_webhook_receipts ORDER BY app,payload_sha256').all()).results;
      const bound=before.find(row=>row.receipt!==null);
      assert.ok(bound);
      const receipt=JSON.parse(bound.receipt);
      assert.equal(receipt.identity.kind,'organization');
      assert.equal(receipt.account_id,9001);
      assert.equal(receipt.requester_id,701);
      assert.equal(bound.expires_at-bound.received_at,604800);
      for (const update of ['receipt=NULL','received_at=received_at+1,expires_at=expires_at+1',"app='repo'"]) {
        await assert.rejects(db.prepare(`UPDATE organization_webhook_receipts SET ${update} WHERE app='org' AND payload_sha256=?`).bind(bound.payload_sha256).run(),/constraint|organization webhook receipt conflict/i);
        assert.deepEqual((await db.prepare('SELECT * FROM organization_webhook_receipts ORDER BY app,payload_sha256').all()).results,before);
      }
      const plan=(await db.prepare('EXPLAIN QUERY PLAN SELECT payload_sha256 FROM organization_webhook_receipts WHERE expires_at<=? ORDER BY expires_at,payload_sha256 LIMIT 128').bind(bound.expires_at).all()).results;
      assert.ok(plan.some(row=>/organization_webhook_receipts_expiry/.test(row.detail)));
    }
    if (scenario === 'scenario_organization_delivery') {
      const steps=(await db.prepare('SELECT * FROM delivery_steps ORDER BY delivery_id,step').all()).results;
      assert.equal(steps.length,4);
      const owned=steps.find(row=>JSON.parse(row.receipt).create.outcome.via?.kind==='organization_invitation');
      const pending=steps.find(row=>JSON.parse(row.receipt).create.outcome.via?.kind==='pending_membership');
      assert.equal(JSON.parse(owned.receipt).create.outcome.via.upstream_id,990);
      assert.equal(Object.hasOwn(JSON.parse(pending.receipt).create.outcome.via,'upstream_id'),false);
      for (const update of [
        "receipt=json_set(receipt,'$.create.outcome.kind','upgraded'),outcome='upgraded',projection_revision=projection_revision+1",
        "receipt=json_set(receipt,'$.settlement',NULL),projection_revision=projection_revision+1",
        "outcome='upgraded'",
      ]) {
        await assert.rejects(db.prepare(`UPDATE delivery_steps SET ${update} WHERE delivery_id=? AND step=?`).bind(owned.delivery_id,owned.step).run(),/constraint|organization step receipt conflict/i);
        assert.deepEqual((await db.prepare('SELECT * FROM delivery_steps ORDER BY delivery_id,step').all()).results,steps);
      }
      await assert.rejects(db.prepare('INSERT INTO projection_assertions(unchanged) VALUES(0)').run(),/projection_changed/i);
      const assertions=await db.prepare('SELECT COUNT(*) AS n FROM projection_assertions').first();
      assert.equal(assertions.n,0);
    }
    if (scenario === 'scenario_link_projection_conflicts') {
      await db.prepare('DELETE FROM invitation_link_targets').run();
      await fixture('verify-link-repair');
    }
    if (scenario === 'scenario_request_cancellation') {
      const before = await db.prepare('SELECT * FROM invitation_requests').all();
      assert.equal(before.results.length, 1);
      assert.equal(before.results[0].state, 'cancelled');
      assert.equal(before.results[0].cancellation_reason, 'link_revoked');
      for (const sql of [
        'UPDATE invitation_requests SET cancellation_reason=NULL',
        "UPDATE invitation_requests SET cancellation_reason='arbitrary_upstream_text'",
        "UPDATE invitation_requests SET state='approved'",
      ]) {
        await assert.rejects(db.prepare(sql).run(), /constraint|invalid request cancellation/i);
        assert.deepEqual((await db.prepare('SELECT * FROM invitation_requests').all()).results, before.results);
      }
    }
  });
  console.log(`PASS D1 storage suite ${scenario}`);
}
console.log(`PASS shared SQLx/D1 storage conformance: ${scenarios.length} scenarios on actual D1`);

// D1-only: the shared audit seek query over historical UTC encodings,
// nanosecond/ID boundaries and exact page edges, decoded by the actual adapter,
// and D1's plans for it (expression-index seeks, no temporary sort).
await withWorker(async ({ db, fixture }) => {
  const account = 939393;
  const crockford = '0123456789ABCDEFGHJKMNPQRSTVWXYZ';
  const id = n => { let v = BigInt(n) + 1n, s = ''; for (let i = 0; i < 26; i++) { s = crockford[Number(v % 32n)] + s; v /= 32n; } return s; };
  const time = n => ['2026-05-04T12:00:00Z', '2026-05-04T12:00:00+00:00', '2026-05-04T12:00:00.000000001Z',
    '2026-05-04T12:00:00.100Z', '2026-05-04T12:00:00.100000+00:00'][n] ?? `2026-05-04T12:00:00.${100000000 + n}+00:00`;
  await db.batch(Array.from({ length: 53 }, (_, n) => db.prepare(
    "INSERT INTO audit_events (id,account_id,occurred_at,event_type,actor_kind,target_kind,target_id) VALUES (?,?,?,?,'system','installation',?)",
  ).bind(id(n), n === 52 ? account + 1 : account, time(n), n === 51 ? 'request.declined' : 'request.created', `row-${n}`)));
  const range = (from, to) => Array.from({ length: to - from + 1 }, (_, i) => from + i);
  const filtered = 'request.created';
  // Pages are newest first in every direction.
  const cases = [
    [null, 'latest', null, range(27, 51).reverse()],
    [filtered, 'latest', null, range(26, 50).reverse()],
    [filtered, 'before', 26, range(1, 25).reverse()],
    [filtered, 'before', 1, [0]],
    [filtered, 'after', 0, range(1, 25).reverse()],
    [filtered, 'after', 25, range(26, 50).reverse()],
    [filtered, 'before', 0, []],
  ];
  for (const [filter, kind, at, expected] of cases) {
    const input = [account, filter, kind, at === null ? null : id(at), at === null ? null : time(at)];
    const page = await fixture('audit-page', input);
    assert.deepEqual(page.events.map(event => event.target_id), expected.map(n => `row-${n}`), JSON.stringify(input));
    if (kind === 'after' && at === 25) assert.equal(page.has_newer, false, 'no phantom newer page at exact 25-row boundary');
    const plan = JSON.stringify(await fixture('audit-plan', input));
    assert.ok(plan.includes(filter ? 'idx_audit_account_event_order' : 'idx_audit_account_order'), plan);
    assert.ok(!plan.includes('TEMP B-TREE'), plan);
    if (kind !== 'latest') assert.ok(plan.includes('<expr>'), plan);
  }
});
console.log('PASS D1 audit seeks over historical encodings use expression-index plans');

// Operational telemetry cannot authorize a repository update for another kind,
// and a successful observation changes no retained/lifecycle/projection column.
await withWorker(async ({db,fixture}) => {
  await fixture('storage-suite/scenario_repository_telemetry');
  const row=await db.prepare("SELECT * FROM deliveries WHERE create_outcome='outcome_unknown' ORDER BY id LIMIT 1").first();
  const input={request_id:row.request_id,target_id:row.target_id,delivery_generation:row.delivery_generation,account_id:100,unknown_since:null,last_observed_at:'2026-05-04T15:00:00Z'};
  const outcomesBefore=(await db.prepare('SELECT * FROM delivery_outcomes ORDER BY delivery_id').all()).results;
  // An email row shares the unprefixed physical shape, reaching the kind guard
  // itself; an organization row additionally exercises namespace separation.
  await db.prepare("UPDATE deliveries SET kind='email' WHERE id=?").bind(row.id).run();
  await fixture('repository-telemetry',input,409);
  assert.deepEqual(await db.prepare('SELECT * FROM deliveries WHERE id=?').bind(row.id).first(),{...row,kind:'email'});
  await db.prepare("UPDATE deliveries SET kind='repo' WHERE id=?").bind(row.id).run();
  const orgId=`org:${row.id}`;
  // Keep the complete fixture referentially valid under the app-scoped ID CHECK.
  // Deferred checks are enforced when this atomic D1 batch commits.
  const moveKind = (from,to,kind) => db.batch([
    db.prepare('PRAGMA defer_foreign_keys=ON'),
    db.prepare('UPDATE deliveries SET kind=?, id=? WHERE id=?').bind(kind,to,from),
    db.prepare('UPDATE delivery_outcomes SET delivery_id=? WHERE delivery_id=?').bind(to,from),
  ]);
  await moveKind(row.id,orgId,'org');
  await fixture('repository-telemetry',input,409);
  const rejected=await db.prepare('SELECT * FROM deliveries WHERE id=?').bind(orgId).first();
  assert.deepEqual(rejected,{...row,kind:'org',id:orgId});
  // The unified reader sees this intentionally wrong org/account identity and
  // refuses the whole report instead of silently omitting the corrupt row.
  await fixture('delivery-signals','2026-05-04T15:00:00Z',409);
  await moveKind(orgId,row.id,'repo');
  const report=await fixture('delivery-signals','2026-05-04T15:00:00Z');
  assert.equal(report.repository_deliveries,14);
  await fixture('repository-telemetry',input);
  const after=await db.prepare('SELECT * FROM deliveries WHERE id=?').bind(row.id).first();
  assert.equal(after.last_observed_at,'2026-05-04T15:00:00.000000000Z');
  assert.deepEqual({...after,last_observed_at:row.last_observed_at},row);
  assert.deepEqual((await db.prepare('SELECT * FROM delivery_outcomes ORDER BY delivery_id').all()).results,outcomesBefore,'kind-negative fixture leaves exact child receipt bytes unchanged');
});
console.log('PASS operational telemetry kind guard and unchanged receipt/lifecycle columns on actual D1');

// Atomic digest recording must roll back its header and retention cleanup on a
// failed item insert. A retry may never append to the frozen daily snapshot.
await withWorker(async ({db,fixture}) => {
  await fixture('storage-suite/scenario_blocked_digest');
  const rows = async sql => (await db.prepare(sql).all()).results;
  await db.exec("UPDATE deliveries SET create_outcome='blocked',outcome_reason='mint_forbidden' WHERE tracking_stopped_reason IS NOT NULL OR settlement IS NOT NULL");
  const deliveries = await rows('SELECT * FROM deliveries ORDER BY id');
  const headers = await rows('SELECT * FROM blocked_digest_days ORDER BY day');
  const items = await rows('SELECT * FROM blocked_digest_items ORDER BY day,delivery_key');
  await db.exec("CREATE TRIGGER digest_failure BEFORE INSERT ON blocked_digest_items BEGIN SELECT RAISE(ABORT,'digest-test-failure'); END");
  await fixture('record-blocked-digest','2026-05-12T00:05:00Z',409);
  assert.deepEqual(await rows('SELECT * FROM blocked_digest_days ORDER BY day'),headers);
  assert.deepEqual(await rows('SELECT * FROM blocked_digest_items ORDER BY day,delivery_key'),items);
  await db.exec('DROP TRIGGER digest_failure');
  await fixture('record-blocked-digest','2026-05-12T00:05:00Z');
  assert.equal((await db.prepare("SELECT COUNT(*) AS n FROM blocked_digest_items WHERE day='2026-05-12' AND account_id=100").first()).n,5);
  assert.deepEqual(await rows('SELECT * FROM deliveries ORDER BY id'),deliveries);
  assert.equal((await rows('SELECT day FROM blocked_digest_days')).length,7);
  assert.deepEqual(await rows('SELECT i.day FROM blocked_digest_items i LEFT JOIN blocked_digest_days h ON h.day=i.day WHERE h.day IS NULL'),[]);
  await db.exec("CREATE TRIGGER digest_no_retry_scan BEFORE INSERT ON blocked_digest_items BEGIN SELECT RAISE(ABORT,'must-not-reinsert'); END");
  await fixture('record-blocked-digest','2026-05-12T01:05:00Z');
  await fixture('record-blocked-digest','2026-05-04T00:05:00Z');
});
console.log('PASS D1 daily blocked digest atomic failure, retention, frozen retries and unchanged delivery columns');

// Unified operational signals expose independent organization/team steps and
// refuse a partial report or partially committed daily snapshot at capacity.
await withWorker(async ({db,fixture}) => {
  await fixture('storage-suite/scenario_organization_telemetry');
  const report=await fixture('delivery-signals','2026-05-04T16:00:00Z');
  assert.equal(report.organization_deliveries,1);
  assert.equal(report.team_steps,3);
  assert.deepEqual(report.steps.map(row=>row.step),['org','team:5','team:6','team:7']);
  assert.equal(report.pending.future_timestamp,1);
  const oldDays=(await db.prepare('SELECT * FROM blocked_digest_days ORDER BY day').all()).results;
  const oldItems=(await db.prepare('SELECT * FROM blocked_digest_items ORDER BY day,delivery_key,step').all()).results;
  await db.exec("WITH RECURSIVE n(i) AS (SELECT 100 UNION ALL SELECT i+1 FROM n WHERE i<10100) INSERT INTO deliveries(id,request_id,kind,target_id,delivery_generation,create_outcome,outcome_reason,created_at,updated_at) SELECT r.id||':'||n.i||':1',r.id,'repo',n.i,1,'blocked','mint_forbidden',r.created_at,r.created_at FROM n JOIN (SELECT * FROM invitation_requests ORDER BY id LIMIT 1) r");
  await fixture('delivery-signals','2026-05-04T16:00:00Z',409);
  await fixture('record-blocked-digest','2026-05-05T00:05:00Z',409);
  assert.deepEqual((await db.prepare('SELECT * FROM blocked_digest_days ORDER BY day').all()).results,oldDays);
  assert.deepEqual((await db.prepare('SELECT * FROM blocked_digest_items ORDER BY day,delivery_key,step').all()).results,oldItems);
  await fixture('record-blocked-digest','2026-05-04T16:05:00Z');
});
console.log('PASS D1 organization/team signals, independent step identities, report overflow and atomic digest overflow');

// A public decline message is a new immutable field, never a historical-note
// backfill. Raw SQL cannot erase or replace an accepted explicit message.
await withWorker(async ({db,fixture}) => {
  await fixture('storage-suite/scenario_requester_message');
  const rows=async()=> (await db.prepare('SELECT * FROM invitation_requests ORDER BY id').all()).results;
  const before=await rows();
  const shared=before.find(r=>r.requester_message==='Please contact the team.');
  const legacy=before.find(r=>r.decline_reason==='Historical secret');
  assert.ok(shared&&legacy);
  assert.equal(legacy.requester_message,null);
  for(const [id,message] of [[shared.id,null],[shared.id,'replacement'],[legacy.id,'Historical secret']]) {
    await assert.rejects(()=>db.prepare('UPDATE invitation_requests SET requester_message=? WHERE id=?').bind(message,id).run(),/immutable or invalid requester message/);
    assert.deepEqual(await rows(),before);
  }
  await db.prepare('UPDATE invitation_requests SET requester_message=requester_message WHERE id=?').bind(shared.id).run();
  assert.deepEqual(await rows(),before);
});
console.log('PASS D1 explicit requester message immutability and no historical private-note backfill');

// First positive advisory timing is immutable independently of create receipts.
// D1 must preserve exact sub-millisecond timestamps, including raw SQL writes.
await withWorker(async ({db,fixture}) => {
  await fixture('storage-suite/scenario_invitation_timing');
  const rows=async()=> (await db.prepare('SELECT delivery_id,request_id,receipt FROM delivery_outcomes ORDER BY delivery_id').all()).results;
  const before=await rows();
  assert.equal(before.length,2);
  for(const row of before) {
    const original=JSON.parse(row.receipt);
    for(const [field,value] of [
      ['created_at','2026-05-04T13:00:01.000000001Z'],
      ['observed_at','2026-05-04T12:59:59.999999999Z'],
      ['created_at','invalid'],['upstream_id',998],['owned_lifetime_seconds',0],
    ]) {
      const changed=structuredClone(original);changed.invitation_timing[field]=value;
      await assert.rejects(()=>db.batch([
        db.prepare('DELETE FROM delivery_outcomes WHERE delivery_id=?').bind(row.delivery_id),
        db.prepare('INSERT INTO delivery_outcomes(delivery_id,request_id,receipt) VALUES(?,?,?)').bind(row.delivery_id,row.request_id,JSON.stringify(changed)),
      ]),/invitation timing conflict/);
      assert.deepEqual(await rows(),before,'invalid first timing insert rolls back the delete');
    }
    for(const change of [
      value=>{delete value.invitation_timing;},
      value=>{value.invitation_timing.created_at='2026-05-04T13:00:00.000000001Z';},
      value=>{value.invitation_timing.observed_at='2026-05-04T13:00:01.000000001Z';},
      value=>{value.invitation_timing.owned_lifetime_seconds=7200;},
    ]) {
      const changed=structuredClone(original);changed.revision++;change(changed);
      await assert.rejects(()=>db.prepare('UPDATE delivery_outcomes SET receipt=? WHERE delivery_id=?').bind(JSON.stringify(changed),row.delivery_id).run(),/invitation timing conflict/);
      assert.deepEqual(await rows(),before,'raw timing mutation cannot alter the receipt or source clock');
    }
  }
});
console.log('PASS D1 invitation advisory timing: exact source identity, immutable subsecond clocks and policy, INSERT transaction rollback and unchanged create receipts');

// Optional GitHub profile facts are shared by both adapters; old producers may
// omit them, while malformed raw clocks cannot replace a valid cached profile.
await withWorker(async ({db,fixture}) => {
  await fixture('storage-suite/scenario_requester_profiles');
  const before=await db.prepare('SELECT * FROM users WHERE user_id=701').first();
  assert.ok(before.github_created_at.startsWith('2010-01-02T03:04:05'));
  await assert.rejects(()=>db.prepare('UPDATE users SET github_created_at=? WHERE user_id=701').bind('invalid').run(),/CHECK constraint failed/);
  assert.deepEqual(await db.prepare('SELECT * FROM users WHERE user_id=701').first(),before);
  await db.prepare('INSERT INTO users(user_id,login,avatar_url,last_seen_at) VALUES(702,?,NULL,?)').bind('legacy','2026-05-04T12:00:00Z').run();
  assert.equal((await db.prepare('SELECT github_created_at FROM users WHERE user_id=702').first()).github_created_at,null);
});
console.log('PASS D1 optional requester profile metadata, legacy omission and invalid-clock rollback');
