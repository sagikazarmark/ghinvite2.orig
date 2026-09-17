import assert from 'node:assert/strict';
import { fileURLToPath } from 'node:url';
import { Miniflare, Response } from 'miniflare';

let outgoing = 0;
const mf = new Miniflare({
  modules: true,
  scriptPath: fileURLToPath(new URL('./worker.mjs', import.meta.url)),
  modulesRules: [{ type: 'CompiledWasm', include: ['**/*.wasm'] }],
  // Match wrangler/web.toml, including its intentionally older compatibility date.
  compatibilityDate: '2024-09-23',
  compatibilityFlags: ['nodejs_compat'],
  kvNamespaces: ['SESSIONS'],
  d1Databases: ['DB'],
  bindings: {
    GHINVITE_BASE_URL: 'https://session.test',
    GHINVITE_SESSION_SECRET: '07'.repeat(32),
    GHINVITE_RESTATE_INGRESS: 'https://restate.invalid',
    GHINVITE_RESTATE_AUTH: 'local-unauthenticated',
    GHINVITE_GITHUB_INSTALL_URL: 'https://github.com/apps/dummy/installations/new',
    GHINVITE_GITHUB_CLIENT_ID: 'dummy-client-id',
    GHINVITE_GITHUB_CLIENT_SECRET: 'dummy-client-secret',
    GHINVITE_WEBHOOK_SECRET: 'dummy-webhook-secret',
  },
  outboundService(request) {
    outgoing++;
    console.error('Blocked unexpected outbound request:', request.url);
    return new Response('Outbound network disabled', { status: 502 });
  },
});

try {
  const health = await mf.dispatchFetch('https://session.test/health', { redirect: 'manual' });
  assert.equal(health.status, 200);
  assert.equal(await health.text(), 'ok');
  console.log('PASS /health: 200 ok');

  const login = await mf.dispatchFetch('https://session.test/login?return_to=%2Fconsole', { redirect: 'manual' });
  assert.equal(login.status, 303, await login.text());
  const location = new URL(login.headers.get('location'));
  assert.equal(location.origin, 'https://github.com');
  assert.equal(location.pathname, '/login/oauth/authorize');
  assert.equal(location.searchParams.get('client_id'), 'dummy-client-id');
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
  const raw = Buffer.from(await kv.get(key, 'arrayBuffer'));
  const magic = Buffer.from('ghinvite-session\x01');
  assert.deepEqual(raw.subarray(0, magic.length), magic);
  assert.ok(raw.length > magic.length + 8 + 24 + 16, 'header, deadline, nonce, tag and ciphertext');
  for (const plaintext of [state, 'oauth_csrf', 'ghinvite_lifetime', '/console']) {
    assert.equal(raw.includes(Buffer.from(plaintext)), false, `plaintext leak: ${plaintext}`);
  }
  console.log(`PASS /login: 303; one protected KV record (${raw.length} bytes), no plaintext state`);

  const mismatch = await mf.dispatchFetch('https://session.test/oauth/callback?code=dummy&state=wrong', {
    headers: { cookie }, redirect: 'manual',
  });
  assert.equal(mismatch.status, 400);
  // "Mismatch", rather than "no CSRF state", proves a later request decrypted
  // and loaded the pending OAuth state through the actual Worker KV adapter.
  assert.equal(await mismatch.text(), 'OAuth error: CSRF state mismatch');
  assert.equal(mismatch.headers.get('set-cookie'), null);
  assert.deepEqual(Buffer.from(await kv.get(key, 'arrayBuffer')), raw);
  console.log('PASS callback mismatch: 400; persisted state loaded, ciphertext unchanged');

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

  assert.equal(outgoing, 0);
  console.log('PASS Worker runtime smoke; outbound requests: 0');
} finally {
  await mf.dispose();
}
