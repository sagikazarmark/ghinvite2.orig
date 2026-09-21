import assert from 'node:assert/strict';
import { fileURLToPath } from 'node:url';
import { Miniflare, Response, Log, LogLevel } from 'miniflare';
import { stalledResponse } from './deadline-recovery.mjs';

// No fixture forwarding deadline: only the application's real fetch timer can
// complete these requests. The watchdog fails the test, never supplies a result.
const cleanup = [];
let phase = process.env.STALL_PHASE ?? 'headers';
const calls = [];
const mf = new Miniflare({
  log: new Log(LogLevel.ERROR), modules: true,
  scriptPath: fileURLToPath(new URL('./worker.mjs', import.meta.url)),
  modulesRules: [{ type: 'CompiledWasm', include: ['**/*.wasm'] }],
  compatibilityDate: '2024-09-23', compatibilityFlags: ['nodejs_compat'],
  bindings: { GHINVITE_RESTATE_INGRESS: 'https://transport.test' },
  async outboundService(request) {
    assert.equal(new URL(request.url).origin, 'https://transport.test');
    calls.push({ path: new URL(request.url).pathname, body: await request.text() });
    if (phase === 'healthy') return Response.json({ recovered: true });
    return stalledResponse(phase, cleanup);
  },
});

let watchdog;
try {
  await Promise.race([
    (async () => {
      const methods = ['github', 'call', 'send'];
      await Promise.all(methods.map(async method => {
        const start = Date.now();
        const response = await mf.dispatchFetch(`https://worker.test/__fixture/${method}?original-operation`);
        const body = await response.text();
        // send needs only response headers: a successful ingress acknowledgement
        // does not require consuming an unused body.
        if (method === 'send' && phase === 'body') {
          assert.equal(response.status, 200, body);
          return;
        }
        assert.equal(response.status, 503, body);
        if (method !== 'github') assert.match(body, /outcome unknown/i);
        const policy = method === 'github' ? 30_000 : 15_000;
        assert.ok(Date.now() - start >= policy - 1000, `${method}: premature timeout`);
        assert.ok(Date.now() - start < policy + 5000, `${method}: deadline exceeded`);
      }));
      console.log(`PASS Worker ${phase} stalls bounded by production GitHub/Restate deadlines`);
      phase = 'healthy';
      for (const method of ['call', 'send']) {
        const response = await mf.dispatchFetch(`https://worker.test/__fixture/${method}?original-operation`);
        assert.equal(response.status, 200, await response.text());
      }
      const mutations = calls.filter(call => call.path !== '/github');
      assert.equal(mutations.length, 4, 'one request per call, no automatic HTTP retry');
      assert.ok(mutations.every(call => JSON.parse(call.body).operation_id === 'original-operation'));
    })(),
    new Promise((_, reject) => { watchdog = setTimeout(() => reject(new Error('application deadline missing (35s watchdog)')), 35_000); }),
  ]);
} finally {
  clearTimeout(watchdog);
  for (const release of cleanup) release();
  await mf.dispose();
}
