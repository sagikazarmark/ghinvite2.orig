import assert from 'node:assert/strict';
import childProcess from 'node:child_process';
import { basename } from 'node:path';
import { finished } from 'node:stream/promises';

const settled = promise => promise.then(() => null, error => error);

// Miniflare 4.20260302.0 destroys its workerd pipe sources before killing the
// child and waits for exit, not close. Preserve those pipes during disposal so
// their Transform destinations receive EOF after all available output drains.
// This changes fixture shutdown only; workerd still receives the original kill.
export class RuntimeOutput {
  #children = [];
  #streams = [];
  #outputs = [];
  #chunks = [];
  #streamChunks = [];
  #bytes = 0;
  #errors = [];
  #restoreSpawn;
  constructor({ limit = 1_000_000, timeoutMs = 10_000 } = {}) {
    this.limit = limit;
    this.timeoutMs = timeoutMs;
  }
  install() {
    assert.equal(this.#restoreSpawn, undefined);
    const original = childProcess.spawn;
    const self = this;
    function spawn(command, ...args) {
      const child = Reflect.apply(original, this, [command, ...args]);
      if (/^workerd(?:\.exe)?$/.test(basename(String(command)))) self.trackChild(child);
      return child;
    }
    childProcess.spawn = spawn;
    this.#restoreSpawn = () => {
      assert.equal(childProcess.spawn, spawn, 'Unexpected concurrent spawn hook');
      childProcess.spawn = original;
    };
  }
  trackChild(child) {
    const closed = settled(new Promise((resolve, reject) => {
      child.once('error', reject);
      child.once('close', resolve);
    }));
    const rawDrains = [child.stdout, child.stderr].map(stream => {
      stream.on('error', error => {
        this.#errors.push(error);
        for (const output of this.#outputs) output.destroy(error);
      });
      return settled(finished(stream, { readable: true, writable: false }));
    });
    this.#children.push({ child, closed, rawDrains });
  }
  capture(...streams) {
    for (const stream of streams) {
      const chunks = [];
      this.#streamChunks.push(chunks);
      const drained = settled(finished(stream, { readable: true, writable: false }));
      this.#streams.push(drained);
      this.#outputs.push(stream);
      stream.on('data', chunk => {
        const bytes = Buffer.from(chunk);
        this.#bytes += bytes.length;
        if (this.#bytes <= this.limit) { this.#chunks.push(bytes); chunks.push(bytes); }
        else if (!this.#errors.length) this.#errors.push(new Error('Unexpected unbounded Worker logs'));
      });
    }
  }
  text() { return Buffer.concat(this.#chunks).toString('utf8'); }
  texts() { return this.#streamChunks.map(chunks => Buffer.concat(chunks).toString('utf8')); }
  async disposeAndDrain(mf) {
    const restores = [];
    for (const { child } of this.#children) {
      for (const stream of [child.stdout, child.stderr]) {
        assert.ok(stream, 'Tracked workerd must have both output pipes');
        const destroy = stream.destroy;
        stream.destroy = function (error) {
          // Never suppress genuine I/O errors. Only defer Miniflare's eager
          // clean destruction until EOF/close, while allowing natural autoDestroy after EOF.
          return error || this.readableEnded ? Reflect.apply(destroy, this, [error]) : this;
        };
        restores.push(() => { stream.destroy = destroy; stream.destroy(); });
      }
    }
    let timer;
    try {
      const completion = (async () => {
        const errors = [...this.#errors];
        try { await mf.dispose(); } catch (error) { errors.push(error); }
        // A failed disposal must still terminate only our tracked children.
        if (errors.length) for (const { child } of this.#children) child.kill('SIGKILL');
        const terminal = await Promise.all([...this.#children.flatMap(x => [x.closed, ...x.rawDrains]), ...this.#streams]);
        errors.push(...terminal.filter(Boolean), ...this.#errors.filter(error => !errors.includes(error)));
        if (errors.length) throw new AggregateError(errors, 'Worker disposal or output drain failed');
        assert.ok(this.#children.length > 0 && this.#streams.length === 2 * this.#children.length, 'Missing tracked Worker output streams');
      })();
      await Promise.race([completion, new Promise((_, reject) => {
        timer = setTimeout(() => reject(new Error('Worker output drain deadline exceeded')), this.timeoutMs);
      })]);
    } finally {
      clearTimeout(timer);
      for (const { child } of this.#children) child.kill('SIGKILL');
      for (const restore of restores) restore();
      this.#restoreSpawn?.();
    }
    return { children: this.#children.length, streams: this.#streams.length, bytes: this.#bytes, drained: true };
  }
}
