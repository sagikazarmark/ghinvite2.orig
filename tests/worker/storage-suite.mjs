import assert from 'node:assert/strict';
import { readFileSync, readdirSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { Miniflare, Response, Log, LogLevel } from 'miniflare';

// Runs every shared Storage conformance scenario (ghinvite-core test_suite)
// against the real D1Storage inside workerd. Scenarios use fixed IDs, so each
// gets its own Miniflare instance: an in-memory, freshly migrated D1 database
// that is discarded with the instance. A failed assertion panics and aborts
// that instance's Wasm, so no poisoned module is reused by the next scenario.
const migrations = readdirSync(new URL('../../migrations/', import.meta.url))
  .filter(file => file.endsWith('.sql')).sort()
  .map(file => readFileSync(new URL(`../../migrations/${file}`, import.meta.url), 'utf8'));

async function withWorker(run) {
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
    for (const sql of migrations) await db.exec(sql.replace(/^--.*$/gm, '').replaceAll('\n', ' '));
    const fixture = async (path, input = null) => {
      const response = await mf.dispatchFetch(`http://worker.test/__fixture/${path}`, {
        method: 'POST', body: JSON.stringify(input), headers: { 'content-type': 'application/json' },
      });
      const text = await response.text();
      assert.equal(response.status, 200, `${path}: ${response.status} ${text}\n${logs.join('')}`);
      return JSON.parse(text);
    };
    return await run({ db, fixture });
  } finally {
    await mf.dispose();
  }
}

const scenarios = await withWorker(({ fixture }) => fixture('storage-suite'));
assert.ok(scenarios.length >= 9, 'scenario inventory must not be empty');
for (const scenario of scenarios) {
  await withWorker(({ fixture }) => fixture(`storage-suite/${scenario}`));
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
