import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { EventEmitter } from 'node:events';
import { PassThrough } from 'node:stream';
import test from 'node:test';
import { RuntimeOutput, registerFinalLogScan } from './runtime-output.mjs';

function fake() {
  const child = new EventEmitter(); child.stdout = new PassThrough(); child.stderr = new PassThrough();
  child.exitCode = null; child.signalCode = null;
  child.kill = () => { child.signalCode = 'SIGKILL'; child.stdout.end(); child.stderr.end(); child.emit('close'); };
  const output = new RuntimeOutput({ timeoutMs: 100 }); output.trackChild(child);
  return { output, child };
}
test('early Miniflare destruction across two lifetimes preserves late raw bytes and close', async () => {
  const output = new RuntimeOutput();
  for (let i = 0; i < 2; i++) {
    const child = spawn(process.execPath, ['-e', "process.stdin.resume();process.stdin.on('end',()=>{process.stdout.write('late stdout');process.stderr.write('late stderr');});"], { stdio: ['pipe','pipe','pipe'] });
    output.trackChild(child);
    const destination = child.stdout.pipe(new PassThrough()); destination.resume();
    child.stdout.destroy(); child.stderr.destroy(); child.stdin.end();
  }
  const result = await output.drain();
  assert.equal(result.children, 2); assert.equal(result.rawEof, true);
  assert.deepEqual(output.buffers().map(x => x.toString()), ['late stdout','late stderr','late stdout','late stderr']);
});
test('late dynamic privacy scanner detects split marker despite interleaved stream', async () => {
  const { output, child } = fake(); output.install();
  registerFinalLogScan(bytes => assert.ok(!bytes.includes(Buffer.from('PRIVATE_MARKER'))));
  child.stdout.write('PRIVATE_'); child.stderr.write('unrelated'); child.stdout.write('MARKER'); child.kill();
  await assert.rejects(output.drain(), AggregateError);
});
test('overflow is bounded and cannot pass', async () => {
  const { output, child } = fake(); output.limit = 16;
  child.stdout.write('x'.repeat(32)); child.kill();
  await assert.rejects(output.drain(), error => error.errors.some(x => /bounded/.test(x.message)));
  assert.equal(output.buffers().reduce((n,x) => n+x.length, 0), 0);
});
test('real pipe errors are never suppressed', async () => {
  const { output, child } = fake(); child.stdout.destroy(new Error('pipe error')); child.kill();
  await assert.rejects(output.drain(), error => error.errors.some(x => x.message === 'pipe error'));
});
test('premature non-EOF close fails rather than silently truncating', async () => {
  const { output, child } = fake(); child.stdout.emit('close'); child.kill();
  await assert.rejects(output.drain(), error => error.errors.some(x => x.code === 'ERR_STREAM_PREMATURE_CLOSE'));
});
test('failure still drains owned child and preserves original error', async () => {
  const { output } = fake();
  await assert.rejects(output.drain(new Error('disposal failure')), error => error.errors.some(x => x.message === 'disposal failure'));
});
test('missing EOF fails bounded and restores clean destroy', async () => {
  const { output, child } = fake(); const guarded = child.stdout.destroy;
  await assert.rejects(output.drain(), /drain deadline/);
  assert.notEqual(child.stdout.destroy, guarded);
});

test('early process.exit(0) cannot manufacture launcher completion evidence', async () => {
  const { mkdtempSync, writeFileSync, existsSync, readFileSync, rmSync } = await import('node:fs');
  const { tmpdir } = await import('node:os'); const { join } = await import('node:path');
  const { fileURLToPath } = await import('node:url');
  const fixture = `runtime-early-exit-control-${process.pid}.mjs`;
  const path = fileURLToPath(new URL(fixture, import.meta.url));
  const directory = mkdtempSync(join(tmpdir(), 'p1-early-exit-'));
  try {
    writeFileSync(path, 'try { process.exit(0); } catch {}\n');
    const child = spawn(process.execPath, [fileURLToPath(new URL('run-runtime.mjs', import.meta.url)), fixture],
      { env: { ...process.env, P1_RUNTIME_EVIDENCE: join(directory, 'capture') }, stdio: 'ignore' });
    const code = await new Promise(resolve => child.once('close', resolve));
    assert.notEqual(code, 0); assert.equal(existsSync(join(directory, 'capture', 'started.json')), true);
    assert.equal(JSON.parse(readFileSync(join(directory, 'capture', 'completion.json'))).result, 'failed');
  } finally { rmSync(path, { force: true }); rmSync(directory, { recursive: true, force: true }); }
});

test('spawn interception ignores an ordinary child owned by the same Node', async () => {
  const output = new RuntimeOutput(); output.install();
  const child = spawn(process.execPath, ['-e', "console.log('not a workerd');"], { stdio: 'ignore' });
  await new Promise(resolve => child.once('close', resolve));
  assert.equal(output.children.length, 0);
  await assert.rejects(output.drain(), /no owned workerd/);
});

test('uncaught premature exit unwinds fixture finally before failed completion', async () => {
  const { mkdtempSync, writeFileSync, readFileSync, rmSync } = await import('node:fs');
  const { tmpdir } = await import('node:os'); const { join } = await import('node:path');
  const { fileURLToPath } = await import('node:url');
  const fixture = `runtime-unwind-control-${process.pid}.mjs`;
  const path = fileURLToPath(new URL(fixture, import.meta.url));
  const directory = mkdtempSync(join(tmpdir(), 'p1-exit-unwind-'));
  try {
    writeFileSync(path, `import {writeFileSync} from 'node:fs';try {process.exit(0);} finally {writeFileSync(${JSON.stringify(join(directory,'cleanup'))},'finished');}\n`);
    const child = spawn(process.execPath, [fileURLToPath(new URL('run-runtime.mjs', import.meta.url)), fixture],
      { env: { ...process.env, P1_RUNTIME_EVIDENCE: join(directory, 'capture') }, stdio: 'ignore' });
    assert.notEqual(await new Promise(resolve => child.once('close', resolve)), 0);
    assert.equal(readFileSync(join(directory,'cleanup'),'utf8'),'finished');
    assert.equal(JSON.parse(readFileSync(join(directory,'capture','completion.json'))).result,'failed');
  } finally { rmSync(path,{force:true});rmSync(directory,{recursive:true,force:true}); }
});
