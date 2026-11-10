import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { PassThrough } from 'node:stream';
import { EventEmitter } from 'node:events';
import test from 'node:test';
import { RuntimeOutput } from './runtime-output.mjs';

function realChild(text, options) {
  const output = new RuntimeOutput(options);
  const child = spawn(process.execPath, ['-e', `process.stdin.resume();process.stdin.on('end',()=>{process.stdout.write(${JSON.stringify(text)});process.stderr.write('late stderr');});`], { stdio: ['pipe', 'pipe', 'pipe'] });
  output.trackChild(child);
  output.capture(child.stdout.pipe(new PassThrough()), child.stderr.pipe(new PassThrough()));
  return { child, output, mf: { async dispose() {
    // Same destructive source-pipe ordering as pinned Miniflare, then trigger
    // natural output/EOF instead of a timed sleep. dispose resolves first.
    child.stdout.destroy(); child.stderr.destroy(); child.stdin.end();
  } } };
}
test('disposal waits for late output, natural raw EOF and child close', async () => {
  const { child, output, mf } = realChild('late output');
  const result = await output.disposeAndDrain(mf);
  assert.equal(result.drained, true); assert.equal(result.children, 1);
  assert.equal(child.stdout.readableEnded, true); assert.equal(child.stderr.readableEnded, true);
  assert.equal(child.exitCode, 0); assert.match(output.text(), /late output/); assert.match(output.text(), /late stderr/);
});
test('marker emitted only after disposal starts cannot pass privacy scan', async () => {
  const { output, mf } = realChild('ghr_MARKER_late');
  assert.doesNotMatch(output.text(), /ghr_MARKER_/); // Previous pre-dispose scan misses it.
  await output.disposeAndDrain(mf);
  assert.throws(() => assert.doesNotMatch(output.text(), /ghr_MARKER_/), /ghr_MARKER_/);
});
test('late output overflow fails without unbounded retained chunks', async () => {
  const { output, mf } = realChild('x'.repeat(4096), { limit: 32 });
  await assert.rejects(output.disposeAndDrain(mf), error => error instanceof AggregateError && error.errors.some(x => /unbounded/.test(x.message)));
  assert.ok(Buffer.byteLength(output.text()) <= 32);
});
function fakeChild() {
  const child = new EventEmitter(); child.stdout = new PassThrough(); child.stderr = new PassThrough();
  child.kill = () => { child.stdout.end(); child.stderr.end(); child.emit('close', null, 'SIGKILL'); };
  const output = new RuntimeOutput({ timeoutMs: 100 }); output.trackChild(child);
  output.capture(child.stdout.pipe(new PassThrough()), child.stderr.pipe(new PassThrough()));
  return { child, output };
}
test('failed disposal still kills and drains owned child, preserving failure', async () => {
  const { output } = fakeChild();
  await assert.rejects(output.disposeAndDrain({ async dispose() { throw new Error('dispose failure'); } }), error => error instanceof AggregateError && error.errors.some(x => x.message === 'dispose failure'));
});
test('raw pipe error is retained and cannot produce a passing drain', async () => {
  const { child, output } = fakeChild();
  await assert.rejects(output.disposeAndDrain({ async dispose() {
    child.stdout.destroy(new Error('raw output failure')); child.stderr.end(); child.emit('close');
  } }), error => error instanceof AggregateError && error.errors.some(x => x.message === 'raw output failure')); 
});
test('premature destination close refuses rather than silently dropping output', async () => {
  const { child, output } = fakeChild();
  const premature = new PassThrough(); output.capture(premature); premature.destroy();
  await assert.rejects(output.disposeAndDrain({ async dispose() { child.kill(); } }), error => error instanceof AggregateError && error.errors.some(x => x.code === 'ERR_STREAM_PREMATURE_CLOSE'));
});
test('missing EOF has a bounded failure and restores owned pipe methods', async () => {
  const { child, output } = fakeChild(); const original = child.stdout.destroy;
  await assert.rejects(output.disposeAndDrain({ async dispose() {} }), /drain deadline exceeded/);
  assert.equal(child.stdout.destroy, original);
});

test('per-stream concatenation detects a marker split around unrelated stream output', async () => {
  const { child, output } = fakeChild();
  await output.disposeAndDrain({ async dispose() {
    child.stdout.write('ghr_MAR'); child.stderr.write('unrelated'); child.stdout.write('KER_late'); child.kill();
  } });
  assert.doesNotMatch(output.text(), /ghr_MARKER_/);
  assert.equal(output.texts()[0], 'ghr_MARKER_late');
  assert.throws(() => output.texts().forEach(text => assert.doesNotMatch(text, /ghr_MARKER_/)), /ghr_MARKER_/);
});
