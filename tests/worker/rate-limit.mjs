import assert from 'node:assert/strict';
import { fileURLToPath } from 'node:url';
import { Miniflare, Log, LogLevel } from 'miniflare';

const options = {
  log: new Log(LogLevel.ERROR), modules: true,
  scriptPath: fileURLToPath(new URL('./worker.mjs', import.meta.url)),
  modulesRules: [{ type: 'CompiledWasm', include: ['**/*.wasm'] }],
  compatibilityDate: '2024-09-23', compatibilityFlags: ['nodejs_compat'],
};
const request = (mf, bucket, key) => mf.dispatchFetch(`http://worker.test/__fixture/rate-limit/${bucket}${bucket !== 'ip' ? `?${key}` : ''}`, {
  headers: bucket === 'ip' ? { 'CF-Connecting-IP': key } : {},
});
const missing = new Miniflare(options);
try { assert.equal((await request(missing, 'ip', '192.0.2.1')).status, 503); }
finally { await missing.dispose(); }
const mf = new Miniflare({ ...options, ratelimits: {
  RL_IP: { simple: { limit: 30, period: 60 } },
  RL_USER: { simple: { limit: 10, period: 60 } },
  AUTHZ_RL: { simple: { limit: 10, period: 60 } },
} });
try {
  for (let i=0;i<30;i++) assert.equal((await request(mf, 'ip', '192.0.2.1')).status, 204);
  assert.equal((await request(mf, 'ip', '192.0.2.1')).status, 429);
  assert.equal((await request(mf, 'ip', '192.0.2.2')).status, 204);
  const statuses = await Promise.all(Array.from({length: 20}, async () => (await request(mf,'user',42)).status));
  assert.equal(statuses.filter(status=>status===204).length, 10);
  assert.equal(statuses.filter(status=>status===429).length, 10);
  assert.equal((await request(mf,'user',43)).status, 204);
  const spoofed = await mf.dispatchFetch('http://worker.test/__fixture/rate-limit/ip', {headers:{'X-Forwarded-For':'198.51.100.3','CF-Connecting-IP':'invalid'}});
  assert.equal(spoofed.status,503);
  for(let i=0;i<10;i++) assert.equal((await request(mf,'authority','repo:1')).status,204);
  assert.equal((await request(mf,'authority','repo:1')).status,429);
  assert.equal((await request(mf,'authority','org:1')).status,204);
  assert.equal((await request(mf,'authority','repo:2')).status,204);
  console.log('PASS actual Worker RL_IP/RL_USER bindings: required wiring, 30/60 IP, 10/60 user, concurrent callers and isolated identities');
} finally { await mf.dispose(); }
