import assert from 'node:assert/strict';
import { setupCommand } from './installation-fixture.mjs';
import { admitCapability } from './capability.mjs';

export async function sweepSchedule({ ingress, githubUrl, http, storage, db, id, creation, eventually, pause, setClock, calls }) {
  pause(true);
  for (const user of [7,91]) await db.prepare("INSERT INTO users(user_id,login,avatar_url,last_seen_at) VALUES (?, ?, NULL, '2026-01-01T00:00:00Z')").bind(user,`user-${user}`).run();
  await http(`${ingress}/Installation/repo:1/onboard`,setupCommand({app:'repo',installation_id:1,account_id:100,account_login:'acme',account_type:'Organization',selected_repos:'all',installed_at:'2026-01-01T00:00:00Z'}));
  const schedule=(handler,body)=>http(`${ingress}/SweepSchedule/repo:100/${handler}`,body);
  let idle=await eventually(()=>schedule('status'),state=>state?.next_run);
  assert.equal(idle.cadence_seconds,86400);
  setClock(Date.parse(idle.next_run));
  await schedule('run',idle.token);
  idle=await schedule('status');
  assert.equal(idle.completed_runs,1);
  const t=Date.parse(idle.next_run)-86400000+60000;
  setClock(t);
  await http(`${githubUrl}/identity`,{login:'user-91',addressed_id:91,role_name:'none'});
  const commands=[];
  // Parent projections exist; only delivery projection is unavailable.
  for(let i=0;i<2;i++) {
    const input={...creation(),approval_required:false}; input.targets.repos[0].grant='write';
    await http(`${ingress}/InvitationLink/${input.link_id}/create`,input);
    const admitted=await http(`${ingress}/InvitationLink/${input.link_id}/admit`,{...admitCapability(input.link_id),link_id:input.link_id,operation_id:id(),requester_id:91});
    const request=admitted.result.request_id;
    const plan=await http(`${ingress}/InvitationRequest/${request}/approved_plan`,{link_id:input.link_id,request_id:request,requester_id:91});
    await eventually(()=>storage('request',request),row=>row?.state==='approved');
    commands.push(plan.commands[0]);
  }
  await db.prepare("CREATE TRIGGER sweep_projection_failure BEFORE INSERT ON deliveries BEGIN SELECT RAISE(ABORT,'fixture delivery projection unavailable'); END").run();
  const delivery=(command,handler,body)=>http(`${ingress}/RepositoryDelivery/${command.request_id}:${command.repo_id}:${command.delivery_generation}/${handler}`,body);
  const created=await delivery(commands[0],'create',commands[0]);
  assert.equal(created.outcome.kind,'created');
  assert.equal((await delivery(commands[1],'create',commands[1])).outcome.kind,'already_pending');
  const active=await eventually(()=>schedule('status'),state=>state?.tracked?.['10']?.length===2);
  assert.equal(active.token,idle.token+1);
  assert.equal(Date.parse(active.next_run),t+3600000);
  assert.equal((await db.prepare('SELECT COUNT(*) AS n FROM deliveries').first()).n,0);
  await schedule('run',idle.token);
  assert.equal((await schedule('status')).token,active.token);
  const pendingSnapshot=await delivery(commands[0],'status');
  await new Promise(resolve=>setTimeout(resolve,20));
  const observedAt=Date.now();
  const observation={delivery_key:`${commands[0].request_id}:10:1`,expected_revision:pendingSnapshot.revision,
    at:new Date(observedAt).toISOString(),evidence:{kind:'listed',upstream_id:created.outcome.upstream_id,expired:false}};
  await delivery(commands[0],'reconcile',observation);
  assert.ok(observedAt-1>Date.parse(pendingSnapshot.create.confirmed_at));
  await delivery(commands[0],'reconcile',{...observation,at:new Date(observedAt-1).toISOString(),
    evidence:{kind:'absent',availability_verified:true,role_name:'none',direct_collaborator:null,membership:null}});
  assert.deepEqual(await delivery(commands[0],'status'),pendingSnapshot,
    'a newer no-change listing protects the same revision from stale absence');
  await http(`${githubUrl}/repos/acme/api/invitations/${created.outcome.upstream_id}`,undefined,'DELETE');
  await http(`${githubUrl}/identity`,{login:'user-91',addressed_id:91,role_name:'write'});
  setClock(t+300000);
  await schedule('run',active.token);
  assert.equal((await delivery(commands[0],'status')).settlement,null);
  calls.length=0;
  setClock(t+3600000);
  await schedule('run',active.token);
  for(const command of commands) await eventually(()=>delivery(command,'status'),value=>value?.settlement?.state==='accepted');
  const emptied=await eventually(()=>schedule('status'),state=>Object.keys(state.tracked).length===0);
  assert.equal(emptied.completed_runs,2);
  assert.equal(calls.filter(call=>call.method==='GET'&&call.path==='/repos/acme/api/invitations').length,1,'one repository list shared by both delivery observations');
  assert.equal((await db.prepare('SELECT COUNT(*) AS n FROM deliveries').first()).n,0,'actual D1 outage cannot hide or prevent retained acceptance');
  assert.ok(calls.every(call=>!['PUT','PATCH','DELETE'].includes(call.method)),'scheduled recovery is read only');
  await schedule('run',active.token);
  assert.equal((await schedule('status')).completed_runs,2,'duplicate token never forks a chain');
  const registration={target_id:10,delivery_key:`${commands[0].request_id}:10:1`};
  await schedule('track',registration);
  assert.deepEqual((await schedule('status')).tracked,{},'late registration cannot resurrect terminal work');
  setClock(Date.parse(emptied.next_run));
  await schedule('run',emptied.token);
  const idleAgain=await schedule('status');
  assert.equal(idleAgain.cadence_seconds,86400);
  assert.equal(idleAgain.token,emptied.token+1);
  await db.prepare('DROP TRIGGER sweep_projection_failure').run();
  for(const command of commands) await eventually(()=>storage('delivery-by-key',`${command.request_id}:${command.repo_id}:${command.delivery_generation}`),row=>row?.settlement?.state==='accepted');
  const events=await db.prepare("SELECT COUNT(*) AS n FROM audit_events WHERE event_type='invitation.accepted'").first();
  assert.equal(events.n,2,'D1 catches up with both retained settlement receipts');
  // Successful settlement observations retain their age even after an actual
  // projection outage; the timestamps do not alter creation/lifecycle facts.
  for (const command of commands) {
    const row = await eventually(
      ()=>db.prepare('SELECT unknown_since,last_observed_at,create_outcome,settlement FROM deliveries WHERE request_id=?').bind(command.request_id).first(),
      row=>row?.last_observed_at != null);
    assert.equal(row.unknown_since,null);
    assert.equal(row.settlement,'accepted');
    assert.ok(['created','already_pending'].includes(row.create_outcome));
  }
  setClock(undefined);
  await http(`${githubUrl}/identity`,{login:'user-91',addressed_id:91,role_name:'none'});
  await http(`${githubUrl}/outcomes`,{owner:'acme',repo:'api',user:'user-91',outcome:'transient_once'});
  const input={...creation(),approval_required:false}; input.targets.repos[0].grant='write';
  await http(`${ingress}/InvitationLink/${input.link_id}/create`,input);
  const admitted=await http(`${ingress}/InvitationLink/${input.link_id}/admit`,{...admitCapability(input.link_id),link_id:input.link_id,operation_id:id(),requester_id:91});
  const request=admitted.result.request_id;
  const plan=await http(`${ingress}/InvitationRequest/${request}/approved_plan`,{link_id:input.link_id,request_id:request,requester_id:91});
  await eventually(()=>storage('request',request),row=>row?.state==='approved');
  const unknown=plan.commands[0];
  const putsBefore=calls.filter(call=>call.method==='PUT').length;
  const receipt=await delivery(unknown,'create',unknown);
  assert.equal(receipt.outcome.kind,'outcome_unknown');
  const readUnknown=()=>db.prepare('SELECT * FROM deliveries WHERE request_id=?').bind(request).first();
  const row=await eventually(readUnknown,row=>row?.unknown_since != null);
  assert.equal(row.create_outcome,'outcome_unknown');
  assert.ok(Date.parse(row.unknown_since)>=Date.parse(unknown.approved_at));
  assert.deepEqual(await delivery(unknown,'create',unknown),receipt);
  assert.deepEqual(await readUnknown(),row,'read-only recovery does not reset uncertainty or alter receipt');
  assert.equal(calls.filter(call=>call.method==='PUT').length,putsBefore+1);
  pause(false);
  console.log('PASS actual Restate/workerd/D1 sweep: idle24h→active1h, one chain, shared repository walk, no webhook, stale tokens/track inert, projection outage then convergence, durable observation and immutable unknown timing with one PUT');
}
