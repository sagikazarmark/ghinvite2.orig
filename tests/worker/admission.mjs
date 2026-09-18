import assert from 'node:assert/strict';
import { readFileSync, readdirSync, mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { Miniflare, Response, Log, LogLevel } from 'miniflare';
import { execFileSync, execFile, spawn } from 'node:child_process';
import { once } from 'node:events';
import { createServer } from 'node:http2';
import { randomBytes } from 'node:crypto';
import { browserAdmission } from './browser-admission.mjs';
import { settlement } from './settlement.mjs';
import { frames, fields } from './protocol.mjs';
import { deadlineRecovery, stalledResponse } from './deadline-recovery.mjs';
import { installationRecovery } from './installation.mjs';

const root = fileURLToPath(new URL('../../', import.meta.url));
const metadata = JSON.parse(execFileSync('cargo', ['metadata', '--locked', '--offline', '--no-deps', '--format-version', '1'], { cwd: root, encoding: 'utf8', timeout: 120_000 }));
const project = `ghinvite-worker-${process.pid}-${randomBytes(4).toString('hex')}`;
const compose = (...args) => execFileSync('docker', ['compose', '-f', 'compose.yaml', '--profile', 'smoke', '-p', project, ...args], {
  cwd: root, encoding: 'utf8', timeout: 180_000,
});
const deadline = setTimeout(() => terminate('Worker gate deadline', 1), 300_000);
process.once('SIGINT', () => terminate('SIGINT', 130));
process.once('SIGTERM', () => terminate('SIGTERM', 143));
let server;
let stopping = false;
const sessions = new Set();
const runtimeLogs = [];
const traffic = [];
let interruption;
let clock;
let pauseWorkflows = false;
let github;
let githubUrl;
let unexpectedOutbound = 0;
let migrationDirectory;
const children = new Set();
let cleanupPromise;
let primaryFailure;
let networkFault;
let installationObservation;
let auditAckAccount;
let lostAuditAcks = 0;
let lostAuditInvocation;
let faultPuts = 0;
const stalledCleanup = [];
function fault(phase) {
  if (!phase) { networkFault = null; return; }
  faultPuts = 0;
  let wrote, observed;
  const write = new Promise(resolve => { wrote = resolve; });
  const observation = new Promise(resolve => { observed = resolve; });
  networkFault = { phase, wrote, observed };
  return { write, observation, puts: () => faultPuts };
}
const stopCompose = () => new Promise((resolve, reject) => {
  execFile('docker', ['compose', '-f', 'compose.yaml', '--profile', 'smoke', '-p', project,
    'down', '--volumes', '--remove-orphans', '--timeout', '5'],
    { cwd: root, timeout: 15_000, killSignal: 'SIGKILL' }, (error, stdout, stderr) => {
      if (error) reject(new Error(`Compose cleanup failed for ${project}: ${stderr}`, { cause: error }));
      else resolve();
    });
});
async function cleanup() {
  if (cleanupPromise) return cleanupPromise;
  stopping = true;
  cleanupPromise = (async () => {
    const errors = [];
    const step = async action => { try { await action(); } catch (error) { errors.push(error); } };
    await step(stopCompose);
    for (const release of stalledCleanup) release();
    for (const child of children) await step(async () => {
      if (child.exitCode !== null || child.signalCode !== null) return;
      const exited = once(child, 'exit');
      child.kill('SIGTERM');
      const kill = setTimeout(() => child.kill('SIGKILL'), 2000);
      let timeout;
      try { await Promise.race([exited, new Promise((_, reject) => { timeout = setTimeout(() => reject(new Error('child cleanup timeout')), 5000); })]); }
      finally { clearTimeout(kill); clearTimeout(timeout); }
    });
    await step(async () => {
      if (server) { for (const session of sessions) session.destroy(); await new Promise(resolve => server.close(resolve)); }
    });
    await step(() => mf.dispose());
    await step(async () => { if (migrationDirectory) rmSync(migrationDirectory, { recursive: true, force: true }); });
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
const creation = () => ({ version: 1, link_id: id(), admin: { account_id: 100, user_id: 7 },
  account_id: 100, installation_id: 1, description: 'Workshop', internal_note: 'Private context',
  expires_at: null, max_uses: 1, permission: 'pull', approval_required: true,
  repos: [{ repo_id: 10, repo_full_name: 'acme/api' }] });
const http = async (url, body, method = 'POST') => {
  const response = await fetch(url, { method, headers: { ...(body === undefined ? {} : { 'content-type': 'application/json' }), accept: 'application/json' },
    body: body === undefined ? undefined : JSON.stringify(body), signal: AbortSignal.timeout(25_000) });
  const text = await response.text();
  assert.ok(response.ok, `${method} ${new URL(url).pathname}: ${response.status} ${text}`);
  return text ? JSON.parse(text) : null;
};

const mf = new Miniflare({
  log: new Log(LogLevel.ERROR),
  handleRuntimeStdio(stdout, stderr) {
    // Keep bounded diagnostics, including panic text, without per-suspension noise.
    for (const stream of [stdout, stderr]) stream.on('data', chunk => {
      runtimeLogs.push(chunk.toString()); if (runtimeLogs.length > 50) runtimeLogs.shift();
    });
  },
  modules: true,
  scriptPath: fileURLToPath(new URL('./workflows-worker.mjs', import.meta.url)),
  modulesRules: [{ type: 'CompiledWasm', include: ['**/*.wasm'] }],
  compatibilityDate: '2024-09-23',
  compatibilityFlags: ['nodejs_compat'],
  d1Databases: ['DB', 'DB_DELIVERY'],
  bindings: {
    GHINVITE_ADMISSION_MODE: 'authoritative',
    GHINVITE_GITHUB_APP_ID: '123',
    GHINVITE_GITHUB_APP_PRIVATE_KEY: readFileSync(new URL('../../crates/ghinvite-github/src/jwt_test_key.pem', import.meta.url)).toString('base64'),
  },
  async outboundService(request) {
    if (new URL(request.url).origin !== 'https://api.github.com' || !githubUrl) {
      unexpectedOutbound++;
      return new Response('Outbound network disabled', { status: 502 });
    }
    if (request.method === 'PUT') faultPuts++;
    if (installationObservation) return installationObservation(request);
    if (networkFault && (request.method === 'PUT' || new URL(request.url).pathname === '/app/installations/1')) {
      if (request.method === 'PUT') networkFault.wrote(); else networkFault.observed();
      return stalledResponse(networkFault.phase, stalledCleanup);
    }
    const response = await fetch(githubUrl + new URL(request.url).pathname + new URL(request.url).search, {
      method: request.method, headers: request.headers,
      body: ['GET', 'HEAD'].includes(request.method) ? undefined : Buffer.from(await request.arrayBuffer()),
      signal: AbortSignal.timeout(10_000),
    });
    return new Response([204, 205, 304].includes(response.status) ? null : await response.arrayBuffer(), { status: response.status, headers: response.headers });
  },
});

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
    headers: { accept: 'application/vnd.restate.endpointmanifest.v3+json' },
  });
  assert.equal(response.status, 200, await response.clone().text());
  const manifest = await response.json();
  assert.ok(manifest.services.some(service => service.name === 'InvitationLinkV1'), 'authoritative link endpoint must be deployed');
  assert.ok(manifest.services.some(service => service.name === 'InvitationProjectionV1'), 'D1 projector must be deployed');
  console.log('PASS actual workflows Worker authoritative discovery');
  compose('up', '-d', 'restate-smoke');
  const admin = `http://${compose('port', 'restate-smoke', '9070').trim()}`;
  const ingress = `http://${compose('port', 'restate-smoke', '8080').trim()}`;
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
      if ((interruption?.blocked && interruption.blocked === invocation) || (pauseWorkflows && request.url.endsWith('/InvitationRequestV1/run'))) {
        response.writeHead(503); response.end(); return;
      }
      const result = await mf.dispatchFetch(`http://worker.test${request.url}`, {
        method: request.method, headers: { ...Object.fromEntries(Object.entries(request.headers).filter(([key]) => !key.startsWith(':'))),
          ...(clock === undefined ? {} : { 'x-test-clock': String(clock) }),
          ...(auditAckAccount && objectKey === auditAckAccount && request.url.endsWith('/InstallationProjectionV1/apply')
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
      const cut = interruption && body.includes(Buffer.from(interruption.operation)) && outgoing.find(frame => frame.type === (interruption.type ?? 0x0403));
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
  github = spawn(`${metadata.target_directory}/debug/examples/stub`, ['--port', '0'], { stdio: ['ignore', 'ignore', 'pipe'] });
  children.add(github);
  await new Promise((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error('GitHub stub startup timeout')), 10_000);
    let diagnostics = '';
    github.once('error', error => { clearTimeout(timer); reject(error); });
    github.once('exit', code => { clearTimeout(timer); reject(new Error(`GitHub stub exited ${code}: ${diagnostics}`)); });
    github.stderr.on('data', chunk => {
      diagnostics += chunk;
      const address = diagnostics.match(/github-stub listening on (127\.0\.0\.1:\d+)\s/)?.[1];
      if (address) { githubUrl = `http://${address}`; clearTimeout(timer); resolve(); }
    });
  });
  const db = await mf.getD1Database('DB');
  const deliveryDb = await mf.getD1Database('DB_DELIVERY');
  for (const file of readdirSync(new URL('../../migrations/', import.meta.url)).filter(file => file.endsWith('.sql')).sort()) {
    const sql = readFileSync(new URL(`../../migrations/${file}`, import.meta.url), 'utf8');
    await db.exec(sql.replace(/^--.*$/gm, '').replaceAll('\n', ' '));
    await deliveryDb.exec(sql.replace(/^--.*$/gm, '').replaceAll('\n', ' '));
  }
  if (process.env.INSTALLATION_ONLY === '1') {
    for (const user of [7, 91, 92]) await db.prepare("INSERT INTO users VALUES (?, ?, NULL, '2026-01-01T00:00:00Z')").bind(user, `user-${user}`).run();
    await installationRecovery({ ingress, http, storage, id, creation, eventually,
      observe: value => { installationObservation = value; },
      loseAuditAck: account => {
        auditAckAccount = account; lostAuditAcks = 0; lostAuditInvocation = undefined;
        return { count: async () => lostAuditAcks, invocation: () => lostAuditInvocation,
          release: () => { auditAckAccount = null; } };
      },
    });
    assert.equal(unexpectedOutbound, 0);
  } else {
  await storage('delivery-suite', null);
  console.log('PASS shared SQLx/D1 confirmed-create audit conformance: all outcomes, replay, ordering and immutable content');
  await db.prepare("INSERT INTO installations VALUES (1,100,'acme','Organization','2026-01-01T00:00:00Z',NULL,'[10,11]')").run();
  // Adopt existing installation facts before exercising projection failure.
  // Missing user parents keep link/request projection unavailable.
  const input = creation();
  const command = (handler, body) => http(`${ingress}/InvitationLinkV1/${input.link_id}/${handler}`, body);
  const created = await command('create', input);
  assert.equal(created.uses, 0);
  assert.ok(Math.abs(Date.parse(created.created_at) - Date.now()) < 30_000, 'actual Worker clock');
  console.log('PASS actual Restate → Worker create with real clock and durable dispatch');
  const attempts = [91, 92].map(requester_id => ({ version: 1, link_id: input.link_id, operation_id: id(), requester_id }));
  const receipts = await Promise.all(attempts.map(attempt => command('admit', attempt)));
  assert.equal(receipts.filter(receipt => receipt.result.kind === 'accepted').length, 1);
  assert.equal(receipts.filter(receipt => receipt.result.reason === 'exhausted').length, 1);
  const winner = receipts.findIndex(receipt => receipt.result.kind === 'accepted');
  const receipt = receipts[winner];
  assert.equal(Date.parse(receipt.result.decision_deadline) - Date.parse(receipt.decided_at), 604800000);
  await command('revoke', { link_id: input.link_id, admin: input.admin });
  assert.deepEqual(await command('admit', attempts[winner]), receipt);
  const conflict = await fetch(`${ingress}/InvitationLinkV1/${input.link_id}/admit`, {
    method: 'POST', headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ ...attempts[winner], justification: 'changed' }), signal: AbortSignal.timeout(25_000),
  });
  assert.equal(conflict.status, 409);
  assert.equal((await command('link_status', { link_id: input.link_id, admin: input.admin })).uses, 1);
  console.log('PASS final-use concurrency, seven-day deadline, revoke/replay/conflict while D1 unavailable');
  await browserAdmission(ingress, created.invitation_code, attempts[winner].requester_id, attempts[winner].operation_id);
  for (const user of [7, 91, 92]) await db.prepare("INSERT INTO users VALUES (?, ?, NULL, '2026-01-01T00:00:00Z')").bind(user, `user-${user}`).run();
  console.log('PASS compatible D1 migrations and restored identity parents');
  await eventually(() => storage('link', input.link_id), link => link?.uses_count === 1 && link.revoked_at);
  await eventually(() => storage('request', receipt.result.request_id), request => request?.state === 'pending');
  await eventually(() => storage('audit', 100), page => page.events.length === 4);
  console.log('PASS real asynchronous D1 adapter converges after missing parent recovery');
  const projection = creation();
  const snapshot = { link_id: projection.link_id, creation: projection, invitation_code: 'projection123456',
    created_at: '2026-01-01T00:00:00Z', uses: 0, revision: 1, revoked_at: null, revoked_by: null };
  const event = { event_id: `fixture/${projection.link_id}`, kind: 'invitation_link.created', actor_id: 7,
    target_id: projection.link_id, effective_at: snapshot.created_at, evaluated_at: snapshot.created_at };
  const old = { version: 1, transition_id: 'fixture/old', link: snapshot, requests: [], events: [event] };
  const newer = { ...old, transition_id: 'fixture/new', events: [], link: { ...snapshot, revision: 2,
    revoked_at: '2026-01-02T00:00:00Z', revoked_by: 7 } };
  await storage('apply', newer);
  await storage('apply', old);
  await storage('apply', old);
  assert.equal((await storage('link', projection.link_id)).revoked_at, '2026-01-02T00:00:00Z');
  const audit = await storage('audit', 100);
  assert.equal(audit.events.filter(row => row.target_id === projection.link_id).length, 1);
  await storage('apply', { ...newer, events: [{ ...event, event_id: 'must-rollback' }],
    link: { ...newer.link, uses: 1 } }, 409);
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
  const recovery = (handler, body) => http(`${ingress}/InvitationLinkV1/${recoveryInput.link_id}/${handler}`, body);
  await recovery('create', recoveryInput);
  const attempt = { version: 1, link_id: recoveryInput.link_id, operation_id: id(), requester_id: 91 };
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
  await http(`${ingress}/InvitationLinkV1/${undecidedInput.link_id}/create`, undecidedInput);
  const undecidedAttempt = { version: 1, link_id: undecidedInput.link_id, operation_id: id(), requester_id: 91 };
  interruption = { operation: undecidedAttempt.operation_id, type: 0x0402 };
  const undecided = http(`${ingress}/InvitationLinkV1/${undecidedInput.link_id}/admit`, undecidedAttempt);
  await eventually(async () => interruption.blocked, Boolean);
  clock = Date.parse('2026-01-03T00:00:00Z');
  interruption = undefined;
  const rejected = await undecided;
  assert.equal(rejected.decided_at, '2026-01-03T00:00:00Z');
  assert.equal(rejected.result.reason, 'expired');
  console.log('PASS undecided admission samples later clock after lazy-read interruption');
  // A clock sample belongs to the complete journaled decision, not a separate
  // timestamp read before suspension. Move time after the recorded decision.
  pauseWorkflows = true;
  clock = Date.parse('2026-01-01T00:00:00Z');
  const timedInput = { ...creation(), max_uses: 5 };
  const timed = (handler, body) => http(`${ingress}/InvitationLinkV1/${timedInput.link_id}/${handler}`, body);
  const timedLink = await timed('create', timedInput);
  const timedAttempt = { version: 1, link_id: timedInput.link_id, operation_id: id(), requester_id: 91 };
  const first = await timed('admit', timedAttempt);
  assert.equal(first.decided_at, '2026-01-01T00:00:00Z');
  assert.equal(first.result.decision_deadline, '2026-01-08T00:00:00Z');
  clock = Date.parse(first.result.decision_deadline);
  const late = await timed('decide', { version: 1, link_id: timedInput.link_id, request_id: first.result.request_id,
    operation_id: id(), admin: timedInput.admin, action: { kind: 'approve' } });
  assert.equal(late.request.state, 'expired', 'deadline equality expires');
  const second = await timed('admit', { ...timedAttempt, operation_id: id() });
  assert.equal(second.result.kind, 'accepted');
  clock = Date.parse(second.result.decision_deadline) + 1;
  const thirdReceipt = await timed('admit', { ...timedAttempt, operation_id: id() });
  assert.equal(thirdReceipt.result.kind, 'accepted');
  assert.equal((await timed('request_status', { link_id: timedInput.link_id, request_id: second.result.request_id, requester_id: 91 })).state, 'expired');
  assert.equal((await timed('link_status', { link_id: timedInput.link_id, admin: timedInput.admin })).uses, 3);
  assert.deepEqual(await timed('admit', timedAttempt), first);
  // Notify before startup; late startup must consume authority, never re-decide.
  const declined = await timed('decide', { version: 1, link_id: timedInput.link_id, request_id: thirdReceipt.result.request_id,
    operation_id: id(), admin: timedInput.admin, action: { kind: 'decline', reason: 'fixture' } });
  assert.equal(declined.request.state, 'declined');
  await eventually(() => http(`${ingress}/InvitationRequestV1/${thirdReceipt.result.request_id}/notification_status`), Boolean);
  clock = undefined;
  pauseWorkflows = false;
  const completed = await http(`${ingress}/restate/workflow/InvitationRequestV1/${thirdReceipt.result.request_id}/attach`, undefined, 'GET');
  assert.equal(completed.state, 'declined');
  console.log('PASS exact deadline equality, overdue readmission without refund, direct notification before durable startup');
  pauseWorkflows = true;
  const timerInput = creation();
  const timerCall = (handler, body) => http(`${ingress}/InvitationLinkV1/${timerInput.link_id}/${handler}`, body);
  await timerCall('create', timerInput);
  clock = Date.now() - 604800000 + 5000;
  const timerReceipt = await timerCall('admit', { version: 1, link_id: timerInput.link_id, operation_id: id(), requester_id: 92 });
  clock = undefined;
  pauseWorkflows = false;
  await eventually(async () => traffic.filter(row => row.objectKey === timerReceipt.result.request_id), rows =>
    rows.some(row => row.frames.some(frame => frame.type === 0x040C)) && rows.some(row => row.frames.some(frame => frame.type === 0x0001)));
  assert.ok(Date.now() < Date.parse(timerReceipt.result.decision_deadline), 'timer scheduled before deadline');
  const waiting = await timerCall('request_status', { link_id: timerInput.link_id, request_id: timerReceipt.result.request_id, requester_id: 92 });
  assert.equal(waiting.state, 'pending');
  const timerResult = await http(`${ingress}/restate/workflow/InvitationRequestV1/${timerReceipt.result.request_id}/attach`, undefined, 'GET');
  assert.equal(timerResult.state, 'expired');
  assert.ok(Date.now() >= Date.parse(timerReceipt.result.decision_deadline));
  console.log('PASS actual Worker durable timer expiry after short historical admission window');

  const history = creation();
  await http(`${ingress}/InvitationLinkV1/${history.link_id}/create`, history);
  await http(`${ingress}/InvitationLinkV1/${history.link_id}/revoke`, { link_id: history.link_id, admin: history.admin });
  const measureAttempt = async () => {
    const start = traffic.length;
    await http(`${ingress}/InvitationLinkV1/${history.link_id}/admit`, {
      version: 1, link_id: history.link_id, operation_id: id(), requester_id: 91, justification: 'x'.repeat(16_384),
    });
    const calls = traffic.slice(start).filter(row => row.path.endsWith('/InvitationLinkV1/admit'));
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
  await http(`${githubUrl}/identity`, { login: 'user-91', addressed_id: 91 });
  const autoInput = { ...creation(), approval_required: false };
  const auto = (handler, body) => http(`${ingress}/InvitationLinkV1/${autoInput.link_id}/${handler}`, body);
  await auto('create', autoInput);
  const autoAttempt = { version: 1, link_id: autoInput.link_id, operation_id: id(), requester_id: 91 };
  const approved = await auto('admit', autoAttempt);
  assert.equal(approved.result.state, 'approved');
  assert.equal(approved.result.decision_deadline, null);
  const progressQuery = { link_id: autoInput.link_id, request_id: approved.result.request_id, requester_id: 91 };
  const progress = await eventually(() => auto('delivery_progress', progressQuery), rows => rows.some(row => row.stage === 'submitted'));
  const plan = await auto('prepare_dispatch', progressQuery);
  const receiving = await eventually(() => http(`${ingress}/GithubCreateV1/${plan.commands[0].invitation_id}/status`), value => value?.outcome.kind === 'created');
  assert.ok(receiving.outcome.upstream_id > 0);
  await http(`${ingress}/GithubCreateV1/${receiving.command.invitation_id}/create`, receiving.command);
  const createEvents = async receipt => (await storage('audit', 100)).events.filter(event => event.target_id === receipt.command.invitation_id);
  const firstEvents = await createEvents(receiving);
  assert.equal(firstEvents.length, 1);
  assert.equal(firstEvents[0].event_type, 'invitation.sent');
  assert.equal(firstEvents[0].occurred_at, receiving.confirmed_at);
  assert.equal(firstEvents[0].actor_kind, 'system');
  await http(`${ingress}/GithubCreateV1/${receiving.command.invitation_id}/create`, receiving.command);
  assert.deepEqual(await createEvents(receiving), firstEvents);
  // D1 batch rolls back receipt/lifecycle if the audit insertion fails. Retry
  // after repair (and replay after an unobserved successful response) is safe.
  for (const [outcome, eventType, actor] of [
    [{ kind: 'created', upstream_id: 9876 }, 'invitation.sent', 'system'],
    [{ kind: 'already_collaborator' }, 'invitation.accepted', 'github'],
    [{ kind: 'failed', status: 422 }, 'invitation.send_failed', 'system'],
  ]) {
    const receipt = { ...receiving, command: { ...receiving.command, invitation_id: id() }, outcome };
    await db.prepare("CREATE TRIGGER fail_delivery_audit BEFORE INSERT ON audit_events WHEN NEW.target_kind='github_invitation' BEGIN SELECT RAISE(ABORT, 'fixture audit unavailable'); END").run();
    await storage('delivery', receipt, 409);
    assert.ok(!(await storage('delivery-read', receipt.command.request_id)).some(row => row.command.invitation_id === receipt.command.invitation_id));
    assert.deepEqual(await createEvents(receipt), []);
    await db.prepare('DROP TRIGGER fail_delivery_audit').run();
    await storage('delivery', receipt);
    const events = await createEvents(receipt);
    assert.equal(events.length, 1);
    assert.equal(events[0].event_type, eventType);
    assert.equal(events[0].actor_kind, actor);
    assert.equal(events[0].occurred_at, receipt.confirmed_at);
    await storage('delivery', receipt);
    assert.deepEqual(await createEvents(receipt), events);
  }
  console.log('PASS actual D1 audit-write failure rollback and lost-ack replay for all three confirmed outcomes');
  const calls = await http(`${githubUrl}/calls`, undefined, 'GET');
  assert.equal(calls.requests.filter(call => call.method === 'PUT').length, 1);
  assert.deepEqual(await auto('admit', autoAttempt), approved);
  assert.deepEqual(await auto('delivery_progress', progressQuery), progress);
  assert.equal((await http(`${githubUrl}/calls`, undefined, 'GET')).requests.filter(call => call.method === 'PUT').length, 1);
  assert.equal(unexpectedOutbound, 0);
  console.log('PASS auto-approval, actual Worker GitHub stub delivery and confirmed-create replay');
  for (const [stubOutcome, kind, eventType, actor] of [
    ['already_collaborator', 'already_collaborator', 'invitation.accepted', 'github'],
    ['terminal_failure', 'failed', 'invitation.send_failed', 'system'],
  ]) {
    await http(`${githubUrl}/outcomes`, { owner: 'acme', repo: 'api', user: 'user-91', outcome: stubOutcome });
    const input = { ...creation(), approval_required: false };
    const call = (handler, body) => http(`${ingress}/InvitationLinkV1/${input.link_id}/${handler}`, body);
    await call('create', input);
    const admitted = await call('admit', { version: 1, link_id: input.link_id, operation_id: id(), requester_id: 91 });
    const plan = await call('prepare_dispatch', { link_id: input.link_id, request_id: admitted.result.request_id, requester_id: 91 });
    const receipt = await http(`${ingress}/GithubCreateV1/${plan.commands[0].invitation_id}/create`, plan.commands[0]);
    assert.equal(receipt.outcome.kind, kind);
    const events = await createEvents(receipt);
    assert.equal(events.length, 1);
    assert.equal(events[0].event_type, eventType);
    assert.equal(events[0].actor_kind, actor);
    assert.equal(events[0].occurred_at, receipt.confirmed_at);
    assert.deepEqual(await http(`${ingress}/GithubCreateV1/${receipt.command.invitation_id}/create`, receipt.command), receipt);
    assert.deepEqual(await createEvents(receipt), events);
  }
  await http(`${githubUrl}/reset`, undefined, 'DELETE');
  await http(`${githubUrl}/identity`, { login: 'user-91', addressed_id: 91 });
  console.log('PASS actual Worker GitHub 204/422 outcomes publish retained D1 audit events');
  const manualInput = creation();
  const manual = (handler, body) => http(`${ingress}/InvitationLinkV1/${manualInput.link_id}/${handler}`, body);
  await manual('create', manualInput);
  const manualReceipt = await manual('admit', { version: 1, link_id: manualInput.link_id, operation_id: id(), requester_id: 91 });
  const manualDecision = await manual('decide', { version: 1, link_id: manualInput.link_id, request_id: manualReceipt.result.request_id,
    operation_id: id(), admin: manualInput.admin, action: { kind: 'approve' } });
  assert.equal(manualDecision.request.state, 'approved');
  const manualResult = await http(`${ingress}/restate/workflow/InvitationRequestV1/${manualReceipt.result.request_id}/attach`, undefined, 'GET');
  assert.equal(manualResult.state, 'approved');
  await eventually(() => http(`${ingress}/GithubCreateV1/${manualResult.dispatch.commands[0].invitation_id}/status`), value => value?.outcome.kind === 'created');
  console.log('PASS manual approval and direct notification to waiting Worker lifecycle');
  await settlement({ ingress, githubUrl, http, storage, db, id, creation, eventually,
    requestId: approved.result.request_id, invitationId: plan.commands[0].invitation_id,
    pause: value => { pauseWorkflows = value; } });
  if (process.env.SETTLEMENT_ONLY !== '1') {
  await deadlineRecovery({ ingress, githubUrl, http, storage, id, creation, eventually, fault,
    pause: value => { pauseWorkflows = value; } });
  await browserAdmission(ingress, created.invitation_code, attempts[winner].requester_id, attempts[winner].operation_id, { creation, http });
  migrationDirectory = mkdtempSync(join(tmpdir(), 'ghinvite-d1-cutover-'));
  execFileSync('python3', ['tests/worker/migration.py', migrationDirectory], { cwd: root, timeout: 30_000 });
  // Import the adopted offline checkpoint into this disposable D1. Existing
  // schema is identical; isolate account-parent collisions with the test rows.
  const archive = readFileSync(join(migrationDirectory, 'adopted.sql'), 'utf8');
  const statements = archive.split('\n').filter(line => line.startsWith('INSERT INTO') || line.startsWith('CREATE TRIGGER'));
  const inserts = statements.filter(line => line.startsWith('INSERT INTO') && !line.startsWith('INSERT INTO "users"'));
  for (const user of [8, 9]) await db.prepare("INSERT INTO users VALUES (?, ?, NULL, '2026-01-01T00:00:00Z')").bind(user, `user-${user}`).run();
  // Fixture's installation is a historical replacement for account 100.
  await db.prepare("UPDATE installations SET uninstalled_at='2026-01-01T00:00:00Z'").run();
  for (const sql of archive.match(/CREATE TABLE admission_(?:cutover|import_progress)[\s\S]*?;/g) ?? []) await db.exec(sql.replaceAll('\n', ' '));
  await db.batch([db.prepare('PRAGMA defer_foreign_keys=ON'), ...inserts.map(sql => db.prepare(sql)), db.prepare('PRAGMA defer_foreign_keys=OFF')]);
  for (const sql of archive.match(/CREATE TRIGGER cutover_[\s\S]*?END;/g) ?? []) await db.prepare(sql).run();
  const cli = async (...args) => {
    const child = spawn('python3', ['scripts/admission-cutover.py', ...args], { cwd: root, stdio: ['ignore', 'pipe', 'pipe'] });
    children.add(child);
    let text = '';
    child.stdout.on('data', chunk => { text += chunk; }); child.stderr.on('data', chunk => { text += chunk; });
    const timer = setTimeout(() => child.kill('SIGKILL'), 60_000);
    const [code] = await once(child, 'exit'); clearTimeout(timer);
    return { code, text };
  };
  const args = ['import', '--database', join(migrationDirectory, 'checkpoint.db'), '--manifest', join(migrationDirectory, 'prepared.json'), '--ingress', ingress];
  const migrationManifest = JSON.parse(readFileSync(join(migrationDirectory, 'prepared.json'), 'utf8'));
  const entry = migrationManifest.links[0];
  const begin = { ...entry.begin, manifest_checksum: migrationManifest.checksum };
  const migrationPath = `${ingress}/InvitationLinkV1/${entry.begin.link.link_id}`;
  await http(`${migrationPath}/begin_import`, begin);
  const closed = await fetch(`${migrationPath}/link_status`, { method: 'POST', headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ link_id: begin.link.link_id, admin: begin.link.creation.admin }), signal: AbortSignal.timeout(25_000) });
  assert.equal(closed.status, 503, 'partial import remains closed');
  const changedManifest = await fetch(`${migrationPath}/begin_import`, { method: 'POST', headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ ...begin, manifest_checksum: 'f'.repeat(64) }), signal: AbortSignal.timeout(25_000) });
  assert.equal(changedManifest.status, 409);
  let imported = await cli(...args); assert.equal(imported.code, 0, imported.text);
  imported = await cli(...args, '--activate'); assert.equal(imported.code, 0, imported.text);
  imported = await cli(...args, '--activate'); assert.equal(imported.code, 0, imported.text);
  const legacyLink = '01ARZ3NDEKTSV4RRFFQ69G5FAV';
  const legacy = (handler, body) => http(`${ingress}/InvitationLinkV1/${legacyLink}/${handler}`, body);
  const confirmed = await http(`${ingress}/GithubCreateV1/01ARZ3NDEKTSV4RRFFQ69G5FB0/status`);
  assert.deepEqual(confirmed.outcome, { kind: 'created', upstream_id: 1234 });
  const oldDecision = await fetch(`${ingress}/InvitationRequest/01ARZ3NDEKTSV4RRFFQ69G5FAW/decide`, {
    method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ Approve: { decided_by: 7, decided_at: '2026-01-01T00:00:00Z' } }), signal: AbortSignal.timeout(25_000),
  });
  assert.equal(oldDecision.status, 410, 'obsolete command cannot regain authority');
  for (const [suffix, state, requester_id] of [['FAX', 'approved', 9], ['FAY', 'cancelled', 8], ['FAZ', 'expired', 8]]) {
    assert.equal((await legacy('request_status', { link_id: legacyLink, request_id: `01ARZ3NDEKTSV4RRFFQ69G5${suffix}`, requester_id })).state, state);
  }
  const newRequest = await legacy('admit', { version: 1, link_id: legacyLink, operation_id: id(), requester_id: 8 });
  assert.equal(newRequest.result.kind, 'accepted');
  await eventually(() => storage('link', legacyLink), value => value?.uses_count === 5);
  await assert.rejects(db.prepare("UPDATE invitation_links SET revoked_at='2026-09-01T00:00:00Z',revoked_by=7 WHERE id=?").bind(legacyLink).run(), /obsolete writer/);
  await assert.rejects(db.prepare("UPDATE invitation_requests SET state='approved' WHERE id='01ARZ3NDEKTSV4RRFFQ69G5FAW'").run(), /obsolete writer/);
  const rollback = await cli('restore-legacy', '--database', join(migrationDirectory, 'checkpoint.db'), '--restored-coordinated-checkpoint', 'stale');
  assert.notEqual(rollback.code, 0); assert.match(rollback.text, /reverse reconciliation/);
  console.log('PASS #58 offline checkpoint → actual D1/Worker import, legacy states, resumable activation, late-writer fence and forward-only recovery');
  // #66 on actual D1 bindings: installation 40 is the shape adoption exists for.
  // Its command already retains the numeric account binding, while account 300
  // has never been observed and must adopt the existing row.
  await http(`${githubUrl}/installation-identity`, { id: 300, login: 'adopted', type: 'Organization' });
  await db.prepare("INSERT INTO installations VALUES (40,300,'adopted','Organization','2026-01-01T00:00:00Z',NULL,'[]')").run();
  await fetch(`${admin}/services/Installation/state`, { method: 'POST', headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ object_key: '40', new_state: { account_id: [...Buffer.from('300')] } }), signal: AbortSignal.timeout(25_000) });
  await eventually(() => http(`${admin}/query`, { query: "SELECT key FROM state WHERE service_name = 'Installation' AND service_key = '40'" }),
    state => state.rows.length > 0);
  // Installation reads fail on the real binding while GitHub stays reachable.
  await db.prepare('ALTER TABLE installations RENAME TO installations_offline').run();
  const reposChanged = { installation_id: 40, selected_repos: 'all' };
  const acknowledged = await http(`${ingress}/Installation/40/repos_changed/send`, reposChanged);
  assert.ok(acknowledged.invocationId, 'repository webhook is acknowledged by its durable send');
  // The command completes rather than holding exclusivity, and a duplicate
  // event joins the retained continuation instead of competing with it.
  await http(`${ingress}/Installation/40/repos_changed`, reposChanged);
  await http(`${ingress}/AccountInstallationV1/300/refresh`, 41);
  const held = Date.now();
  const offline = await fetch(`${ingress}/AccountInstallationV1/300/status`, { method: 'POST', signal: AbortSignal.timeout(25_000) });
  assert.equal(offline.status, 503, 'synchronous observation fails promptly instead of holding exclusivity');
  assert.ok(Date.now() - held < 15_000);
  await db.prepare('ALTER TABLE installations_offline RENAME TO installations').run();
  // Restoration alone converges: no further webhook and no user action.
  await eventually(() => db.prepare('SELECT selected_repos FROM installations WHERE installation_id=40').first(),
    row => row?.selected_repos === '[10,11]');
  const adopted = await http(`${ingress}/AccountInstallationV1/300/status`);
  assert.equal(adopted.account.installation_id, 40);
  assert.deepEqual(adopted.observation.repo_ids, [10, 11]);
  await http(`${githubUrl}/installation-identity`, { id: 100, login: 'acme', type: 'Organization' });
  console.log('PASS #66 acknowledged webhook survives first D1 adoption outage and converges without another event');
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
