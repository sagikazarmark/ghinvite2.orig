import assert from 'node:assert/strict';
import { Response } from 'miniflare';
import { admitCapability } from './capability.mjs';

// A non-GitHub upstream (Restate ingress, the transport fixtures) that never
// finishes. Cleanup releases it only after assertions, and cannot turn a
// watchdog failure into a successful application timeout. GitHub's stalls
// are octoemu `stall` fault rules (below).
export function stalledResponse(phase, cleanup) {
  if (phase === 'headers') return new Promise(resolve => cleanup.push(() => resolve(Response.json({ late: true }))));
  return new Response(new ReadableStream({ start(controller) {
    controller.enqueue(new TextEncoder().encode('{"partial":'));
    cleanup.push(() => { try { controller.close(); } catch {} });
  } }), { headers: { 'content-type': 'application/json' } });
}

export async function deadlineRecovery({ ingress, github, http, storage, id, creation, eventually, pause }) {
  // GitHub stops answering in `phase` (headers or body): the next PUT, and
  // once it is in flight the installation read. Neither request reaches the
  // emulator's handler.
  const rules = [];
  const fault = async (phase, installationId) => {
    for (const rule of rules.splice(0)) await github.emulator.removeFault(rule);
    if (!phase) return;
    const stall = { kind: 'stall', phase };
    const put = await github.emulator.fault({ method: 'PUT', path: '/repos/{owner}/{repo}/collaborators/{username}', label: `${phase} stall`, action: stall });
    rules.push(put);
    // The read's rule is in place before the write counts as reached.
    let read;
    const write = github.emulator.waitHeld(put, 1, 10_000).then(async () => {
      read = await github.emulator.fault({ method: 'GET', path: `/app/installations/${installationId}`, label: `${phase} stall`, action: stall });
      rules.push(read);
    });
    const observation = write.then(() => github.emulator.waitHeld(read, 1, 10_000));
    return { write, observation, puts: async () => (await github.requests('PUT')).length };
  };
  const installation = await http(`${ingress}/AccountInstallation/repo:100/status`);
  const installationId = installation.account.installation_id;
  const waitForFault = async (promise, label) => {
    let timer;
    try {
      await Promise.race([promise, new Promise((_, reject) => {
        timer = setTimeout(() => reject(new Error(`fault not reached: ${label}`)), 10_000);
      })]);
    } finally { clearTimeout(timer); }
  };
  const call = async (path, input) => {
    const response = await fetch(`${ingress}/${path}`, { method: 'POST', headers: input === undefined ? {} : { 'content-type': 'application/json' },
      body: input === undefined ? undefined : JSON.stringify(input), signal: AbortSignal.timeout(40_000) });
    const value = await response.json();
    return { status: response.status, value };
  };
  pause(true);
  try {
    for (const phase of ['headers', 'body']) {
      // A clean GitHub side: no invitations for the requester, and the
      // installation back on acme/api and acme/web (repo-12 out of scope).
      await github.reset('user-91');
      await github.select([10, 11]);
      // Start each phase from the scope the reset stub reports, so the previous
      // phase's restored observation cannot answer this one's admission.
      await http(`${ingress}/AccountInstallation/repo:100/refresh`, installationId);
      const input = { ...creation(), approval_required: false };
      const link = `InvitationLink/${input.link_id}`;
      await http(`${ingress}/${link}/create`, input);
      const receipt = await http(`${ingress}/${link}/admit`, { link_id: input.link_id, operation_id: id(), requester_id: 91, ...admitCapability(input.link_id) });
      const query = { link_id: input.link_id, request_id: receipt.result.request_id, requester_id: 91 };
      const plan = await http(`${ingress}/InvitationRequest/${receipt.result.request_id}/approved_plan`, query);
      const command = plan.commands[0];
      await eventually(() => storage('request', receipt.result.request_id), Boolean);
      // Scope evidence is read by the caller, outside admission. Asking beyond
      // the cached scope forces a real GitHub read. Its deadline must release
      // AccountInstallation exclusivity even after Restate replaces a waker.
      // Admission itself accepts missing evidence without waiting on this I/O.
      const base = creation();
      const pending = { ...base, targets: {...base.targets, repos: [...base.targets.repos, { repo_id: 12, repo_full_name: 'acme/repo-12', grant: 'read' }]} };
      await http(`${ingress}/InvitationLink/${pending.link_id}/create`, pending);
      const attempt = { link_id: pending.link_id, operation_id: id(), requester_id: 91, ...admitCapability(pending.link_id) };
      const seen = await fault(phase, installationId);
      const write = call(`RepositoryDelivery/${command.request_id}:${command.repo_id}:${command.delivery_generation}/create`, command);
      await waitForFault(seen.write, 'GitHub PUT');
      const observation = call('AccountInstallation/repo:100/admission_evidence', { account_id: 100, repo_ids: pending.targets.repos.map(repo => repo.repo_id) });
      await waitForFault(seen.observation, `installation ${installationId} observation`);
      const admission = call(`InvitationLink/${pending.link_id}/admit`, attempt);
      const queuedStatus = call('AccountInstallation/repo:100/status');
      const [created, evidence, admitted, status] = await Promise.all([write, observation, admission, queuedStatus]);
      assert.equal(created.status, 200);
      assert.equal(created.value.outcome.kind, 'outcome_unknown');
      assert.equal(evidence.status, 200, JSON.stringify(evidence));
      assert.equal(evidence.value.eligibility.kind, 'unknown', 'transport uncertainty is not negative scope evidence');
      assert.equal(admitted.status, 200, JSON.stringify(admitted));
      assert.equal(admitted.value.result.kind, 'accepted', 'missing evidence does not reject admission');
      assert.equal(status.status, 200, `queued exclusive status must complete: ${JSON.stringify(status.value)}`);
      assert.equal(status.value.observation.kind, 'unknown');
      await fault(null);
      // Restore observation/projection, then replay with no pending invitation
      // or collaborator evidence. Absence must never authorize another PUT.
      await github.select([10, 11, 12]);
      await http(`${ingress}/AccountInstallation/repo:100/refresh`, installationId);
      const recovered = await http(`${ingress}/InvitationLink/${pending.link_id}/admit`, attempt);
      assert.deepEqual(recovered, admitted.value, 'later evidence cannot alter the retained admission receipt');
      assert.deepEqual(await http(`${ingress}/InvitationLink/${pending.link_id}/admit`, attempt), recovered);
      for (let n = 0; n < 2; n++) {
        const replay = await http(`${ingress}/RepositoryDelivery/${command.request_id}:${command.repo_id}:${command.delivery_generation}/create`, command);
        assert.equal(replay.outcome.kind, 'outcome_unknown');
      }
      assert.equal((await http(`${ingress}/RepositoryDelivery/${command.request_id}:${command.repo_id}:${command.delivery_generation}/status`)).create.outcome.kind, 'outcome_unknown');
      assert.equal(await seen.puts(), 1, 'timeout fence prohibits automatic repeat PUT');
      console.log(`PASS Worker ${phase} timeout: retained unknown delivery/fence, unknown caller scope evidence, released account exclusivity, missing-evidence admission and exact replay`);
    }
    // Leave the installation making exactly what every other fixture expects.
    await github.select([10, 11]);
    await http(`${ingress}/AccountInstallation/repo:100/refresh`, installationId);
  } finally { await fault(null); pause(false); }
}
