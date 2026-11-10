import assert from 'node:assert/strict';
import { readFileSync, readdirSync } from 'node:fs';
import { execFileSync } from 'node:child_process';
import { Miniflare, Log, LogLevel } from 'miniflare';

// Raw migration proof on actual D1, independent of application fixture Wasm.
// The shared adapter/owner suites separately prove the resulting application.
const directory = new URL('../../migrations/', import.meta.url);
const cutover = readFileSync(new URL('0023_canonical_delivery_identity.sql', directory), 'utf8');
const seed = readFileSync(new URL('../fixtures/canonical-delivery-migration.sql', import.meta.url), 'utf8');
const executable = sql => sql.replace(/^[ \t]*--.*$/gm, '').replaceAll('\n', ' ');
// D1 batch is transactional. Use SQLite's parser to keep trigger bodies whole;
// splitting on semicolons would corrupt their BEGIN/END statements.
const statements = JSON.parse(execFileSync('python3', ['-c', `
import json,sqlite3,sys
statements=[]
pending=''
for char in sys.stdin.read():
 pending+=char
 if char==';' and sqlite3.complete_statement(pending):
  statements.append(pending)
  pending=''
assert not pending.strip(), 'unterminated migration statement'
print(json.dumps(statements))
`], { input: cutover, encoding: 'utf8' }));
const applyCutover = db => db.batch(statements.map(sql => db.prepare(executable(sql))));

async function snapshot(db) {
  const tables = (await db.prepare("SELECT name FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%' AND name NOT GLOB '_cf_*' ORDER BY name").all()).results;
  const result = {};
  for (const { name } of tables) {
    assert.match(name, /^[a-zA-Z_][a-zA-Z0-9_]*$/);
    const columns = (await db.prepare(`PRAGMA table_info(${name})`).all()).results
      .map(row => row.name).filter(column => name !== 'deliveries' || column !== 'invitation_id');
    result[name] = (await db.prepare(`SELECT json_array(${columns.join(',')}) AS raw FROM ${name} ORDER BY raw`).all()).results;
  }
  return result;
}

async function withDatabase(run) {
  const mf = new Miniflare({
    log: new Log(LogLevel.ERROR), modules: true,
    script: 'export default { fetch() { return new Response("migration proof") } }',
    compatibilityDate: '2024-09-23', d1Databases: ['DB'],
    outboundService() { throw new Error('Unexpected outbound request'); },
  });
  try {
    const db = await mf.getD1Database('DB');
    for (const name of readdirSync(directory).filter(name => /^\d+_.*\.sql$/.test(name)).sort()) {
      if (Number(name.split('_')[0]) <= 22) {
        await db.exec(executable(readFileSync(new URL(name, directory), 'utf8')));
      }
    }
    await db.exec(executable(seed));
    await run(db);
  } finally {
    await mf.dispose();
  }
}

await withDatabase(async db => {
  const missingClock = "SELECT count(*) AS count FROM operational_delivery_steps WHERE create_outcome='created' AND settlement IS NULL AND sent_at IS NULL";
  assert.equal((await db.prepare(missingClock).first()).count, 1);
  const before = await snapshot(db);
  const schema = (await db.prepare("SELECT type,name,tbl_name,sql FROM sqlite_schema WHERE type IN ('trigger','view') ORDER BY type,name").all()).results;
  const columns = (await db.prepare('PRAGMA table_info(deliveries)').all()).results;
  const children = {};
  for (const table of Object.keys(before)) {
    children[table] = (await db.prepare(`PRAGMA foreign_key_list(${table})`).all()).results;
  }
  await applyCutover(db);
  assert.equal((await db.prepare(missingClock).first()).count, 1);
  assert.deepEqual(await snapshot(db), before, 'every retained raw column and child/audit/fence byte');
  assert.deepEqual((await db.prepare('PRAGMA table_info(deliveries)').all()).results.map(({ cid, ...column }) => column),
    columns.filter(column => column.name !== 'invitation_id').map(({ cid, ...column }) => column));
  assert.deepEqual((await db.prepare("SELECT type,name,tbl_name,sql FROM sqlite_schema WHERE type IN ('trigger','view') ORDER BY type,name").all()).results, schema);
  for (const [table, expected] of Object.entries(children)) {
    assert.deepEqual((await db.prepare(`PRAGMA foreign_key_list(${table})`).all()).results, expected);
  }
  assert.deepEqual((await db.prepare('PRAGMA foreign_key_check').all()).results, []);
  for (const sql of [
    'UPDATE deliveries SET upstream_invitation_id=999',
    'UPDATE deliveries SET tracking_stopped_reason=NULL',
    "INSERT INTO organization_step_telemetry VALUES('missing','org',NULL,NULL)",
    "UPDATE delivery_outcomes SET receipt=json_set(receipt,'$.create.command.requester_id',999)",
  ]) {
    await assert.rejects(db.prepare(sql).run(), /conflict|constraint/i);
  }
  assert.deepEqual(await snapshot(db), before);
});
console.log('PASS actual D1 raw delivery rebuild preserves all columns, receipts, children, fences, audit bytes, views, triggers and FK guards');

await withDatabase(async db => {
  await db.prepare("DELETE FROM delivery_outcomes WHERE delivery_id NOT LIKE 'org:%'").run();
  const before = await snapshot(db);
  await assert.rejects(applyCutover(db), /constraint/i);
  assert.deepEqual(await snapshot(db), before, 'preflight refusal preserves the complete original database');
  assert.ok((await db.prepare('PRAGMA table_info(deliveries)').all()).results.some(row => row.name === 'invitation_id'));
});
console.log('PASS actual D1 cutover refuses a bridge-only historical audit identity before dropping any source data');
