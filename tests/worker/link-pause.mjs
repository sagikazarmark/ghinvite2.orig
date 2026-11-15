import assert from 'node:assert/strict';
import { setupCommand, establishInstallingOwner } from './installation-fixture.mjs';
import { admitCapability } from './capability.mjs';
import { revokeCommand } from './revocation-fixture.mjs';

export async function linkPause({ ingress,githubUrl,http,db,id,creation,eventually }) {
  // Projection parents normally come from sign-in; direct owner calls bypass it.
  for (const user of [7, 91]) {
    await db.prepare("INSERT INTO users(user_id,login,avatar_url,last_seen_at) VALUES (?, ?, NULL, '2026-01-01T00:00:00Z')")
      .bind(user, `user-${user}`).run();
  }
  await http(`${ingress}/Installation/repo:1/onboard`,setupCommand({app:'repo',installation_id:1,account_id:100,account_login:'acme',account_type:'Organization',selected_repos:'all',installed_at:'2026-01-01T00:00:00Z'}));
  const command={...creation(),max_uses:3};
  const link=command.link_id;
  const call=(handler,input)=>http(`${ingress}/InvitationLink/${link}/${handler}`,input);
  const lifecycle=()=>({link_id:link,admin:command.admin,operation_id:id()});
  await call('create',command);
  const attempt=()=>({link_id:link,requester_id:91,operation_id:id(),justification:'Request',...admitCapability(link)});
  const first=attempt(), accepted=await call('admit',first);
  assert.equal(accepted.result.kind,'accepted');
  const pause=lifecycle(), paused=await call('pause',pause);
  assert.equal(paused.Ok.pause.reason.kind,'manual');
  const pageInput={link_id:link,requester_id:92,...admitCapability(link)};
  delete pageInput.scope_evidence;
  const page=await call('requester_page',pageInput);
  assert.equal(page.paused,true);assert.equal(page.can_start_fresh,false);
  assert.equal(page.pause,undefined);
  const invalidPage=await fetch(`${ingress}/InvitationLink/${link}/requester_page`,{method:'POST',headers:{'content-type':'application/json'},body:JSON.stringify({...pageInput,secret_sha256:'99'.repeat(32)})});
  assert.equal(invalidPage.status,404);await invalidPage.arrayBuffer();

  const fresh=attempt(), refused=await call('admit',fresh);
  assert.equal(refused.result.reason,'paused');
  const row=await eventually(()=>db.prepare('SELECT * FROM invitation_links WHERE id=?').bind(link).first(),value=>value?.state==='paused');
  assert.equal(row.uses_count,1);
  assert.equal(row.paused_by,7);
  assert.equal(row.pause_reason,'manual');
  assert.equal(row.pause_epoch,paused.Ok.pause.epoch);
  assert.deepEqual(await call('admit',first),accepted);
  const unknown={...attempt(),secret_sha256:'99'.repeat(32)};
  assert.equal((await call('admit',unknown)).result.reason,'not_available');
  assert.deepEqual(await call('pause',lifecycle()),{Err:'already_paused'});
  const resume=lifecycle(), resumed=await call('resume',resume);
  assert.equal(resumed.Ok.pause,undefined);
  await eventually(()=>db.prepare('SELECT * FROM invitation_links WHERE id=?').bind(link).first(),value=>value?.state==='active' && value.pause_epoch===null && value.paused_at===null);
  assert.deepEqual(await call('pause',pause),paused);
  assert.deepEqual(await call('resume',resume),resumed);
  assert.deepEqual(await call('admit',fresh),refused);
  const changed=await fetch(`${ingress}/InvitationLink/${link}/resume`,{method:'POST',headers:{'content-type':'application/json'},body:JSON.stringify(pause)});
  assert.equal(changed.status,409);await changed.arrayBuffer();
  const missingOperation=await fetch(`${ingress}/InvitationLink/${link}/revoke`,{method:'POST',headers:{'content-type':'application/json'},body:JSON.stringify({link_id:link,admin:command.admin})});
  assert.equal(missingOperation.status,400);await missingOperation.arrayBuffer();
  const revocation=revokeCommand(command,id()), revoked=await call('revoke',revocation);
  assert.ok(revoked.Ok);
  assert.deepEqual(await call('revoke',revocation),revoked);
  const revokeConflict=await fetch(`${ingress}/InvitationLink/${link}/revoke`,{method:'POST',headers:{'content-type':'application/json'},body:JSON.stringify({...revocation,cascade:{...revocation.cascade,decline_pending_requests:true}})});
  assert.equal(revokeConflict.status,409);await revokeConflict.arrayBuffer();
  assert.deepEqual(await call('revoke',revokeCommand(command,id())),{Err:'revoked'});
  assert.deepEqual(await call('admit',first),accepted);
  assert.deepEqual(await call('resume',lifecycle()),{Err:'revoked'});
  assert.deepEqual(await call('pause',pause),paused);
  const audit=await eventually(()=>db.prepare("SELECT event_type,COUNT(*) AS count FROM audit_events WHERE target_id=? AND event_type IN ('invitation_link.paused','invitation_link.resumed') GROUP BY event_type ORDER BY event_type").bind(link).all(),value=>value.results.length===2);
  assert.deepEqual(audit.results,[{event_type:'invitation_link.paused',count:1},{event_type:'invitation_link.resumed',count:1}]);
  await establishInstallingOwner({http,ingress});
  await http(`${githubUrl}/identity`,{login:'user-91',addressed_id:91});
  const writes=async()=> (await http(`${githubUrl}/calls`,undefined,'GET')).requests.filter(row=>['PUT','DELETE'].includes(row.method)).length;
  for(const revoke of [false,true]) {
    const command={...creation(),max_uses:null,approval_required:revoke,abuse_threshold_per_hour:1,
      expires_at:revoke?null:new Date(Date.now()+86400000).toISOString()};
    const link=command.link_id, call=(handler,input)=>http(`${ingress}/InvitationLink/${link}/${handler}`,input);
    await call('create',command);
    if(!revoke) {
      await http(`${githubUrl}/installation-repositories`,[]);
      await http(`${ingress}/Installation/repo:1/repos_changed`,{app:'repo',installation_id:1,selected_repos:[]});
      assert.deepEqual((await http(`${ingress}/AccountInstallation/repo:100/current_installation`)).selected_repos,[]);
    }
    const attempt={link_id:link,requester_id:91,operation_id:id(),...admitCapability(link)};
    const accepted=await call('admit',attempt), request_id=accepted.result.request_id;
    assert.equal(accepted.result.kind,'accepted');
    const delivery=()=>http(`${ingress}/RepositoryDelivery/${request_id}:10:1/status`);
    if(!revoke) {
      assert.equal(accepted.result.state,'approved');
      await eventually(delivery,v=>v?.create.outcome.kind==='blocked');
    }
    const blocked={...attempt,requester_id:92,operation_id:request_id};
    assert.equal((await call('admit',blocked)).result.reason,'paused');
    assert.deepEqual(await call('admit',attempt),accepted);
    const row=await eventually(()=>db.prepare('SELECT * FROM invitation_links WHERE id=?').bind(link).first(),v=>v?.state==='paused');
    assert.equal(row.uses_count,1);assert.equal(row.pause_reason,'abuse_suspected');assert.equal(row.paused_by,null);
    assert.equal(row.abuse_threshold,1);assert.equal(row.invitation_lifetime_secs,604800);
    const before=await writes();
    const decision={link_id:link,request_id,operation_id:id(),admin:command.admin,action:{kind:'approve'}};
    const approved=await http(`${ingress}/InvitationRequest/${request_id}/decide`,decision);
    assert.equal(approved.request.state,'approved');
    await eventually(delivery,v=>v?.create.outcome.kind==='held');
    assert.equal(await writes(),before);
    if(revoke) {
      const revocation=revokeCommand(command,id()), revoked=await call('revoke',revocation);
      assert.ok(revoked.Ok);
      await eventually(delivery,v=>v?.create.outcome.kind==='aborted');
      assert.equal(await writes(),before);
      assert.deepEqual(await call('revoke',revocation),revoked);
      assert.deepEqual(await call('admit',attempt),accepted);
    } else {
      await http(`${githubUrl}/installation-repositories`,[10,11]);
      await http(`${ingress}/Installation/repo:1/repos_changed`,{app:'repo',installation_id:1,selected_repos:[10,11]});
      assert.equal(await writes(),before);
      const resume={link_id:link,admin:command.admin,operation_id:id()};
      const resumed=await call('resume',resume);
      await eventually(delivery,v=>v?.create.outcome.kind==='created');
      assert.equal(await writes(),before+1);
      assert.deepEqual(await call('resume',resume),resumed);
    }
    assert.deepEqual(await http(`${ingress}/InvitationRequest/${request_id}/decide`,decision),approved);
    const audited=await eventually(()=>db.prepare("SELECT COUNT(*) AS count FROM audit_events WHERE target_id=? AND event_type='invitation_link.auto_paused'").bind(link).first(),v=>v?.count===1);
    assert.equal(audited.count,1);
  }
  // Unlimited links default to 20 admissions per sliding hour. Concurrent
  // arrivals are serialized by the same owner that pauses and emits holds.
  const unlimited={...creation(),max_uses:null,approval_required:true};
  await http(`${ingress}/InvitationLink/${unlimited.link_id}/create`,unlimited);
  const attempts=[];
  for(let user=200;user<221;user++) {
    await db.prepare("INSERT INTO users(user_id,login,avatar_url,last_seen_at) VALUES (?, ?, NULL, '2026-01-01T00:00:00Z')").bind(user,`user-${user}`).run();
    attempts.push({link_id:unlimited.link_id,requester_id:user,operation_id:id(),...admitCapability(unlimited.link_id)});
  }
  // The owner admits one at a time (~0.8 s each locally, slower on hosted
  // runners), so the last of 21 waits for all others: give it room.
  const outcomes=await Promise.all(attempts.map(input=>http(`${ingress}/InvitationLink/${unlimited.link_id}/admit`,input,'POST',90_000)));
  assert.equal(outcomes.filter(value=>value.result.kind==='accepted').length,20);
  assert.equal(outcomes.filter(value=>value.result.reason==='paused').length,1);
  const counted=await eventually(()=>db.prepare('SELECT * FROM invitation_links WHERE id=?').bind(unlimited.link_id).first(),v=>v?.state==='paused');
  assert.equal(counted.uses_count,20);assert.equal(counted.abuse_threshold,null);
  const firstAccepted=outcomes.findIndex(value=>value.result.kind==='accepted');
  assert.deepEqual(await http(`${ingress}/InvitationLink/${unlimited.link_id}/admit`,attempts[firstAccepted]),outcomes[firstAccepted]);
  console.log('PASS signed Worker/D1 manual pause/resume: admission fence, exact retained results, projection, once-only audit, automatic hold/approval/release/revoke and preserved receipts');
}
