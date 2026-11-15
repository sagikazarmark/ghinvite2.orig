import { setupCommand, establishInstallingOwner } from './installation-fixture.mjs';
import { installedWebApp, signinApp, signinRevocation } from './app-config.mjs';
import assert from 'node:assert/strict';
import { fileURLToPath } from 'node:url';
import { Response } from 'miniflare';
import { createHmac, createHash, randomBytes } from 'node:crypto';

const credentialNonce = randomBytes(16).toString('hex');
export const journeyCredentials = Object.freeze({
  creatorCode: `OAUTH_CODE_MARKER_CREATOR_${credentialNonce}`,
  requesterCode: `OAUTH_CODE_MARKER_REQUESTER_${credentialNonce}`,
  creatorToken: `ghu_MARKER_CREATOR_${credentialNonce}`,
  requesterToken: `ghu_MARKER_REQUESTER_${credentialNonce}`,
  authorityToken: `ghs_MARKER_AUTHORITY_${credentialNonce}`,
  workflowReadToken: `ghs_MARKER_WORKFLOW_READ_${credentialNonce}`,
  workflowWriteToken: `ghs_MARKER_WORKFLOW_WRITE_${credentialNonce}`,
});

const entropyFixture = process.env.LINK_SECRET_FIXTURE === '1';
const knownSecret = sequence => Buffer.from(`LINKSECRET_MARKER_${String(sequence).padStart(14, '0')}`).toString('base64url');
const secretHash = secret => createHash('sha256').update(Buffer.from(secret, 'base64url')).digest('hex');

// Actual web Wasm + workflow Wasm + Restate + shared D1. Only OAuth/GitHub HTTP
// responses and the ingress acknowledgement are controlled at network boundaries.
export function journeyWorker(ingress) {
  const lost = new Set();
  const commands = new Map();
  return {
    name: 'journey-web', modules: true,
    scriptPath: fileURLToPath(new URL(entropyFixture ? './link-secret-worker.mjs' : './worker.mjs', import.meta.url)),
    modulesRules: [{ type: 'CompiledWasm', include: ['**/*.wasm'] }],
    compatibilityDate: '2024-09-23', compatibilityFlags: ['nodejs_compat'],
    kvNamespaces: ['SESSIONS', 'AUTHZ_CACHE'],
    ratelimits: { RL_IP: { simple: { limit: 30, period: 60 } }, RL_USER: { simple: { limit: 10, period: 60 } }, AUTHZ_RL: { simple: { limit: 10, period: 60 } } },
    d1Databases: { DB: 'journey-shared' },
    bindings: {
      ...(entropyFixture ? { P1_LINK_SECRET_FIXTURE: 'known-marker-v1' } : {}),
      P1_HOST: 'journey.test', P1_KEYRING: 'kekring1;name=p1;current=1;hwm=1;k1=BwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwc.9fdab21f330c89b7',
      RESTATE_INGRESS_URL: 'https://ingress.fixture.test', RESTATE_INGRESS_TOKEN: 'synthetic-worker-ingress-key',
      GHINVITE_GITHUB_INSTALL_URL: 'https://github.com/apps/dummy/installations/new',
      ...signinApp, ...installedWebApp, GHINVITE_WEBHOOK_SECRET: 'journey-secret',
    },
    async outboundService(request) {
      const url = new URL(request.url);
      if (url.origin === 'https://ingress.fixture.test') {
        assert.equal(request.headers.get('authorization'), 'Bearer synthetic-worker-ingress-key');
        const input = Buffer.from(await request.arrayBuffer());
        const response = await fetch(`${ingress}${url.pathname}${url.search}`, { method: request.method, headers: request.headers, body: input, signal: AbortSignal.timeout(25_000) });
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
        assert.match(request.headers.get("content-type"), /^application\/x-www-form-urlencoded/);
        const code = new URLSearchParams(await request.text()).get("code");
        assert.ok([journeyCredentials.creatorCode, journeyCredentials.requesterCode].includes(code), 'OAuth exchange must consume the exact fixture code');
        return Response.json({ access_token: code === journeyCredentials.requesterCode ? journeyCredentials.requesterToken : journeyCredentials.creatorToken, token_type: 'bearer', scope: '' });
      }
      if (request.url === 'https://api.github.com/user') {
        const authorization = request.headers.get('authorization');
        assert.ok([`Bearer ${journeyCredentials.creatorToken}`, `Bearer ${journeyCredentials.requesterToken}`].includes(authorization), 'user read must consume the actual exchanged fixture token');
        return Response.json(authorization === `Bearer ${journeyCredentials.requesterToken}` ? { id: 8, login: 'alice' } : { id: 7, login: 'creator' });
      }
      const revoked = signinRevocation(request); if (revoked) return revoked;
      if (url.pathname === '/app/installations/1/access_tokens') {
        assert.match(request.headers.get('authorization'), /^Bearer [^.]+\.[^.]+\.[^.]+$/);
        assert.deepEqual(await request.json(), {permissions:{metadata:'read'}});
        return Response.json({token:journeyCredentials.authorityToken,expires_at:'2099-01-01T00:00:00Z'},{status:201});
      }
      assert.equal(request.headers.get('authorization'),`Bearer ${journeyCredentials.authorityToken}`);
      if (url.pathname === '/installation/repositories') return Response.json({total_count:1,repositories:[{id:10,full_name:'acme/api',owner:{id:100,login:'acme'}}]});
      if (url.pathname === '/repos/acme/api') return Response.json({id:10,full_name:'acme/api',owner:{id:100,login:'acme'}});
      if (url.pathname === '/repos/acme/api/collaborators/creator/permission') return Response.json({user:{id:7,login:'creator'},role_name:'admin'});
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
    headers: { cookie, 'CF-Connecting-IP': '192.0.2.21', ...(body === undefined ? {} : { 'content-type': 'application/x-www-form-urlencoded' }) },
    body: body === undefined ? undefined : new URLSearchParams(body).toString(),
  });
  const login = async (code) => {
    const start = await visit('', '/login');
    const state = new URL(start.headers.get('location')).searchParams.get('state');
    const response = await visit(start.headers.get('set-cookie').split(';')[0], `/oauth/callback?code=${code}&state=${state}`);
    assert.equal(response.status, 303, await response.clone().text());
    return response.headers.get('set-cookie').split(';')[0];
  };
    await http(`${ingress}/Installation/repo:1/onboard`, setupCommand({ installation_id: 1, actor_user_id: 7, account_id: 100, account_login: 'acme', account_type: 'Organization', selected_repos: 'all', installed_at: new Date().toISOString() }));
    await establishInstallingOwner({http,ingress});
    await eventually(async () => (await db.prepare("SELECT installed_by_user_id FROM installations WHERE app='repo' AND installation_id=1").first())?.installed_by_user_id === 7, Boolean);
    const admin = await login(journeyCredentials.creatorCode);
    const form = await (await visit(admin, '/console/accounts/acme/links/new')).text();
    const action = form.match(/action="(\/console\/accounts\/acme\/links\?[^"]+)"/)[1].replaceAll('&#38;', '&').replaceAll('&amp;', '&');
    const link = new URL(action, 'https://journey.test').searchParams.get('link_id');
    assert.match(link, /^[0-9A-HJKMNP-TV-Z]{26}$/);
    const csrf = field(form, 'csrf_token');
    await db.prepare("CREATE TRIGGER delay_link BEFORE INSERT ON invitation_links BEGIN SELECT RAISE(ABORT, 'delayed projection'); END").run();
    const creation = { csrf_token: csrf, description: 'Worker journey', permission: 'pull', repo_ids: '10', repo_grants: '10:pull', approval_required: 'on' };
    const lostCreation = await visit(admin, action, creation);
    assert.equal(lostCreation.status, 502);
    if (entropyFixture) {
      assert.ok(!(await lostCreation.text()).includes(knownSecret(1)), 'lost acknowledgement never rediscloses creation secret');
      const created = await http(`${ingress}/InvitationLink/${link}/link_status`, {link_id:link,admin:{account_id:100,user_id:7}});
      assert.equal(created.secret.sha256, secretHash(knownSecret(1)), 'production create hashed exact deterministic entropy');
      assert.equal(created.secret.secret_generation, 1);
    }
    assert.equal((await visit(admin, `/console/accounts/acme/attempts/create-${link}`, { csrf_token: csrf })).status, 303);
    assert.equal(await storage('link', link), null);
    const unavailableHistory = await visit(admin, `/console/accounts/acme/links/${link}/requests`);
    assert.equal(unavailableHistory.status, 503);
    assert.match(await unavailableHistory.text(), /Request history unavailable/);
    const rotation = await visit(admin, `/console/accounts/acme/links/${link}/rotate`, {csrf_token:csrf,operation_id:id()});
    assert.equal(rotation.status,200,await rotation.clone().text());
    const secret = (await rotation.text()).match(/\/i#([A-Za-z0-9_-]{43})/)[1];
    const capability = await http(`${ingress}/InvitationLink/${link}/link_status`, {link_id:link,admin:{account_id:100,user_id:7}});
    if (entropyFixture) {
      assert.equal(secret, knownSecret(2), 'production rotate consumes next fixture entropy exactly once');
      assert.equal(capability.secret.sha256, secretHash(knownSecret(2)));
      assert.equal(capability.secret.secret_generation, 2);
    }
    const alice = await login(journeyCredentials.requesterCode);
    const bootstrap = await (await visit(alice,'/i')).text();
    const selected = await visit(alice,'/i',{secret,csrf_token:field(bootstrap,'csrf_token')});
    assert.equal(selected.status,303,await selected.clone().text());
    assert.equal(selected.headers.get('location'),'/join');
    const page = await (await visit(alice, '/join')).text();
    const operation = field(page, 'operation_id');
    const submission = { csrf_token: field(page, 'csrf_token'), operation_id: operation, justification: 'Original worker input' };
    const unknown = await visit(alice, '/join', submission);
    assert.equal(unknown.status, 502);
    assert.match(await unknown.text(), /Outcome unknown/);
    assert.equal((await visit(alice, '/join', submission)).status, 303);
    const result = await http(`${ingress}/InvitationLink/${link}/requester_page`, { link_id: link, requester_id: 8, operation_id: operation, secret_sha256:capability.secret.sha256, secret_generation:capability.secret.secret_generation });
    const requestId = result.request.request_id;
    const retainedDetail = await visit(admin, `/console/accounts/acme/requests/${requestId}`);
    assert.equal(retainedDetail.status, 200);
    assert.match(await retainedDetail.text(), /Original worker input/);
    const decision = { csrf_token: csrf, link_id: link, operation_id: operation };
    const approve = `/console/accounts/acme/requests/${requestId}/approve`;
    assert.equal((await visit(admin, approve, decision)).status, 502);
    assert.equal((await visit(admin, approve, decision)).status, 303);
    const delivered = await eventually(() => http(`${ingress}/RepositoryDelivery/${requestId}:10:1/status`), row => row?.create.outcome.kind === 'created');
    assert.equal(await storage('delivery-run-losses', null), 3, 'three real guarded ctx.run closures failed after a confirmed external effect/recovery');
    assert.deepEqual(await http(`${ingress}/RepositoryDelivery/${requestId}:10:1/create`, delivered.create.command), delivered.create, 'confirmed create receipt remains immutable after run retries');
    assert.equal(await storage('request', requestId), null);
    // Request-ID navigation uses retained owners even while request/link SQL is
    // delayed; the account admin's identity is not this requester's identity.
    const ownedStatus = await visit(alice, `/requests/${requestId}`);
    assert.equal(ownedStatus.status, 200, await ownedStatus.clone().text());
    assert.equal(ownedStatus.headers.get('cache-control'), 'no-store');
    assert.equal(ownedStatus.headers.get('referrer-policy'), 'no-referrer');
    const ownedHtml = await ownedStatus.text();
    assert.match(ownedHtml, /Request status/);
    assert.match(ownedHtml, /Refresh repository delivery/);
    assert.equal(ownedHtml.includes('Original worker input'), false);
    const foreignStatus = await visit(admin, `/requests/${requestId}`);
    assert.equal(foreignStatus.status, 404);
    assert.equal((await foreignStatus.text()).includes('acme/api'), false);
    const refreshToken = field(ownedHtml, 'csrf_token');
    const requesterRefresh = (token = refreshToken) => web.fetch(`https://journey.test/requests/${requestId}/refresh`, {
      method: 'POST', redirect: 'manual',
      headers: { cookie: alice, 'CF-Connecting-IP': '192.0.2.21', accept: 'application/json', 'content-type': 'application/x-www-form-urlencoded' },
      body: new URLSearchParams({ csrf_token: token, target: 'repo:10' }).toString(),
    });
    assert.equal((await requesterRefresh('invalid')).status, 403);
    const requestedRefresh = await requesterRefresh();
    assert.equal(requestedRefresh.status, 200, await requestedRefresh.clone().text());
    assert.deepEqual(await requestedRefresh.json(), { kind: 'requested' });
    const throttledRefresh = await requesterRefresh();
    assert.equal(throttledRefresh.status, 429, await throttledRefresh.clone().text());
    const throttledReceipt = await throttledRefresh.json();
    assert.equal(throttledReceipt.kind, 'throttled');
    assert.ok(Date.parse(throttledReceipt.retry_at) > Date.now());
    assert.deepEqual((await http(`${ingress}/RepositoryDelivery/${requestId}:10:1/status`)).create, delivered.create);
    console.log('PASS signed requester-ID status during SQL outage, foreign404, CSRF and durable read-only refresh throttle');
    assert.match(await (await visit(admin, '/console/accounts/acme/requests')).text(), /Request approved/);
    await db.prepare('DROP TRIGGER delay_link').run();
    const sent = await eventually(() => storage('delivery-by-key', `${requestId}:10:1`), row => row?.create.outcome.kind === 'created' && !row.settlement);
    await http(`${githubUrl}/repos/acme/api/invitations/${sent.create.outcome.upstream_id}`, undefined, 'DELETE');
    await http(`${githubUrl}/identity`, { login: 'alice', addressed_id: 8, role_name: 'read' });
    const payload = JSON.stringify({ action: 'added', installation: { id: 1 }, repository: { id: 10, owner: { id: 100 } }, member: { id: 8 } });
    const signature = createHmac('sha256', 'fixture-repo-webhook-current').update(payload).digest('hex');
    for (let n = 0; n < 2; n++) {
      const response = await web.fetch('https://journey.test/webhooks/repo', { method: 'POST', body: payload,
        headers: { 'content-type': 'application/json', 'x-github-event': 'member', 'x-github-delivery': 'worker-journey', 'x-hub-signature-256': `sha256=${signature}` } });
      assert.equal(response.status, 202);
    }
    // Repository app has no member events; settlement requires read-only observation.
    await http(`${ingress}/DeliveryObservation/observe`, delivered);
    await eventually(async () => (await visit(alice, `/join?operation_id=${operation}`)).text(), text => text.includes('Repository access accepted'));
    const settledRefresh = await requesterRefresh();
    assert.equal(settledRefresh.status, 200, await settledRefresh.clone().text());
    assert.deepEqual(await settledRefresh.json(), { kind: 'not_tracked' });
    await eventually(async () => (await visit(admin, '/console/accounts/acme/audit')).text(), text => text.includes('GitHub invitation accepted'));
    const events = (await storage('audit', 100)).events;
    assert.equal(events.filter(event => event.target_id === `${requestId}:10:1` && event.event_type === 'invitation.accepted').length, 1);
    const recovery = await http(`${ingress}/DeliveryRecovery/recover`, { link_id: link, request_id: requestId, requester_id: 8 });
    for (const submitted of recovery.submitted) await http(`${ingress}/restate/invocation/${submitted.invocation_id}/attach`, undefined, 'GET');
    await eventually(() => http(`${adminUrl}/query`, { query: `SELECT id FROM sys_invocation WHERE target_service_name = 'DeliveryProjection' AND target_service_key = '${requestId}:10:1' AND status != 'completed'` }), result => result.rows.length === 0);
    assert.equal((await http(`${githubUrl}/calls`, undefined, 'GET')).requests.filter(call => call.method === 'PUT').length, 1);
    console.log('PASS three real guarded ctx.run failures, immutable confirmed receipt replay, and exactly one GitHub PUT');
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
    return {
      credentialMarkers: Object.values(journeyCredentials),
      ...(entropyFixture
        ? { secrets: [knownSecret(1), knownSecret(2)], markerPrefixes: ['LINKSECRET_MARKER_'] }
        : { secrets: [secret] }),
    };
}
