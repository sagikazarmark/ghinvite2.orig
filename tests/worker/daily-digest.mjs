import { registerFinalLogScan } from './runtime-output.mjs';
import assert from 'node:assert/strict';
import { readFileSync, readdirSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { Miniflare, Log, LogLevel, Response } from 'miniflare';
import { installedWebApp, signinApp } from './app-config.mjs';
import { seedPersonalInstallation } from './installation-fixture.mjs';

// Exercise the exported production scheduled handler, not a fixture invocation
// of the storage method. Miniflare's Worker.scheduled API runs the real event.
async function run(scenario) {
const logs = [];
let logBytes = 0;
let logOverflow = false;
let outbound = 0;
const worker = (name, script) => ({
  name, modules: true,
  // Projection-only scenarios: the sweep watchdog has its own run below.
  bindings: { GH_SWEEP_WATCHDOG_ENABLED: 'false' },
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

// The same scheduled export with the sweep watchdog on (the runtime-tests
// default): one authenticated ingress check over the projected active
// accounts, recorded in the daily digest; an ingress failure fails the event
// without disclosing the ingress answer or credential, and the next run recovers.
async function watchdog() {
  const logs = [];
  const checks = [];
  let answer = 'ok';
  let unexpected = 0;
  const mf = new Miniflare({
    log: new Log(LogLevel.ERROR), modules: true,
    scriptPath: fileURLToPath(new URL('./worker.mjs', import.meta.url)),
    modulesRules: [{ type: 'CompiledWasm', include: ['**/*.wasm'] }],
    compatibilityDate: '2024-09-23', compatibilityFlags: ['nodejs_compat'],
    d1Databases: { DB: 'daily-digest-watchdog' },
    bindings: {
      ...installedWebApp, ...signinApp, P1_HOST: 'watchdog.test',
      P1_KEYRING: 'kekring1;name=p1;current=1;hwm=1;k1=BwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwc.9fdab21f330c89b7',
      RESTATE_INGRESS_URL: 'https://ingress.test', RESTATE_INGRESS_TOKEN: 'WATCHDOG_INGRESS_SECRET_MARKER',
      GHINVITE_GITHUB_INSTALL_URL: 'https://github.com/apps/fixture/installations/new',
    },
    async outboundService(request) {
      const url = new URL(request.url);
      if (url.origin !== 'https://ingress.test' || url.pathname !== '/SweepWatchdog/check') {
        unexpected++;
        return new Response('Unexpected outbound request', { status: 502 });
      }
      checks.push({ authorization: request.headers.get('authorization'), body: await request.json() });
      if (answer !== 'ok') return Response.json({ message: 'private-watchdog-ingress-marker' }, { status: 503 });
      return Response.json({ checked: 1, missing: 0, overdue: 1, unavailable: 0,
        oldest_overdue: { account: { app: 'repo', account_id: 42 }, due_at: '2026-11-20T10:00:00Z' },
        failed: [{ account: { app: 'repo', account_id: 42 }, failed_runs: 2, last_failed_at: '2026-11-24T09:00:00Z' }] });
    },
    handleRuntimeStdio(stdout, stderr) {
      for (const stream of [stdout, stderr]) stream.on('data', chunk => logs.push(chunk.toString()));
    },
  });
  try {
    const db = await mf.getD1Database('DB');
    for (const file of readdirSync(new URL('../../migrations/', import.meta.url)).filter(f => f.endsWith('.sql')).sort()) {
      const sql = readFileSync(new URL(`../../migrations/${file}`, import.meta.url), 'utf8');
      await db.exec(sql.replace(/^[ \t]*--.*$/gm, '').replaceAll('\n', ' '));
    }
    await seedPersonalInstallation(db, { installationId: 1, accountId: 42 });
    await seedPersonalInstallation(db, { installationId: 2, accountId: 43, login: 'retired' });
    await db.exec("UPDATE installations SET uninstalled_at='2026-02-01T00:00:00Z' WHERE installation_id=2");
    const web = await mf.getWorker();
    const run = () => web.scheduled({ cron: '5 * * * *', scheduledTime: new Date('2001-01-01T00:05:00Z') });
    assert.equal((await run()).outcome, 'ok');
    assert.equal(checks.length, 1);
    assert.equal(checks[0].authorization, 'Bearer WATCHDOG_INGRESS_SECRET_MARKER');
    assert.deepEqual(checks[0].body, { accounts: [{ app: 'repo', account_id: 42 }] }, 'active projected accounts only');
    const rows = async () => (await db.prepare('SELECT * FROM sweep_digest_days').all()).results;
    const [day] = await rows();
    assert.equal(day.checks, 1);
    assert.equal(day.rearmed, 1);
    assert.equal(day.oldest_overdue_schedule, 'repo:42');
    assert.equal(day.oldest_overdue_due_at, '2026-11-20T10:00:00.000000000Z');
    assert.equal(day.failed_schedules, 1);
    assert.equal(day.latest_failed_schedule, 'repo:42');
    assert.equal((await db.prepare('SELECT COUNT(*) AS n FROM blocked_digest_days').first()).n, 1);
    answer = 'unavailable';
    assert.notEqual((await run()).outcome, 'ok', 'a failed check fails the scheduled event');
    assert.deepEqual(await rows(), [day], 'an incomplete check records nothing');
    answer = 'ok';
    assert.equal((await run()).outcome, 'ok');
    assert.equal((await rows())[0].checks, 2);
    assert.equal(checks.length, 3);
    assert.equal(unexpected, 0, 'the watchdog never calls GitHub');
    const output = logs.join('');
    assert.ok(output.includes('sweep_watchdog_failed'));
    for (const marker of ['private-watchdog-ingress-marker', 'WATCHDOG_INGRESS_SECRET_MARKER']) assert.ok(!output.includes(marker), marker);
    console.log('PASS actual scheduled Worker sweep watchdog: projected accounts, authenticated ingress check, daily digest record, failure and recovery');
  } finally {
    await mf.dispose();
  }
}
await watchdog();

registerFinalLogScan(bytes => {
  for (const marker of ['private-cron-database-failure-marker', 'private-watchdog-ingress-marker', 'WATCHDOG_INGRESS_SECRET_MARKER'])
    assert.ok(!bytes.includes(Buffer.from(marker)), marker);
});
