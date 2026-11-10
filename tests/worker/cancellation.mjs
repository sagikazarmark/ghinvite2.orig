import assert from 'node:assert/strict';
import { setupCommand, establishInstallingOwner } from './installation-fixture.mjs';
import { admitCapability } from './capability.mjs';
import { revokeCommand } from './revocation-fixture.mjs';

// Runs only against the separately compiled preactivation composition, with
// real Restate owners, D1 fences and InviteBudget. No production enable switch.
export async function cancellation({ingress,githubUrl,http,db,id,creation,eventually,loseNextDelete,lostDeletes,setClock,holdPut}) {
  for (const user of [7,91,92,93,94,95,96,97]) await db.prepare("INSERT INTO users(user_id,login,avatar_url,last_seen_at) VALUES (?, ?, NULL, '2026-01-01T00:00:00Z')")
    .bind(user,`user-${user}`).run();
  await http(`${ingress}/Installation/repo:1/onboard`,setupCommand({app:'repo',installation_id:1,account_id:100,account_login:'acme',account_type:'Organization',selected_repos:'all',installed_at:'2026-01-01T00:00:00Z'}));
  await establishInstallingOwner({http,ingress});
  const budget=()=>http(`${ingress}/InviteBudget/repo:100/snapshot`);
  const calls=()=>http(`${githubUrl}/calls`,undefined,'GET');
  const deletions=async upstream=>(await calls()).requests.filter(row=>row.method==='DELETE' && row.path===`/repos/acme/api/invitations/${upstream}`);
  const fixture=async(user,foreign=false,lifetime=604800,queueCancel=false)=>{
    await http(`${githubUrl}/identity`,{login:`user-${user}`,addressed_id:user});
    if(foreign) await http(`${githubUrl}/repos/acme/api/collaborators/user-${user}`,{permission:'pull'},'PUT');
    const create={...creation(),max_uses:3,approval_required:true,invitation_lifetime_seconds:lifetime};
    await http(`${ingress}/InvitationLink/${create.link_id}/create`,create);
    const admission=await http(`${ingress}/InvitationLink/${create.link_id}/admit`,{...admitCapability(create.link_id),link_id:create.link_id,requester_id:user,operation_id:id()});
    assert.equal(admission.result.kind,'accepted');
    const request_id=admission.result.request_id;
    const query={link_id:create.link_id,request_id,requester_id:user};
    const request=await http(`${ingress}/InvitationRequest/${request_id}/request_status`,query);
    const decision={link_id:create.link_id,request_id,operation_id:id(),admin:create.admin,action:{kind:'approve'}};
    const key=`${request_id}:10:1`, call=(handler,input)=>http(`${ingress}/RepositoryDelivery/${key}/${handler}`,input);
    const control={envelope:{identity:{link_id:create.link_id,request_id,kind:'repository',target_id:10,delivery_generation:1},operation_id:id(),revision:100,hold_epoch:0,at:new Date().toISOString(),
      control:{kind:'revoked',cancel_pending_invitations:true,decline_pending_requests:false,abort_hold:null}},admitted_at:request.admitted_at,invitation_lifetime_seconds:lifetime};
    const held=queueCancel?holdPut():null;
    let approved;
    try {
      approved=await http(`${ingress}/InvitationRequest/${request_id}/decide`,decision);
      assert.equal(approved.outcome,'applied');
      if(held) {
        await Promise.race([held.entered,new Promise((_,reject)=>setTimeout(()=>reject(new Error('PUT did not reach cancellation race barrier')),5000))]);
        // Durable send ACK while PUT is blocked establishes the actual ordering.
        const queued=await call('control/send',control);
        held.release();
        await http(`${ingress}/restate/invocation/${queued.invocationId}/attach`,undefined,'GET');
      }
    } finally {held?.release();}
    const snapshot=await eventually(()=>call('status'),value=>value?.create.outcome.kind===(foreign?'already_pending':'created'));
    const upstream=snapshot.create.outcome.upstream_id;
    return {key,call,control,snapshot,upstream,decision,approved};
  };
  const replay=async fixture=>{
    for(let n=0;n<3;n++) await fixture.call('drive_cancellation');
    assert.deepEqual(await fixture.call('create',fixture.snapshot.create.command),fixture.snapshot.create);
    assert.deepEqual(await http(`${ingress}/InvitationRequest/${fixture.decision.request_id}/decide`,fixture.decision),fixture.approved);
  };

  const owned=await fixture(91), beforeOwned=await budget();
  const originalControl=await owned.call('control',owned.control);
  const cancelled=await eventually(()=>owned.call('status'),value=>value?.settlement?.state==='cancelled');
  assert.deepEqual(cancelled.create,owned.snapshot.create);
  assert.equal(cancelled.settlement.event.metadata.reason,'link_revoked');
  await replay(owned);
  assert.deepEqual(await owned.call('control',owned.control),originalControl);
  assert.equal((await deletions(owned.upstream)).length,1);
  const fences=await db.prepare('SELECT step,attempt_generation,retryable FROM delivery_attempts WHERE delivery_key=? ORDER BY step').bind(owned.key).all();
  assert.deepEqual(fences.results,[{step:'cancel',attempt_generation:1,retryable:0},{step:'repo',attempt_generation:1,retryable:0}]);
  assert.equal((await budget()).content_used_minute,beforeOwned.content_used_minute+1);
  await eventually(()=>db.prepare('SELECT settlement FROM deliveries WHERE id=?').bind(owned.key).first(),row=>row?.settlement==='cancelled');
  console.log('PASS signed cancellation: real budget charge, separate D1 cancel fence, one DELETE and immutable create/control/decision receipts');

  const foreign=await fixture(92,true), beforeForeign=await budget();
  await foreign.call('control',foreign.control);
  const stopped=await eventually(()=>foreign.call('status'),value=>value?.tracking_stopped==='link_revoked');
  assert.deepEqual(stopped.create,foreign.snapshot.create);assert.equal(stopped.settlement,null);
  await replay(foreign);
  assert.equal((await deletions(foreign.upstream)).length,0);
  assert.equal((await db.prepare("SELECT COUNT(*) AS count FROM delivery_attempts WHERE delivery_key=? AND step='cancel'").bind(foreign.key).first()).count,0);
  assert.equal((await budget()).content_used_minute,beforeForeign.content_used_minute);
  console.log('PASS signed cancellation: foreign invitation stops tracking without DELETE, cancellation fence, settlement or budget charge');

  const unknown=await fixture(93), beforeUnknown=await budget();
  loseNextDelete();
  await unknown.call('control',unknown.control);
  await eventually(async()=>({lost:lostDeletes(),calls:(await deletions(unknown.upstream)).length}),value=>value.lost===1&&value.calls===1);
  await replay(unknown);
  const unresolved=await unknown.call('status');
  assert.deepEqual(unresolved.create,unknown.snapshot.create);assert.equal(unresolved.settlement,null);
  assert.equal((await deletions(unknown.upstream)).length,1);
  assert.equal((await budget()).content_used_minute,beforeUnknown.content_used_minute+1);
  assert.deepEqual(await db.prepare("SELECT attempt_generation,retryable FROM delivery_attempts WHERE delivery_key=? AND step='cancel'").bind(unknown.key).first(),{attempt_generation:1,retryable:0});
  console.log('PASS signed cancellation: applied DELETE with lost response remains uncertain, charged and fenced through three later drives');

  const raced=await fixture(95,false,604800,true);
  const raceWinner=await eventually(()=>raced.call('status'),value=>value?.settlement?.state==='cancelled');
  assert.deepEqual(raceWinner.create,raced.snapshot.create);
  assert.equal((await deletions(raced.upstream)).length,1);
  await raced.call('reconcile',{delivery_key:raced.key,expected_revision:raceWinner.revision,at:new Date().toISOString(),evidence:{kind:'absent',availability_verified:true,role_name:'none',direct_collaborator:null,membership:null}});
  assert.deepEqual(await raced.call('status'),raceWinner,'late declined evidence cannot rewrite confirmed cancellation');
  await replay(raced);
  assert.equal((await deletions(raced.upstream)).length,1);
  console.log('PASS signed cancellation: retained control queued behind blocked actual PUT cancels once after create; late declined evidence preserves winner');

  const accepted=await fixture(96), beforeAccepted=await budget();
  // Upstream acceptance: invitation disappears and the numeric user has access.
  await http(`${githubUrl}/repos/acme/api/invitations/${accepted.upstream}`,undefined,'DELETE');
  await http(`${githubUrl}/identity`,{login:'user-96',addressed_id:96,role_name:'read'});
  const acceptanceDeletes=(await deletions(accepted.upstream)).length;
  await accepted.call('control',accepted.control);
  const acceptedWinner=await eventually(()=>accepted.call('status'),value=>value?.settlement?.state==='accepted');
  assert.deepEqual(acceptedWinner.create,accepted.snapshot.create);
  await replay(accepted);
  assert.equal((await deletions(accepted.upstream)).length,acceptanceDeletes,'cancellation sends no DELETE after verified acceptance');
  assert.equal((await budget()).content_used_minute,beforeAccepted.content_used_minute);
  console.log('PASS signed cancellation: fresh verified acceptance wins without cancellation DELETE or charge');

  const cascade=await fixture(97);
  const linkInput={link_id:cascade.snapshot.create.command.link_id,admin:cascade.decision.admin};
  const revocation=revokeCommand(linkInput,id());
  revocation.cascade.cancel_pending_invitations=true;
  const linkCall=(handler,input)=>http(`${ingress}/InvitationLink/${linkInput.link_id}/${handler}`,input);
  const cascadeReceipt=await linkCall('revoke',revocation);
  assert.deepEqual(cascadeReceipt,{Ok:{addressed_requests:0,addressed_deliveries:1}});
  assert.deepEqual(await linkCall('revoke',revocation),cascadeReceipt);
  const cascaded=await eventually(()=>cascade.call('status'),value=>value?.settlement?.state==='cancelled');
  assert.deepEqual(cascaded.create,cascade.snapshot.create);
  assert.equal((await deletions(cascade.upstream)).length,1);
  const progress=await eventually(()=>linkCall('cascade_progress',linkInput),value=>value?.done_deliveries===1);
  assert.deepEqual(progress,{addressed_requests:0,addressed_deliveries:1,done_requests:0,done_deliveries:1,waiting_on_github:0,unavailable:0});
  assert.deepEqual(await linkCall('revoke_status',revocation),cascadeReceipt);
  assert.deepEqual(await linkCall('revoke',revocation),cascadeReceipt);
  const conflicting=await fetch(`${ingress}/InvitationLink/${linkInput.link_id}/revoke`,{method:'POST',headers:{'content-type':'application/json'},body:JSON.stringify({...revocation,cascade:{...revocation.cascade,cancel_pending_invitations:false}})});
  assert.equal(conflicting.status,409);await conflicting.arrayBuffer();
  await replay(cascade);
  assert.equal((await deletions(cascade.upstream)).length,1);
  console.log('PASS signed link cascade: operation-bound revoke captures one delivery, fans out to owned DELETE, retains exact counts and reports completed progress');

  const expiring=await fixture(94,false,3600);
  // Let the actual owner obtain upstream created_at and arrange generation 1.
  await expiring.call('drive_cancellation');
  await expiring.call('tick_own_expiry',{generation:1,upstream_id:expiring.upstream});
  assert.equal((await expiring.call('status')).settlement,null,'early timer cannot cancel');
  assert.equal((await deletions(expiring.upstream)).length,0);
  const chargedHour=(await budget()).content_used_hour;
  for(let retry=1;retry<=3;retry++) {
    setClock(Date.now()+retry*70_000);
    await unknown.call('drive_cancellation');
    assert.equal((await unknown.call('status')).settlement,null);
    assert.equal((await deletions(unknown.upstream)).length,1);
    assert.equal((await budget()).content_used_hour,chargedHour,'due unknown recovery never refunds or acquires another write slot');
  }
  setClock(Date.now()+3_610_000);
  try {
    await expiring.call('tick_own_expiry',{generation:99,upstream_id:expiring.upstream});
    assert.equal((await deletions(expiring.upstream)).length,0,'wrong timer generation cannot cancel');
    await expiring.call('tick_own_expiry',{generation:1,upstream_id:expiring.upstream});
    const expired=await eventually(()=>expiring.call('status'),value=>value?.settlement?.state==='cancelled');
    assert.deepEqual(expired.create,expiring.snapshot.create);
    assert.equal(expired.settlement.event.metadata.reason,'own_expiry');
    await expiring.call('tick_own_expiry',{generation:1,upstream_id:expiring.upstream});
    assert.equal((await deletions(expiring.upstream)).length,1);
  } finally {setClock(undefined);}
  console.log('PASS signed cancellation: early/stale own-expiry ticks make no DELETE; due tick verifies pending and cancels once (fixture clock advanced, no wall-time timer claim)');
}
