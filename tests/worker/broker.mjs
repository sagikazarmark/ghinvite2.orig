import assert from 'node:assert/strict';
import { fileURLToPath } from 'node:url';
import { Miniflare, Response, Log, LogLevel } from 'miniflare';

// This file consumes build.mjs's actual compiled Worker: ghinvite-github's
// GithubApps over octotoken's AppTokenProvider, built per incoming request with
// its isolate-scope token cache (PD-4). It never substitutes a JavaScript token
// cache or transport for the production Rust modules.
const counts = new Map();
const waiting = new Map();
const controls = new Map();
const listeners = new Set();
const cleanup = [];
const now = Math.floor(Date.now() / 1000);
function notify() { for (const listener of [...listeners]) listener(); }
async function until(predicate, label) {
  if (predicate()) return;
  let timer;
  await new Promise((resolve, reject) => {
    const listener = () => { if (predicate()) { clearTimeout(timer); listeners.delete(listener); resolve(); } };
    timer = setTimeout(() => { listeners.delete(listener); reject(new Error(`timed out: ${label}`)); }, 10000);
    listeners.add(listener);
    listener();
  });
}
function minted(id, ordinal, lifetime = 3600) {
  return Response.json({ token: `runtime-token-${id}-${ordinal}-${'x'.repeat(640)}`, expires_at: new Date((now + lifetime) * 1000).toISOString() }, { status: 201 });
}
const mf = new Miniflare({
  log: new Log(LogLevel.ERROR), modules: true,
  scriptPath: fileURLToPath(new URL('./worker.mjs', import.meta.url)),
  modulesRules: [{ type: 'CompiledWasm', include: ['**/*.wasm'] }],
  compatibilityDate: '2024-09-23', compatibilityFlags: ['nodejs_compat'],
  async outboundService(request) {
    const url = new URL(request.url);
    assert.equal(url.origin, 'https://broker.test');
    const control = url.pathname.match(/^\/control\/cancel\/(\d+)$/);
    if (control) {
      const id = Number(control[1]);
      assert.equal(request.headers.get('authorization'), null);
      if (id === 103) { await until(() => counts.get(id) === 1, 'cancel-late first mint'); return Response.json({ release: true }); }
      return new Promise(resolve => { controls.set(id, () => resolve(Response.json({ release: true }))); notify(); });
    }
    const mint = url.pathname.match(/^\/app\/installations\/(\d+)\/access_tokens$/);
    assert.ok(mint, `unexpected broker call ${url.pathname}`);
    assert.equal(request.method, 'POST');
    assert.equal(request.headers.get('user-agent'), 'runtime-broker-fixture');
    assert.match(request.headers.get('authorization'), /^Bearer [A-Za-z0-9_-]+\.[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+$/);
    assert.deepEqual(await request.json(), { permissions: { metadata: 'read' } });
    const id = Number(mint[1]);
    const ordinal = (counts.get(id) ?? 0) + 1;
    counts.set(id, ordinal);
    if (id === 103 && ordinal === 2) { notify(); return minted(id, ordinal); }
    return new Promise(resolve => {
      const release = lifetime => resolve(minted(id, ordinal, lifetime));
      waiting.set(`${id}:${ordinal}`, release); cleanup.push(() => release(3600)); notify();
    });
  },
});
async function dispatch(operation, id) {
  const response = await mf.dispatchFetch(`https://worker.test/__fixture/broker/${operation}?installation=${id}`);
  const text = await response.text();
  assert.equal(response.status, 200, text);
  assert.ok(!text.includes('runtime-token') && !text.includes('Bearer'), 'credential escaped fixture response');
  return JSON.parse(text);
}
async function bounded(promise, label) {
  let timer;
  try { return await Promise.race([promise, new Promise((_, reject) => { timer = setTimeout(() => reject(new Error(`request timed out: ${label}`)), 15000); })]); }
  finally { clearTimeout(timer); }
}
function releaseAll(id, lifetime = 3600) {
  for (const [key, release] of [...waiting]) if (key.startsWith(`${id}:`)) { waiting.delete(key); release(lifetime); }
}
async function settled(id) {
  // No new token creation for this installation for 200 ms.
  for (let seen = -1; seen !== counts.get(id);) { seen = counts.get(id); await new Promise(resolve => setTimeout(resolve, 200)); }
}
try {
  // Twenty concurrent misses in one incoming request. On Workers requests never
  // wait on each other's I/O, so there is no coalescing: each miss may create
  // its own token (ADR 0003 amendment). All must succeed.
  const coalesced = dispatch('coalesce', 101);
  await until(() => waiting.has('101:1'), 'first concurrent creation');
  await settled(101);
  const created = counts.get(101);
  assert.ok(created >= 1 && created <= 20, `concurrent misses created ${created} tokens`);
  releaseAll(101);
  assert.equal((await bounded(coalesced, 'concurrent misses')).ok, true);
  assert.equal((await bounded(dispatch('ensure', 101), 'isolate cache')).ok, true);
  assert.equal(counts.get(101), created, 'a later incoming request must reuse the isolate-cached token');

  // Both contexts create concurrently. Cancelling A must not cancel or donate
  // its request-owned fetch to B; B has reached its own upstream before A is released.
  const cancelled = dispatch('cancel', 102);
  await until(() => waiting.has('102:1') && controls.has(102), 'first incoming context');
  const survivor = dispatch('ensure', 102);
  await until(() => waiting.has('102:2'), 'independent second incoming context');
  controls.get(102)();
  assert.deepEqual(await bounded(cancelled, 'cancellation'), { cancelled: true });
  waiting.get('102:2')(3600);
  assert.equal((await bounded(survivor, 'surviving context')).ok, true);
  assert.equal(counts.get(102), 2);
  assert.equal((await bounded(dispatch('ensure', 102), 'cache after cancellation')).ok, true);
  assert.equal(counts.get(102), 2);

  // A cancelled creation leaves nothing cached: the late caller in the same
  // request creates again instead of awaiting a dead future.
  assert.equal((await bounded(dispatch('cancel-late', 103), 'late waiter')).ok, true);
  assert.equal(counts.get(103), 2);

  // Distinct contexts can finish out of order. A shorter-lived late completion
  // cannot overwrite the longer-lived token already shared by the isolate.
  const older = dispatch('ensure', 104);
  await until(() => waiting.has('104:1'), 'first overlapping mint');
  const newer = dispatch('ensure', 104);
  await until(() => waiting.has('104:2'), 'second overlapping mint');
  waiting.get('104:2')(7200);
  assert.equal((await bounded(newer, 'newer token')).expires_at, now + 7200);
  waiting.get('104:1')(3600);
  await bounded(older, 'older completion');
  assert.equal((await bounded(dispatch('ensure', 104), 'later expiry wins')).expires_at, now + 7200);
  assert.equal(counts.get(104), 2);
  console.log(`PASS production GithubApps in workerd: ${created} creation(s) for 20 concurrent misses, isolate cache, cross-request isolation, cancellation, late waiter, later-expiry replacement`);
} finally {
  for (const release of cleanup) release();
  for (const release of controls.values()) release();
  await mf.dispose();
}
