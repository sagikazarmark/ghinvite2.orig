import { installedWebApp, signinApp } from './app-config.mjs';
import assert from 'node:assert/strict';
import { readFileSync, readdirSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { Miniflare, Response } from 'miniflare';

let outgoing = 0;
const options = {
  modules: true,
  scriptPath: fileURLToPath(new URL('./worker.mjs', import.meta.url)),
  modulesRules: [{ type: 'CompiledWasm', include: ['**/*.wasm'] }],
  // Match wrangler/web.toml, including its intentionally older compatibility date.
  compatibilityDate: '2024-09-23',
  compatibilityFlags: ['nodejs_compat'],
  ratelimits: { RL_IP: { simple: { limit: 30, period: 60 } }, RL_USER: { simple: { limit: 10, period: 60 } }, AUTHZ_RL: { simple: { limit: 10, period: 60 } } },
  kvNamespaces: ['SESSIONS', 'AUTHZ_CACHE'],
  d1Databases: ['DB'],
  bindings: {
    P1_HOST: 'session.test',
    P1_KEYRING: 'kekring1;name=p1;current=1;hwm=1;k1=BwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwc.9fdab21f330c89b7',
    RESTATE_INGRESS_URL: 'https://restate.invalid',
    RESTATE_INGRESS_TOKEN: 'synthetic-worker-ingress-key',
    GHINVITE_GITHUB_INSTALL_URL: 'https://github.com/apps/dummy/installations/new',
    ...signinApp,
    ...installedWebApp, GHINVITE_WEBHOOK_SECRET: 'dummy-webhook-secret',
  },
  outboundService(request) {
    outgoing++;
    console.error('Blocked unexpected outbound request:', request.url);
    return new Response('Outbound network disabled', { status: 502 });
  },
};
const mf = new Miniflare(options);

try {
  const db = await mf.getD1Database('DB');
  for (const file of readdirSync(new URL('../../migrations/', import.meta.url)).filter(f => f.endsWith('.sql')).sort()) {
    await db.exec(readFileSync(new URL(`../../migrations/${file}`, import.meta.url), 'utf8').replace(/^[ \t]*--.*$/gm, '').replaceAll('\n', ' '));
  }
  const health = await mf.dispatchFetch('https://session.test/health', { redirect: 'manual' });
  assert.equal(health.status, 200);
  assert.equal(await health.text(), 'ok');
  console.log('PASS /health: 200 ok');
  const immutable = await mf.dispatchFetch('https://session.test/health', {
    headers: { 'x-test-keyring-descriptor': 'malformed-secret-must-not-be-read-again' },
  });
  assert.equal(immutable.status, 200, 'loaded keyring is immutable for this isolate');


  const login = await mf.dispatchFetch('https://session.test/login?return_to=%2Fconsole', { redirect: 'manual' });
  assert.equal(login.status, 303, await login.text());
  const location = new URL(login.headers.get('location'));
  assert.equal(location.origin, 'https://github.com');
  assert.equal(location.pathname, '/login/oauth/authorize');
  assert.equal(location.searchParams.get('client_id'), signinApp.GH_SIGNIN_CLIENT_ID);
  const state = location.searchParams.get('state');
  assert.ok(state?.length >= 32);
  const setCookie = login.headers.get('set-cookie');
  assert.ok(setCookie);
  assert.match(setCookie, /HttpOnly/);
  assert.match(setCookie, /Secure/);
  const cookie = setCookie.split(';')[0];
  assert.match(cookie, /^id=.+/);

  const kv = await mf.getKVNamespace('SESSIONS');
  const key = `session:${cookie.slice(3)}`;
  const keys = (await kv.list()).keys;
  assert.equal(keys.length, 1);
  assert.equal(keys[0].name, key);
  let raw = Buffer.from(await kv.get(key, 'arrayBuffer'));
  assert.equal(raw.subarray(0, 3).toString(), 'AKE', 'anykms envelope magic');
  assert.equal(raw[3], 1, 'anykms envelope v1');
  assert.ok(raw.length > 100, 'wrapped DEK, authenticated header and encrypted record');
  // Key-wrapping slot: kek_id is the keyring name, then a 56-byte kekring v1 blob (krw1 magic).
  const kekId = raw.subarray(50, 50 + raw[49]).toString();
  const slot = 50 + raw[49];
  assert.equal(kekId, 'p1', 'envelope kek_id is the keyring name');
  assert.equal(raw.readUInt16BE(slot), 56, 'kekring wrapped key is 56 bytes');
  assert.equal(raw.subarray(slot + 2, slot + 6).toString(), 'krw1', 'kekring blob magic');
  assert.equal(raw.readUInt32BE(slot + 6), 1, 'wrapped under generation 1');
  assert.ok(keys[0].expiration <= Math.floor(Date.now() / 1000) + 30 * 24 * 60 * 60, 'session TTL bounded by thirty days');
  for (const plaintext of [state, 'ghinvite_lifetime', '/console']) {
    assert.equal(raw.includes(Buffer.from(plaintext)), false, `plaintext leak: ${plaintext}`);
  }
  // The pending sign-in (state, PKCE verifier) is one encrypted D1 row.
  const pending = await db.prepare('SELECT ciphertext FROM pending_signins').all();
  assert.equal(pending.results.length, 1);
  assert.equal(JSON.stringify(pending.results).includes(state), false, 'plaintext state in D1');
  console.log(`PASS /login: 303; one protected KV record (${raw.length} bytes), one encrypted pending sign-in, no plaintext state`);

  const mismatch = await mf.dispatchFetch('https://session.test/oauth/callback?code=dummy&state=wrong', {
    headers: { cookie, 'CF-Connecting-IP':'192.0.2.1' }, redirect: 'manual',
  });
  assert.equal(mismatch.status, 400);
  // The failure page says ghinvite's own words, never the upstream or internal
  // diagnostic. "Did not match the sign-in started", rather than the
  // no-sign-in-pending copy, proves a later request loaded the session through
  // the actual Worker KV adapter and decrypted its pending sign-in from D1.
  const mismatchBody = await mismatch.text();
  assert.match(mismatchBody, /did not match the sign-in started in this browser/);
  assert.doesNotMatch(mismatchBody, /no sign-in in progress/);
  assert.match(mismatchBody, /href="\/login"/);
  assert.doesNotMatch(mismatchBody, /CSRF/);
  assert.equal(mismatch.headers.get('set-cookie'), null);
  // The callback consumed the pending sign-in; the session stays one record
  // under the same ID (re-sealed before the callback's reads).
  assert.equal((await db.prepare('SELECT COUNT(*) AS n FROM pending_signins').first()).n, 0);
  assert.deepEqual((await kv.list()).keys.map(k => k.name), [key]);
  raw = Buffer.from(await kv.get(key, 'arrayBuffer'));
  assert.equal(raw.includes(Buffer.from(state)), false);
  console.log('PASS callback mismatch: 400; pending sign-in loaded and consumed, session ID unchanged');

  const logout = await mf.dispatchFetch('https://session.test/logout', {
    method: 'POST',
    headers: { cookie, 'x-test-fail-kv-read': '1', 'content-type': 'application/x-www-form-urlencoded' },
    body: 'csrf_token=dummy', redirect: 'manual',
  });
  assert.equal(logout.status, 503);
  assert.match(logout.headers.get('set-cookie'), /Max-Age=0/);
  assert.equal(logout.headers.get('location'), null);
  const logoutBody = await logout.text();
  assert.match(logoutBody, /server sign-out could not be confirmed/);
  assert.equal(logoutBody.includes('injected'), false);
  assert.equal(logout.headers.get('x-test-kv-mutations'), '0');
  assert.deepEqual(Buffer.from(await kv.get(key, 'arrayBuffer')), raw);
  assert.equal((await kv.list({ prefix: 'revoked:' })).keys.length, 0);
  console.log('PASS logout KV-read failure: 503; cookie cleared, no storage mutation');

  const continuation = await mf.dispatchFetch('https://session.test/__fixture/continuation-envelope');
  assert.equal(continuation.status, 200, await continuation.clone().text());
  assert.deepEqual(await continuation.json(), { ok: true });
  console.log('PASS actual D1 envelope continuation roundtrip, binding recovery, substitution rejection');

  assert.equal(outgoing, 0);
  console.log('PASS Worker runtime smoke; outbound requests: 0');
} finally {
  await mf.dispose();
}

for (const descriptor of [undefined, 'malformed-secret-must-not-leak']) {
  const bindings = { ...options.bindings };
  if (descriptor === undefined) delete bindings.P1_KEYRING;
  else bindings.P1_KEYRING = descriptor;
  const invalid = new Miniflare({ ...options, bindings });
  try {
    const response = await invalid.dispatchFetch('https://session.test/health');
    assert.equal(response.status, 500, 'missing/malformed keyring fails startup closed');
    assert.doesNotMatch(await response.text(), /malformed-secret-must-not-leak/);
    assert.equal(response.headers.get('set-cookie'), null);
  } finally { await invalid.dispose(); }
}
console.log('PASS missing/malformed P1_KEYRING startup fails closed without disclosure');
