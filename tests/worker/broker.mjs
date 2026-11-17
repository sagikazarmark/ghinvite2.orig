import assert from 'node:assert/strict';
import { fileURLToPath } from 'node:url';
import { Miniflare, Response, Log, LogLevel } from 'miniflare';
import { GITHUB_TOKEN, startOctoemu } from './octoemu.mjs';
import { brokerApp } from './github-world.mjs';

// This file consumes build.mjs's actual compiled Worker: ghinvite-github's
// GithubApps over octotoken's AppTokenProvider, built per incoming request with
// its isolate-scope token cache (PD-4). It never substitutes a JavaScript token
// cache or transport for the production Rust modules. GitHub is octoemu: token
// creations are held, released and counted with fault rules.
const github = await startOctoemu({
  orgs: [101, 102, 103, 104].map(id => ({ id: 9000 + id, login: `tenant-${id}` })),
  apps: [brokerApp],
  installations: [101, 102, 103, 104].map(id => ({ id, app_id: 99001, account: `tenant-${id}` })),
});
const mint = id => `/app/installations/${id}/access_tokens`;
const hold = (id, action = 'hold', extra = {}) => github.fault({ method: 'POST', path: mint(id), label: `${action} ${id}`, ...extra, action: { kind: action } });
const rule = id => github.admin('GET', `/faults/${id}`);
// Token creations that reached the emulator (held ones included).
const arrivals = async id => (await rule(id)).matched;
const controls = new Map();
const cleanup = [];
async function until(predicate, label) {
  const end = Date.now() + 10_000;
  while (!(await predicate())) {
    if (Date.now() > end) throw new Error(`timed out: ${label}`);
    await new Promise(resolve => setTimeout(resolve, 20));
  }
}
let late;
const mf = new Miniflare({
  log: new Log(LogLevel.ERROR), modules: true,
  scriptPath: fileURLToPath(new URL('./worker.mjs', import.meta.url)),
  modulesRules: [{ type: 'CompiledWasm', include: ['**/*.wasm'] }],
  compatibilityDate: '2024-09-23', compatibilityFlags: ['nodejs_compat'],
  bindings: github.bindings,
  async outboundService(request) {
    if (github.owns(request)) return github.forward(request);
    // The fixture's cancellation control (not GitHub).
    const url = new URL(request.url);
    assert.equal(url.origin, 'https://broker.test');
    const control = url.pathname.match(/^\/control\/cancel\/(\d+)$/);
    assert.ok(control, `unexpected broker call ${url.pathname}`);
    assert.equal(request.headers.get('authorization'), null);
    const id = Number(control[1]);
    if (id === 103) { await github.waitHeld(late); return Response.json({ release: true }); }
    return new Promise(resolve => { controls.set(id, () => resolve(Response.json({ release: true }))); });
  },
});
async function dispatch(operation, id) {
  const response = await mf.dispatchFetch(`https://worker.test/__fixture/broker/${operation}?installation=${id}`);
  const text = await response.text();
  assert.equal(response.status, 200, text);
  assert.doesNotMatch(text, GITHUB_TOKEN, 'credential escaped fixture response');
  assert.ok(!text.includes('Bearer'), 'credential escaped fixture response');
  return JSON.parse(text);
}
async function bounded(promise, label) {
  let timer;
  try { return await Promise.race([promise, new Promise((_, reject) => { timer = setTimeout(() => reject(new Error(`request timed out: ${label}`)), 15000); })]); }
  finally { clearTimeout(timer); }
}
async function settled(id) {
  // No new token creation for this installation for 200 ms.
  for (let seen = -1, now; seen !== (now = await arrivals(id));) { seen = now; await new Promise(resolve => setTimeout(resolve, 200)); }
}
try {
  // Twenty concurrent misses in one incoming request. On Workers requests never
  // wait on each other's I/O, so there is no coalescing: each miss may create
  // its own token (ADR 0003 amendment). All must succeed.
  const held101 = await hold(101);
  const coalesced = dispatch('coalesce', 101);
  await github.waitHeld(held101);
  await settled(held101);
  const created = await arrivals(held101);
  assert.ok(created >= 1 && created <= 20, `concurrent misses created ${created} tokens`);
  await github.releaseFault(held101);
  assert.equal((await bounded(coalesced, 'concurrent misses')).ok, true);
  assert.equal((await bounded(dispatch('ensure', 101), 'isolate cache')).ok, true);
  assert.equal(await arrivals(held101), created, 'a later incoming request must reuse the isolate-cached token');

  // Both contexts create concurrently. Cancelling A must not cancel or donate
  // its request-owned fetch to B; B has reached its own upstream before A is released.
  const held102 = await hold(102);
  const cancelled = dispatch('cancel', 102);
  await github.waitHeld(held102);
  await until(() => controls.has(102), 'first incoming context');
  const survivor = dispatch('ensure', 102);
  await until(async () => await arrivals(held102) === 2, 'independent second incoming context');
  controls.get(102)();
  assert.deepEqual(await bounded(cancelled, 'cancellation'), { cancelled: true });
  await github.releaseFault(held102);
  assert.equal((await bounded(survivor, 'surviving context')).ok, true);
  assert.equal(await arrivals(held102), 2);
  assert.equal((await bounded(dispatch('ensure', 102), 'cache after cancellation')).ok, true);
  assert.equal(await arrivals(held102), 2);

  // A cancelled creation leaves nothing cached: the late caller in the same
  // request creates again instead of awaiting a dead future.
  late = await hold(103, 'hold', { times: 1 });
  cleanup.push(() => github.releaseFault(late));
  assert.equal((await bounded(dispatch('cancel-late', 103), 'late waiter')).ok, true);
  assert.equal(await arrivals(late), 2);

  // Distinct contexts can finish out of order. A shorter-lived late completion
  // cannot overwrite the longer-lived token already shared by the isolate:
  // the first creation is applied at once but answers late; the second is
  // created later (so expires later) and answers first.
  const slow = await hold(104, 'hold_response', { times: 1 });
  const fresh = await hold(104, 'hold', { times: 1 });
  const older = dispatch('ensure', 104);
  await github.waitHeld(slow);
  const newer = dispatch('ensure', 104);
  await github.waitHeld(fresh);
  await new Promise(resolve => setTimeout(resolve, 1500));
  await github.releaseFault(fresh);
  const later = (await bounded(newer, 'newer token')).expires_at;
  await github.releaseFault(slow);
  await bounded(older, 'older completion');
  // Two tokens, the first created (so expiring) earlier; the isolate keeps the later one.
  const expiries = (await github.state()).tokens.filter(token => token.installation_id === 104).map(token => Math.floor(Date.parse(token.expires_at) / 1000)).sort();
  assert.equal(expiries.length, 2);
  assert.ok(expiries[0] < expiries[1], `distinct expiries ${expiries}`);
  assert.equal(later, expiries[1]);
  assert.equal((await bounded(dispatch('ensure', 104), 'later expiry wins')).expires_at, later);
  assert.equal(await arrivals(slow), 2);
  // Every creation asked for the metadata profile only.
  for (const call of await github.calls('POST /app/installations/{installation_id}/access_tokens')) assert.deepEqual(call.body_fields?.permissions, { metadata: 'read' });
  console.log(`PASS production GithubApps in workerd: ${created} creation(s) for 20 concurrent misses, isolate cache, cross-request isolation, cancellation, late waiter, later-expiry replacement`);
} finally {
  for (const release of cleanup) await release();
  for (const release of controls.values()) release();
  await mf.dispose();
  await github.stop();
}
