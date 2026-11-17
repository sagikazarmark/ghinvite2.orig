import { setupCommand } from './installation-fixture.mjs';
import { auditHistory } from './audit-history.mjs';
import assert from 'node:assert/strict';
import {createHash} from 'node:crypto';
import { admitCapability } from './capability.mjs';

// Public command and Storage boundaries; SQL is used only for fault injection.
export async function settlement({ admin, ingress, github, http, storage, db, id, creation, eventually, requestId, initialCommand, pause }) {
  const at = new Date().toISOString();
  const registered = command => eventually(()=>http(`${ingress}/SweepSchedule/repo:100/status`), state=>state?.tracked?.[String(command.repo_id)]?.includes(`${command.request_id}:${command.repo_id}:${command.delivery_generation}`));
  const sweep = async () => {
    const state=await http(`${ingress}/SweepSchedule/repo:100/status`);
    await http(`${ingress}/Reconcile/sweep`,{account:{app:'repo',account_id:100},token:state.token,tracked:state.tracked});
    await eventually(()=>http(`${admin}/query`,{query:"SELECT id FROM sys_invocation WHERE status != 'completed' AND (target_service_name = 'Reconcile' OR (target_service_name = 'RepositoryDelivery' AND target_handler_name = 'reconcile'))"}),result=>result.rows.length===0);
  };
  const key = command => `${command.request_id}:${command.repo_id}:${command.delivery_generation}`;
  const settlementId = command => {
    const bytes=createHash('sha256').update(`ghinvite/delivery/audit/v2/${key(command)}/settlement`).digest().subarray(0,16);
    let value=BigInt(`0x${bytes.toString('hex')}`), encoded='';
    const alphabet='0123456789ABCDEFGHJKMNPQRSTVWXYZ';
    for(let i=0;i<26;i++){encoded=alphabet[Number(value&31n)]+encoded;value>>=5n;}
    return encoded;
  };
  const lifecycle = (command, handler, body) => http(`${ingress}/RepositoryDelivery/${key(command)}/${handler}`, body);
  const pending = snapshot => snapshot?.create.outcome.kind === 'created' && !snapshot.settlement;
  const read = command => storage('delivery-by-key',key(command));
  const sent = await eventually(() => read(initialCommand), pending);
  const sentCommand=sent.create.command;
  const requester = (await storage('request', requestId)).requester_id;
  const beforeWinner = await lifecycle(sentCommand, 'status');
  const evidence = { delivery_key: key(sentCommand), expected_revision: beforeWinner.revision, at,
    evidence: { kind: 'absent', availability_verified: true, role_name: 'none', direct_collaborator: null, membership: null } };
  // The requester is a write collaborator on GitHub (accepted, role raised).
  await github.grant(`user-${requester}`, 'push');
  await db.prepare("CREATE TRIGGER settlement_audit_failure BEFORE INSERT ON audit_events WHEN NEW.target_kind = 'repository_delivery' BEGIN SELECT RAISE(ABORT, 'injected audit failure'); END").run();
  const invocation = await http(`${ingress}/DeliveryObservation/observe/send`,beforeWinner);
  await new Promise(resolve => setTimeout(resolve, 300));
  assert.ok(pending(await read(sentCommand)), 'audit failure cannot commit terminal state');
  await db.prepare('DROP TRIGGER settlement_audit_failure').run();
  await http(`${ingress}/restate/invocation/${invocation.invocationId}/attach`, undefined, 'GET');
  await eventually(() => lifecycle(sentCommand, 'status'), value => value?.settlement?.state === 'accepted');
  await Promise.all([lifecycle(sentCommand, 'reconcile', evidence), lifecycle(sentCommand, 'reconcile', evidence)]);
  await eventually(() => read(sentCommand), row => row?.settlement?.state === 'accepted');
  let events = (await auditHistory(storage, 100)).filter(event => event.target_id === key(sentCommand));
  assert.equal(events.length, 2);
  assert.equal(events.filter(event => event.event_type === 'invitation.sent').length, 1);
  assert.equal(events.filter(event => event.event_type === 'invitation.accepted').length, 1);
  assert.equal((await read(sentCommand)).create.outcome.upstream_id, sent.create.outcome.upstream_id);

  // D1 really commits, then the JS binding loses the batch acknowledgement.
  const rowCommand={...sentCommand,delivery_generation:sentCommand.delivery_generation+1};
  const row={create:{...sent.create,command:rowCommand,outcome:{kind:'created',upstream_id:987654}},revision:1,settlement:null};
  await storage('delivery',row);
  const transition={...row,revision:2,settlement:{state:'cancelled',event:{id:settlementId(rowCommand),account_id:100,occurred_at:at,
    event_type:'invitation.cancelled',actor_kind:'system',actor_id:null,target_kind:'repository_delivery',target_id:key(rowCommand),
    metadata:{reconciled:true,reason:'link_revoked'},request_id:null}}};
  await storage('delivery',transition,409,{'x-test-lose-batch-ack':'1'});
  assert.equal((await read(rowCommand)).settlement.state,'cancelled');
  await storage('delivery',transition);
  events=(await auditHistory(storage,100)).filter(event=>event.target_id===key(rowCommand));
  assert.equal(events.length,2);
  assert.deepEqual(events.find(event=>event.event_type==='invitation.cancelled'),transition.settlement.event);
  await assert.rejects(db.prepare("UPDATE deliveries SET settlement='accepted', upstream_invitation_id=NULL WHERE id=?").bind(key(rowCommand)).run(),/settled invitation writer conflict/);
  assert.equal((await read(rowCommand)).settlement.state,'cancelled');
  // A projected receipt does not create retained owner authority.
  const projectedCommand={...rowCommand,delivery_generation:rowCommand.delivery_generation+1};
  await storage('delivery',{...row,create:{...row.create,command:projectedCommand,outcome:{kind:'created',upstream_id:987655}}});
  assert.equal(await lifecycle(projectedCommand,'status'),null);
  console.log('PASS actual D1 immutable delivery projection, settlement audit recovery and canonical owner routing');

  // A real blocked receiver, ordinary concurrent sweeps, then safe recovery.
  // Other scenarios' pending invitations would correctly trigger S1's
  // AlreadyPending result instead of exercising this fresh write failure.
  await github.reset('user-91');
  pause(true);
  const input = { ...creation(), approval_required: false };
  const link = (handler, body) => http(`${ingress}/InvitationLink/${input.link_id}/${handler}`, body);
  await link('create', input);
  const admitted = await link('admit', { ...admitCapability(input.link_id), link_id: input.link_id, operation_id: id(), requester_id: 91 });
  const plan = await http(`${ingress}/InvitationRequest/${admitted.result.request_id}/approved_plan`, { link_id: input.link_id, request_id: admitted.result.request_id, requester_id: 91 });
  await github.outcome('user-91', 'access_lost_once');
  const command = plan.commands[0];
  const create = () => http(`${ingress}/RepositoryDelivery/${command.request_id}:${command.repo_id}:${command.delivery_generation}/create`, command);
  assert.equal((await create()).outcome.kind, 'blocked');
  await Promise.all([sweep(), sweep()]);
  assert.equal((await read(command)).create.outcome.kind, 'blocked');
  const recovered = await create();
  assert.equal(recovered.outcome.kind, 'created');
  await registered(command);
  const repaired = await eventually(() => read(command), pending);
  assert.ok(pending(repaired));
  assert.equal(repaired.create.outcome.upstream_id, recovered.outcome.upstream_id);
  // Accepted, raised to write, then renamed: settlement follows the user ID.
  await github.accept('user-91', { id: recovered.outcome.upstream_id, role: 'push' });
  await github.rename('user-91', 'renamed');
  await sweep();
  await eventually(() => read(command), row => row?.settlement?.state === 'accepted');
  await github.rename('renamed', 'user-91');
  await github.forget('user-91');
  // #65: retained sweep-set discovery across installation provenance, with the
  // replacement established through the production account installation owner.
  const historicalInput = { ...creation(), approval_required: false };
  const historical = (handler, body) => http(`${ingress}/InvitationLink/${historicalInput.link_id}/${handler}`, body);
  await historical('create', historicalInput);
  const admittedHistorical = await historical('admit', { ...admitCapability(historicalInput.link_id), link_id: historicalInput.link_id, operation_id: id(), requester_id: 91 });
  const query = { link_id: historicalInput.link_id, request_id: admittedHistorical.result.request_id, requester_id: 91 };
  const historicalPlan = await http(`${ingress}/InvitationRequest/${query.request_id}/approved_plan`, query);
  await eventually(() => storage('request', query.request_id), row => row?.state === 'approved');
  const originalLink = await storage('link', historicalInput.link_id);
  const originalRequest = await storage('request', query.request_id);
  const account = (handler, body) => http(`${ingress}/AccountInstallation/repo:100/${handler}`, body);
  await account('uninstall', { app:'repo', installation_id: 1, uninstalled_at: at });
  // On GitHub too: the App's installation on acme is now 19.
  await github.emulator.admin('DELETE', '/installations/1');
  await github.emulator.admin('POST', '/installations', { installation: { id: 19, app_id: 123, account: 'acme',
    repository_selection: 'selected', repositories: ['acme/api', 'acme/web'] } }, 201);
  github.installation = 19;
  await account('onboard', setupCommand({ app:'repo', installation_id: 19, account_id: 100, account_login: 'acme', account_type: 'Organization',
    actor_user_id: 7, selected_repos: 'all', installed_at: at }));
  const historicalCommand = historicalPlan.commands[0];
  const deliveryUrl = `${ingress}/RepositoryDelivery/${historicalCommand.request_id}:${historicalCommand.repo_id}:${historicalCommand.delivery_generation}`;
  const delivered = await eventually(() => http(`${deliveryUrl}/create`, historicalCommand), value => value.outcome.kind === 'created');
  assert.equal(delivered.command.installation_id, 1);
  await registered(historicalCommand);
  await github.accept('user-91', { id: delivered.outcome.upstream_id, role: 'push' });
  // GitHub reports the installation under another account ID (an injected
  // answer: GitHub never moves an installation between accounts).
  const mismatch = await github.emulator.fault({ method: 'GET', path: '/app/installations/19', label: 'account mismatch', action: { kind: 'respond', status: 200,
    body: { id: 19, app_id: 123, account: { id: 200, login: 'acme', type: 'Organization' }, repository_selection: 'selected',
      permissions: { administration: 'write', metadata: 'read' }, events: [], created_at: at, updated_at: at, suspended_at: null } } });
  await sweep();
  assert.ok(pending(await read(historicalCommand)), 'another account cannot settle history');
  await github.emulator.removeFault(mismatch);
  await sweep();
  const settled = await eventually(() => read(historicalCommand), row => row?.settlement?.state === 'accepted');
  assert.equal(settled.settlement.state, 'accepted');
  assert.equal(settled.create.outcome.upstream_id, delivered.outcome.upstream_id);
  assert.deepEqual(await storage('link', historicalInput.link_id), originalLink);
  assert.deepEqual(await storage('request', query.request_id), originalRequest);
  assert.deepEqual(await http(`${ingress}/InvitationRequest/${query.request_id}/approved_plan`, query), historicalPlan);
  assert.deepEqual((await http(`${deliveryUrl}/status`)).create, delivered);
  const accepted = (await auditHistory(storage, 100)).filter(event => event.target_id === key(historicalCommand) && event.event_type === 'invitation.accepted');
  assert.equal(accepted.length, 1);
  assert.deepEqual(accepted[0].metadata, { reconciled: true, evidence: { kind: 'effective_role', role_name: 'write' } });
  await github.forget('user-91');
  // A blocked PUT queues refused legacy cancellation or stale canonical evidence.
  // Actual complete observation after the race establishes GitHub expiry.
  for (const handler of ['cancel', 'reconcile']) {
    const input = { ...creation(), installation_id: 19, approval_required: false };
    await http(`${ingress}/InvitationLink/${input.link_id}/create`, input);
    const admitted = await http(`${ingress}/InvitationLink/${input.link_id}/admit`, { ...admitCapability(input.link_id), link_id: input.link_id, operation_id: id(), requester_id: 91 });
    const plan = await http(`${ingress}/InvitationRequest/${admitted.result.request_id}/approved_plan`, { link_id: input.link_id, request_id: admitted.result.request_id, requester_id: 91 });
    const command = plan.commands[0];
    const url = `${ingress}/RepositoryDelivery/${command.request_id}:${command.repo_id}:${command.delivery_generation}`;
    const held = await github.holdPut();
    let created;
    try {
      const creating = http(`${url}/create`, command);
      await Promise.race([held.entered, new Promise((_, reject) => setTimeout(() => reject(new Error('PUT did not reach race barrier')), 5000))]);
      // Durable send acknowledgement proves the command queued while the real
      // owner is still awaiting the PUT, rather than after creation completed.
      const staleEvidence={delivery_key:key(command),expected_revision:0,at:new Date().toISOString(),evidence:{kind:'absent',availability_verified:true,role_name:'none',direct_collaborator:null,membership:null}};
      const withdrawal = await http(`${url}/${handler}/send`, handler==='cancel' ? {invitation_id:id(),installation_id:19,by_user:7,at:new Date().toISOString()} : staleEvidence);
      held.release();
      created = await creating;
      if(handler==='cancel') await assert.rejects(()=>http(`${ingress}/restate/invocation/${withdrawal.invocationId}/attach`, undefined, 'GET'),/409/);
      else await http(`${ingress}/restate/invocation/${withdrawal.invocationId}/attach`, undefined, 'GET');
    } finally { held.release(); }
    let winner = await http(`${url}/status`);
    if(handler==='cancel') {
      assert.equal(winner.settlement,null,'legacy input cannot authorize an owned DELETE');
      assert.deepEqual(winner.create,created);
      assert.equal((await github.requests('DELETE',`/repos/acme/api/invitations/${created.outcome.upstream_id}`)).length,0);
      assert.deepEqual(await http(`${url}/create`,command),created);
      // Explicit upstream fixture cleanup after all no-DELETE assertions keeps
      // the next expiry race independent; this is not a workflow cancellation.
      await github.api('DELETE', `/repos/acme/api/invitations/${created.outcome.upstream_id}`, undefined, { expected: 204 });
      continue; // Positive cancellation races run in the isolated real-budget fixture.
    }

    if (handler === 'reconcile') {
      assert.equal(winner.settlement, null, 'a listed unexpired invitation is not GitHub expiry');
      assert.deepEqual(winner.create, created);
      const deletesBefore = (await github.requests('DELETE')).length;
      await github.expire(created.outcome.upstream_id);
      await http(`${ingress}/DeliveryObservation/observe`,winner);
      winner = await eventually(()=>http(`${url}/status`),value=>value?.settlement?.state==='expired');
      assert.equal((await github.requests('DELETE')).length, deletesBefore, 'GitHub expiry observation never deletes');
    }
    assert.equal(winner.settlement.state, handler === 'cancel' ? 'cancelled' : 'expired');
    assert.deepEqual(winner.create, created);
    await http(`${url}/reconcile`,{delivery_key:key(command),expected_revision:winner.revision,at,evidence:{kind:'absent',availability_verified:true,role_name:'none',direct_collaborator:null,membership:null}});
    assert.deepEqual(await http(`${url}/status`), winner);
    assert.deepEqual(await http(`${url}/create`, command), created);
  }
  console.log('PASS #65 real Restate + D1 historical delivery through replacement, account mismatch and missed-webhook recovery');
  pause(false);
  console.log('PASS real Restate + D1 retained settlement, atomic projection/audit, lost batch acknowledgement and blocked-create recovery');
}
