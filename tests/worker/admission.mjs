import { retainRuntimeLog, trackFixtureOutput } from './runtime-output.mjs';
import { identitySigner } from './restate-identity.mjs';
import { emailRing, emailVaultWorker, emailDelivery } from './email-delivery.mjs';
import { setupCommand, establishInstallingOwner } from './installation-fixture.mjs';
import { revokeCommand } from './revocation-fixture.mjs';
import { capabilitySecrecy } from './capability-secrecy.mjs';
import { capabilityFor, admitCapability } from './capability.mjs';
import { auditHistory } from './audit-history.mjs';
import assert from 'node:assert/strict';
import { readFileSync, readdirSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { Miniflare, Response, Log, LogLevel } from 'miniflare';
import { execFileSync, execFile } from 'node:child_process';
import { once } from 'node:events';
import { createServer } from 'node:http2';
import { randomBytes } from 'node:crypto';
import { browserAdmission } from './browser-admission.mjs';
import { settlement } from './settlement.mjs';
import { sweepSchedule } from './sweep-schedule.mjs';
import { frames, fields } from './protocol.mjs';
import { deadlineRecovery } from './deadline-recovery.mjs';
import { installationRecovery } from './installation.mjs';
import { orgObservation } from './org-observation.mjs';
import { organizationDelivery } from './organization-delivery.mjs';
import { organizationCancellation } from './organization-cancellation.mjs';
import { linkPause } from './link-pause.mjs';
import { deliveryControl } from './delivery-control.mjs';
import { cancellation } from './cancellation.mjs';
import { approvedDelivery } from './approved-delivery.mjs';
import { invitationJourney, journeyWorker } from './invitation-journey.mjs';
import { startOctoemu } from './octoemu.mjs';
import { GithubWorld, acmeSeed } from './github-world.mjs';

const signer = identitySigner();
const root = fileURLToPath(new URL('../../', import.meta.url));
const project = `ghinvite-worker-${process.pid}-${randomBytes(4).toString('hex')}`;
const composeFiles = ['-f', 'compose.yaml', ...(process.env.JOURNEY_ONLY === '1' ? ['-f', 'tests/worker/journey-compose.yaml'] : [])];
const compose = (...args) => execFileSync('docker', ['compose', ...composeFiles, '--profile', 'smoke', '-p', project, ...args], {
  cwd: root, encoding: 'utf8', timeout: 180_000, maxBuffer: 16 * 1024 * 1024,
});
const deadline = setTimeout(() => terminate('Worker gate deadline', 1), 300_000);
process.once('SIGINT', () => terminate('SIGINT', 130));
process.once('SIGTERM', () => terminate('SIGTERM', 143));
let server;
let stopping = false;
const sessions = new Set();
const runtimeLogs = [];
let runtimeLogBytes = 0;
let runtimeLogOverflow = false;
const traffic = [];
let interruption;
let clock;
let sweepClock;
let pauseWorkflows = false;
let unexpectedOutbound = 0;
let cleanupPromise;
let primaryFailure;
let auditAckAccount;
let lostAuditAcks = 0;
let lostAuditInvocation;
// GitHub is the octoemu emulator; each mode starts from its own world
// (github-world.mjs, or the sub-driver's `seed`).
const modeSeeds = {
  ORG_OBSERVATION_ONLY: () => import('./org-observation.mjs').then(m => m.seed),
  BUDGET_CAP_ONLY: () => import('./org-observation.mjs').then(m => m.seed),
  ORG_DELIVERY_ONLY: () => import('./organization-delivery.mjs').then(m => m.seed),
  ORG_CANCELLATION_ONLY: () => import('./organization-cancellation.mjs').then(m => m.seed),
  ORG_WEBHOOK_ONLY: () => import('./organization-webhooks.mjs').then(m => m.seed),
  ACCESS_REMOVAL_ONLY: () => import('./access-removal.mjs').then(m => m.seed),
  EMAIL_DELIVERY_ONLY: () => import('./email-delivery.mjs').then(m => m.seed),
  INSTALLATION_ONLY: () => import('./installation.mjs').then(m => m.seed),
};
const mode = Object.keys(modeSeeds).find(name => process.env[name] === '1');
const github = await startOctoemu(mode ? (await modeSeeds[mode]())() : acmeSeed());
const world = new GithubWorld(github);
const composeCleanup = args => new Promise((resolve, reject) => {
  execFile('docker', ['compose', ...composeFiles, '--profile', 'smoke', '-p', project, ...args],
    { cwd: root, timeout: 15_000, killSignal: 'SIGKILL', maxBuffer: 16 * 1024 * 1024 }, (error, stdout, stderr) => {
      if (error) reject(new Error(`Compose cleanup failed for ${project}`, { cause: error }));
      else resolve({stdout, stderr});
    });
});
async function stopCompose() {
  const errors = [];
  try { await composeCleanup(['stop', '--timeout', '5', 'restate-smoke']); } catch (error) { errors.push(error); }
  try {
    const logs = await composeCleanup(['logs', '--no-color', 'restate-smoke']);
    retainRuntimeLog('restate-compose-stdout', logs.stdout);
    retainRuntimeLog('restate-compose-stderr', logs.stderr);
  } catch (error) { errors.push(error); }
  try { await composeCleanup(['down', '--volumes', '--remove-orphans', '--timeout', '5']); } catch (error) { errors.push(error); }
  if (errors.length) throw new AggregateError(errors, 'Compose stop/logs/down failed');
}
async function cleanup() {
  if (cleanupPromise) return cleanupPromise;
  stopping = true;
  cleanupPromise = (async () => {
    const errors = [];
    const step = async action => { try { await action(); } catch (error) { errors.push(error); } };
    await step(stopCompose);
    await step(() => github.stop());
    await step(async () => {
      if (server) { for (const session of sessions) session.destroy(); await new Promise(resolve => server.close(resolve)); }
    });
    await step(() => mf.dispose());
    if (errors.length) throw new AggregateError(errors, `cleanup failed for ${project}`);
  })();
  return cleanupPromise;
}
async function terminate(reason, code) {
  console.error(`FAIL ${reason}`);
  const hardStop = setTimeout(() => { console.error(`Cleanup deadline; retry docker compose -p ${project} -f compose.yaml down --volumes`); process.exit(code); }, 30_000);
  try { await cleanup(); } catch (error) { console.error(error); }
  clearTimeout(hardStop);
  process.exit(code);
}
const id = () => '01' + randomBytes(12).toString('hex').toUpperCase();
const creation = () => {
  const link_id = id();
  return { link_id, secret_sha256: capabilityFor(link_id).sha256,
    admin: { account_id: 100, user_id: 7 }, account_id: 100, installation_id: 1,
    description: 'Workshop', internal_note: 'Private context', expires_at: null,
    max_uses: 1, approval_required: true,
    targets: { organization: null, teams: [], repos: [{ repo_id: 10, repo_full_name: 'acme/api', grant: 'read' }] } };
};
const http = async (url, body, method = 'POST', timeout = 25_000) => {
  const response = await fetch(url, { method, headers: { ...(body === undefined ? {} : { 'content-type': 'application/json' }), accept: 'application/json' },
    body: body === undefined ? undefined : JSON.stringify(body), signal: AbortSignal.timeout(timeout) });
  const text = await response.text();
  assert.ok(response.ok, `${method} ${new URL(url).pathname}: ${response.status} ${text}`);
  return text ? JSON.parse(text) : null;
};

const workflowOptions = {
  name: 'journey-workflows',
  log: new Log(LogLevel.ERROR),
  handleRuntimeStdio(stdout, stderr) {
    // Keep bounded diagnostics, including panic text, without per-suspension noise.
    for (const stream of [stdout, stderr]) stream.on('data', chunk => {
      if (process.env.JOURNEY_ONLY === '1' || process.env.EMAIL_DELIVERY_ONLY === '1') {
        runtimeLogBytes += chunk.length;
        if (runtimeLogBytes > 16 * 1024 * 1024) runtimeLogOverflow = true;
        else runtimeLogs.push(chunk.toString());
      } else { runtimeLogs.push(chunk.toString()); if (runtimeLogs.length > 50) runtimeLogs.shift(); }
    });
  },
  modules: true,
  scriptPath: fileURLToPath(new URL((process.env.CANCELLATION_ONLY === '1' || process.env.BUDGET_CAP_ONLY === '1') ? './cancellation-worker.mjs' : './workflows-worker.mjs', import.meta.url)),
  modulesRules: [{ type: 'CompiledWasm', include: ['**/*.wasm'] }],
  compatibilityDate: '2024-09-23',
  compatibilityFlags: ['nodejs_compat'],
  d1Databases: { DB: 'journey-shared' },
  bindings: {
    ...(['JOURNEY_ONLY','DELIVERY_CONTROL_ONLY'].some(name=>process.env[name]==='1') ? {P1_DELIVERY_RUN_RETRY_FIXTURE:'three-losses-v1'} : {}),
    ...((process.env.ORG_OBSERVATION_ONLY === '1' || process.env.BUDGET_CAP_ONLY === '1' || process.env.ORG_DELIVERY_ONLY === '1' || process.env.ORG_CANCELLATION_ONLY === '1' || process.env.ORG_WEBHOOK_ONLY === '1' || process.env.ACCESS_REMOVAL_ONLY === '1' || process.env.EMAIL_DELIVERY_ONLY === '1') ? {
      P1_APP_STAGE:'repo-and-org', GH_ORG_APP_ID:'456', GH_ORG_CLIENT_ID:'Iv.fixture-org',
      GH_ORG_PRIVATE_KEYS:`fixture:${readFileSync(new URL('../../test-fixtures/github-app-rsa.pem', import.meta.url)).toString('base64')}`,
    } : {}),
    ...((process.env.ORG_DELIVERY_ONLY === '1' || process.env.ORG_CANCELLATION_ONLY === '1' || process.env.ORG_WEBHOOK_ONLY === '1' || process.env.ACCESS_REMOVAL_ONLY === '1' || process.env.EMAIL_DELIVERY_ONLY === '1') ? {GH_ORG_DELIVERY_ENABLED:'true'} : {}),
    ...(process.env.ACCESS_REMOVAL_ONLY === '1' ? {GH_ACCESS_REMOVAL_ENABLED:'true'} : {}),
    ...(process.env.REISSUE_ONLY === '1' ? {GH_REISSUE_ENABLED:'true'} : {}),
    ...(process.env.EMAIL_DELIVERY_ONLY === '1' ? {GH_BULK_ENABLED:'true',P1_EMAIL_KEYRING:emailRing} : {}),
    ...github.bindings,
    RESTATE_IDENTITY_KEYS: signer.key,
    P1_HOST: 'production.test', GH_REPO_APP_ID: '123', GH_REPO_CLIENT_ID: 'Iv.fixture-repo',
    GH_REPO_PRIVATE_KEYS: `fixture:${readFileSync(new URL('../../test-fixtures/github-app-rsa.pem', import.meta.url)).toString('base64')}`,
  },
  // Only GitHub (the emulator) is reachable; it is passed through unchanged.
  async outboundService(request) {
    if (github.owns(request)) return github.forward(request);
    unexpectedOutbound++;
    return new Response('Outbound network disabled', { status: 502 });
  },
};
const mf = new Miniflare(workflowOptions);

const storage = async (operation, input, expected = 200, headers = {}) => {
  const response = await mf.dispatchFetch(`http://worker.test/__fixture/${operation}`, {
    method: 'POST', body: JSON.stringify(input), headers: { 'content-type': 'application/json', ...headers },
  });
  const value = await response.json();
  assert.equal(response.status, expected, JSON.stringify(value));
  return value;
};
const eventually = async (read, predicate) => {
  const end = Date.now() + 25_000;
  let value;
  while (Date.now() < end) { value = await read(); if (predicate(value)) return value; await new Promise(resolve => setTimeout(resolve, 100)); }
  assert.fail(`convergence deadline exceeded: ${JSON.stringify(value)}`);
};

try {
  const response = await mf.dispatchFetch('http://worker.test/discover', {
    headers: { accept: 'application/vnd.restate.endpointmanifest.v3+json', ...signer.headers('/discover') },
  });
  assert.equal(response.status, 200, await response.clone().text());
  const manifest = await response.json();
  assert.ok(manifest.services.some(service => service.name === 'InvitationLink'), 'authoritative link endpoint must be deployed');
  assert.ok(manifest.services.some(service => service.name === 'InvitationProjection'), 'D1 projector must be deployed');
  console.log('PASS actual workflows Worker authoritative discovery');
  compose('up', '-d', 'restate-smoke');
  const admin = `http://${compose('port', 'restate-smoke', '9070').trim()}`;
  const ingress = `http://${compose('port', 'restate-smoke', '8080').trim()}`;
  if (process.env.JOURNEY_ONLY === '1') {
    const { log, handleRuntimeStdio, ...worker } = workflowOptions;
    await mf.setOptions({ log, handleRuntimeStdio, workers: [worker, journeyWorker(ingress, github)] });
  } else if (process.env.EMAIL_DELIVERY_ONLY === '1') {
    const { log, handleRuntimeStdio, ...worker } = workflowOptions;
    await mf.setOptions({log,handleRuntimeStdio,workers:[worker,emailVaultWorker()]});
  } else if (process.env.ORG_WEBHOOK_ONLY === '1') {
    const { organizationWebhookWorker } = await import('./organization-webhooks.mjs');
    const { log, handleRuntimeStdio, ...worker } = workflowOptions;
    await mf.setOptions({ log, handleRuntimeStdio, workers: [worker, organizationWebhookWorker(ingress)] });
  }
  for (let attempt = 0; ; attempt++) {
    try { await http(`${admin}/health`, undefined, 'GET'); break; }
    catch (error) { if (attempt === 50) throw error; await new Promise(resolve => setTimeout(resolve, 100)); }
  }
  server = createServer(async (request, response) => {
    try {
      const chunks = [];
      for await (const chunk of request) chunks.push(chunk);
      const body = Buffer.concat(chunks);
      const incoming = request.url.startsWith('/invoke/') ? frames(body) : [];
      const invocation = incoming[0]?.type === 0 ? fields(incoming[0].payload).get(2)?.toString() : undefined;
      const objectKey = incoming[0]?.type === 0 ? fields(incoming[0].payload).get(6)?.toString() : undefined;
      if ((interruption?.blocked && interruption.blocked === invocation) || (pauseWorkflows && request.url.endsWith('/InvitationRequest/dispatch'))) {
        response.writeHead(503); response.end(); return;
      }
      const requestClock = request.url.includes('/SweepSchedule/') && sweepClock !== undefined ? sweepClock : clock;
      const signatureTime = Math.floor((requestClock ?? Date.now()) / 1000);
      const result = await mf.dispatchFetch(`http://worker.test${request.url}`, {
        method: request.method, headers: { ...Object.fromEntries(Object.entries(request.headers).filter(([key]) => !key.startsWith(':'))),
          ...signer.headers(request.url, {iat:signatureTime,nbf:signatureTime-60,exp:signatureTime+60}),
          ...(clock === undefined ? {} : { 'x-test-clock': String(clock) }),
          ...(sweepClock === undefined || !request.url.includes('/SweepSchedule/') ? {} : {'x-test-clock':String(sweepClock)}),
          ...(auditAckAccount && objectKey === auditAckAccount && request.url.endsWith('/InstallationProjection/apply')
            ? { 'x-test-lose-audit-ack': '1' } : {}) },
        body: body.length ? body : undefined,
      });
      if (result.status >= 500) console.error('Worker failure:', await result.clone().text());
      const output = Buffer.from(await result.arrayBuffer());
      const lost = Number(result.headers.get('x-test-lost-audit-acks') || 0);
      lostAuditAcks += lost;
      if (lost) lostAuditInvocation = invocation;
      const outgoing = incoming.length && result.status === 200 ? frames(output) : [];
      if (incoming.length) traffic.push({ path: request.url, input: body.length, output: output.length,
        frames: outgoing.map(frame => ({ type: frame.type, bytes: frame.payload.length })), invocation, objectKey });
      response.writeHead(result.status, Object.fromEntries([...result.headers].filter(([key]) => !['transfer-encoding', 'connection', 'keep-alive'].includes(key))));
      const cut = interruption && body.includes(Buffer.from(interruption.operation)) && outgoing.find(frame => frame.type === (interruption.type ?? 0x0403)
        && (interruption.type || fields(frame.payload).get(1)?.toString() === 'link'));
      if (cut && !interruption.blocked) {
        interruption.blocked = invocation;
        // Deliver the real first state write, lose the remainder. Restate must
        // recover the original journal; no handler/state implementation is mocked.
        response.end(output.subarray(0, cut.end));
      } else response.end(output);
    } catch (error) { if (!stopping) console.error(error); if (!response.destroyed) { response.writeHead(503); response.end(); } }
  });
  server.on('session', session => { sessions.add(session); session.on('close', () => sessions.delete(session)); });
  await new Promise(resolve => server.listen(0, '0.0.0.0', resolve));
  await http(`${admin}/deployments`, { uri: `http://host.docker.internal:${server.address().port}`, additionalHeaders: {} });
  const db = await mf.getD1Database('DB');
  for (const file of readdirSync(new URL('../../migrations/', import.meta.url)).filter(file => file.endsWith('.sql')).sort()) {
    const sql = readFileSync(new URL(`../../migrations/${file}`, import.meta.url), 'utf8');
    await db.exec(sql.replace(/^[ \t]*--.*$/gm, '').replaceAll('\n', ' '));
  }
  if (process.env.JOURNEY_ONLY === '1') {
    const proof = await invitationJourney({ mf, adminUrl: admin, ingress, github: world, http, storage, db, eventually, id });
    await capabilitySecrecy({ ...proof, http, admin, db, sessions: await mf.getKVNamespace('SESSIONS','journey-web'), authorityCache: await mf.getKVNamespace('AUTHZ_CACHE','journey-web'), logs: () => {
      assert.equal(runtimeLogOverflow,false,'complete journey runtime log capture exceeded16MiB');
      return [...runtimeLogs, compose('logs', '--no-color', 'restate-smoke')];
    } });
    assert.equal(unexpectedOutbound, 0);
  } else if (process.env.EMAIL_DELIVERY_ONLY === '1') {
    for(const name of ['BulkInviteBatch','EmailInviteDelivery','InviteBudget'])assert.ok(manifest.services.some(s=>s.name===name));
    await emailDelivery({mf,ingress,admin,http,id,db,eventually,github:world,logs:()=>{
      assert.equal(runtimeLogOverflow,false,'complete email runtime log capture exceeded16MiB');
      return [...runtimeLogs,compose('logs','--no-color','restate-smoke')];
    }});
    assert.equal(unexpectedOutbound,0);
  } else if (process.env.CANCELLATION_ONLY === '1') {
    assert.ok(manifest.services.some(service=>service.name==='InviteBudget'),'explicit composition binds the real budget owner');
    await cancellation({ingress,github:world,http,db,id,creation,eventually,setClock:value=>{clock=value;}});
    assert.equal(unexpectedOutbound,0);
  } else if (process.env.DELIVERY_CONTROL_ONLY === '1') {
    await deliveryControl({ingress,github:world,http,db,id,creation,eventually,storage});
    assert.equal(unexpectedOutbound,0);
  } else if (process.env.SWEEP_ONLY === '1') {
    await sweepSchedule({ingress,github:world,http,storage,db,id,creation,eventually,pause:value=>{pauseWorkflows=value;},setClock:value=>{sweepClock=value;}});
    assert.equal(unexpectedOutbound,0);
  } else if (process.env.APPROVED_DELIVERY_ONLY === '1') {
    await approvedDelivery({ ingress, github: world, http, db, creation, id, eventually });
    assert.equal(unexpectedOutbound, 0);
  } else if (process.env.DEADLINE_ONLY === '1') {
    for (const user of [7, 91, 92]) await db.prepare("INSERT INTO users(user_id,login,avatar_url,last_seen_at) VALUES (?, ?, NULL, '2026-01-01T00:00:00Z')").bind(user, `user-${user}`).run();
    await http(`${ingress}/Installation/repo:1/onboard`, setupCommand({ app:'repo', installation_id: 1, actor_user_id: 7, account_id: 100,
      account_login: 'acme', account_type: 'Organization', selected_repos: 'all', installed_at: '2026-01-01T00:00:00Z' }));
    await deadlineRecovery({ ingress, github: world, http, storage, id, creation, eventually,
      pause: value => { pauseWorkflows = value; } });
    assert.equal(unexpectedOutbound, 0);
  } else if (process.env.PAUSE_ONLY === '1') {
    await linkPause({ingress,github:world,http,db,id,creation,eventually});
    assert.equal(unexpectedOutbound,0);
  } else if (process.env.ORG_WEBHOOK_ONLY === '1') {
    const { organizationWebhooks } = await import('./organization-webhooks.mjs');
    await organizationWebhooks({mf,ingress,http,id,db,eventually,github:world});
    assert.equal(unexpectedOutbound,0);
  } else if (process.env.REISSUE_ONLY === '1') {
    const { reissue } = await import('./reissue.mjs');
    assert.ok(manifest.services.some(service=>service.name==='AccessGrant'));
    assert.ok(manifest.services.some(service=>service.name==='InviteBudget'));
    await reissue({ingress,github:world,http,db,id,creation,eventually});
    assert.equal(unexpectedOutbound,0);
  } else if (process.env.ACCESS_REMOVAL_ONLY === '1') {
    const { accessRemoval } = await import('./access-removal.mjs');
    assert.ok(manifest.services.some(service=>service.name==='AccessGrant'));
    await accessRemoval({ingress,http,id,db,eventually,github:world});
    assert.equal(unexpectedOutbound,0);
  } else if (process.env.ORG_CANCELLATION_ONLY === '1') {
    assert.ok(manifest.services.some(service=>service.name==='OrganizationDelivery'));
    assert.ok(manifest.services.some(service=>service.name==='InviteBudget'));
    await organizationCancellation({ingress,http,id,db,eventually,github:world,setClock:value=>{clock=value;}});
    assert.equal(unexpectedOutbound,0);
  } else if (process.env.ORG_DELIVERY_ONLY === '1') {
    assert.ok(manifest.services.some(service=>service.name==='OrganizationDelivery'));
    assert.ok(manifest.services.some(service=>service.name==='InviteBudget'));
    await organizationDelivery({ingress,http,id,db,eventually,github:world});
    assert.equal(unexpectedOutbound,0);
  } else if (process.env.ORG_OBSERVATION_ONLY === '1' || process.env.BUDGET_CAP_ONLY === '1') {
    await orgObservation({ingress,http,id,github:world,budget:process.env.BUDGET_CAP_ONLY === '1' ? {db,eventually} : null});
    assert.equal(unexpectedOutbound,0);
  } else if (process.env.INSTALLATION_ONLY === '1') {
    for (const user of [7, 91, 92]) await db.prepare("INSERT INTO users(user_id,login,avatar_url,last_seen_at) VALUES (?, ?, NULL, '2026-01-01T00:00:00Z')").bind(user, `user-${user}`).run();
    await installationRecovery({ ingress, http, storage, id, creation, eventually,
      github: world,
      loseAuditAck: account => {
        auditAckAccount = account == null ? null : `repo:${account}`; lostAuditAcks = 0; lostAuditInvocation = undefined;
        return { count: async () => lostAuditAcks, invocation: () => lostAuditInvocation,
          release: () => { auditAckAccount = null; } };
      },
    });
    assert.equal(unexpectedOutbound, 0);
  } else {
   await http(`${ingress}/Installation/repo:1/onboard`, setupCommand({ app:'repo', installation_id: 1, actor_user_id: 7, account_id: 100,
     account_login: 'acme', account_type: 'Organization', selected_repos: 'all', installed_at: '2026-01-01T00:00:00Z' }));
   await establishInstallingOwner({http,ingress});
   // Establish retained installation facts before exercising projection failure.
  // Missing user parents keep link/request projection unavailable.
  const input = creation();
  const command = (handler, body) => http(`${ingress}/InvitationLink/${input.link_id}/${handler}`, body);
  const acknowledgment = await command('create', input);
  assert.deepEqual(acknowledgment, { Ok: { link_id: input.link_id, secret_generation: 1 } });
  const created = await command('link_status', { link_id: input.link_id, admin: input.admin });
  assert.equal(created.uses, 0);
  assert.ok(Math.abs(Date.parse(created.created_at) - Date.now()) < 30_000, 'actual Worker clock');
  console.log('PASS actual Restate → Worker create with real clock and durable dispatch');
  const attempts = [91, 92].map(requester_id => ({ ...admitCapability(input.link_id), link_id: input.link_id, operation_id: id(), requester_id }));
  const receipts = await Promise.all(attempts.map(attempt => command('admit', attempt)));
  assert.equal(receipts.filter(receipt => receipt.result.kind === 'accepted').length, 1);
  assert.equal(receipts.filter(receipt => receipt.result.reason === 'not_available').length, 1);
  const winner = receipts.findIndex(receipt => receipt.result.kind === 'accepted');
  const receipt = receipts[winner];
  assert.equal(Date.parse(receipt.result.decision_deadline) - Date.parse(receipt.decided_at), 604800000);
  const revocation = revokeCommand(input, id());
  const revoked = await command('revoke', revocation);
  assert.ok(revoked.Ok);
  assert.deepEqual(await command('revoke', revocation), revoked);
  const revokeConflict = await fetch(`${ingress}/InvitationLink/${input.link_id}/revoke`, {
    method: 'POST', headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ ...revocation, cascade: { ...revocation.cascade, decline_pending_requests: true } }),
    signal: AbortSignal.timeout(25_000),
  });
  assert.equal(revokeConflict.status, 409);
  await revokeConflict.arrayBuffer();
  assert.deepEqual(await command('revoke', revokeCommand(input, id())), { Err: 'revoked' });
  assert.deepEqual(await command('admit', attempts[winner]), receipt);
  const conflict = await fetch(`${ingress}/InvitationLink/${input.link_id}/admit`, {
    method: 'POST', headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ ...attempts[winner], justification: 'changed' }), signal: AbortSignal.timeout(25_000),
  });
  assert.equal(conflict.status, 409);
  assert.equal((await command('link_status', { link_id: input.link_id, admin: input.admin })).uses, 1);
  console.log('PASS final-use concurrency, seven-day deadline, revoke/replay/conflict while D1 unavailable');
   await browserAdmission(world, ingress, created.link_id, attempts[winner].requester_id, attempts[winner].operation_id);
  for (const user of [7, 91, 92]) await db.prepare("INSERT INTO users(user_id,login,avatar_url,last_seen_at) VALUES (?, ?, NULL, '2026-01-01T00:00:00Z')").bind(user, `user-${user}`).run();
  console.log('PASS compatible D1 migrations and restored identity parents');
  await eventually(() => storage('link', input.link_id), link => link?.uses_count === 1 && link.revoked_at);
  await eventually(() => storage('request', receipt.result.request_id), request => request?.state === 'pending');
  await eventually(() => storage('audit', 100), page => page.events.filter(event =>
    [input.link_id, receipt.result.request_id].includes(event.target_id)).length === 4);
  console.log('PASS real asynchronous D1 adapter converges after missing parent recovery');
  const projection = creation();
   const snapshot = { link_id: projection.link_id, creation: projection, secret: { secret_generation:1, sha256:projection.secret_sha256 },
    created_at: '2026-01-01T00:00:00Z', uses: 0, revision: 1, revoked_at: null, revoked_by: null };
  const event = { event_id: `fixture/${projection.link_id}`, kind: 'invitation_link.created', actor_id: 7,
    target_id: projection.link_id, effective_at: snapshot.created_at, evaluated_at: snapshot.created_at };
   const old = { transition_id: 'fixture/old', link: snapshot, events: [event] };
  const newer = { ...old, transition_id: 'fixture/new', events: [], link: { ...snapshot, revision: 2,
    revoked_at: '2026-01-02T00:00:00Z', revoked_by: 7 } };
  await storage('apply', newer);
  await storage('apply', old);
  await storage('apply', old);
  assert.equal((await storage('link', projection.link_id)).revoked_at, '2026-01-02T00:00:00Z');
  const audit = await storage('audit', 100);
  assert.equal(audit.events.filter(row => row.target_id === projection.link_id).length, 1);
  await storage('apply', { ...newer, events: [{ ...event, event_id: 'must-rollback' }],
     link: { ...newer.link, created_at: '2026-01-01T00:00:01Z' } }, 409);
  assert.equal((await storage('link', projection.link_id)).uses_count, 0);
  assert.deepEqual(await storage('audit', 100), audit, 'conflicting batch must roll back events too');
  await storage('apply', { ...old, events: [{ ...event, actor_id: 91 }] }, 409);
  const third = { ...newer, link: { ...newer.link, revision: 3, metadata: { description: 'Updated', internal_note: null } } };
  await storage('apply', third, 409, { 'x-test-lose-batch-ack': '1' });
  assert.equal((await storage('link', projection.link_id)).description, 'Updated');
  await storage('apply', third);
  assert.deepEqual(await storage('audit', 100), audit);
  console.log('PASS D1 fixed-batch reorder/duplicate, stale missing audit, equal-version/event conflict rollback, commit-ack loss replay');
  clock = Date.parse('2026-01-01T00:00:00Z');
  const recoveryInput = { ...creation(), max_uses: 3, expires_at: '2026-01-02T00:00:00Z' };
  const recovery = (handler, body) => http(`${ingress}/InvitationLink/${recoveryInput.link_id}/${handler}`, body);
  await recovery('create', recoveryInput);
  const attempt = { ...admitCapability(recoveryInput.link_id), link_id: recoveryInput.link_id, operation_id: id(), requester_id: 91 };
  interruption = { operation: attempt.operation_id };
  const pending = recovery('admit', attempt);
  await eventually(async () => interruption.blocked, Boolean);
  let statusCompleted = false;
  const status = recovery('link_status', { link_id: recoveryInput.link_id, admin: recoveryInput.admin }).then(value => { statusCompleted = true; return value; });
  await new Promise(resolve => setTimeout(resolve, 250));
  assert.equal(statusCompleted, false, 'exclusive status cannot observe partial writes');
  clock = Date.parse('2026-01-03T00:00:00Z');
  interruption = undefined;
  const recovered = await pending;
  assert.equal(recovered.decided_at, '2026-01-01T00:00:00Z');
  assert.equal(recovered.result.kind, 'accepted', 'journaled complete decision survives link expiration');
  assert.equal((await status).uses, 1);
  assert.deepEqual(await recovery('admit', attempt), recovered);
  clock = undefined;
  console.log('PASS response loss after first state write; exclusive status waits and complete decision survives expiration');
  clock = Date.parse('2026-01-01T00:00:00Z');
  const undecidedInput = { ...creation(), expires_at: '2026-01-02T00:00:00Z' };
  await http(`${ingress}/InvitationLink/${undecidedInput.link_id}/create`, undecidedInput);
  const undecidedAttempt = { ...admitCapability(undecidedInput.link_id), link_id: undecidedInput.link_id, operation_id: id(), requester_id: 91 };
  interruption = { operation: undecidedAttempt.operation_id, type: 0x0402 };
  const undecided = http(`${ingress}/InvitationLink/${undecidedInput.link_id}/admit`, undecidedAttempt);
  await eventually(async () => interruption.blocked, Boolean);
  clock = Date.parse('2026-01-03T00:00:00Z');
  interruption = undefined;
  const rejected = await undecided;
  assert.equal(rejected.decided_at, '2026-01-03T00:00:00Z');
  assert.equal(rejected.result.reason, 'not_available');
  console.log('PASS undecided admission samples later clock after lazy-read interruption');
  // A clock sample belongs to the complete journaled decision, not a separate
  // timestamp read before suspension. Move time after the recorded decision.
  // Anchor this timeline at the real clock. The SDK schedules `send_after` as
  // the Worker's (pinned) Date.now() plus the delay, so a fictional past
  // deadline would fire tick_expire immediately on the real server and, on
  // Restate >= 1.8 (VQueues), starve the decide call queued behind it. Every
  // deadline here stays in the real future, so lazy expiry does the work.
  pauseWorkflows = true;
  const timedBase = Math.floor(Date.now() / 60000) * 60000;
  clock = timedBase;
  const timedInput = { ...creation(), max_uses: 5 };
  const timed = (handler, body) => http(`${ingress}/InvitationLink/${timedInput.link_id}/${handler}`, body);
  const timedLink = await timed('create', timedInput);
  const timedAttempt = { ...admitCapability(timedInput.link_id), link_id: timedInput.link_id, operation_id: id(), requester_id: 91 };
  const first = await timed('admit', timedAttempt);
  assert.equal(Date.parse(first.decided_at), timedBase);
  assert.equal(Date.parse(first.result.decision_deadline), timedBase + 604800000);
  clock = Date.parse(first.result.decision_deadline);
  const late = await http(`${ingress}/InvitationRequest/${first.result.request_id}/decide`, { link_id: timedInput.link_id, request_id: first.result.request_id,
    operation_id: id(), admin: timedInput.admin, action: { kind: 'approve' } });
  assert.equal(late.request.state, 'expired', 'deadline equality expires');
  const second = await timed('admit', { ...timedAttempt, operation_id: id() });
  assert.equal(second.result.kind, 'accepted');
  clock = Date.parse(second.result.decision_deadline) + 1;
  const thirdReceipt = await timed('admit', { ...timedAttempt, operation_id: id() });
  assert.equal(thirdReceipt.result.kind, 'accepted');
  assert.equal((await http(`${ingress}/InvitationRequest/${second.result.request_id}/request_status`, { link_id: timedInput.link_id, request_id: second.result.request_id, requester_id: 91 })).state, 'expired');
  assert.equal((await timed('link_status', { link_id: timedInput.link_id, admin: timedInput.admin })).uses, 3);
  assert.deepEqual(await timed('admit', timedAttempt), first);
  // Notify before startup; late startup must consume authority, never re-decide.
  const declined = await http(`${ingress}/InvitationRequest/${thirdReceipt.result.request_id}/decide`, { link_id: timedInput.link_id, request_id: thirdReceipt.result.request_id,
    operation_id: id(), admin: timedInput.admin, action: { kind: 'decline', reason: 'fixture' } });
  assert.equal(declined.request.state, 'declined');
  clock = undefined;
  pauseWorkflows = false;
   const completed = await http(`${ingress}/InvitationRequest/${thirdReceipt.result.request_id}/request_status`, {link_id: timedInput.link_id, request_id: thirdReceipt.result.request_id, requester_id: 91});
  assert.equal(completed.state, 'declined');
   console.log('PASS exact deadline equality, overdue readmission without refund, request-owned terminal decision');
  pauseWorkflows = true;
  const timerInput = creation();
  const timerCall = (handler, body) => http(`${ingress}/InvitationLink/${timerInput.link_id}/${handler}`, body);
  await timerCall('create', timerInput);
  clock = Date.now() - 604800000 + 5000;
  const timerReceipt = await timerCall('admit', { ...admitCapability(timerInput.link_id), link_id: timerInput.link_id, operation_id: id(), requester_id: 92 });
  clock = undefined;
  pauseWorkflows = false;
  assert.ok(Date.now() < Date.parse(timerReceipt.result.decision_deadline), 'timer scheduled before deadline');
  const waiting = await http(`${ingress}/InvitationRequest/${timerReceipt.result.request_id}/request_status`, { link_id: timerInput.link_id, request_id: timerReceipt.result.request_id, requester_id: 92 });
  assert.equal(waiting.state, 'pending');
   const timerResult = await eventually(() => db.prepare('SELECT state FROM invitation_requests WHERE id=?').bind(timerReceipt.result.request_id).first(), row => row?.state === 'expired');
  assert.equal(timerResult.state, 'expired');
  assert.ok(Date.now() >= Date.parse(timerReceipt.result.decision_deadline));
  console.log('PASS actual Worker durable timer expiry after short historical admission window');

  const history = creation();
  await http(`${ingress}/InvitationLink/${history.link_id}/create`, history);
  await http(`${ingress}/InvitationLink/${history.link_id}/revoke`, revokeCommand(history, id()));
  const measureAttempt = async () => {
    const start = traffic.length;
    await http(`${ingress}/InvitationLink/${history.link_id}/admit`, {
      ...admitCapability(history.link_id), link_id: history.link_id, operation_id: id(), requester_id: 91, justification: 'x'.repeat(500),
    });
    const calls = traffic.slice(start).filter(row => row.path.endsWith('/InvitationLink/admit'));
    return { roundTrips: calls.length, maxRequest: Math.max(...calls.map(row => row.input)), maxResponse: Math.max(...calls.map(row => row.output)) };
  };
  const before = await measureAttempt();
  for (let n = 0; n < 12; n++) await measureAttempt();
  const after = await measureAttempt();
  assert.equal(after.roundTrips, before.roundTrips);
  assert.ok(after.maxRequest <= before.maxRequest + 1024, 'unrelated retained history is not eagerly transferred');
  const observed = traffic.flatMap(row => row.frames);
  assert.ok(observed.some(frame => frame.type === 0x0402), 'actual lazy reads');
  assert.ok(observed.some(frame => frame.type === 0x0001), 'actual request-response suspension');
  const bounds = { before, after, maxStateWrite: Math.max(...observed.filter(f => f.type === 0x0403).map(f => f.bytes)),
    maxMessage: Math.max(...observed.map(f => f.bytes)) };
  console.log(`PASS lazy history bounds (bytes): ${JSON.stringify(bounds)}`);
  const autoInput = { ...creation(), approval_required: false };
  const auto = (handler, body) => http(`${ingress}/InvitationLink/${autoInput.link_id}/${handler}`, body);
  await auto('create', autoInput);
  const autoAttempt = { ...admitCapability(autoInput.link_id), link_id: autoInput.link_id, operation_id: id(), requester_id: 91 };
  const approved = await auto('admit', autoAttempt);
  assert.equal(approved.result.state, 'approved');
  assert.equal(approved.result.decision_deadline, null);
  const progressQuery = { link_id: autoInput.link_id, request_id: approved.result.request_id, requester_id: 91 };
  const progress = await eventually(() => http(`${ingress}/InvitationRequest/${approved.result.request_id}/delivery_progress`, progressQuery), rows => rows.some(row => row.stage === 'submitted'));
  const plan = await http(`${ingress}/InvitationRequest/${approved.result.request_id}/approved_plan`, progressQuery);
  const delivery = (command, handler, body) => http(`${ingress}/RepositoryDelivery/${command.request_id}:${command.repo_id}:${command.delivery_generation}/${handler}`, body);
  const receiving = (await eventually(() => delivery(plan.commands[0], 'status'), value => value?.create.outcome.kind === 'created')).create;
  assert.ok(receiving.outcome.upstream_id > 0);
  await delivery(receiving.command, 'create', receiving.command);
  const createEvents = async receipt => (await auditHistory(storage, 100)).filter(event => event.target_id === `${receipt.command.request_id}:${receipt.command.repo_id}:${receipt.command.delivery_generation}`);
  const firstEvents = await createEvents(receiving);
  assert.equal(firstEvents.length, 1);
  assert.equal(firstEvents[0].event_type, 'invitation.sent');
  assert.equal(firstEvents[0].occurred_at, receiving.confirmed_at);
  assert.equal(firstEvents[0].actor_kind, 'system');
  await delivery(receiving.command, 'create', receiving.command);
  assert.deepEqual(await createEvents(receiving), firstEvents);
  // D1 batch rolls back receipt/lifecycle if the audit insertion fails. Retry
  // after repair (and replay after an unobserved successful response) is safe.
  // Give fault-injected audit generations their own approved target. Otherwise
  // these retained rows intentionally make the later bounded webhook lookup
  // ambiguous and can crowd its original delivery out of LIMIT 2.
  const auditInput = { ...creation(), approval_required: false, targets: { organization: null, teams: [], repos: [{ repo_id: 11, repo_full_name: 'acme/repo-11', grant: 'read' }] } };
  await http(`${ingress}/InvitationLink/${auditInput.link_id}/create`, auditInput);
  const auditAdmission = await http(`${ingress}/InvitationLink/${auditInput.link_id}/admit`, { ...admitCapability(auditInput.link_id), link_id: auditInput.link_id, operation_id: id(), requester_id: 91 });
  const auditPlan = await http(`${ingress}/InvitationRequest/${auditAdmission.result.request_id}/approved_plan`, { link_id: auditInput.link_id, request_id: auditAdmission.result.request_id, requester_id: 91 });
  await eventually(() => storage('request', auditAdmission.result.request_id), Boolean);
  let projectedGeneration = auditPlan.commands[0].delivery_generation;
  for (const [outcome, eventType, actor] of [
    [{ kind: 'created', upstream_id: 9876 }, 'invitation.sent', 'system'],
    [{ kind: 'already_satisfied', effective_role: 'read' }, 'delivery.already_satisfied', 'github'],
    [{ kind: 'failed', reason: { kind: 'rejected', status: 422 } }, 'invitation.send_failed', 'system'],
  ]) {
    const receipt = { ...receiving, command: { ...auditPlan.commands[0], delivery_generation: ++projectedGeneration }, outcome };
    await db.prepare("CREATE TRIGGER fail_delivery_audit BEFORE INSERT ON audit_events WHEN NEW.target_kind='repository_delivery' BEGIN SELECT RAISE(ABORT, 'fixture audit unavailable'); END").run();
    const snapshot = { create: receipt, revision: receipt.revision, settlement: null };
    await storage('delivery', snapshot, 409);
    assert.ok(!(await storage('delivery-read', receipt.command.request_id)).some(row => row.create.command.delivery_generation === receipt.command.delivery_generation && row.create.command.repo_id === receipt.command.repo_id));
    assert.deepEqual(await createEvents(receipt), []);
    await db.prepare('DROP TRIGGER fail_delivery_audit').run();
    await storage('delivery', snapshot);
    const events = await createEvents(receipt);
    assert.equal(events.length, 1);
    assert.equal(events[0].event_type, eventType);
    assert.equal(events[0].actor_kind, actor);
    assert.equal(events[0].occurred_at, receipt.confirmed_at);
    await storage('delivery', snapshot);
    assert.deepEqual(await createEvents(receipt), events);
  }
  console.log('PASS actual D1 audit-write failure rollback and lost-ack replay for all three confirmed outcomes');
  const apiPuts = async () => (await world.requests('PUT')).filter(call => call.path.startsWith('/repos/acme/api/')).length;
  assert.equal(await apiPuts(), 1);
  assert.deepEqual(await auto('admit', autoAttempt), approved);
  assert.deepEqual(await http(`${ingress}/InvitationRequest/${approved.result.request_id}/delivery_progress`, progressQuery), progress);
  assert.equal(await apiPuts(), 1);
  assert.equal(unexpectedOutbound, 0);
  console.log('PASS auto-approval, actual Worker GitHub (octoemu) delivery and confirmed-create replay');
  for (const [requesterId, stubOutcome, kind, eventType, actor] of [
    // A read collaborator asked for write: GitHub changes the role (204).
    [92, 'already_collaborator', 'upgraded', 'delivery.upgraded', 'system'],
    [93, 'terminal_failure', 'failed', 'invitation.send_failed', 'system'],
  ]) {
    await db.prepare("INSERT INTO users(user_id,login,avatar_url,last_seen_at) VALUES (?, ?, NULL, '2026-01-01T00:00:00Z') ON CONFLICT(user_id) DO NOTHING").bind(requesterId, `user-${requesterId}`).run();
    await world.outcome(`user-${requesterId}`, stubOutcome);
    const input = { ...creation(), approval_required: false };
    if (stubOutcome === 'already_collaborator') input.targets.repos[0].grant = 'write';
    const call = (handler, body) => http(`${ingress}/InvitationLink/${input.link_id}/${handler}`, body);
    await call('create', input);
    const admitted = await call('admit', { ...admitCapability(input.link_id), link_id: input.link_id, operation_id: id(), requester_id: requesterId });
    const plan = await http(`${ingress}/InvitationRequest/${admitted.result.request_id}/approved_plan`, { link_id: input.link_id, request_id: admitted.result.request_id, requester_id: requesterId });
    const receipt = await delivery(plan.commands[0], 'create', plan.commands[0]);
    assert.equal(receipt.outcome.kind, kind);
    const events = await createEvents(receipt);
    assert.equal(events.length, 1);
    assert.equal(events[0].event_type, eventType);
    assert.equal(events[0].actor_kind, actor);
    assert.equal(events[0].occurred_at, receipt.confirmed_at);
    assert.deepEqual(await delivery(receipt.command, 'create', receipt.command), receipt);
    assert.deepEqual(await createEvents(receipt), events);
  }
  // Start the next scenarios from a clean GitHub side for these requesters.
  await world.reset('user-91', 'user-92', 'user-93');
  console.log('PASS actual Worker GitHub 204/422 outcomes publish retained D1 audit events');
  const manualInput = creation();
  const manual = (handler, body) => http(`${ingress}/InvitationLink/${manualInput.link_id}/${handler}`, body);
  await manual('create', manualInput);
  const manualReceipt = await manual('admit', { ...admitCapability(manualInput.link_id), link_id: manualInput.link_id, operation_id: id(), requester_id: 91 });
  const manualDecision = await http(`${ingress}/InvitationRequest/${manualReceipt.result.request_id}/decide`, { link_id: manualInput.link_id, request_id: manualReceipt.result.request_id,
    operation_id: id(), admin: manualInput.admin, action: { kind: 'approve' } });
  assert.equal(manualDecision.request.state, 'approved');
   const manualPlan = await http(`${ingress}/InvitationRequest/${manualReceipt.result.request_id}/approved_plan`, {link_id: manualInput.link_id, request_id: manualReceipt.result.request_id, requester_id:91});
   await eventually(() => delivery(manualPlan.commands[0], 'status'), value => value?.create.outcome.kind === 'created');
   console.log('PASS manual approval and request-owned Worker dispatch');
  await settlement({ admin, ingress, github: world, http, storage, db, id, creation, eventually,
    requestId: approved.result.request_id, initialCommand: plan.commands[0],
    pause: value => { pauseWorkflows = value; } });
  if (process.env.SETTLEMENT_ONLY !== '1') {
  await deadlineRecovery({ ingress, github: world, http, storage, id, creation, eventually,
    pause: value => { pauseWorkflows = value; } });
   await browserAdmission(world, ingress, created.link_id, attempts[winner].requester_id, attempts[winner].operation_id, { creation, http });
   // Fresh onboarding and refresh require no installation read projection.
   // The App is installed on `adopted` (300) as 40 (github-world.mjs).
   // Installation reads fail on the real binding while GitHub stays reachable.
   await db.prepare('ALTER TABLE installations RENAME TO installations_offline').run();
   await http(`${ingress}/Installation/repo:40/onboard`, setupCommand({ app:'repo', installation_id: 40, actor_user_id: 7, account_id: 300,
     account_login: 'adopted', account_type: 'Organization', selected_repos: 'all', installed_at: '2026-01-01T00:00:00Z' }));
  const reposChanged = { app:'repo', installation_id: 40, selected_repos: 'all' };
  const acknowledged = await http(`${ingress}/Installation/repo:40/repos_changed/send`, reposChanged);
  assert.ok(acknowledged.invocationId, 'repository webhook is acknowledged by its durable send');
  // The command completes rather than holding exclusivity, and a duplicate
  // event joins the retained continuation instead of competing with it.
  await http(`${ingress}/Installation/repo:40/repos_changed`, reposChanged);
  await http(`${ingress}/AccountInstallation/repo:300/refresh`, 41);
  const held = Date.now();
  const offline = await fetch(`${ingress}/AccountInstallation/repo:300/status`, { method: 'POST', signal: AbortSignal.timeout(25_000) });
   assert.equal(offline.status, 200, 'retained context remains readable without SQL');
  assert.ok(Date.now() - held < 15_000);
  await db.prepare('ALTER TABLE installations_offline RENAME TO installations').run();
  // Restoration alone converges: no further webhook and no user action.
  await eventually(() => db.prepare('SELECT selected_repos FROM installations WHERE installation_id=40').first(),
    row => row?.selected_repos === '[30,31]');
  const adopted = await http(`${ingress}/AccountInstallation/repo:300/status`);
  assert.equal(adopted.account.installation_id, 40);
  assert.deepEqual(adopted.observation.repo_ids, [30, 31]);
   console.log('PASS onboarding and refresh during D1 projection outage converge without another event');
  }
  }
} catch (error) {
  primaryFailure = error;
  console.error(runtimeLogs.join(''));
  try { console.error(compose('logs', '--no-color', '--tail', '40', 'restate-smoke')); }
  catch (diagnosticError) { console.error('Runtime diagnostics unavailable:', diagnosticError.message); }
  throw error;
} finally {
  try { await cleanup(); }
  catch (error) { if (primaryFailure) console.error(error); else throw error; }
  finally { clearTimeout(deadline); }
}
