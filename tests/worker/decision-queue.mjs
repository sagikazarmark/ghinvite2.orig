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
  scriptPath: fileURLToPath(new URL('./worker.mjs', import.meta.url)),
  modulesRules: [{ type: 'CompiledWasm', include: ['**/*.wasm'] }],
  compatibilityDate: '2024-09-23', compatibilityFlags: ['nodejs_compat'],
  kvNamespaces: ['SESSIONS'], d1Databases: ['DB'],
  bindings: {
    GHINVITE_ADMISSION_MODE: 'authoritative', GHINVITE_BASE_URL: 'https://queue.test',
    GHINVITE_SESSION_SECRET: '07'.repeat(32), GHINVITE_RESTATE_INGRESS: 'https://restate.test',
    GHINVITE_RESTATE_AUTH: 'local-unauthenticated',
    GHINVITE_GITHUB_INSTALL_URL: 'https://github.com/apps/dummy/installations/new',
    GHINVITE_GITHUB_CLIENT_ID: 'dummy', GHINVITE_GITHUB_CLIENT_SECRET: 'dummy', GHINVITE_WEBHOOK_SECRET: 'dummy',
  },
  async outboundService(request) {
    if (request.url === 'https://github.com/login/oauth/access_token') return Response.json({ access_token: 'fixture', token_type: 'bearer', scope: '' });
    if (request.url === 'https://api.github.com/user') return Response.json({ id: 42, login: 'octocat', avatar_url: null });
    if (/^https:\/\/restate.test\/InvitationLinkV1\/[A-Z0-9]+\/decide$/.test(request.url)) {
      decisions.push(await request.json());
      assert.ok(receipt, 'unexpected decision');
      if (loseAcknowledgement) { loseAcknowledgement = false; return new Response('acknowledgement lost', { status: 503 }); }
      return Response.json(receipt);
    }
    if (request.url.endsWith('/decision_status')) return Response.json(receipt);
    throw new Error(`unexpected outbound ${request.url}`);
  },
});

try {
  const db = await mf.getD1Database('DB');
  for (const file of readdirSync(new URL('../../migrations/', import.meta.url)).filter(file => file.endsWith('.sql')).sort()) {
    await db.exec(readFileSync(new URL(`../../migrations/${file}`, import.meta.url), 'utf8').replace(/^--.*$/gm, '').replaceAll('\n', ' '));
  }
  await db.prepare("INSERT INTO installations VALUES (1,42,'octocat','User','2026-01-01T00:00:00Z',NULL,'[]')").run();
  for (const user of [42, 99]) await db.prepare("INSERT INTO users VALUES (?, ?, NULL, '2026-01-01T00:00:00Z')").bind(user, `user-${user}`).run();
  const link = '01ARZ3NDEKTSV4RRFFQ69G5FAV';
  let request = '01ARZ3NDEKTSV4RRFFQ69G5FAW';
  await db.prepare(`INSERT INTO invitation_links (id,slug,installation_id,account_id,created_by,created_at,permission,approval_required,description)
    VALUES (?,'QueueDeadline001',1,42,42,'2026-01-01T00:00:00Z','pull',1,'Deadline fixture')`).bind(link).run();
  await db.prepare("INSERT INTO invitation_link_repos VALUES (?,10,'octocat/api')").bind(link).run();
  await db.prepare(`INSERT INTO invitation_requests (id,invitation_link_id,requester_id,state,created_at,decision_deadline)
    VALUES (?,?,99,'pending','2026-01-01T01:00:00Z','2026-01-03T12:34:56Z')`).bind(request, link).run();

  const login = await mf.dispatchFetch('https://queue.test/login', { redirect: 'manual' });
  const state = new URL(login.headers.get('location')).searchParams.get('state');
  let cookie = login.headers.get('set-cookie').split(';')[0];
  const callback = await mf.dispatchFetch(`https://queue.test/oauth/callback?code=dummy&state=${state}`, { headers: { cookie }, redirect: 'manual' });
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
  const recovery = `/console/accounts/octocat/attempts/decision-${link}-${original.operation_id}`;
  const list = await mf.dispatchFetch('https://queue.test/console/accounts/octocat/attempts', { headers: { cookie } });
  assert.ok((await list.text()).includes(recovery));
  const status = await mf.dispatchFetch(`https://queue.test${recovery}`, { headers: { cookie } });
  assert.ok((await status.text()).includes('Request approved.'));
  const retry = await mf.dispatchFetch(`https://queue.test${recovery}`, {
    method: 'POST', headers: { cookie, 'content-type': 'application/x-www-form-urlencoded' }, redirect: 'manual',
    body: new URLSearchParams({ csrf_token: field('csrf_token') }).toString(),
  });
  assert.equal(retry.status, 303);
  assert.deepEqual(decisions.at(-1), original);
  console.log('PASS Worker/D1 decision queue: independent historical deadlines, missing data, auto-approval, overdue projection and authoritative late actions');
} finally {
  await mf.dispose();
}
