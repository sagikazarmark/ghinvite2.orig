import { initSync, fetch as workerFetch } from './.generated-workflows/index.js';
import wasmModule from './.generated-workflows/index_bg.wasm';
import { AsyncLocalStorage } from 'node:async_hooks';

// Clock is the only mocked system API, opt-in per request and isolated across
// concurrent handlers. Uncontrolled tests exercise real workerd Date/performance.
const clocks = new AsyncLocalStorage();
const RealDate = Date;
globalThis.Date = class extends RealDate {
  constructor(...args) { super(...(args.length || clocks.getStore() === undefined ? args : [clocks.getStore()])); }
  static now() { return clocks.getStore() ?? RealDate.now(); }
};

initSync({ module: wasmModule });
export default {
  async fetch(request, env, ctx) {
    return clocks.run(request.headers.has('x-test-clock') ? Number(request.headers.get('x-test-clock')) : undefined, () => execute(request, env, ctx));
  },
};

async function execute(request, env, ctx) {
    if (request.headers.has('x-test-lose-audit-ack')) {
      // Lose a successful installation-created audit write at the binding seam.
      // Other installation events remain free to drain before replacement.
      let lost = 0;
      const statement = target => new Proxy(target, { get(target, key) {
        if (key === 'constructor') return target.constructor;
        if (key === 'bind') return (...args) => args[3] === 'installation.created' ? statement(target.bind(...args)) : target.bind(...args);
        if (key === 'run') return async (...args) => {
          await target.run(...args);
          lost++;
          throw new Error('fixture: committed audit acknowledgement lost');
        };
        const value = target[key];
        return typeof value === 'function' ? value.bind(target) : value;
      } });
      const db = new Proxy(env.DB, { get(target, key) {
        if (key === 'constructor') return target.constructor;
        if (key === 'prepare') return sql => /INSERT INTO audit_events/i.test(sql) ? statement(target.prepare(sql)) : target.prepare(sql);
        const value = target[key];
        return typeof value === 'function' ? value.bind(target) : value;
      } });
      const response = await workerFetch(request, { ...env, DB: db }, ctx);
      // The handler runs while its response stream is drained, so count only
      // after consuming it rather than sampling before the D1 call executes.
      const body = await response.arrayBuffer();
      const result = new Response(body, response);
      result.headers.set('x-test-lost-audit-acks', String(lost));
      return result;
    }
    if (!request.headers.has('x-test-lose-batch-ack')) return workerFetch(request, env, ctx);
    // Execute the actual local D1 batch, then lose only its acknowledgement.
    const db = new Proxy(env.DB, { get(target, key) {
      if (key === 'constructor') return target.constructor;
      if (key === 'batch') return async statements => {
        await target.batch(statements);
        throw new Error('fixture: committed batch acknowledgement lost');
      };
      const value = target[key];
      return typeof value === 'function' ? value.bind(target) : value;
    } });
    return workerFetch(request, { ...env, DB: db }, ctx);
}
