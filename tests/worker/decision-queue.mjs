import { seedPersonalInstallation } from './installation-fixture.mjs';
import { installedWebApp, signinApp, signinRevocation } from './app-config.mjs';
import assert from 'node:assert/strict';
import { readFileSync, readdirSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { Miniflare, Response, Log, LogLevel } from 'miniflare';

// Exercise the production D1 adapter and authenticated HTTP routes. Only the
// external OAuth and lifecycle service responses are controlled at the boundary.
let receipt;
let loseAcknowledgement = false;
const decisions = [];
const mf = new Miniflare({
  log: new Log(LogLevel.ERROR), modules: true,
  scriptPath: fileURLToPath(new URL('./decision-queue-worker.mjs', import.meta.url)),
  modulesRules: [{ type: 'CompiledWasm', include: ['**/*.wasm'] }],
  compatibilityDate: '2024-09-23', compatibilityFlags: ['nodejs_compat'],
  ratelimits: { RL_IP: { simple: { limit: 30, period: 60 } }, RL_USER: { simple: { limit: 10, period: 60 } }, AUTHZ_RL: { simple: { limit: 10, period: 60 } } },
  kvNamespaces: ['SESSIONS', 'AUTHZ_CACHE'], d1Databases: ['DB'],
  bindings: {
    P1_HOST: 'queue.test',
    P1_KEYRING: 'akr1;name=p1;current=1;k1=BwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwc.74c0aab4b913ee29', RESTATE_INGRESS_URL: 'https://restate.test',
    RESTATE_INGRESS_TOKEN: 'synthetic-worker-ingress-key',
    GHINVITE_GITHUB_INSTALL_URL: 'https://github.com/apps/dummy/installations/new',
    ...signinApp, ...installedWebApp, GHINVITE_WEBHOOK_SECRET: 'dummy',
  },
  async outboundService(request) {
    if (request.url === 'https://github.com/login/oauth/access_token') return Response.json({ access_token: 'fixture', token_type: 'bearer', scope: '' });
    const revoked = signinRevocation(request); if (revoked) return revoked;
    if (request.url === 'https://api.github.com/user') return Response.json({ id: 42, login: 'octocat', avatar_url: null });
    if (/^https:\/\/restate.test\/InvitationRequest\/[A-Z0-9]+\/decide$/.test(request.url)) {
      decisions.push(await request.json());
      assert.ok(receipt, 'unexpected decision');
      if (loseAcknowledgement) { loseAcknowledgement = false; return new Response('acknowledgement lost', { status: 503 }); }
      return Response.json(receipt);
    }
    if (request.url.endsWith('/decision_status')) return Response.json(receipt);
    if (/^https:\/\/restate.test\/InvitationLink\/[A-Z0-9]+\/link_status$/.test(request.url)) {
      const {link_id,admin} = await request.json();
      assert.deepEqual(admin,{account_id:42,user_id:42});
      return Response.json({link_id,creation:{link_id,account_id:42,installation_id:1,admin,
        description:'Deadline fixture',approval_required:true,secret_sha256:'2a'.repeat(32),
        targets:{organization:null,teams:[],repos:[{repo_id:10,repo_full_name:'octocat/api',grant:'read'}]}},
        secret:{secret_generation:1,sha256:'2a'.repeat(32)},created_at:'2026-01-01T00:00:00Z',uses:1,revision:1,
        revoked_at:null,revoked_by:null});
    }
    throw new Error(`unexpected outbound ${request.url}`);
  },
});

try {
  const db = await mf.getD1Database('DB');
  for (const file of readdirSync(new URL('../../migrations/', import.meta.url)).filter(file => file.endsWith('.sql')).sort()) {
    await db.exec(readFileSync(new URL(`../../migrations/${file}`, import.meta.url), 'utf8').replace(/^[ \t]*--.*$/gm, '').replaceAll('\n', ' '));
  }
  await seedPersonalInstallation(db);
  for (const user of [42, 99]) await db.prepare("INSERT INTO users(user_id,login,avatar_url,last_seen_at) VALUES (?, ?, NULL, '2026-01-01T00:00:00Z')").bind(user, `user-${user}`).run();
  const link = '01ARZ3NDEKTSV4RRFFQ69G5FAV';
  let request = '01ARZ3NDEKTSV4RRFFQ69G5FAW';
  await db.prepare(`INSERT INTO invitation_links (id,installation_id,account_id,created_by,created_at,approval_required,description,projection_revision,projection_content,secret_generation,secret_sha256)
    VALUES (?,1,42,42,'2026-01-01T00:00:00Z',1,'Deadline fixture',1,'{}',1,'2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a')`).bind(link).run();
  await db.prepare("INSERT INTO invitation_link_targets VALUES (?,'repo',10,'octocat/api','read')").bind(link).run();
  await db.prepare(`INSERT INTO invitation_requests (id,invitation_link_id,requester_id,state,created_at,decision_deadline,projection_revision)
    VALUES (?,?,99,'pending','2026-01-01T01:00:00Z','2026-01-03T12:34:56Z',1)`).bind(request, link).run();

  const login = await mf.dispatchFetch('https://queue.test/login', { redirect: 'manual' });
  const state = new URL(login.headers.get('location')).searchParams.get('state');
  let cookie = login.headers.get('set-cookie').split(';')[0];
  const callback = await mf.dispatchFetch(`https://queue.test/oauth/callback?code=dummy&state=${state}`, { headers: { cookie, 'CF-Connecting-IP':'192.0.2.30' }, redirect: 'manual' });
  assert.equal(callback.status, 303, await callback.clone().text());
  cookie = callback.headers.get('set-cookie').split(';')[0];
  const queue = async () => {
    const response = await mf.dispatchFetch('https://queue.test/console/accounts/octocat/requests', { headers: { cookie } });
    assert.equal(response.status, 200, await response.clone().text());
    return response.text();
  };
  for (const [expiration, label] of [
    [null, 'No expiration'],
    ['2026-01-02T00:00:00Z', '2026-01-02 00:00:00 UTC'],
    ['2026-02-01T00:00:00Z', '2026-02-01 00:00:00 UTC'],
  ]) {
    await db.prepare('UPDATE invitation_links SET expires_at=? WHERE id=?').bind(expiration, link).run();
    const html = await queue();
    assert.ok(html.includes('Decision deadline: 2026-01-03 12:34:56 UTC'));
    assert.ok(html.includes(`Invitation link expiration: ${label}`));
    assert.ok(html.includes('Decision deadline passed. Queue updates may be delayed'));
  }
  await db.prepare('UPDATE invitation_requests SET decision_deadline=NULL WHERE id=?').bind(request).run();
  let html = await queue();
  assert.ok(html.includes('Decision deadline: Unavailable'));
  assert.ok(!html.includes('Decision deadline: No expiration'));
  assert.ok(!html.includes('2026-01-08'));
  await db.prepare("UPDATE invitation_requests SET state='approved' WHERE id=?").bind(request).run();
  assert.ok((await queue()).includes('No pending requests'));

  // The D1 queue is stale pending while the authoritative service has expired or
  // already decided the request. The HTTP action must report its receipt.
  await db.prepare("UPDATE invitation_requests SET state='pending',decision_deadline='2026-01-03T12:34:56Z' WHERE id=?").bind(request).run();
  for (const [action, outcome, state, status, message] of [
    ['approve', 'incompatible', 'expired', 409, 'Request expired. The decision deadline passed; the requested decision was not applied.'],
    ['decline', 'incompatible', 'expired', 409, 'Request expired. The decision deadline passed; the requested decision was not applied.'],
    ['approve', 'already_completed', 'approved', 303, 'Request already approved.'],
    ['decline', 'incompatible', 'approved', 409, 'Request is approved. The requested decision was not applied.'],
  ]) {
    // Each case represents a different original request; navigation must not
    // allocate another decision operation for an already submitted request.
    const next = `01ARZ3NDEKTSV4RRFFQ69G5FB${decisions.length}`;
    await db.prepare('UPDATE invitation_requests SET id=? WHERE id=?').bind(next, request).run();
    request = next;
    html = await queue();
    const form = html.match(new RegExp(`<form[^>]*action="[^"]+/${action}"[^>]*>([\\s\\S]*?)</form>`))?.[1];
    assert.ok(form);
    const field = name => form.match(new RegExp(`name="${name}"[^>]*value="([^"]+)"`))?.[1];
    receipt = { outcome, request: { request_id: request, link_id: link, account_id: 42, requester_id: 99,
      state, admitted_at: '2026-01-01T01:00:00Z', decision_deadline: '2026-01-03T12:34:56Z', revision: 2 } };
    const response = await mf.dispatchFetch(`https://queue.test/console/accounts/octocat/requests/${request}/${action}`, {
      method: 'POST', headers: { cookie, 'content-type': 'application/x-www-form-urlencoded' }, redirect: 'manual',
      body: new URLSearchParams({ csrf_token: field('csrf_token'), link_id: field('link_id'), operation_id: field('operation_id') }).toString(),
    });
    assert.equal(response.status, status, await response.clone().text());
    assert.ok((status === 303 ? await queue() : await response.text()).includes(message));
    assert.deepEqual(decisions.at(-1).admin, { account_id: 42, user_id: 42 });
    assert.equal(decisions.at(-1).request_id, request);
    assert.equal(decisions.at(-1).operation_id, field('operation_id'));
  }
  assert.equal(decisions.length, 4);
  const next = '01ARZ3NDEKTSV4RRFFQ69G5FC0';
  await db.prepare('UPDATE invitation_requests SET id=? WHERE id=?').bind(next, request).run();
  request = next;
  html = await queue();
  const form = html.match(/<form[^>]*action="[^"]+\/approve"[^>]*>([\s\S]*?)<\/form>/)[1];
  const field = name => form.match(new RegExp(`name="${name}"[^>]*value="([^"]+)"`))[1];
  receipt = { outcome: 'applied', request: { request_id: request, link_id: link, account_id: 42, requester_id: 99,
    state: 'approved', admitted_at: '2026-01-01T01:00:00Z', decision_deadline: '2026-01-03T12:34:56Z', revision: 2 } };
  loseAcknowledgement = true;
  const submit = operation => mf.dispatchFetch(`https://queue.test/console/accounts/octocat/requests/${request}/approve`, {
    method: 'POST', headers: { cookie, 'content-type': 'application/x-www-form-urlencoded' }, redirect: 'manual',
    body: new URLSearchParams({ csrf_token: field('csrf_token'), link_id: link, operation_id: operation }).toString(),
  });
  const concurrent = await Promise.all([submit(field('operation_id')), submit('01ARZ3NDEKTSV4RRFFQ69G5FC1')]);
  assert.deepEqual(concurrent.map(r => r.status).sort(), [303, 502]);
  assert.equal(decisions.length, 5, 'D1 atomically binds just one original decision');
  const original = decisions.at(-1);
  const recovery = `/console/accounts/octocat/attempts/decision-${original.request_id}-${original.operation_id}`;
  const list = await mf.dispatchFetch('https://queue.test/console/accounts/octocat/attempts', { headers: { cookie } });
  assert.ok((await list.text()).includes(recovery));
  const status = await mf.dispatchFetch(`https://queue.test${recovery}`, { headers: { cookie } });
  assert.ok((await status.text()).includes('Request approved.'));
  // Expired records from other sessions are physically reclaimed in bounded
  // batches by retention, while the live original input remains recoverable.
  await db.batch(Array.from({ length: 205 }, (_, i) => db.prepare(
    'INSERT INTO attempt_continuations(scope,id,binding,payload,expires_at) VALUES (?,?,?,?,?)'
  ).bind('expired-session', `expired-${i}`, `expired-${i}`, 'expired-ciphertext', 1)));
  const retryOriginal = () => mf.dispatchFetch(`https://queue.test${recovery}`, {
    method: 'POST', headers: { cookie, 'content-type': 'application/x-www-form-urlencoded' }, redirect: 'manual',
    body: new URLSearchParams({ csrf_token: field('csrf_token') }).toString(),
  });
  const retry = await retryOriginal();
  assert.equal(retry.status, 303);
  assert.deepEqual(decisions.at(-1), original);
  for (const remaining of [105, 5, 0]) {
    assert.equal(await db.prepare("SELECT count(*) AS count FROM attempt_continuations WHERE scope='expired-session'").first('count'), remaining);
    if (remaining > 0) assert.equal((await retryOriginal()).status, 303);
  }
  assert.deepEqual(decisions.at(-1), original);
  assert.ok((await (await mf.dispatchFetch(`https://queue.test${recovery}`, { headers: { cookie } })).text()).includes('Request approved.'));

  // Repeated requester AND link identities, with more than two pages. Terminal
  // rows and unrelated accounts must not inflate page work or shift a cursor.
  await db.prepare("UPDATE invitation_requests SET state='approved'").run();
  const id = n => `01ARZ3NDEKTSV4RRFFQ69G5${String(n).padStart(3, '0')}`;
  await db.batch(Array.from({ length: 153 }, (_, i) => db.prepare(
    `INSERT INTO invitation_requests(id,invitation_link_id,requester_id,state,created_at,decision_deadline,projection_revision,justification)
     VALUES(?,?,99,?,'2026-01-01T01:00:00Z','2026-01-03T12:34:56Z',1,?)`
  ).bind(id(i + 1), link, i < 53 ? 'pending' : 'approved', `Queue item ${i + 1}`)));
  const page = async (path = '/console/accounts/octocat/requests') => {
    const response = await mf.dispatchFetch(`https://queue.test${path}`, { headers: { cookie, 'x-test-observe-d1': '1' } });
    assert.equal(response.status, 200, await response.clone().text());
    const statements = JSON.parse(response.headers.get('x-test-d1-classes'));
    assert.deepEqual(statements.map(s => s.kind), [
      'installation_login:repo', 'installation_login:org',
      'installation_authority:repo', 'installation_authority:org',
      'pending_badge', 'queue_page',
    ], 'exact app-scoped discovery/authority, one scoped badge and one queue page; no per-row reads');
    assert.equal(Number(response.headers.get('x-test-d1-queries')), statements.length);
    assert.equal(Number(response.headers.get('x-test-d1-rows-read')), statements.reduce((sum, s) => sum + s.rows_read, 0));
    // The navigation badge counts all scoped pending rows; only the seek page
    // has a cardinality-independent bound. Observe them separately.
    assert.ok(statements.find(s => s.kind === 'queue_page').rows_read < 350, 'bounded page work');
    const plans = JSON.parse(response.headers.get('x-test-d1-plans')).join('\n');
    assert.match(plans, /SEARCH r USING INDEX idx_pending_queue_seek \(queue_account_id=\?/);
    if (path.includes('?after=')) assert.ok(plans.includes('<expr>>?'), plans);
    const html = await response.text();
    return { html, next: html.match(/rel="next" href="([^"]+)"/)?.[1] };
  };
  const first = await page();
  assert.equal((first.html.match(/Approve request/g) ?? []).length, 25);
  assert.ok(first.html.includes(`/${id(1)}/approve`));
  assert.ok(!first.html.includes(`/${id(26)}/approve`));
  assert.ok(first.html.includes('>Deadline fixture</a>'));
  // Work stays page-bounded as another account and terminal history grow.
  const otherLink = '01ARZ3NDEKTSV4RRFFQ69G5FD0';
  await seedPersonalInstallation(db, { installationId: 2, accountId: 777, login: 'other', repoId: 11 });
  await db.prepare(`INSERT INTO invitation_links(id,installation_id,account_id,created_by,created_at,approval_required,description,projection_revision,projection_content,secret_generation,secret_sha256)
    VALUES(?,2,777,42,'2026-01-01T00:00:00Z',1,'Private other account',1,'{}',1,'2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a')`).bind(otherLink).run();
  await db.batch(Array.from({ length: 500 }, (_, i) => db.prepare(
    `INSERT INTO invitation_requests(id,invitation_link_id,requester_id,state,created_at,projection_revision)
     VALUES(?,?,99,?,'2025-01-01T00:00:00Z',1)`
  ).bind(id(i + 200), i % 2 ? otherLink : link, i % 2 ? 'pending' : 'approved')));
  assert.ok(!(await page()).html.includes('Private other account'));
  // The overview counts the same account-scoped pending rows in one D1 statement.
  const overview = await mf.dispatchFetch('https://queue.test/console/accounts/octocat', { headers: { cookie, 'x-test-observe-d1': '1' } });
  assert.equal(overview.status, 200, await overview.clone().text());
  const overviewPlans = JSON.parse(overview.headers.get('x-test-d1-plans')).join('\n');
  assert.match(overviewPlans, /idx_pending_queue_seek \(queue_account_id=\?/, overviewPlans);
  assert.ok((await overview.text()).includes('53 pending invitation requests need an account admin decision.'));
  // A large equal-time prefix must be skipped by the index, not scanned then
  // filtered on request ID. Keep every earlier request pending while seeking.
  await db.batch(Array.from({ length: 500 }, (_, i) => db.prepare(
    `INSERT INTO invitation_requests(id,invitation_link_id,requester_id,state,created_at,projection_revision)
     VALUES(?,?,99,'pending','2026-01-01T01:00:00Z',1)`
  ).bind(`01ARZ3NDEKTSV4RRFFQ69G4${String(i).padStart(3, '0')}`, link)));
  await page(first.next);
  await db.prepare("UPDATE invitation_requests SET state='approved' WHERE id LIKE '01ARZ3NDEKTSV4RRFFQ69G4%'").run();
  await db.batch(Array.from({ length: 26 }, (_, i) => db.prepare("UPDATE invitation_requests SET state='declined' WHERE id=?").bind(id(i + 1))));
  const second = await page(first.next);
  assert.ok(second.html.includes(`/${id(27)}/approve`));
  assert.ok(second.html.includes('Back to oldest requests'));
  const third = await page(second.next);
  assert.equal((third.html.match(/Approve request/g) ?? []).length, 2);
  assert.equal(third.next, undefined);
  // Both UTC encodings and nanosecond precision must survive cursor round trips.
  await db.prepare("UPDATE invitation_requests SET state='expired'").run();
  for (const [n, time] of [[1, '2026-01-01T00:00:00.000000001Z'], [2, '2026-01-01T00:00:00+00:00'], [3, '2026-01-01T00:00:00Z']]) {
    await db.prepare("UPDATE invitation_requests SET state='pending',created_at=? WHERE id=?").bind(time, id(n)).run();
  }
  const mixed = await page();
  assert.ok(mixed.html.indexOf(`/${id(2)}/approve`) < mixed.html.indexOf(`/${id(3)}/approve`));
  assert.ok(mixed.html.indexOf(`/${id(3)}/approve`) < mixed.html.indexOf(`/${id(1)}/approve`));
  const boundary = Buffer.from(JSON.stringify({ created_at: '2026-01-01T00:00:00Z', request_id: id(3) })).toString('base64url');
  assert.equal(((await page(`/console/accounts/octocat/requests?after=${boundary}`)).html.match(/Approve request/g) ?? []).length, 1);
  await db.prepare('DELETE FROM invitation_link_targets WHERE link_id=?').bind(link).run();
  const missingRepos = await page();
  assert.ok(missingRepos.html.includes('Repository details unavailable'));
  assert.ok(!missingRepos.html.includes('Approve request'));
  // Corrupt a decoded timestamp after removing target rows; malformed data
  // must fail the page rather than look like an empty queue.
  await db.prepare("UPDATE invitation_requests SET created_at='corrupt' WHERE id=?").bind(id(1)).run();
  const corrupt = await mf.dispatchFetch('https://queue.test/console/accounts/octocat/requests', { headers: { cookie } });
  assert.equal(corrupt.status, 500);
  assert.ok(!(await corrupt.text()).includes('No pending requests'));
  await db.prepare("UPDATE invitation_requests SET created_at='2026-01-01T00:00:00.000000001Z' WHERE id=?").bind(id(1)).run();
  await db.prepare("UPDATE invitation_requests SET state='expired'").run();
  const stale = await page(first.next);
  assert.ok(stale.html.includes('No more pending requests on this page'));
  assert.ok(stale.html.includes('Back to oldest requests'));
  // Restore an orphaned request's owner: the derived queue key must recover too.
  await db.prepare("UPDATE invitation_requests SET state='pending' WHERE id=?").bind(id(1)).run();
  await db.batch([
    db.prepare('PRAGMA defer_foreign_keys=ON'),
    db.prepare('DELETE FROM invitation_links WHERE id=?').bind(link),
    db.prepare('UPDATE invitation_requests SET queue_account_id=NULL WHERE invitation_link_id=?').bind(link),
    db.prepare(`INSERT INTO invitation_links(id,installation_id,account_id,created_by,created_at,approval_required,description,projection_revision,projection_content,secret_generation,secret_sha256)
      VALUES(?,1,42,42,'2026-01-01T00:00:00Z',1,'Restored',1,'{}',1,'2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a2a')`).bind(link),
  ]);
  assert.ok((await page()).html.includes('>Restored</a>'));
  const failed = await mf.dispatchFetch('https://queue.test/console/accounts/octocat/requests', {
    headers: { cookie, 'x-test-observe-d1': '1', 'x-test-fail-queue': '1' },
  });
  assert.equal(failed.status, 500);
  assert.ok(!(await failed.text()).includes('No pending requests'));
  console.log('PASS Worker/D1 decision queue: independent historical deadlines, missing data, auto-approval, overdue projection and authoritative late actions');
} finally {
  await mf.dispose();
}
