import assert from 'node:assert/strict';

// Production Worker/D1 + real Restate + production Fetch GitHub client.
export async function approvedDelivery({ ingress, githubUrl, http, db, creation, id, eventually }) {
  const puts = async () => (await http(`${githubUrl}/calls`, undefined, 'GET')).requests.filter(row => row.method === 'PUT').length;
  await db.prepare("CREATE TRIGGER hold_installation BEFORE INSERT ON installations BEGIN SELECT RAISE(ABORT, 'projection withheld'); END").run();
  await http(`${ingress}/Installation/1/onboard`, { installation_id: 1, actor_user_id: 7, account_id: 100,
    account_login: 'acme', account_type: 'Organization', selected_repos: 'all', installed_at: '2026-01-01T00:00:00Z' });
  await http(`${githubUrl}/identity`, { login: 'user-91', addressed_id: 91 });
  await http(`${githubUrl}/outcomes`, { owner: 'acme', repo: 'api', user: 'user-91', outcome: 'created_response_lost' });
  const input = { ...creation(), approval_required: false };
  const link = (handler, body) => http(`${ingress}/InvitationLink/${input.link_id}/${handler}`, body);
  await link('create', input);
  const admitted = await link('admit', { link_id: input.link_id, operation_id: id(), requester_id: 91 });
  assert.equal(admitted.result.state, 'approved');
  const plan = await link('prepare_dispatch', { link_id: input.link_id, request_id: admitted.result.request_id, requester_id: 91 });
  const command = plan.commands[0];
  const receiver = (handler, body) => http(`${ingress}/RepositoryDelivery/${command.request_id}:${command.repo_id}/${handler}`, body);
  const unknown = await eventually(() => receiver('status'), row => row?.create.outcome.kind === 'outcome_unknown');
  assert.deepEqual(unknown.create.command, command);
  for (const table of ['installations', 'users', 'invitation_links', 'invitation_requests']) {
    assert.equal((await db.prepare(`SELECT COUNT(*) AS count FROM ${table}`).first()).count, 0, table);
  }
  assert.equal(await puts(), 1);
  const confirmed = await receiver('create', command);
  assert.equal(confirmed.outcome.kind, 'created');
  assert.deepEqual(confirmed.command, command);
  assert.equal(await puts(), 1, 'unknown reconciliation only reads');
  await receiver('on_webhook', { invitation_id: command.invitation_id, action: 'accepted', at: '2026-09-24T12:00:00Z' });
  const settled = await receiver('status');
  assert.deepEqual(settled.create, confirmed);
  assert.equal(settled.settlement.state, 'accepted');
  await receiver('on_webhook', { invitation_id: command.invitation_id, action: 'declined', at: '2026-09-24T13:00:00Z' });
  assert.deepEqual(await receiver('status'), settled, 'first valid terminal winner is immutable');
  await db.prepare('DROP TRIGGER hold_installation').run();
  for (const user of [7, 91]) await db.prepare("INSERT INTO users VALUES (?, ?, NULL, '2026-01-01T00:00:00Z')").bind(user, `user-${user}`).run();
  await eventually(() => db.prepare('SELECT state FROM github_invitations WHERE id=?').bind(command.invitation_id).first(), row => row?.state === 'accepted');
  const events = await db.prepare('SELECT * FROM audit_events WHERE target_id=?').bind(command.invitation_id).all();
  assert.equal(events.results.length, 2);
  assert.deepEqual(await receiver('create', command), confirmed);
  assert.equal(await puts(), 1);
  console.log('PASS Worker/D1 approved delivery with all projected parents withheld, applied PUT/result loss, read-only recovery and independent convergence');
}
