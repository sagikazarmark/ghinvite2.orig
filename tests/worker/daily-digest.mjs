import { registerFinalLogScan } from './runtime-output.mjs';
import assert from 'node:assert/strict';
import { readFileSync, readdirSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { Miniflare, Log, LogLevel, Response } from 'miniflare';

// Exercise the exported production scheduled handler, not a fixture invocation
// of the storage method. Miniflare's Worker.scheduled API runs the real event.
async function run(scenario) {
const logs = [];
let logBytes = 0;
let logOverflow = false;
let outbound = 0;
const worker = (name, script) => ({
  name, modules: true,
  scriptPath: fileURLToPath(new URL(script, import.meta.url)),
  modulesRules: [{ type: 'CompiledWasm', include: ['**/*.wasm'] }],
  compatibilityDate: '2024-09-23', compatibilityFlags: ['nodejs_compat'],
  d1Databases: { DB: 'daily-digest-shared' },
  outboundService() { outbound++; return new Response('Unexpected outbound request', { status: 502 }); },
});
const mf = new Miniflare({
  log: new Log(LogLevel.ERROR),
  handleRuntimeStdio(stdout, stderr) {
    for (const stream of [stdout, stderr]) stream.on('data', chunk => {
      logBytes += chunk.length;
      if (logBytes > 128 * 1024) logOverflow = true;
      else logs.push(chunk.toString());
    });
  },
  workers: [worker('web', './worker.mjs'), worker('fixture', './workflows-worker.mjs')],
});
try {
  const db = await mf.getD1Database('DB', 'web');
  for (const file of readdirSync(new URL('../../migrations/', import.meta.url)).filter(f => f.endsWith('.sql')).sort()) {
    const sql = readFileSync(new URL(`../../migrations/${file}`, import.meta.url), 'utf8');
    await db.exec(sql.replace(/^[ \t]*--.*$/gm, '').replaceAll('\n', ' '));
  }
  const fixture = await mf.getWorker('fixture');
  const seeded = await fixture.fetch('http://fixture/__fixture/storage-suite/'+scenario, { method: 'POST', body: 'null' });
  assert.equal(seeded.status, 200, await seeded.text());
  // The shared fixture proves scoped reads and leaves realistic projected rows.
  // Start this event test without its synthetic dated snapshots.
  await db.exec('DELETE FROM blocked_digest_items; DELETE FROM blocked_digest_days;');
  const rows = async table => (await db.prepare(`SELECT * FROM ${table} ORDER BY 1,2`).all()).results;
  const original = await rows('deliveries');
  const web = await mf.getWorker('web');
  const run = () => web.scheduled({ cron: '5 * * * *', scheduledTime: new Date('2001-01-01T00:05:00Z') });
  const before = Date.now();
  assert.equal((await run()).outcome, 'ok');
  const after = Date.now();
  const headers = await rows('blocked_digest_days');
  assert.equal(headers.length, 1);
  const observed = Date.parse(headers[0].observed_at);
  assert.ok(observed >= before && observed <= after, JSON.stringify(headers));
  assert.equal(headers[0].day, headers[0].observed_at.slice(0, 10));
  assert.equal(headers[0].complete, 1);
  const items = await rows('blocked_digest_items');
  assert.ok(items.length > 0, 'fixture must exercise nonempty daily counts');
  assert.ok(items.every(item=>item.kind === (scenario === 'scenario_blocked_digest' ? 'repo' : 'team')));
  assert.ok(items.every(item=>item.step === (item.kind === 'repo' ? 'repo' : 'team:7')));
  assert.deepEqual(await rows('deliveries'), original, 'cron must not change delivery facts');
  await db.exec("UPDATE deliveries SET outcome_reason='ip_allow_list' WHERE create_outcome='blocked'");
  // A second invocation cannot rewrite observations or append changed reasons.
  assert.equal((await run()).outcome, 'ok');
  assert.deepEqual(await rows('blocked_digest_days'), headers);
  assert.deepEqual(await rows('blocked_digest_items'), items);

  // Force an actual transactional failure, then recover through the same export.
  await db.exec('DELETE FROM blocked_digest_items; DELETE FROM blocked_digest_days;');
  await db.exec("CREATE TRIGGER cron_failure BEFORE INSERT ON blocked_digest_items BEGIN SELECT RAISE(ABORT,'private-cron-database-failure-marker'); END");
  assert.notEqual((await run()).outcome, 'ok', 'failed SQL must fail the scheduled event');
  assert.deepEqual(await rows('blocked_digest_days'), []);
  assert.deepEqual(await rows('blocked_digest_items'), []);
  await db.exec('DROP TRIGGER cron_failure');
  assert.equal((await run()).outcome, 'ok');
  assert.equal((await rows('blocked_digest_days')).length, 1);
  assert.ok((await rows('blocked_digest_items')).length > 0);
  assert.equal(outbound, 0, 'projection maintenance must never call GitHub or Restate');
  assert.equal(logOverflow, false, 'complete runtime log capture must fit its byte limit');
  assert.ok(logs.join('').includes('daily_delivery_digest_failed'));
  assert.ok(!logs.join('').includes('private-cron-database-failure-marker'));
  console.log('PASS actual scheduled Worker/D1 '+scenario+': processing date, immutable retry, atomic failure, recovery and bounded logs');
} finally {
  await mf.dispose();
}

}
await run('scenario_blocked_digest');
await run('scenario_organization_telemetry');

registerFinalLogScan(bytes => { assert.ok(!bytes.includes(Buffer.from('private-cron-database-failure-marker'))); });
