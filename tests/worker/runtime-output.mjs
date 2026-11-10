import assert from 'node:assert/strict';
import childProcess from 'node:child_process';
import { syncBuiltinESMExports } from 'node:module';
import { basename } from 'node:path';
import { finished } from 'node:stream/promises';
import { createHash } from 'node:crypto';
import { mkdirSync, writeFileSync } from 'node:fs';

const settled = promise => promise.then(() => null, error => error);
let active;
export function registerFinalLogScan(scan) { active?.scans.push(scan); }
export function retainRuntimeLog(name, bytes) { active?.retain(name, bytes); }
export function trackFixtureOutput(child, name) { active?.trackChild(child, name, false); }

// Fixture-only: pinned Miniflare destroys workerd's source pipes before killing
// the owned child. Guard immediately, including setOptions replacements, so EOF
// reaches both our raw capture and the fixture's existing Transform callbacks.
export class RuntimeOutput {
  children = []; streams = []; scans = []; errors = []; restores = [];
  bytes = 0;
  constructor({ limit = 32 * 1024 * 1024, timeoutMs = 10_000 } = {}) {
    this.limit = limit; this.timeoutMs = timeoutMs;
  }
  install() {
    assert.equal(active, undefined, 'nested runtime launcher'); active = this;
    const original = childProcess.spawn, owner = this;
    function spawn(command, ...args) {
      const child = Reflect.apply(original, this, [command, ...args]);
      if (/^workerd(?:\.exe)?$/.test(basename(String(command)))) owner.trackChild(child, 'workerd', true);
      return child;
    }
    childProcess.spawn = spawn; syncBuiltinESMExports();
    this.restoreSpawn = () => {
      assert.equal(childProcess.spawn, spawn, 'concurrent spawn hook');
      childProcess.spawn = original; syncBuiltinESMExports(); active = undefined;
    };
  }
  append(record, chunk) {
    const bytes = Buffer.from(chunk); this.bytes += bytes.length;
    record.bytes += bytes.length;
    if (this.bytes <= this.limit) record.chunks.push(bytes);
    else if (!this.overflow) { this.overflow = true; this.errors.push(new Error('runtime output exceeded bounded capture')); }
  }
  retain(name, bytes) {
    const record = { name, chunks: [], bytes: 0, drain: Promise.resolve(null) };
    this.streams.push(record); this.append(record, bytes);
  }
  trackChild(child, name = 'workerd', guard = true) {
    const index = this.children.length;
    const closed = settled(new Promise((resolve, reject) => { child.once('error', reject); child.once('close', resolve); }));
    this.children.push({ child, closed, name });
    for (const [fd, stream] of [[1, child.stdout], [2, child.stderr]]) {
      if (!stream) continue; // Explicit fixture stderr-only child.
      const record = { name: `${index}-${name}-${fd}`, chunks: [], bytes: 0,
        drain: settled(finished(stream, { readable: true, writable: false })) };
      this.streams.push(record);
      stream.on('data', chunk => this.append(record, chunk));
      if (guard) {
        const original = stream.destroy;
        stream.destroy = function(error) {
          return error || this.readableEnded ? Reflect.apply(original, this, [error]) : this;
        };
        this.restores.push(() => { stream.destroy = original; stream.destroy(); });
      }
    }
  }
  buffers() { return this.streams.map(record => Buffer.concat(record.chunks)); }
  async drain(failure) {
    let timer;
    try {
      if (failure) { this.errors.push(failure); for (const { child } of this.children) child.kill('SIGKILL'); }
      const results = await Promise.race([
        Promise.all([...this.children.map(x => x.closed), ...this.streams.map(x => x.drain)]),
        new Promise((_, reject) => { timer = setTimeout(() => reject(new Error('runtime output drain deadline exceeded')), this.timeoutMs); }),
      ]);
      this.errors.push(...results.filter(Boolean));
      // Scan each complete byte stream separately: unrelated interleaved stderr
      // must not hide a marker split between two stdout chunks.
      for (const scan of this.scans) for (const bytes of this.buffers()) {
        try { scan(bytes); } catch (error) { this.errors.push(error); }
      }
      if (this.errors.length) throw new AggregateError(this.errors, 'runtime completion or privacy scan failed');
      assert.ok(this.children.some(x => x.name === 'workerd'), 'no owned workerd captured');
      return { children: this.children.length, streams: this.streams.length, bytes: this.bytes,
        finalScanners: this.scans.length, childClose: true, rawEof: true, finalScan: true };
    } finally {
      clearTimeout(timer);
      for (const { child } of this.children) if (child.exitCode === null && child.signalCode === null) child.kill('SIGKILL');
      for (const restore of this.restores) restore();
      this.restoreSpawn?.();
    }
  }
  save(directory) {
    mkdirSync(directory, { recursive: true });
    return this.streams.map((record, index) => {
      const bytes = Buffer.concat(record.chunks), file = `${index}.raw.log`;
      writeFileSync(`${directory}/${file}`, bytes);
      return { file, name: record.name, bytes: record.bytes, retainedBytes: bytes.length,
        sha256: createHash('sha256').update(bytes).digest('hex') };
    });
  }
}
