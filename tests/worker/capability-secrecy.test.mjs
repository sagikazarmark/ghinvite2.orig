import assert from 'node:assert/strict';
import test from 'node:test';
import { capabilitySecrecy } from './capability-secrecy.mjs';

const marker = 'ghs_MARKER_SCANNER_NEGATIVE_CONTROL';
const secret = Buffer.alloc(32, 7).toString('base64url');
const inventory = ['sys_journal', 'sys_invocation', 'sys_new_table'].map(table_name => ({table_schema:'public',table_name}));
const run = (rows, tables = inventory, count = rows.length) => capabilitySecrecy({
  secrets:[secret], credentialMarkers:[marker], admin:'http://admin.test',
  http: async (_url, {query}) => {
    if (query === 'SHOW TABLES') return {rows:tables};
    if (query === 'SELECT * FROM "sys_new_table" LIMIT 100001') return {rows};
    if (query === 'SELECT COUNT(*) AS total FROM "sys_new_table"') return {rows:[{total:count}]};
    if (/^SELECT \* FROM "sys_(journal|invocation)" LIMIT 100001$/.test(query)) return {rows:[]};
    if (/^SELECT COUNT\(\*\) AS total FROM "sys_(journal|invocation)"$/.test(query)) return {rows:[{total:0}]};
    throw new Error('unexpected query after system-table guard');
  },
});

test('newly discovered system tables detect plain, hex, base64 and byte-array secrets', async () => {
  for (const value of [marker, Buffer.from(marker).toString('hex'), Buffer.from(marker).toString('base64'), [...Buffer.from(marker)]]) {
    await assert.rejects(run([{payload:value}]), /secret marker escaped its permitted boundary/);
  }
});
test('empty or foreign-schema system inventories cannot pass', async () => {
  await assert.rejects(run([], []), /discover public system tables/);
  await assert.rejects(run([], inventory.map(row => ({...row,table_schema:'private'}))), /discover public system tables/);
});
test('discovered SQL identifiers and truncated table results fail closed', async () => {
  await assert.rejects(run([], [...inventory, {table_schema:'public',table_name:'sys_x; SELECT 1'}]), /regular expression/);
  await assert.rejects(run([], inventory, 1), /complete sys_new_table scan/);
  await assert.rejects(run(Array(100001).fill({})), /explicit row bound/);
});
