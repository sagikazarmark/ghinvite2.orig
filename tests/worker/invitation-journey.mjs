import assert from 'node:assert/strict';
import { fileURLToPath } from 'node:url';
import { Response } from 'miniflare';
import { createHmac } from 'node:crypto';

// Actual web Wasm + workflow Wasm + Restate + shared D1. Only OAuth/GitHub HTTP
// responses and the ingress acknowledgement are controlled at network boundaries.
export function journeyWorker(ingress) {
  let requester = false;
  const lost = new Set();
  const commands = new Map();
  return {
    name: 'journey-web', modules: true,
    scriptPath: fileURLToPath(new URL('./worker.mjs', import.meta.url)),
    modulesRules: [{ type: 'CompiledWasm', include: ['**/*.wasm'] }],
    compatibilityDate: '2024-09-23', compatibilityFlags: ['nodejs_compat'],
    kvNamespaces: ['SESSIONS'],
    d1Databases: { DB: 'journey-shared' },
    bindings: {
      GHINVITE_BASE_URL: 'https://journey.test', GHINVITE_SESSION_SECRET: '07'.repeat(32),
      GHINVITE_RESTATE_INGRESS: ingress, GHINVITE_RESTATE_AUTH: 'local-unauthenticated',
      GHINVITE_GITHUB_INSTALL_URL: 'https://github.com/apps/dummy/installations/new',
      GHINVITE_GITHUB_CLIENT_ID: 'dummy', GHINVITE_GITHUB_CLIENT_SECRET: 'dummy', GHINVITE_WEBHOOK_SECRET: 'journey-secret',
    },
    async outboundService(request) {
      const url = new URL(request.url);
      if (url.origin === ingress) {
        const input = Buffer.from(await request.arrayBuffer());
        const response = await fetch(request.url, { method: request.method, headers: request.headers, body: input, signal: AbortSignal.timeout(25_000) });
        const body = await response.arrayBuffer();
        const method = url.pathname.split('/').at(-1);
        if (['create', 'admit', 'decide'].includes(method) && response.ok) {
          const original = commands.get(method);
          if (original) assert.deepEqual(JSON.parse(input), original, 'recovery preserves command input/identity');
          else commands.set(method, JSON.parse(input));
          if (!lost.has(method)) { lost.add(method); return new Response(null, { status: 502 }); }
        }
        return new Response(body, { status: response.status, headers: response.headers });
      }
      if (request.url === 'https://github.com/login/oauth/access_token') {
        requester = (await request.json()).code === 'alice';
        return Response.json({ access_token: requester ? 'alice' : 'creator', token_type: 'bearer', scope: '' });
      }
      if (request.url === 'https://api.github.com/user') return Response.json(requester ? { id: 8, login: 'alice' } : { id: 7, login: 'creator' });
      if (url.pathname === '/user/memberships/orgs/acme') return Response.json({ role: 'admin', state: 'active', organization: { id: 100 } });
      if (url.pathname === '/user/installations/1/repositories') return Response.json({ total_count: 1, repositories: [{ id: 10, full_name: 'acme/api', private: true }] });
      throw new Error(`unexpected journey outbound ${url.origin}${url.pathname}`);
    },
  };
}

export async function invitationJourney({ mf, adminUrl, ingress, githubUrl, http, storage, db, eventually, id }) {
  const web = await mf.getWorker('journey-web');
  const field = (html, name) => {
    const value = html.match(new RegExp(`name="${name}"[^>]*value="([^"]*)"`))?.[1];
    assert.notEqual(value, undefined, `missing ${name}`);
    return value;
  };
  const visit = (cookie, path, body) => web.fetch(`https://journey.test${path}`, {
    method: body === undefined ? 'GET' : 'POST', redirect: 'manual',
    headers: { cookie, ...(body === undefined ? {} : { 'content-type': 'application/x-www-form-urlencoded' }) },
    body: body === undefined ? undefined : new URLSearchParams(body).toString(),
  });
  const login = async (code) => {
    const start = await visit('', '/login');
    const state = new URL(start.headers.get('location')).searchParams.get('state');
    const response = await visit(start.headers.get('set-cookie').split(';')[0], `/oauth/callback?code=${code}&state=${state}`);
    assert.equal(response.status, 303, await response.clone().text());
    return response.headers.get('set-cookie').split(';')[0];
  };
    await http(`${ingress}/Installation/1/onboard`, { installation_id: 1, actor_user_id: 7, account_id: 100, account_login: 'acme', account_type: 'Organization', selected_repos: 'all', installed_at: new Date().toISOString() });
    await eventually(() => storage('installation', 1), Boolean);
    const admin = await login('creator');
    const form = await (await visit(admin, '/console/accounts/acme/links/new')).text();
    const action = form.match(/action="(\/console\/accounts\/acme\/links\?[^"]+)"/)[1].replaceAll('&#38;', '&').replaceAll('&amp;', '&');
    const link = new URL(action, 'https://journey.test').searchParams.get('link_id');
    assert.match(link, /^[0-9A-HJKMNP-TV-Z]{26}$/);
    const csrf = field(form, 'csrf_token');
    await db.prepare("CREATE TRIGGER delay_link BEFORE INSERT ON invitation_links BEGIN SELECT RAISE(ABORT, 'delayed projection'); END").run();
    const creation = { csrf_token: csrf, description: 'Worker journey', permission: 'pull', repo_ids: '10', approval_required: 'on' };
    assert.equal((await visit(admin, action, creation)).status, 502);
    assert.equal((await visit(admin, `/console/accounts/acme/attempts/create-${link}`, { csrf_token: csrf })).status, 303);
    assert.equal(await storage('link', link), null);
    const alice = await login('alice');
    const page = await (await visit(alice, `/i/${link}`)).text();
    const operation = field(page, 'operation_id');
    const submission = { csrf_token: field(page, 'csrf_token'), operation_id: operation, justification: 'Original worker input' };
    const unknown = await visit(alice, `/i/${link}`, submission);
    assert.equal(unknown.status, 502);
    assert.match(await unknown.text(), /Outcome unknown/);
    assert.equal((await visit(alice, `/i/${link}`, submission)).status, 303);
    const result = await http(`${ingress}/InvitationLink/${link}/requester_page`, { link_id: link, requester_id: 8, operation_id: operation });
    const requestId = result.request.request_id;
    const decision = { csrf_token: csrf, link_id: link, operation_id: operation };
    const approve = `/console/accounts/acme/requests/${requestId}/approve`;
    assert.equal((await visit(admin, approve, decision)).status, 502);
    assert.equal((await visit(admin, approve, decision)).status, 303);
    const delivered = await eventually(() => http(`${ingress}/RepositoryDelivery/${requestId}:10/status`), row => row?.create.outcome.kind === 'created');
    assert.equal(await storage('request', requestId), null);
    assert.match(await (await visit(admin, '/console/accounts/acme/requests')).text(), /Request approved/);
    await db.prepare('DROP TRIGGER delay_link').run();
    const sent = await eventually(() => storage('invitation', delivered.create.command.invitation_id), row => row?.state === 'sent');
    await http(`${githubUrl}/repos/acme/api/invitations/${sent.github_invitation_id}`, undefined, 'DELETE');
    await http(`${githubUrl}/identity`, { login: 'alice', addressed_id: 8, role_name: 'read' });
    const payload = JSON.stringify({ action: 'added', installation: { id: 1 }, repository: { id: 10, owner: { id: 100 } }, member: { id: 8 } });
    const signature = createHmac('sha256', 'journey-secret').update(payload).digest('hex');
    for (let n = 0; n < 2; n++) {
      const response = await web.fetch('https://journey.test/webhooks/github', { method: 'POST', body: payload,
        headers: { 'content-type': 'application/json', 'x-github-event': 'member', 'x-github-delivery': 'worker-journey', 'x-hub-signature-256': `sha256=${signature}` } });
      assert.equal(response.status, 204);
    }
    await eventually(async () => (await visit(alice, `/i/${link}?operation_id=${operation}`)).text(), text => text.includes('Repository access accepted'));
    await eventually(async () => (await visit(admin, '/console/accounts/acme/audit')).text(), text => text.includes('GitHub invitation accepted'));
    const events = (await storage('audit', 100)).events;
    assert.equal(events.filter(event => event.target_id === sent.id && event.event_type === 'invitation.accepted').length, 1);
    const recovery = await http(`${ingress}/DeliveryRecovery/recover`, { link_id: link, request_id: requestId, requester_id: 8 });
    for (const submitted of recovery.submitted) await http(`${ingress}/restate/invocation/${submitted.invocation_id}/attach`, undefined, 'GET');
    await eventually(() => http(`${adminUrl}/query`, { query: `SELECT id FROM sys_invocation WHERE target_service_name = 'DeliveryProjection' AND target_service_key = '${requestId}:10' AND status != 'completed'` }), result => result.rows.length === 0);
    assert.equal((await http(`${githubUrl}/calls`, undefined, 'GET')).requests.filter(call => call.method === 'PUT').length, 1);
    assert.deepEqual((await storage('audit', 100)).events, events);
    // Real request batch commits; only the D1 binding acknowledgement is lost.
    const snapshot = await http(`${ingress}/InvitationRequest/${requestId}/request_status`, { link_id: link, request_id: requestId, requester_id: 8 });
    const fresh = { ...snapshot, request_id: id(), revision: 1, state: 'pending', decision: null };
    const event = { event_id: `request.created/${fresh.request_id}`, kind: 'request.created', actor_id: 8,
      target_id: fresh.request_id, effective_at: fresh.admitted_at, evaluated_at: fresh.admitted_at };
    const envelope = { request: fresh, events: [event] };
    assert.equal(await storage('request', fresh.request_id), null);
    await storage('apply-request', envelope, 409, { 'x-test-lose-batch-ack': '1' });
    const committed = await storage('request', fresh.request_id);
    assert.equal(committed.state, 'pending', 'first request snapshot really committed despite lost binding acknowledgement');
    const history = (await storage('audit', 100)).events.filter(e => e.target_id === fresh.request_id);
    assert.equal(history.length, 1);
    assert.equal(history[0].event_type, 'request.created');
    await storage('apply-request', envelope);
    assert.deepEqual(await storage('request', fresh.request_id), committed);
    assert.deepEqual((await storage('audit', 100)).events.filter(e => e.target_id === fresh.request_id), history);
    // An inconsistent restore must not hide behind the retained JSON content.
    await db.prepare('UPDATE audit_events SET account_id=101 WHERE id=?').bind(history[0].id).run();
    await storage('apply-request', envelope, 409);
    await db.prepare('UPDATE audit_events SET account_id=100 WHERE id=?').bind(history[0].id).run();
    await storage('apply-request', envelope);
    console.log('PASS authenticated web Worker → real Restate owners → production GitHub client → signed settlement, shared D1, delayed projections and lost creation/admission/decision responses');
}
