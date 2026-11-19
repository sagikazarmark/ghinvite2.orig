import { installedWebApp, signinApp } from './app-config.mjs';
import { capabilityFor } from './capability.mjs';
import assert from 'node:assert/strict';
import { readFileSync, readdirSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { Miniflare, Response, Log, LogLevel } from 'miniflare';
import { stalledResponse } from './deadline-recovery.mjs';

const ingressApiKey = 'synthetic-worker-ingress-key';

// GitHub is octoemu (`github`, a GithubWorld): the requester signs in on its authorize page.
export async function browserAdmission(github, ingress, code, requester, operation, recovery) {
  let authenticatedIngressCalls = 0;
  let stall;
  let committed;
  const cleanup = [];
  const attempted = [];
  const mf = new Miniflare({
    log: new Log(LogLevel.ERROR), modules: true,
    scriptPath: fileURLToPath(new URL('./worker.mjs', import.meta.url)),
    modulesRules: [{ type: 'CompiledWasm', include: ['**/*.wasm'] }],
    compatibilityDate: '2024-09-23', compatibilityFlags: ['nodejs_compat'],
    kvNamespaces: ['SESSIONS', 'AUTHZ_CACHE'], d1Databases: ['DB'],
    ratelimits: { RL_IP: { namespace_id: '1001', simple: { limit: 30, period: 60 } }, RL_USER: { namespace_id: '1002', simple: { limit: 10, period: 60 } }, AUTHZ_RL: { namespace_id: '1003', simple: { limit: 10, period: 60 } } },
    bindings: {
      P1_HOST: 'browser.test',
      P1_KEYRING: 'kekring1;name=p1;current=1;hwm=1;k1=BwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwc.9fdab21f330c89b7', RESTATE_INGRESS_URL: 'https://ingress.fixture.test',
      RESTATE_INGRESS_TOKEN: ingressApiKey,
      GHINVITE_GITHUB_INSTALL_URL: 'https://github.com/apps/dummy/installations/new',
      ...signinApp, ...installedWebApp, GHINVITE_WEBHOOK_SECRET: 'dummy', ...github.emulator.bindings,
    },
    async outboundService(request) {
      const url = new URL(request.url);
      if (url.origin === 'https://ingress.fixture.test') {
        assert.equal(request.headers.get('authorization'), `Bearer ${ingressApiKey}`);
        authenticatedIngressCalls++;
        const input = request.method === 'GET' ? undefined : Buffer.from(await request.arrayBuffer());
        const mutation = url.pathname.endsWith('/admit');
        if (mutation) attempted.push(JSON.parse(input));
        const response = await fetch(`${ingress}${url.pathname}${url.search}`, { method: request.method, headers: [...request.headers].filter(([name]) => name !== 'content-length'),
          body: input });
        if (stall && mutation) {
          assert.equal(response.status, 200);
          committed = await response.json();
          return stalledResponse(stall, cleanup);
        }
        return new Response(await response.arrayBuffer(), { status: response.status, headers: response.headers });
      }
      if (github.emulator.owns(request)) return github.emulator.forward(request);
      throw new Error(`unexpected browser outbound ${url.origin}${url.pathname}`);
    },
  });
  try {
    const db = await mf.getD1Database('DB');
    for (const file of readdirSync(new URL('../../migrations/', import.meta.url)).filter(file => file.endsWith('.sql')).sort()) {
      const schema = readFileSync(new URL(`../../migrations/${file}`, import.meta.url), 'utf8');
      await db.exec(schema.replace(/^[ \t]*--.*$/gm, '').replaceAll('\n', ' '));
    }
    const cookie = await github.emulator.signIn((url, init) => mf.dispatchFetch(url, init), 'https://browser.test', `user-${requester}`, { 'CF-Connecting-IP': '192.0.2.20' });
    const select = async linkId => {
      const capability = capabilityFor(linkId);
      await db.prepare('INSERT INTO link_secrets (secret_sha256,link_id,secret_generation,created_at,retired_at) VALUES (?,?,?,?,NULL)').bind(capability.sha256, linkId, capability.generation, new Date().toISOString()).run();
      const bootstrap = await mf.dispatchFetch('https://browser.test/i', { headers: { cookie } });
      const csrf = (await bootstrap.text()).match(/name="csrf_token"[^>]*value="([^"]+)"/)[1];
      const selected = await mf.dispatchFetch('https://browser.test/i', { method: 'POST', headers: { cookie, 'CF-Connecting-IP': '192.0.2.20', 'content-type': 'application/x-www-form-urlencoded' }, body: new URLSearchParams({secret:capability.secret,csrf_token:csrf}).toString(), redirect:'manual' });
      assert.equal(selected.status,303,await selected.clone().text());
      assert.equal(selected.headers.get('location'),'/join');
    };
    await select(code);
    const page = await mf.dispatchFetch(`https://browser.test/join?operation_id=${operation}`, { headers: { cookie }, redirect: 'manual' });
    assert.equal(page.status, 200, await page.clone().text());
    const html = await page.text();
    assert.match(html, /pending|awaiting|Waiting/i);
    assert.equal(html.includes('Private context'), false);
    assert.equal(html.includes(ingressApiKey), false);
    assert.equal(JSON.stringify([...page.headers]).includes(ingressApiKey), false);
    assert.ok(authenticatedIngressCalls > 0, 'browser status must call authenticated ingress');
    assert.equal(page.headers.get('cache-control'), 'no-store');
    console.log('PASS browser Worker Bearer ingress binding and authoritative status without projected link/request rows');
    if (recovery) for (const phase of ['headers', 'body']) {
      const input = recovery.creation();
      const created = await recovery.http(`${ingress}/InvitationLink/${input.link_id}/create`, input);
      assert.deepEqual(created, { Ok: { link_id: input.link_id, secret_generation: 1 } });
      await select(created.Ok.link_id);
      const path = `https://browser.test/join`;
      const form = await mf.dispatchFetch(path, { headers: { cookie } });
      assert.equal(form.status, 200);
      const html = await form.text();
      const field = name => {
        const value = html.match(new RegExp(`name="${name}"[^>]*value="([^"]+)"`))?.[1];
        assert.ok(value, `missing ${name}`);
        return value;
      };
      const operation_id = field('operation_id');
      const body = new URLSearchParams({ csrf_token: field('csrf_token'), operation_id, justification: 'Original input' }).toString();
      const submit = () => mf.dispatchFetch(path, { method: 'POST', headers: { cookie, 'content-type': 'application/x-www-form-urlencoded' }, body, redirect: 'manual' });
      stall = phase;
      const start = Date.now();
      const response = await submit();
      assert.equal(response.status, 502);
      const unknown = await response.text();
      assert.match(unknown, /Outcome unknown/);
      assert.ok(unknown.includes(operation_id));
      assert.ok(unknown.includes('Original input'));
      assert.ok(Date.now() - start >= 14_000 && Date.now() - start < 20_000);
      assert.equal(committed.result.kind, 'accepted');
      assert.equal(attempted.at(-1).operation_id, operation_id);
      stall = null;
      const retried = await submit();
      assert.equal(retried.status, 303, await retried.text());
      assert.ok(retried.headers.get('location').includes(operation_id));
      const status = await mf.dispatchFetch(`https://browser.test${retried.headers.get('location')}`, { headers: { cookie } });
      assert.equal(status.status, 200, await status.text());
      const result = await recovery.http(`${ingress}/InvitationLink/${input.link_id}/admit`, attempted.at(-1));
      assert.deepEqual(result, committed);
      const link = await recovery.http(`${ingress}/InvitationLink/${input.link_id}/link_status`, { link_id: input.link_id, admin: input.admin });
      assert.equal(link.uses, 1);
      console.log(`PASS browser Worker ${phase} mutation timeout after commit: original form identity/input and one-use recovery`);
    }
  } finally { for (const release of cleanup) release(); await mf.dispose(); }
}
