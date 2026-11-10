import { setupCommand } from './installation-fixture.mjs';
import assert from 'node:assert/strict';
import { admitCapability } from './capability.mjs';

// Production Worker/D1 + real Restate + production Fetch GitHub client.
export async function approvedDelivery({ ingress, githubUrl, http, db, creation, id, eventually }) {
  const puts = async () => (await http(`${githubUrl}/calls`, undefined, 'GET')).requests.filter(row => row.method === 'PUT').length;
  await db.prepare("CREATE TRIGGER hold_installation BEFORE INSERT ON installations BEGIN SELECT RAISE(ABORT, 'projection withheld'); END").run();
  await http(`${ingress}/Installation/repo:1/onboard`, setupCommand({ app:'repo', installation_id: 1, actor_user_id: 7, account_id: 100,
    account_login: 'acme', account_type: 'Organization', selected_repos: 'all', installed_at: '2026-01-01T00:00:00Z' }));
  await http(`${githubUrl}/identity`, { login: 'user-91', addressed_id: 91 });
  await http(`${githubUrl}/outcomes`, { owner: 'acme', repo: 'api', user: 'user-91', outcome: 'created_response_lost' });
  const input = { ...creation(), approval_required: false };
  const link = (handler, body) => http(`${ingress}/InvitationLink/${input.link_id}/${handler}`, body);
  await link('create', input);
  const admitted = await link('admit', { ...admitCapability(input.link_id), link_id: input.link_id, operation_id: id(), requester_id: 91 });
  assert.equal(admitted.result.state, 'approved');
  const plan = await http(`${ingress}/InvitationRequest/${admitted.result.request_id}/approved_plan`, { link_id: input.link_id, request_id: admitted.result.request_id, requester_id: 91 });
  const command = plan.commands[0];
  const receiver = (handler, body) => http(`${ingress}/RepositoryDelivery/${command.request_id}:${command.repo_id}:${command.delivery_generation}/${handler}`, body);
  const unknown = await eventually(() => receiver('status'), row => row?.create.outcome.kind === 'outcome_unknown');
  assert.deepEqual(unknown.create.command, command);
  for (const table of ['installations', 'users', 'invitation_links', 'invitation_requests']) {
    assert.equal((await db.prepare(`SELECT COUNT(*) AS count FROM ${table}`).first()).count, 0, table);
  }
  assert.equal(await puts(), 1);
  const replay = await receiver('create', command);
  assert.equal(replay.outcome.kind, 'created', 'positive read-only evidence resolves retained uncertainty during replay');
  const confirmed = (await eventually(() => receiver('status'), value => value?.create.outcome.kind === 'created')).create;
  assert.equal(confirmed.outcome.kind, 'created');
  assert.deepEqual(confirmed, replay);
  assert.deepEqual(confirmed.command, command);
  assert.equal(await puts(), 1, 'unknown reconciliation only reads');
  await http(`${githubUrl}/repos/acme/api/invitations/${confirmed.outcome.upstream_id}`, undefined, 'DELETE');
  await http(`${githubUrl}/identity`, { login: 'user-91', addressed_id: 91, role_name: 'write' });
  await http(`${ingress}/DeliveryObservation/observe`,await receiver('status'));
  const settled = await eventually(() => receiver('status'), value => value?.settlement?.state === 'accepted');
  assert.deepEqual(settled.create, confirmed);
  assert.equal(settled.settlement.state, 'accepted');
  await receiver('reconcile',{delivery_key:`${command.request_id}:${command.repo_id}:${command.delivery_generation}`,expected_revision:settled.revision,at:new Date().toISOString(),evidence:{kind:'absent',availability_verified:true,role_name:'none',direct_collaborator:null,membership:null}});
  assert.deepEqual(await receiver('status'), settled, 'first valid terminal winner is immutable');
  await db.prepare('DROP TRIGGER hold_installation').run();
  for (const user of [7, 91]) await db.prepare("INSERT INTO users(user_id,login,avatar_url,last_seen_at) VALUES (?, ?, NULL, '2026-01-01T00:00:00Z')").bind(user, `user-${user}`).run();
  await eventually(() => db.prepare('SELECT settlement AS state FROM deliveries WHERE id=?').bind(`${command.request_id}:${command.repo_id}:${command.delivery_generation}`).first(), row => row?.state === 'accepted');
  const events = await db.prepare('SELECT * FROM audit_events WHERE target_id=?').bind(`${command.request_id}:${command.repo_id}:${command.delivery_generation}`).all();
  assert.equal(events.results.length, 3);
  assert.deepEqual(events.results.map(event=>event.event_type).sort(),['delivery.outcome_unknown','invitation.accepted','invitation.sent']);
  assert.deepEqual(await receiver('create', command), confirmed);
  assert.equal(await puts(), 1);
  await http(`${githubUrl}/identity`, { login: 'user-91', addressed_id: 91 });
  console.log('PASS Worker/D1 approved delivery with all projected parents withheld, applied PUT/result loss, read-only recovery and independent convergence');
}
