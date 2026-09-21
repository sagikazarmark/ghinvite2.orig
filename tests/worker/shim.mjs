import { initSync, fetch as workerFetch } from './.generated/index.js';
import wasmModule from './.generated/index_bg.wasm';

initSync({ module: wasmModule });

export default {
  async fetch(request, env, ctx) {
    if (request.headers.has('x-test-observe-d1')) {
      let queries = 0;
      let rowsRead = 0;
      const plans = [];
      const observe = (statement, sql, values = []) => new Proxy(statement, {
        get(target, property) {
          if (property === 'constructor') return target.constructor;
          if (property === 'bind') return (...args) => observe(target.bind(...args), sql, args);
          if (['all', 'first', 'run', 'raw'].includes(property)) return async (...args) => {
            queries++;
            const queueRead = sql.startsWith('WITH page');
            if (queueRead || sql.startsWith('SELECT COUNT(*) AS pending')) {
              if (queueRead && request.headers.has('x-test-fail-queue')) throw new Error('private injected queue read failure');
              const plan = await env.DB.prepare(`EXPLAIN QUERY PLAN ${sql}`).bind(...values).all();
              plans.push(...plan.results.map(row => row.detail));
            }
            const result = await target[property](...args);
            rowsRead += result?.meta?.rows_read ?? 0;
            return result;
          };
          const value = target[property];
          return typeof value === 'function' ? value.bind(target) : value;
        },
      });
      const db = new Proxy(env.DB, {
        get(target, property) {
          if (property === 'constructor') return target.constructor;
          if (property === 'prepare') return sql => observe(target.prepare(sql), sql);
          const value = target[property];
          return typeof value === 'function' ? value.bind(target) : value;
        },
      });
      const response = await workerFetch(request, { ...env, DB: db }, ctx);
      response.headers.set('x-test-d1-queries', String(queries));
      response.headers.set('x-test-d1-rows-read', String(rowsRead));
      response.headers.set('x-test-d1-plans', JSON.stringify(plans));
      return response;
    }
    if (!request.headers.has('x-test-fail-kv-read')) {
      return workerFetch(request, env, ctx);
    }

    // Isolated test-only backend failure seam. Only this request's KV binding
    // fails; the compiled Rust routes, middleware, clocks and crypto are real.
    // Count mutations too: throwing alone could hide an attempted write.
    let mutations = 0;
    const failRead = () => Promise.reject(new Error('injected local KV read failure'));
    const failMutation = () => {
      mutations++;
      return Promise.reject(new Error('unexpected mutation during read failure'));
    };
    const failedKV = {
      get: failRead,
      getWithMetadata: failRead,
      put: failMutation,
      delete: failMutation,
      list: env.SESSIONS.list.bind(env.SESSIONS),
    };
    const response = await workerFetch(request, { ...env, SESSIONS: failedKV }, ctx);
    response.headers.set('x-test-kv-mutations', String(mutations));
    return response;
  },
};
