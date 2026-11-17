import { registerFinalLogScan } from './runtime-output.mjs';
import assert from 'node:assert/strict';
import { GITHUB_TOKEN } from './octoemu.mjs';

// Both ordinary random capabilities and the dedicated deterministic entropy
// fixture use these same raw persistence and runtime-log scans.
export async function capabilitySecrecy({ secrets, markerPrefixes = [], credentialMarkers = [], http, admin, db, sessions, authorityCache, logs }) {
  assert.ok(secrets.length > 0);
  const needles = secrets.flatMap(secret => [Buffer.from(secret), Buffer.from(secret, 'base64url')]);
  needles.push(...markerPrefixes.map(prefix => Buffer.from(prefix)));
  needles.push(...credentialMarkers.map(marker => Buffer.from(marker)));
  assert.ok(needles.every(needle => needle.length >= 16), 'scan markers must be distinctive and nonempty');
  const scan = data => {
    const bytes = Buffer.isBuffer(data) ? data : Buffer.from(String(data));
    for (const needle of needles) assert.equal(bytes.includes(needle), false, 'secret marker escaped its permitted boundary');
    // The emulator's GitHub tokens are random: no marker, so look for the shape.
    assert.doesNotMatch(bytes.toString('latin1'), GITHUB_TOKEN, 'a GitHub token escaped its permitted boundary');
  };
  registerFinalLogScan(scan);
  const scanValue = value => {
    scan(JSON.stringify(value));
    if (typeof value === 'string') {
      if (/^(?:[0-9a-fA-F]{2})+$/.test(value)) scan(Buffer.from(value, 'hex'));
      if (/^[A-Za-z0-9+/_=-]+$/.test(value)) scan(Buffer.from(value, 'base64'));
    } else if (Array.isArray(value)) {
      if (value.every(n => Number.isInteger(n) && n >= 0 && n <= 255)) scan(Buffer.from(value));
      value.forEach(scanValue);
    } else if (value && typeof value === 'object') Object.values(value).forEach(scanValue);
  };
  const query = query => http(`${admin}/query`, {query});
  const inventory = await query('SHOW TABLES');
  const systemTables = inventory.rows.filter(row => row.table_schema === 'public' && row.table_name.startsWith('sys_')).map(row => row.table_name).sort();
  assert.ok(systemTables.includes('sys_journal') && systemTables.includes('sys_invocation'), 'discover public system tables, never silently scan an empty inventory');
  assert.equal(new Set(systemTables).size, systemTables.length);
  let systemRows = 0;
  const inventoryCounts = [];
  for (const table of systemTables) {
    assert.match(table, /^sys_[a-z][a-z0-9_]*$/);
    const result = await query(`SELECT * FROM "${table}" LIMIT 100001`);
    assert.ok(result.rows.length <= 100000, 'system table scan exceeded its explicit row bound');
    const count = await query(`SELECT COUNT(*) AS total FROM "${table}"`);
    assert.equal(result.rows.length, Number(count.rows[0].total), `complete ${table} scan (no query truncation or concurrent count change)`);
    result.rows.forEach(scanValue);
    systemRows += result.rows.length;
    inventoryCounts.push(`${table}:${result.rows.length}`);
  }
  const journals = await query('SELECT raw FROM sys_journal LIMIT 100000');
  const count = await query('SELECT COUNT(*) AS total FROM sys_journal');
  assert.ok(journals.rows.length > 0);
  assert.equal(journals.rows.length, Number(count.rows[0].total), 'scan includes every retained journal row');
  for (const row of journals.rows) {
    assert.ok(typeof row.raw === 'string' && /^(?:[0-9a-fA-F]{2})*$/.test(row.raw),'raw journal encoding available');
    scan(Buffer.from(row.raw, 'hex'));
  }
  const state = await query('SELECT * FROM state LIMIT 100000');
  const stateCount = await query('SELECT COUNT(*) AS total FROM state');
  assert.equal(state.rows.length, Number(stateCount.rows[0].total));
  assert.ok(state.rows.length > 0);
  state.rows.forEach(scanValue);
  // D1 owns this reserved table and prohibits reading its key column. It is
  // platform metadata, not application persistence; scan every other table.
  const scanD1 = async () => {
    const tables = await db.prepare("SELECT name FROM sqlite_master WHERE type='table'").all();
    const applicationTables = tables.results.filter(({name}) => name !== '_cf_METADATA');
    let rows = 0;
    for (const {name} of applicationTables) {
      assert.match(name, /^[a-zA-Z_][a-zA-Z_0-9]*$/);
      const result = await db.prepare(`SELECT * FROM "${name}" LIMIT 100001`).all();
      assert.ok(result.results.length <= 100000, 'D1 scan exceeded its explicit row bound');
      const count = await db.prepare(`SELECT COUNT(*) AS total FROM "${name}"`).first();
      assert.equal(result.results.length, Number(count.total), `complete ${name} scan`);
      result.results.forEach(scanValue); rows += result.results.length;
    }
    return { rows, applicationTables };
  };
  // Real persisted negative controls exercise the exact same discovered-table
  // scanner, including D1 BLOB and encoded-text representations. Always remove
  // the isolated fixture table before the positive absence proof.
  await db.prepare('CREATE TABLE secrecy_scan_negative_control (payload BLOB NOT NULL)').run();
  try {
    for (const encoded of [false, true]) {
      const value = encoded ? needles[0].toString('base64') : needles[0].toString();
      await db.prepare(encoded
        ? 'INSERT INTO secrecy_scan_negative_control VALUES (?)'
        : 'INSERT INTO secrecy_scan_negative_control VALUES (CAST(? AS BLOB))').bind(value).run();
      await assert.rejects(scanD1, /secret marker escaped its permitted boundary/, 'persisted marker must make the scan fail');
      await db.prepare('DELETE FROM secrecy_scan_negative_control').run();
    }
  } finally { await db.prepare('DROP TABLE secrecy_scan_negative_control').run(); }
  const { rows, applicationTables } = await scanD1();
  let cursor;
  let sessionRows = 0;
  do {
    const page = await sessions.list({cursor});
    for (const key of page.keys) {
      scanValue(key);
      const value = await sessions.get(key.name, 'arrayBuffer');
      assert.ok(value, 'listed session still exists');
      scan(Buffer.from(value));
      sessionRows++;
    }
    cursor = page.list_complete ? undefined : page.cursor;
  } while (cursor);
  assert.ok(sessionRows > 0);
  let authorityRows = 0;
  cursor = undefined;
  do {
    const page = await authorityCache.list({cursor});
    for (const key of page.keys) {
      scanValue(key);
      const value = await authorityCache.get(key.name);
      assert.ok(value, 'listed authority entry still exists');
      scan(value);
      scanValue(JSON.parse(value));
      authorityRows++;
    }
    cursor = page.list_complete ? undefined : page.cursor;
  } while (cursor);
  assert.ok(authorityRows > 0, 'journey exercised persisted authority decisions');
  for (const log of logs()) scan(log);
  console.log(`PASS discovered system inventory (${systemRows} rows): ${inventoryCounts.join(', ')}`);
  console.log(`PASS ${markerPrefixes.length ? 'deterministic entropy-hook' : 'dynamically generated'} capability, ${credentialMarkers.length} credential marker and GitHub token shape absence: ${journals.rows.length} retained raw journal entries, ${state.rows.length} state rows, ${rows} rows across ${applicationTables.length} D1 application tables, ${sessionRows} session KV rows, ${authorityRows} authority-cache KV rows, body-prefix runtime logs (launcher verifies shutdown suffix); persisted BLOB/encoded-text negative controls rejected`);
}
