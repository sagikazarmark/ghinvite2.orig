import assert from 'node:assert/strict';
import { fileURLToPath } from 'node:url';
import { Miniflare, Response, Log, LogLevel } from 'miniflare';

const ingressApiKey = 'synthetic-worker-ingress-key';

export async function browserAdmission(ingress, code, requester, operation) {
  let authenticatedIngressCalls = 0;
  const mf = new Miniflare({
    log: new Log(LogLevel.ERROR), modules: true,
    scriptPath: fileURLToPath(new URL('./worker.mjs', import.meta.url)),
    modulesRules: [{ type: 'CompiledWasm', include: ['**/*.wasm'] }],
    compatibilityDate: '2024-09-23', compatibilityFlags: ['nodejs_compat'],
    kvNamespaces: ['SESSIONS'], d1Databases: ['DB'],
    bindings: {
      GHINVITE_ADMISSION_MODE: 'authoritative', GHINVITE_BASE_URL: 'https://browser.test',
      GHINVITE_SESSION_SECRET: '07'.repeat(32), GHINVITE_RESTATE_INGRESS: ingress,
      GHINVITE_RESTATE_AUTH: 'bearer', GHINVITE_RESTATE_API_KEY: ingressApiKey,
      GHINVITE_GITHUB_INSTALL_URL: 'https://github.com/apps/dummy/installations/new',
      GHINVITE_GITHUB_CLIENT_ID: 'dummy', GHINVITE_GITHUB_CLIENT_SECRET: 'dummy', GHINVITE_WEBHOOK_SECRET: 'dummy',
    },
    async outboundService(request) {
      const url = new URL(request.url);
      if (url.origin === ingress) {
        assert.equal(request.headers.get('authorization'), `Bearer ${ingressApiKey}`);
        authenticatedIngressCalls++;
        const response = await fetch(request.url, { method: request.method, headers: request.headers,
          body: request.method === 'GET' ? undefined : Buffer.from(await request.arrayBuffer()), signal: AbortSignal.timeout(25_000) });
        return new Response(await response.arrayBuffer(), { status: response.status, headers: response.headers });
      }
      // Controlled OAuth boundary only; Rust session creation/rotation is real.
      if (request.url === 'https://github.com/login/oauth/access_token') return Response.json({ access_token: 'fixture', token_type: 'bearer', scope: '' });
      if (request.url === 'https://api.github.com/user') return Response.json({ id: requester, login: `user-${requester}`, avatar_url: null });
      throw new Error(`unexpected browser outbound ${url.origin}${url.pathname}`);
    },
  });
  try {
    const db = await mf.getD1Database('DB');
    await db.exec('CREATE TABLE users (user_id INTEGER PRIMARY KEY, login TEXT NOT NULL, avatar_url TEXT, last_seen_at TEXT NOT NULL)');
    const login = await mf.dispatchFetch('https://browser.test/login', { redirect: 'manual' });
    const state = new URL(login.headers.get('location')).searchParams.get('state');
    let cookie = login.headers.get('set-cookie').split(';')[0];
    const callback = await mf.dispatchFetch(`https://browser.test/oauth/callback?code=dummy&state=${state}`, { headers: { cookie }, redirect: 'manual' });
    assert.equal(callback.status, 303, await callback.clone().text());
    cookie = callback.headers.get('set-cookie').split(';')[0];
    const page = await mf.dispatchFetch(`https://browser.test/i/${code}?operation_id=${operation}`, { headers: { cookie }, redirect: 'manual' });
    assert.equal(page.status, 200, await page.clone().text());
    const html = await page.text();
    assert.match(html, /pending|awaiting|Waiting/i);
    assert.equal(html.includes('Private context'), false);
    assert.equal(html.includes(ingressApiKey), false);
    assert.equal(JSON.stringify([...page.headers]).includes(ingressApiKey), false);
    assert.ok(authenticatedIngressCalls > 0, 'browser status must call authenticated ingress');
    assert.equal(page.headers.get('cache-control'), 'private, no-store');
    console.log('PASS browser Worker Bearer ingress binding and authoritative status with empty D1');
  } finally { await mf.dispose(); }
}
