import assert from 'node:assert/strict';
import { mkdirSync, writeFileSync } from 'node:fs';
import { resolve, basename } from 'node:path';
import { fileURLToPath } from 'node:url';
import { RuntimeOutput } from './runtime-output.mjs';

const entry = process.argv[2];
assert.match(entry ?? '', /^[a-z][a-z0-9-]*\.mjs$/);
assert.ok(!['run-runtime.mjs', 'build.mjs', 'build-production.mjs'].includes(entry));
const directory = resolve(process.env.P1_RUNTIME_EVIDENCE ?? `.runtime-evidence/${basename(entry, '.mjs')}-${process.pid}`);
mkdirSync(resolve(directory, '..'), { recursive: true });
mkdirSync(directory, { recursive: false });
writeFileSync(`${directory}/started.json`, JSON.stringify({ entry, pid: process.pid, node: process.version, started: new Date().toISOString() }));
const output = new RuntimeOutput(); output.install();
let failure, completion, prematureExit;
const originalExit = process.exit;
process.exit = code => {
  prematureExit ??= new Error(`fixture requested process.exit(${code ?? 0}) before launcher completion`);
  process.exitCode = Number.isInteger(code) && code !== 0 ? code : 1;
  // Unwind fixture finally blocks. A fixture that catches this still cannot
  // clear our retained failure; drain below terminates any owned live workerd.
  throw prematureExit;
};
try { await import(new URL(entry, import.meta.url)); } catch (error) { failure = error; }
failure ??= prematureExit;
if (process.exitCode && !failure) failure = new Error(`fixture set exit code ${process.exitCode}`);
try { completion = await output.drain(failure); }
catch (error) { failure = error; }
process.exit = originalExit;
const streams = output.save(directory);
writeFileSync(`${directory}/completion.json`, JSON.stringify({ entry, result: failure ? 'failed' : 'passed', completion, streams,
  finished: new Date().toISOString(), fixture: fileURLToPath(new URL(entry, import.meta.url)) }, null, 2));
if (failure) throw failure;
console.log(`PASS runtime launcher: close + raw EOF + final privacy scans (${streams.length} streams)`);
