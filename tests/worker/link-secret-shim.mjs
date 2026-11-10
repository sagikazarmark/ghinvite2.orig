import { initSync, fetch as workerFetch, scheduled as workerScheduled } from './.generated-link-secret/index.js';
import wasmModule from './.generated-link-secret/index_bg.wasm';

initSync({ module: wasmModule });
// Configuration is deployment-only: no request headers or parameters can
// enable, reset, choose, or disclose fixture entropy.
export default {
  scheduled: workerScheduled, fetch: workerFetch };
