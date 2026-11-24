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
    const clock = request.headers.has('x-test-clock') ? Number(request.headers.get('x-test-clock')) : undefined;
    const response = await clocks.run(clock, () => execute(request, env, ctx));
    return clock === undefined || response.body === null ? response : new Response(underClock(clock, response.body), response);
  },
};

// The Restate handler runs while workerd drains the response body: each pull
// of the wasm stream polls the handler, which reads the clock. Since workerd
// 1.20260619 (miniflare 4.20260623.0+) the runtime drains a returned JS stream
// with its "draining read" (the `enable-draining-read-on-standard-streams`
// autogate, now unconditional and long on in production), and it calls
// pull() outside the request's AsyncLocalStorage store, so those reads saw
// the real time. Re-enter the clock for every read of the body instead.
function underClock(clock, body) {
  const reader = body.getReader();
  return new ReadableStream({
    pull: controller => clocks.run(clock, async () => {
      const { done, value } = await reader.read();
      if (done) controller.close(); else controller.enqueue(value);
    }),
    cancel: reason => reader.cancel(reason),
  });
}

async function execute(request, env, ctx) {
    if (request.headers.has('x-test-lose-audit-ack')) {
      // Lose only the acknowledgement of a committed installation-created
      // transaction. Preserve the old standalone-audit path for retained tests.
      let lost = 0;
      const originals = new WeakMap();
      const creates = new WeakSet();
      const creation = args => {
        if (args[3] === 'installation.created') return true;
        if (typeof args[0] !== 'string') return false;
        try { return JSON.parse(args[0]).events?.some(event => event.event_type === 'installation.created') === true; }
        catch { return false; }
      };
      const statement = target => new Proxy(target, { get(target, key) {
        if (key === 'constructor') return target.constructor;
        if (key === 'bind') return (...args) => {
          const bound = target.bind(...args);
          if (!creation(args)) return bound;
          const proxy = statement(bound);
          originals.set(proxy,bound); creates.add(proxy);
          return proxy;
        };
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
        if (key === 'batch') return async statements => {
          const shouldLose = statements.some(statement=>creates.has(statement));
          const result = await target.batch(statements.map(statement=>originals.get(statement)??statement));
          if (shouldLose) { lost++; throw new Error('fixture: committed installation batch acknowledgement lost'); }
          return result;
        };
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
