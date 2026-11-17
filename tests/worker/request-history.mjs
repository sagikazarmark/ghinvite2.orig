import { seedPersonalInstallation } from './installation-fixture.mjs';
import { installedWebApp, signinApp } from './app-config.mjs';
import { signinAppSeed, startOctoemu } from './octoemu.mjs';
import assert from 'node:assert/strict';
import { readFileSync, readdirSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { Miniflare, Log, LogLevel } from 'miniflare';

// GitHub is octoemu; octocat (42) signs in.
const github = await startOctoemu({ users: [{ id: 42, login: 'octocat' }], apps: [signinAppSeed({ hosts: ['history.test'] })] });

const mf = new Miniflare({
  log: new Log(LogLevel.ERROR), modules: true,
  scriptPath: fileURLToPath(new URL('./worker.mjs', import.meta.url)),
  modulesRules: [{ type: 'CompiledWasm', include: ['**/*.wasm'] }],
  compatibilityDate: '2024-09-23', compatibilityFlags: ['nodejs_compat'],
  ratelimits: { RL_IP: { simple: { limit: 30, period: 60 } }, RL_USER: { simple: { limit: 10, period: 60 } }, AUTHZ_RL: { simple: { limit: 10, period: 60 } } },
  kvNamespaces: ['SESSIONS', 'AUTHZ_CACHE'], d1Databases: ['DB'],
  bindings: {
    P1_HOST: 'history.test', P1_KEYRING: 'kekring1;name=p1;current=1;hwm=1;k1=BwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwc.9fdab21f330c89b7',
    RESTATE_INGRESS_URL: 'https://restate.test', RESTATE_INGRESS_TOKEN: 'synthetic-worker-ingress-key',
    GHINVITE_GITHUB_INSTALL_URL: 'https://github.com/apps/dummy/installations/new',
    ...signinApp, ...installedWebApp, GHINVITE_WEBHOOK_SECRET: 'dummy', ...github.bindings,
  },
});

try {
  const db = await mf.getD1Database('DB');
  for (const file of readdirSync(new URL('../../migrations/', import.meta.url)).filter(f => f.endsWith('.sql')).sort()) {
    await db.exec(readFileSync(new URL(`../../migrations/${file}`, import.meta.url), 'utf8').replace(/^[ \t]*--.*$/gm, '').replaceAll('\n', ' '));
  }
  await seedPersonalInstallation(db);
  await seedPersonalInstallation(db, { installationId: 2, accountId: 999, login: 'foreign', repoId: 11 });
  await db.prepare("INSERT INTO users(user_id,login,avatar_url,last_seen_at) VALUES (42,'octocat',NULL,'2026-01-01T00:00:00Z')").run();
  const link = '01ARZ3NDEKTSV4RRFFQ69G5FAV';
  const foreign = '01ARZ3NDEKTSV4RRFFQ69G5FAW';
  for (const [id, account] of [[link, 42], [foreign, 999]]) {
    await db.prepare(`INSERT INTO invitation_links (id,installation_id,account_id,created_by,created_at,approval_required,description,projection_revision,projection_content,secret_generation,secret_sha256)
      VALUES (?,?,?,42,'2026-01-01T00:00:00Z',1,'History fixture',1,'{}',1,'2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a')`).bind(id, account === 42 ? 1 : 2, account).run();
    await db.prepare("INSERT INTO invitation_link_targets VALUES (?,'repo',10,'octocat/api','read')").bind(id).run();
  }
  const cookie = await github.signIn((url, init) => mf.dispatchFetch(url, init), 'https://history.test', 'octocat', { 'CF-Connecting-IP': '192.0.2.30' });
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
    await db.prepare(`INSERT INTO invitation_requests (id,invitation_link_id,requester_id,state,created_at,decided_at,decline_reason,projection_revision)
      VALUES (?,?,42,'declined',?,'2026-01-02T00:00:00Z','private decision',1)`)
      .bind(ids[n], n === 52 ? foreign : link, n === 51 ? '2026-01-01T00:00:00.000000001Z' : n % 2 ? '2026-01-01T00:00:00+00:00' : '2026-01-01T00:00:00Z').run();
  }
  const found = [];
  let path = base;
  for (const size of [25, 25, 2]) {
    const html = await get(path);
    const page = [...html.matchAll(/href="\/console\/accounts\/octocat\/requests\/([A-Z0-9]+)"/g)].map(m => m[1]);
    assert.equal(page.length, size);
    assert.equal((html.match(/@octocat · GitHub user ID 42/g) || []).length, size,
      'requester profiles are included by the D1 history query');
    found.push(...page);
    path = html.match(/href="([^"]+\?before=[^"]+)"/)?.[1];
  }
  assert.equal(path, undefined);
  assert.deepEqual(found, ids.slice(0, 52).reverse());
  const detail = `/console/accounts/octocat/requests/${ids[51]}`;
  let html = await get(detail);
  assert.match(html, /private decision/);
  assert.match(html, /octocat\/api/);
  assert.match(html, /Read/);
  assert.match(html, /Requested targets are fixed at admission\. Current availability does not change them\./);
  assert.match(html, /Projected history/);
  assert.doesNotMatch(await get(`/console/accounts/octocat/requests/${ids[52]}`, 404), /private decision/);
  await get(`/console/accounts/octocat/links/${foreign}/requests`, 404);
  await db.prepare("UPDATE users SET last_seen_at='broken'").run();
  assert.match(await get(base), /@octocat · GitHub user ID 42/,
    'history enrichment reads the display login without loading unrelated profile fields');
  assert.match(await get(detail), /GitHub user ID 42 · Profile unavailable/);
  await db.exec('DROP TABLE delivery_outcomes');
  assert.match(await get(detail), /Delivery information unavailable/);
  await db.prepare("UPDATE invitation_requests SET state='bad' WHERE id=?").bind(ids[51]).run();
  assert.match(await get(base, 503), /Request history unavailable/);
  console.log('PASS D1 request history: bounded pages, timestamp precision, terminal details, foreign concealment, missing profile and unavailable reads');
} finally {
  await mf.dispose();
  await github.stop();
}
