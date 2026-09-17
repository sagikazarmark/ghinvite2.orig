import assert from 'node:assert/strict';

// Public command and Storage boundaries; SQL is used only for fault injection.
export async function settlement({ ingress, githubUrl, http, storage, db, id, creation, eventually, requestId, invitationId, pause }) {
  const at = '2026-09-17T12:00:00Z';
  const lifecycle = (key, handler, body) => http(`${ingress}/GithubInvitation/${key}/${handler}`, body);
  const sent = await eventually(() => storage('invitation', invitationId), row => row?.state === 'sent');
  await db.prepare("CREATE TRIGGER settlement_audit_failure BEFORE INSERT ON audit_events WHEN NEW.target_kind = 'github_invitation' BEGIN SELECT RAISE(ABORT, 'injected audit failure'); END").run();
  const invocation = await lifecycle(sent.id, 'on_webhook_v1/send', { invitation_id: sent.id, action: 'accepted', at });
  await new Promise(resolve => setTimeout(resolve, 300));
  assert.equal((await storage('invitation', sent.id)).state, 'sent', 'audit failure cannot commit terminal state');
  await db.prepare('DROP TRIGGER settlement_audit_failure').run();
  await http(`${ingress}/restate/invocation/${invocation.invocationId}/attach`, undefined, 'GET');
  const evidence = { expected: sent, accepted: false, at };
  await Promise.all([lifecycle(sent.id, 'reconcile_v1', evidence), lifecycle(sent.id, 'reconcile_v1', evidence)]);
  assert.equal((await storage('invitation', sent.id)).state, 'accepted');
  let events = (await storage('audit', 100)).events.filter(event => event.target_id === sent.id);
  assert.equal(events.length, 1);
  assert.equal(events[0].event_type, 'invitation.accepted');
  assert.equal((await storage('invitation', sent.id)).github_invitation_id, sent.github_invitation_id);

  // D1 really commits, then the JS binding loses the batch acknowledgement.
  const row = { ...sent, id: id(), invitation_request_id: requestId, github_invitation_id: 987654 };
  await storage('insert_invitation', row);
  const transition = { expected: row, state: 'cancelled', event: { id: id(), account_id: 100, occurred_at: at,
    event_type: 'invitation.cancelled', actor_kind: 'system', actor_id: null, target_kind: 'github_invitation', target_id: row.id,
    metadata: { reconciled: true }, request_id: null } };
  await storage('settle', transition, 409, { 'x-test-lose-batch-ack': '1' });
  assert.equal((await storage('invitation', row.id)).state, 'cancelled');
  await storage('settle', transition);
  events = (await storage('audit', 100)).events.filter(event => event.target_id === row.id);
  assert.deepEqual(events, [transition.event]);
  await assert.rejects(db.prepare("UPDATE github_invitations SET state='accepted', github_invitation_id=NULL WHERE id=?").bind(row.id).run(), /settled invitation writer conflict/);
  assert.equal((await storage('invitation', row.id)).state, 'cancelled');
  // Existing Sent rows need no create receipt or backfill before settlement.
  const cancelled = { ...row, id: id(), github_invitation_id: 987655 };
  await storage('insert_invitation', cancelled);
  await lifecycle(cancelled.id, 'cancel_v1', { invitation_id: cancelled.id, installation_id: 1, by_user: 7, at });
  assert.equal((await storage('invitation', cancelled.id)).state, 'cancelled');
  const expiring = { ...sent, id: id() };
  await storage('insert_invitation', expiring);
  await lifecycle(expiring.id, 'tick_expire_v1', { invitation_id: expiring.id, installation_id: 1, at });
  assert.equal((await storage('invitation', expiring.id)).state, 'expired');

  // A real blocked receiver, ordinary concurrent sweeps, then safe recovery.
  pause(true);
  const input = { ...creation(), approval_required: false };
  const link = (handler, body) => http(`${ingress}/InvitationLinkV1/${input.link_id}/${handler}`, body);
  await link('create', input);
  const admitted = await link('admit', { version: 1, link_id: input.link_id, operation_id: id(), requester_id: 91 });
  const plan = await link('prepare_dispatch', { link_id: input.link_id, request_id: admitted.result.request_id, requester_id: 91 });
  await http(`${githubUrl}/outcomes`, { owner: 'acme', repo: 'api', user: 'user-91', outcome: 'access_lost_once' });
  const command = plan.commands[0];
  const create = () => http(`${ingress}/GithubCreateV1/${command.invitation_id}/create`, command);
  assert.equal((await create()).outcome.kind, 'blocked');
  await Promise.all([http(`${ingress}/Reconcile/daily_run_v1`, { at }), http(`${ingress}/Reconcile/daily_run_v1`, { at })]);
  assert.equal((await storage('invitation', command.invitation_id)).state, 'sending');
  const recovered = await create();
  assert.equal(recovered.outcome.kind, 'created');
  const repaired = await storage('invitation', command.invitation_id);
  assert.equal(repaired.state, 'sent');
  assert.equal(repaired.github_invitation_id, recovered.outcome.upstream_id);
  const renamed = { ...sent, id: id(), github_invitation_id: 987656 };
  await storage('insert_invitation', renamed);
  await http(`${githubUrl}/identity`, { login: 'renamed', addressed_id: 91, role_name: 'write' });
  await http(`${ingress}/Reconcile/daily_run_v1`, { at });
  assert.equal((await storage('invitation', renamed.id)).state, 'accepted', 'settlement resolves numeric requester identity after rename');
  await http(`${githubUrl}/identity`, { login: 'user-91', addressed_id: 91 });
  pause(false);
  console.log('PASS #63 real Restate + D1 settlement races, atomic audit rollback, lost batch acknowledgement, legacy Sent rows and blocked-create recovery');
}
