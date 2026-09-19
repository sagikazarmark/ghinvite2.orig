import assert from 'node:assert/strict';
import { Response } from 'miniflare';

// The upstream never finishes. Cleanup releases it only after assertions, and
// cannot turn a watchdog failure into a successful application timeout.
export function stalledResponse(phase, cleanup) {
  if (phase === 'headers') return new Promise(resolve => cleanup.push(() => resolve(Response.json({ late: true }))));
  return new Response(new ReadableStream({ start(controller) {
    controller.enqueue(new TextEncoder().encode('{"partial":'));
    cleanup.push(() => { try { controller.close(); } catch {} });
  } }), { headers: { 'content-type': 'application/json' } });
}

export async function deadlineRecovery({ ingress, githubUrl, http, storage, id, creation, eventually, pause, fault }) {
  const installation = await http(`${ingress}/AccountInstallationV1/100/status`);
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
    return { status: response.status, value: await response.json() };
  };
  pause(true);
  try {
    for (const phase of ['headers', 'body']) {
      await http(`${githubUrl}/reset`, undefined, 'DELETE');
      await http(`${githubUrl}/identity`, { login: 'user-91', addressed_id: 91 });
      const input = { ...creation(), approval_required: false };
      const link = `InvitationLinkV1/${input.link_id}`;
      await http(`${ingress}/${link}/create`, input);
      const receipt = await http(`${ingress}/${link}/admit`, { version: 1, link_id: input.link_id, operation_id: id(), requester_id: 91 });
      const query = { link_id: input.link_id, request_id: receipt.result.request_id, requester_id: 91 };
      const plan = await http(`${ingress}/${link}/prepare_dispatch`, query);
      const command = plan.commands[0];
      await eventually(() => storage('request', receipt.result.request_id), Boolean);
      // A separate pending admission must report infrastructure uncertainty,
      // retain its identity, and leave the account available to queued status.
      const pending = creation();
      await http(`${ingress}/InvitationLinkV1/${pending.link_id}/create`, pending);
      const attempt = { version: 1, link_id: pending.link_id, operation_id: id(), requester_id: 91 };
      const seen = fault(phase, installationId);
      const write = call(`GithubCreateV1/${command.invitation_id}/create`, command);
      await waitForFault(seen.write, 'GitHub PUT');
      const admission = call(`InvitationLinkV1/${pending.link_id}/admit`, attempt);
      await waitForFault(seen.observation, `installation ${installationId} observation`);
      const queuedStatus = call('AccountInstallationV1/100/status');
      const [created, admitted, status] = await Promise.all([write, admission, queuedStatus]);
      assert.equal(created.status, 200);
      assert.equal(created.value.outcome.kind, 'outcome_unknown');
      assert.equal(admitted.status, 503, JSON.stringify(admitted));
      assert.equal(status.status, 200, `queued exclusive status must complete: ${JSON.stringify(status.value)}`);
      assert.equal(status.value.observation.kind, 'unknown');
      fault(null);
      // Restore observation/projection, then replay with no pending invitation
      // or collaborator evidence. Absence must never authorize another PUT.
      await http(`${ingress}/AccountInstallationV1/100/refresh`, installationId);
      const recovered = await http(`${ingress}/InvitationLinkV1/${pending.link_id}/admit`, attempt);
      assert.equal(recovered.result.kind, 'accepted');
      assert.deepEqual(await http(`${ingress}/InvitationLinkV1/${pending.link_id}/admit`, attempt), recovered);
      for (let n = 0; n < 2; n++) {
        const replay = await http(`${ingress}/GithubCreateV1/${command.invitation_id}/create`, command);
        assert.equal(replay.outcome.kind, 'outcome_unknown');
      }
      assert.equal((await http(`${ingress}/GithubCreateV1/${command.invitation_id}/status`)).outcome.kind, 'outcome_unknown');
      assert.equal(seen.puts(), 1, 'timeout fence prohibits automatic repeat PUT');
      console.log(`PASS Worker ${phase} timeout: retained unknown delivery/fence, admission 503, released account exclusivity, same-attempt recovery`);
    }
  } finally { fault(null); pause(false); }
}
