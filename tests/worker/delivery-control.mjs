import assert from 'node:assert/strict';
import { setupCommand, establishInstallingOwner } from './installation-fixture.mjs';
import { admitCapability } from './capability.mjs';

export async function deliveryControl({ingress,githubUrl,http,db,id,creation,eventually,storage}) {
  for (const user of [7,91]) await db.prepare("INSERT INTO users(user_id,login,avatar_url,last_seen_at) VALUES (?, ?, NULL, '2026-01-01T00:00:00Z')").bind(user,`user-${user}`).run();
  await http(`${ingress}/Installation/repo:1/onboard`,setupCommand({app:'repo',installation_id:1,account_id:100,account_login:'acme',account_type:'Organization',selected_repos:'all',installed_at:'2026-01-01T00:00:00Z'}));
  await establishInstallingOwner({http,ingress});
  await http(`${githubUrl}/identity`,{login:'user-91',addressed_id:91});
  const calls=()=>http(`${githubUrl}/calls`,undefined,'GET');
  const writes=async()=> (await calls()).requests.filter(r=>r.method==='PUT');
  const auditFacts=async key=>(await db.prepare("SELECT id,event_type,occurred_at,target_kind,target_id,metadata FROM audit_events WHERE json_extract(metadata,'$.delivery_id')=? AND event_type IN ('delivery.blocked','delivery.outcome_unknown','delivery.held','delivery.released') ORDER BY json_extract(metadata,'$.transition_revision'),event_type").bind(key).all()).results.map(row=>({...row,metadata:JSON.parse(row.metadata)}));


  const assertCanonicalCommand=(command,key)=> {
    assert.equal(Object.hasOwn(command,'invitation_id'),false,'new commands allocate no legacy invitation ULID');
    assert.equal(command.repo_id,10);
    assert.equal(command.delivery_generation,1);
    assert.equal(`${command.request_id}:${command.repo_id}:${command.delivery_generation}`,key);
  };
  const assertCanonicalReceipt=(receipt,key)=> {
    assert.equal(receipt.audit_version,'canonical_v2');
    assertCanonicalCommand(receipt.command,key);
  };
  const assertCanonicalFacts=(facts,key)=> {
    for(const fact of facts) {
      assert.equal(fact.target_kind,'repository_delivery');
      assert.equal(fact.target_id,key);
      assert.equal(fact.metadata.delivery_id,key);
      assert.equal(fact.metadata.delivery_generation,1);
      assert.deepEqual(fact.metadata.step,{kind:'repository'});
    }
  };
  const assertFence=async(command,key)=> {
    const rows=(await db.prepare('SELECT delivery_key,step,command,attempt_generation,retryable FROM delivery_attempts WHERE delivery_key=?').bind(key).all()).results;
    assert.equal(rows.length,1,'one physical generation and repository step owns the write fence');
    assert.equal(rows[0].delivery_key,key);
    assert.equal(rows[0].step,'repo');
    assert.equal(rows[0].attempt_generation,1,'lost run acknowledgements do not allocate another write attempt');
    assert.equal(rows[0].retryable,0);
    assert.deepEqual(JSON.parse(rows[0].command),command);
  };

  const fixture=async()=> {
    const create={...creation(),max_uses:3,approval_required:true};
    const link_id=create.link_id;
    await http(`${ingress}/InvitationLink/${link_id}/create`,create);
    const admission=await http(`${ingress}/InvitationLink/${link_id}/admit`,{...admitCapability(link_id),link_id,requester_id:91,operation_id:id()});
    assert.equal(admission.result.kind,'accepted');
    const request_id=admission.result.request_id;
    const query={link_id,request_id,requester_id:91};
    const request=await http(`${ingress}/InvitationRequest/${request_id}/request_status`,query);
    const key=`${request_id}:10:1`;
    const call=(method,input)=>http(`${ingress}/RepositoryDelivery/${key}/${method}`,input);
    const identity={link_id,request_id,kind:'repository',target_id:10,delivery_generation:1};
    const window={epoch:100,window_start:request.admitted_at,paused_at:new Date().toISOString()};
    const control=(revision,hold_epoch,control)=>({envelope:{identity,operation_id:id(),revision,hold_epoch,at:new Date().toISOString(),control},admitted_at:request.admitted_at,invitation_lifetime_seconds:604800});
    const approve=async()=> {
      const decision={link_id,request_id,operation_id:id(),admin:create.admin,action:{kind:'approve'}};
      const receipt=await http(`${ingress}/InvitationRequest/${request_id}/decide`,decision);
      assert.equal(receipt.outcome,'applied');
      const plan=await http(`${ingress}/InvitationRequest/${request_id}/approved_plan`,query);
      assert.equal(plan.commands.length,1);
      assertCanonicalCommand(plan.commands[0],key);
      return {decision,receipt,command:plan.commands[0]};
    };
    return {key,call,identity,window,control,approve};
  };
  const held=await fixture();
  const hold=held.control(100,100,{kind:'auto_pause',window:held.window});
  const original=await held.call('control',hold);
  assert.equal(original.disposition.kind,'held');assert.equal(original.initialized,false);
  const approved=await held.approve();
  const parked=await eventually(()=>held.call('status'),v=>v?.create.outcome.kind==='held');
  assertCanonicalReceipt(parked.create,held.key);
  assert.deepEqual(parked.create.command,approved.command);assert.equal((await writes()).length,0);
  assert.deepEqual(await held.call('control',hold),original);
  const release=held.control(101,100,{kind:'active'});
  const released=await held.call('control',release);
  const confirmed=(await eventually(()=>held.call('status'),v=>v?.create.outcome.kind==='created')).create;
  assert.equal(confirmed.audit_version,parked.create.audit_version);
  assertCanonicalReceipt(confirmed,held.key);
  await assertFence(confirmed.command,held.key);
  assert.equal((await writes()).length,1);
  assert.equal(await storage('delivery-run-losses',null),3,'real guarded run retries preserve a single external write');
  assert.deepEqual(await held.call('create',approved.command),confirmed);
  assert.deepEqual(await held.call('control',release),released);
  assert.deepEqual(await held.call('control',hold),original);
  for(const generation of [1,2,2]) await held.call('control_wake',{identity:held.identity,generation});
  assert.equal((await writes()).length,1);
  assert.deepEqual(await http(`${ingress}/InvitationRequest/${approved.command.request_id}/decide`,approved.decision),approved.receipt);
  const projected=await eventually(()=>db.prepare('SELECT create_outcome,held FROM deliveries WHERE id=?').bind(held.key).first(),v=>v?.create_outcome==='created');
  assert.equal(projected.held,0);
  const persisted=await db.prepare("SELECT receipt FROM delivery_outcomes WHERE delivery_id=?").bind(held.key).first();
  assert.deepEqual(JSON.parse(persisted.receipt).create,confirmed);
  const holdHistory=await eventually(()=>auditFacts(held.key),rows=>rows.length===2);
  assertCanonicalFacts(holdHistory,held.key);
  assert.deepEqual(holdHistory.map(row=>row.event_type),['delivery.held','delivery.released']);
  assert.deepEqual(holdHistory.map(row=>row.metadata.transition.hold_epoch),[100,100]);
  assert.deepEqual(await held.call('create',approved.command),confirmed);
  assert.deepEqual(await auditFacts(held.key),holdHistory,'replayed creates/control/wakes preserve audit identity and time');


  const aborted=await fixture();
  const revoke=aborted.control(101,100,{kind:'revoked',cancel_pending_invitations:false,decline_pending_requests:false,abort_hold:aborted.window});
  assert.equal((await aborted.call('control',revoke)).disposition.kind,'aborted');
  const approvedAbort=await aborted.approve();
  const stopped=(await eventually(()=>aborted.call('status'),v=>v?.create.outcome.kind==='aborted')).create;
  assertCanonicalReceipt(stopped,aborted.key);
  assert.equal((await db.prepare('SELECT COUNT(*) AS count FROM delivery_attempts WHERE delivery_key=?').bind(aborted.key).first()).count,0,'pre-create abort claims no write step');
  assert.deepEqual(await aborted.call('create',approvedAbort.command),stopped);
  await aborted.call('control_wake',{identity:aborted.identity,generation:1});
  assert.equal((await writes()).length,1);
  await eventually(()=>db.prepare('SELECT create_outcome FROM deliveries WHERE id=?').bind(aborted.key).first(),v=>v?.create_outcome==='aborted');
  assert.deepEqual(await auditFacts(aborted.key),[],'pre-create revoke never fabricates a Held or Released episode');


  // An already-applied create with a lost HTTP response remains uncertain until
  // positive presence resolves it. Cascade retains intent and never DELETEs.
  await http(`${githubUrl}/reset`,undefined,'DELETE');
  await http(`${githubUrl}/identity`,{login:'user-91',addressed_id:91});
  await http(`${githubUrl}/outcomes`,{owner:'acme',repo:'api',user:'user-91',outcome:'created_response_lost'});
  const uncertain=await fixture();
  const approvedUnknown=await uncertain.approve();
  const unknownSnapshot=await eventually(()=>uncertain.call('status'),v=>v?.create.outcome.kind==='outcome_unknown');
  assertCanonicalReceipt(unknownSnapshot.create,uncertain.key);
  const cascade=uncertain.control(100,0,{kind:'revoked',cancel_pending_invitations:true,decline_pending_requests:false,abort_hold:null});
  const pending=await uncertain.call('control',cascade);
  assert.equal(pending.disposition.kind,'recover_unknown');
  const recovered=(await eventually(()=>uncertain.call('status'),v=>v?.create.outcome.kind==='created')).create;
  assert.equal(recovered.audit_version,unknownSnapshot.create.audit_version);
  assertCanonicalReceipt(recovered,uncertain.key);
  await assertFence(recovered.command,uncertain.key);
  assert.deepEqual(await uncertain.call('create',approvedUnknown.command),recovered);
  assert.equal((await writes()).length,1);
  assert.equal((await calls()).requests.filter(r=>r.method==='DELETE').length,0);
  assert.deepEqual(await uncertain.call('control',cascade),pending,'original operation receipt survives resolution');
  const unknownHistory=await eventually(()=>auditFacts(uncertain.key),rows=>rows.length===1);
  assertCanonicalFacts(unknownHistory,uncertain.key);
  assert.equal(unknownHistory[0].event_type,'delivery.outcome_unknown');
  assert.equal(unknownHistory[0].metadata.transition_revision,unknownSnapshot.revision);
  assert.deepEqual(await uncertain.call('create',approvedUnknown.command),recovered);
  assert.deepEqual(await auditFacts(uncertain.key),unknownHistory);

  // Park before approval dispatch, then release into a fresh negative numeric
  // identity read. Rechecks of the same bounded reason are one audit episode.
  await http(`${githubUrl}/reset`,undefined,'DELETE');
  await http(`${githubUrl}/identity`,{login:'user-91',addressed_id:91});
  const blocked=await fixture();
  await blocked.call('control',blocked.control(100,100,{kind:'auto_pause',window:blocked.window}));
  const blockedApproved=await blocked.approve();
  await eventually(()=>blocked.call('status'),v=>v?.create.outcome.kind==='held');
  await http(`${githubUrl}/identity`,{login:'user-91',addressed_id:999});
  await blocked.call('control',blocked.control(101,100,{kind:'active'}));
  await eventually(()=>blocked.call('status'),v=>v?.create.outcome.kind==='blocked');
  for(let n=0;n<2;n++) assert.equal((await blocked.call('create',blockedApproved.command)).outcome.kind,'blocked');
  const blockedHistory=await eventually(()=>auditFacts(blocked.key),rows=>rows.length===3);
  assertCanonicalFacts(blockedHistory,blocked.key);
  assert.equal(blockedHistory.filter(row=>row.event_type==='delivery.blocked').length,1);
  assert.deepEqual(blockedHistory.find(row=>row.event_type==='delivery.blocked').metadata.transition.reason,{kind:'repository',reason:'requester_identity'});
  assert.equal((await writes()).length,0);

  console.log('PASS signed Worker/D1 delivery S0: pre-create hold/abort, release wake, three lost run ACKs/one PUT, exact historical control/decision replay, projection, unknown presence recovery, no cancellation DELETE, and durable Held/Released/Blocked/Unknown audit history');
}
