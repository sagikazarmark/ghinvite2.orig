import { initSync, fetch as workerFetch } from './.generated/index.js';
import wasmModule from './.generated/index_bg.wasm';

initSync({ module: wasmModule });

export default {
  async fetch(request, env, ctx) {
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
