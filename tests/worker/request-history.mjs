import assert from 'node:assert/strict';
import { readFileSync, readdirSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { Miniflare, Response, Log, LogLevel } from 'miniflare';

const mf = new Miniflare({
  log: new Log(LogLevel.ERROR), modules: true,
  scriptPath: fileURLToPath(new URL('./worker.mjs', import.meta.url)),
  modulesRules: [{ type: 'CompiledWasm', include: ['**/*.wasm'] }],
  compatibilityDate: '2024-09-23', compatibilityFlags: ['nodejs_compat'],
  kvNamespaces: ['SESSIONS'], d1Databases: ['DB'],
  bindings: {
    GHINVITE_BASE_URL: 'https://history.test', GHINVITE_SESSION_SECRET: '07'.repeat(32),
    GHINVITE_RESTATE_INGRESS: 'https://restate.test', GHINVITE_RESTATE_AUTH: 'local-unauthenticated',
    GHINVITE_GITHUB_INSTALL_URL: 'https://github.com/apps/dummy/installations/new',
    GHINVITE_GITHUB_CLIENT_ID: 'dummy', GHINVITE_GITHUB_CLIENT_SECRET: 'dummy', GHINVITE_WEBHOOK_SECRET: 'dummy',
  },
  async outboundService(request) {
    if (request.url === 'https://github.com/login/oauth/access_token') return Response.json({ access_token: 'fixture', token_type: 'bearer', scope: '' });
    if (request.url === 'https://api.github.com/user') return Response.json({ id: 42, login: 'octocat', avatar_url: null });
    throw new Error(`unexpected outbound ${request.url}`);
  },
});

try {
  const db = await mf.getD1Database('DB');
  for (const file of readdirSync(new URL('../../migrations/', import.meta.url)).filter(f => f.endsWith('.sql')).sort()) {
    await db.exec(readFileSync(new URL(`../../migrations/${file}`, import.meta.url), 'utf8').replace(/^--.*$/gm, '').replaceAll('\n', ' '));
  }
  await db.prepare("INSERT INTO installations VALUES (1,42,'octocat','User','2026-01-01T00:00:00Z',NULL,'[]')").run();
  await db.prepare("INSERT INTO users VALUES (42,'octocat',NULL,'2026-01-01T00:00:00Z')").run();
  const link = '01ARZ3NDEKTSV4RRFFQ69G5FAV';
  const foreign = '01ARZ3NDEKTSV4RRFFQ69G5FAW';
  for (const [id, account, code] of [[link, 42, 'HistoryCode00001'], [foreign, 999, 'ForeignCode00001']]) {
    await db.prepare(`INSERT INTO invitation_links (id,slug,installation_id,account_id,created_by,created_at,permission,approval_required,description)
      VALUES (?,?,1,?,42,'2026-01-01T00:00:00Z','pull',1,'History fixture')`).bind(id, code, account).run();
    await db.prepare("INSERT INTO invitation_link_repos VALUES (?,10,'octocat/api')").bind(id).run();
  }
  const login = await mf.dispatchFetch('https://history.test/login', { redirect: 'manual' });
  const state = new URL(login.headers.get('location')).searchParams.get('state');
  let cookie = login.headers.get('set-cookie').split(';')[0];
  const callback = await mf.dispatchFetch(`https://history.test/oauth/callback?code=dummy&state=${state}`, { headers: { cookie }, redirect: 'manual' });
  assert.equal(callback.status, 303);
  cookie = callback.headers.get('set-cookie').split(';')[0];
  const get = async (path, status = 200) => {
    const response = await mf.dispatchFetch(`https://history.test${path}`, { headers: { cookie } });
    assert.equal(response.status, status, await response.clone().text());
    assert.equal(response.headers.get('cache-control'), 'private, no-store');
    return response.text();
  };
  const base = `/console/accounts/octocat/links/${link}/requests`;
  assert.match(await get(base), /No projected requests in this range/);
  const ids = [];
  const alphabet = '0123456789ABCDEFGHJKMNPQRSTVWXYZ';
  for (let n = 0; n < 53; n++) {
    ids[n] = '0'.repeat(24) + alphabet[Math.floor(n / 32)] + alphabet[n % 32];
    await db.prepare(`INSERT INTO invitation_requests (id,invitation_link_id,requester_id,state,created_at,decided_at,decline_reason)
      VALUES (?,?,42,'declined',?,'2026-01-02T00:00:00Z','private decision')`)
      .bind(ids[n], n === 52 ? foreign : link, n === 51 ? '2026-01-01T00:00:00.000000001Z' : n % 2 ? '2026-01-01T00:00:00+00:00' : '2026-01-01T00:00:00Z').run();
  }
  const found = [];
  let path = base;
  for (const size of [25, 25, 2]) {
    const html = await get(path);
    const page = [...html.matchAll(/href="\/console\/accounts\/octocat\/requests\/([A-Z0-9]+)"/g)].map(m => m[1]);
    assert.equal(page.length, size);
    found.push(...page);
    path = html.match(/href="([^"]+\?before=[^"]+)"/)?.[1];
  }
  assert.equal(path, undefined);
  assert.deepEqual(found, ids.slice(0, 52).reverse());
  const detail = `/console/accounts/octocat/requests/${ids[51]}`;
  let html = await get(detail);
  assert.match(html, /private decision/);
  assert.match(html, /Immutable requested permission: pull/);
  assert.match(html, /Projected history/);
  assert.doesNotMatch(await get(`/console/accounts/octocat/requests/${ids[52]}`, 404), /private decision/);
  await get(`/console/accounts/octocat/links/${foreign}/requests`, 404);
  await db.prepare("UPDATE users SET last_seen_at='broken'").run();
  assert.match(await get(detail), /GitHub user ID 42 · Profile unavailable/);
  await db.exec('DROP TABLE delivery_outcomes');
  assert.match(await get(detail), /Delivery information unavailable/);
  await db.prepare("UPDATE invitation_requests SET state='bad' WHERE id=?").bind(ids[51]).run();
  assert.match(await get(base, 503), /Request history unavailable/);
  console.log('PASS D1 request history: bounded pages, timestamp precision, terminal details, foreign concealment, missing profile and unavailable reads');
} finally {
  await mf.dispose();
}
