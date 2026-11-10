import assert from 'node:assert/strict';
import { fileURLToPath } from 'node:url';
import { Miniflare, Response, Log, LogLevel } from 'miniflare';
import { stalledResponse } from './deadline-recovery.mjs';

const calls = [];
const cleanup = [];
const mf = new Miniflare({
  log: new Log(LogLevel.ERROR), modules: true,
  scriptPath: fileURLToPath(new URL('./worker.mjs', import.meta.url)),
  modulesRules: [{ type: 'CompiledWasm', include: ['**/*.wasm'] }],
  compatibilityDate: '2024-09-23', compatibilityFlags: ['nodejs_compat'],
  async outboundService(request) {
    const url = new URL(request.url);
    calls.push(url.href);
    assert.equal(url.origin, 'https://transport.test', 'credentialed request reached a redirect destination');
    assert.match(request.headers.get('authorization'), /^Bearer ghs_x{640}$/);
    if (url.pathname === '/redirect') {
      return new Response(null, { status: 307, headers: { location: 'https://foreign.test/leak' } });
    }
    if (url.pathname === '/oversize') return new Response('x', { headers: { 'content-length': String(10 * 1024 * 1024 + 1) } });
    if (url.pathname === '/oversize-stream') return new Response(new ReadableStream({ start(controller) {
      controller.enqueue(new Uint8Array(10 * 1024 * 1024 + 1));
      controller.close();
    } }));
    if (url.pathname === '/headers' || url.pathname === '/body') return stalledResponse(url.pathname.slice(1), cleanup);
    return Response.json({ ok: true });
  },
});

try {
  const check = async (path, status, expected) => {
    const response = await mf.dispatchFetch(`https://worker.test/__fixture/github-${path}`);
    const body = await response.text();
    assert.equal(response.status, status, body);
    assert.deepEqual(JSON.parse(body), expected);
    assert.ok(!body.includes('ghs_') && !body.includes('foreign.test'), 'credential or URL disclosed');
  };
  await check('redirect', 200, { status: 307, bytes: 0 });
  await check('oversize', 503, { error: 'Oversize' });
  await check('oversize-stream', 503, { error: 'Oversize' });
  await Promise.all(['headers', 'body'].map(phase => check(phase, 503, { error: 'SentNoResponse { timed_out: true }' })));
  // The first request finishes while the second request's timer is still live.
  // Both must retain their own wake-up across incoming request contexts.
  const first = check('headers', 503, { error: 'SentNoResponse { timed_out: true }' });
  await new Promise(resolve => setTimeout(resolve, 80));
  let watchdog;
  try {
    await Promise.race([
      Promise.all([first, check('body', 503, { error: 'SentNoResponse { timed_out: true }' })]),
      new Promise((_, reject) => { watchdog = setTimeout(() => reject(new Error('staggered request timeout did not fire')), 5000); }),
    ]);
  } finally { clearTimeout(watchdog); }
  try {
    await Promise.race([
      check('waker', 503, { error: 'SentNoResponse { timed_out: true }' }),
      new Promise((_, reject) => { watchdog = setTimeout(() => reject(new Error('timeout did not wake the current task after waker replacement')), 5000); }),
    ]);
  } finally { clearTimeout(watchdog); }
  // The same isolate remains usable on subsequent requests; no earlier fetch or
  // timeout promise is reused across incoming request contexts.
  await check('healthy', 200, { status: 200, bytes: 11 });
  await check('healthy', 200, { status: 200, bytes: 11 });
  assert.equal(calls.length, 10, 'one dispatch per operation, no redirects or hidden retries');
  console.log('PASS actual Worker transport: manual redirects, bounded bodies, header/body deadlines, request isolation');
} finally {
  for (const release of cleanup) release();
  await mf.dispose();
}
