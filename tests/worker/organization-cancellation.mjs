import assert from 'node:assert/strict';
import { orgApp } from './github-world.mjs';

// GitHub (octoemu): the organization owner (7) and the organization App;
// each scenario adds its own organization, requester and installation.
export const seed = () => ({ users:[{id:7,login:'owner'}], apps:[orgApp] });

// Production-enabled organization owner, signed Restate calls, real D1 fences
// and budget. Fixture time changes only the Worker clock, never the receipts.
// GitHub is octoemu: invitees accept or decline, invitations fail, an
// organization is flagged `spammed`; a lost DELETE answer is a fault rule.
export async function organizationCancellation({ingress,http,id,db,eventually,github,setClock}) {
  // The Worker's pinned clock and GitHub's move together (JWTs are checked).
  const setTime=async ms=>{setClock(ms);await github.follow(ms);};
  const emulator=github.emulator;
  let next=850;
  const calls=async s=>(await github.calls()).filter(call=>call.path.startsWith(`/orgs/${s.org}/`)||call.path===`/orgs/${s.org}`).map(call=>({method:call.method,path:call.path.split('?')[0]}));
  const open=async s=>(await github.api('GET',`/orgs/${s.org}/invitations`,undefined,{installation:s.installation})).find(value=>value.login===s.login);
  // The invitee declines (the invitation disappears) or accepts it.
  const declineOnGithub=s=>emulator.act(s.login,`orgs/${s.org}/decline`);
  const acceptOnGithub=s=>emulator.act(s.login,`orgs/${s.org}/accept`);
  const fixture=async({foreign=false,lifetime=604800,emailDomains=[],beforeAdmission}={})=>{
    const account=next++,user=account+1000,installation=account*10+2;
    const s={account,user,installation,org:`org-${account}`,login:`user-${user}`};
    await emulator.seed({users:[{id:user,login:s.login}],orgs:[{id:account,login:s.org,created_at:'2020-01-01T00:00:00Z'}],
      memberships:[{org:s.org,user:'owner',role:'admin',state:'active'}],
      // Foreign: the owner invited the requester already, a minute ago.
      org_invitations:foreign?[{id:account*100+1,org:s.org,invitee:s.login,inviter:'owner',role:'direct_member',created_at:new Date(Date.now()-60000).toISOString()}]:[]});
    await emulator.admin('POST','/installations',{installation:{id:installation,app_id:456,account:s.org,repository_selection:'all',created_at:'2026-01-01T00:00:00Z'}},201);
    for(const [uid,login] of [[7,'owner'],[user,s.login]]) await db.prepare('INSERT OR IGNORE INTO users(user_id,login,avatar_url,last_seen_at) VALUES (?, ?, NULL, ?)').bind(uid,login,new Date().toISOString()).run();
    await http(`${ingress}/Installation/org:${installation}/onboard`,{operation_id:id(),verified:{app:'org',account:{installation_id:installation,account_id:account,account_login:s.org,account_type:'Organization',selected_repos:[],installed_at:'2026-01-01T00:00:00Z',uninstalled_at:null},suspended_at:null,permissions:{members:'write',metadata:'read'}}});
    const link=id();
    const create={email_domains:emailDomains,link_id:link,secret_sha256:'3b'.repeat(32),account_id:account,installation_id:installation,admin:{account_id:account,user_id:7},description:'Org lifecycle',max_uses:3,approval_required:false,invitation_lifetime_seconds:lifetime,targets:{organization:{org_id:account,org_login:s.org,grant:'member'},teams:[],repos:[]}};
    assert.ok((await http(`${ingress}/InvitationLink/${link}/create`,create)).Ok);
    const admissionInput={link_id:link,secret_sha256:create.secret_sha256,secret_generation:1,scope_evidence:[],operation_id:id(),requester_id:user};
    if(beforeAdmission) await beforeAdmission({link,admissionInput,s});
    const admission=await http(`${ingress}/InvitationLink/${link}/admit`,admissionInput);
    assert.equal(admission.result.kind,'accepted');
    const request=admission.result.request_id,key=`${request}:${account}:1`;
    const call=(handler,input)=>http(`${ingress}/OrganizationDelivery/${key}/${handler}`,input);
    const snapshot=await eventually(()=>call('status'),v=>v?.create.outcome.kind===(foreign?'already_pending':'created'));
    s.upstream=(await open(s)).id;
    const revoke={link_id:link,admin:create.admin,operation_id:id(),cascade:{decline_pending_requests:false,cancel_pending_invitations:true,remove_granted_access:false}};
    return Object.assign(s,{link,request,key,call,snapshot,admissionInput,admission,revoke,budget:()=>http(`${ingress}/InviteBudget/org:${account}/snapshot`)});
  };
  const deletes=async s=>(await calls(s)).filter(t=>t.method==='DELETE').length;
  const replay=async s=>{
    for(let n=0;n<3;n++) await s.call('drive_cancellation');
    assert.deepEqual((await s.call('create',s.snapshot.create.command)).create,s.snapshot.create);
    assert.deepEqual(await http(`${ingress}/InvitationLink/${s.link}/admit`,s.admissionInput),s.admission);
  };
  try {
    const reconcile=async s=>http(`${ingress}/OrganizationReconcile/target`,{account:{app:'org',account_id:s.account},target_id:s.account,deliveries:[s.key]});
    const domainRules=[{subdomains:'example.com'}];
    const restricted=await fixture({foreign:true,emailDomains:domainRules,beforeAdmission:async({link,admissionInput,s})=>{
      const offerInput={link_id:link,secret_sha256:admissionInput.secret_sha256,secret_generation:1};
      const offer=await http(`${ingress}/InvitationLink/${link}/offer`,offerInput);
      assert.deepEqual(Object.keys(offer).sort(),['account_id','approval_required','email_domains','link_id','paused','targets']);
      assert.deepEqual(offer.email_domains,domainRules);
      assert.equal(offer.account_id,s.account);
      const hidden=await fetch(`${ingress}/InvitationLink/${link}/offer`,{method:'POST',headers:{'content-type':'application/json'},body:JSON.stringify({...offerInput,secret_generation:2}),signal:AbortSignal.timeout(25000)});
      assert.equal(hidden.status,404);
      const checked_at=new Date().toISOString();
      const proof=(domains,at=checked_at,boundLink=link)=>({link_id:boundLink,matched_domains:domains,checked_at:at});
      const mismatch={domain_mismatch:{required:domainRules}};
      for(const [domain_evidence,reason] of [
        [null,'domain_evidence_missing'],
        [proof(['eng.example.com'],new Date(Date.now()-660000).toISOString()),'domain_evidence_stale'],
        [proof(['eng.example.com'],checked_at,id()),'domain_evidence_stale'],
        [proof(['example.com']),mismatch],
        [proof(['badexample.com']),mismatch],
        [proof(['users.noreply.github.com']),mismatch],
        [proof([]),mismatch],
      ]) {
        const attempt={...admissionInput,operation_id:id(),domain_evidence};
        const refused=await http(`${ingress}/InvitationLink/${link}/admit`,attempt);
        assert.deepEqual(refused.result.reason,reason);
        assert.deepEqual(await http(`${ingress}/InvitationLink/${link}/admit`,attempt),refused);
      }
      const current=await http(`${ingress}/InvitationLink/${link}/link_status`,{link_id:link,admin:{account_id:s.account,user_id:7}});
      assert.equal(current.uses,0);
      assert.equal((await calls(s)).filter(t=>t.method==='POST'&&t.path.endsWith('/invitations')).length,0);
      admissionInput.domain_evidence=proof(['ENG.EXAMPLE.COM.']);
    }});
    const domainProjection=await eventually(()=>db.prepare('SELECT domain_evidence_json FROM invitation_requests WHERE id=?').bind(restricted.request).first(),v=>v?.domain_evidence_json);
    const retainedEvidence=JSON.parse(domainProjection.domain_evidence_json);
    assert.deepEqual(retainedEvidence.matched_domains,['eng.example.com']);
    assert.equal(retainedEvidence.link_id,restricted.link);
    assert.deepEqual(JSON.parse((await db.prepare('SELECT email_domains_json FROM invitation_links WHERE id=?').bind(restricted.link).first()).email_domains_json),domainRules);
    await declineOnGithub(restricted);
    await reconcile(restricted);
    await eventually(()=>db.prepare('SELECT state,domain_evidence_json FROM invitation_requests WHERE id=?').bind(restricted.request).first(),v=>v?.state==='lapsed'&&v.domain_evidence_json===domainProjection.domain_evidence_json);
    assert.deepEqual(await http(`${ingress}/InvitationLink/${restricted.link}/admit`,restricted.admissionInput),restricted.admission);
    assert.equal(await deletes(restricted),0);
    console.log('PASS signed email-domain admission: missing/stale/wrong-link/mismatch/noreply refusals consume no use, normalized evidence and immutable rules reach D1, Lapsed preserves evidence and exact admission replay');

    const sweepAccepted=await fixture();
    await acceptOnGithub(sweepAccepted);
    await reconcile(sweepAccepted);
    const acceptance=await eventually(()=>sweepAccepted.call('status'),v=>v?.settlement?.state==='accepted');
    assert.deepEqual(acceptance.create,sweepAccepted.snapshot.create);
    assert.equal(await deletes(sweepAccepted),0);
    await eventually(()=>db.prepare('SELECT settlement FROM deliveries WHERE id=?').bind(`org:${sweepAccepted.key}`).first(),v=>v?.settlement==='accepted');
    await eventually(()=>http(`${ingress}/SweepSchedule/org:${sweepAccepted.account}/status`),v=>Object.keys(v?.tracked??{}).length===0);
    console.log('PASS signed org reconciliation: complete pending/failed lists plus fresh active membership settle Accepted, immutable create, D1 projection and untracking');

    const sweepDeclined=await fixture();await declineOnGithub(sweepDeclined);
    await reconcile(sweepDeclined);
    const decline=await eventually(()=>sweepDeclined.call('status'),v=>v?.settlement?.state==='declined');
    assert.deepEqual(decline.create,sweepDeclined.snapshot.create);
    assert.equal(await deletes(sweepDeclined),0);
    await eventually(()=>db.prepare('SELECT settlement FROM deliveries WHERE id=?').bind(`org:${sweepDeclined.key}`).first(),v=>v?.settlement==='declined');
    console.log('PASS signed org reconciliation: complete absence plus fresh missing membership settles Declined without a write');

    // The invitation failed: it leaves the pending list for the failed one.
    const failed=await fixture();
    await github.api('DELETE',`/orgs/${failed.org}/invitations/${failed.upstream}`,undefined,{installation:failed.installation,expected:204});
    await emulator.seed({org_invitations:[{id:failed.upstream,org:failed.org,invitee:failed.login,inviter:'owner',role:'direct_member',
      failed_at:new Date().toISOString(),failed_reason:'Invitation expired'}]});
    const reads=async suffix=>(await calls(failed)).filter(t=>t.path.endsWith(suffix)).length;
    const membershipReads=await reads(`/memberships/${failed.login}`);
    await reconcile(failed);
    // Barrier: wait for the owner membership read after the complete failed list,
    // then issue another exclusive handler so its observation has completed.
    await eventually(()=>reads('/failed_invitations'),n=>n===1);
    await eventually(()=>reads(`/memberships/${failed.login}`),n=>n>membershipReads);
    assert.equal((await failed.call('refresh')).kind,'requested');
    assert.equal((await failed.call('refresh')).kind,'throttled');
    assert.equal((await failed.call('status')).settlement,null,'failed_at is not an expiry reason');
    assert.equal(await deletes(failed),0);
    console.log('PASS signed org failed-list conservatism and durable on-demand refresh throttle');

    const foreignLapsed=await fixture({foreign:true});await declineOnGithub(foreignLapsed);
    await reconcile(foreignLapsed);
    await eventually(()=>foreignLapsed.call('status'),v=>v?.settlement?.state==='declined');
    const foreignQuery={link_id:foreignLapsed.link,request_id:foreignLapsed.request,requester_id:foreignLapsed.user};
    const foreignRequest=await eventually(()=>http(`${ingress}/InvitationRequest/${foreignLapsed.request}/request_status`,foreignQuery),v=>v?.state==='lapsed');
    assert.equal(foreignRequest.decision.decision_id,`request.approved/${foreignLapsed.request}`);
    await eventually(()=>db.prepare('SELECT state FROM invitation_requests WHERE id=?').bind(foreignLapsed.request).first(),v=>v?.state==='lapsed');
    const longRetry=await http(`${ingress}/InvitationLink/${foreignLapsed.link}/admit`,{...foreignLapsed.admissionInput,operation_id:id()});
    assert.equal(longRetry.result.state,'approved','seven-day lifetime retains autoapproval for a lapsed rerequest');
    await eventually(()=>http(`${ingress}/OrganizationDelivery/${longRetry.result.request_id}:${foreignLapsed.account}:1/status`),v=>v?.create?.outcome?.kind==='created');
    assert.equal(await deletes(foreignLapsed),0);
    console.log('PASS signed foreign invitation disappearance: Lapsed request/D1, original approval retained, long-lifetime rerequest autoapproves without cancelling foreign invite');

    const owned=await fixture(),beforeOwned=await owned.budget();
    const revoke=await http(`${ingress}/InvitationLink/${owned.link}/revoke`,owned.revoke);
    assert.deepEqual(revoke,{Ok:{addressed_requests:0,addressed_deliveries:1}});
    const cancelled=await eventually(()=>owned.call('status'),v=>v?.settlement?.state==='cancelled');
    assert.deepEqual(cancelled.create,owned.snapshot.create);
    assert.equal(cancelled.settlement.event.metadata.reason,'link_revoked');
    await replay(owned);
    assert.deepEqual(await http(`${ingress}/InvitationLink/${owned.link}/revoke`,owned.revoke),revoke);
    assert.equal(await deletes(owned),1);
    assert.equal((await owned.budget()).content_used_minute,beforeOwned.content_used_minute+1);
    const fences=await db.prepare('SELECT step,attempt_generation,retryable FROM delivery_attempts WHERE delivery_key=? ORDER BY step').bind(`org:${owned.key}`).all();
    assert.deepEqual(fences.results,[{step:'cancel',attempt_generation:1,retryable:0},{step:'org',attempt_generation:1,retryable:0}]);
    await eventually(()=>db.prepare('SELECT settlement FROM deliveries WHERE id=?').bind(`org:${owned.key}`).first(),v=>v?.settlement==='cancelled');
    const progress=await eventually(()=>http(`${ingress}/InvitationLink/${owned.link}/cascade_progress`,{link_id:owned.link,admin:owned.revoke.admin}),v=>v?.done_deliveries===1);
    assert.equal(progress.unavailable,0);
    console.log('PASS signed org cascade: one DELETE, separate cancel fence/content charge, immutable create/admission/revoke, D1 settlement and completed cascade');

    const foreign=await fixture({foreign:true}),beforeForeign=await foreign.budget();
    await http(`${ingress}/InvitationLink/${foreign.link}/revoke`,foreign.revoke);
    const stopped=await eventually(()=>foreign.call('status'),v=>v?.tracking_stopped==='link_revoked');
    assert.equal(stopped.settlement,null);assert.deepEqual(stopped.create,foreign.snapshot.create);
    await replay(foreign);assert.equal(await deletes(foreign),0);
    assert.equal((await foreign.budget()).content_used_minute,beforeForeign.content_used_minute);
    console.log('PASS signed org foreign cascade: stops tracking, preserves receipt, no DELETE or extra budget charge');

    const spammed=await fixture();await emulator.admin('POST',`/orgs/${spammed.org}/spammed`,{spammed:true});
    const beforeSpammed=await spammed.budget();
    await http(`${ingress}/InvitationLink/${spammed.link}/revoke`,spammed.revoke);
    const spamBudget=await eventually(()=>spammed.budget(),v=>v?.limit_counts?.spammed===1&&v.content_used_hour===beforeSpammed.content_used_hour);
    assert.equal(spamBudget.limit_counts.rate_limited,0);
    assert.equal(spamBudget.limit_counts.incomplete,true,'new observation coverage cannot claim a full day');
    assert.ok(spamBudget.limit_counts.coverage_started_at);
    assert.ok(Date.parse(spamBudget.content_closed_until)>Date.now()+23*3600000);
    assert.equal(spamBudget.content_used_hour,beforeSpammed.content_used_hour,'positive spam rejection releases unused cancellation capacity');
    await replay(spammed);
    assert.equal(await deletes(spammed),1,'recorded closure prevents another DELETE');
    assert.equal((await spammed.call('status')).settlement,null,'rejection is not cancellation');
    assert.equal((await spammed.budget()).limit_counts.spammed,1,'retained evidence is counted once');
    const persistedSpam=await eventually(()=>db.prepare("SELECT snapshot_json FROM invite_budget_projection WHERE app='org' AND account_id=?").bind(spammed.account).first(),v=>v&&JSON.parse(v.snapshot_json).limit_counts?.spammed===1);
    assert.deepEqual(JSON.parse(persistedSpam.snapshot_json).limit_counts,spamBudget.limit_counts);
    const spamFence=await db.prepare("SELECT attempt_generation,retryable FROM delivery_attempts WHERE delivery_key=? AND step='cancel'").bind(`org:${spammed.key}`).first();
    assert.deepEqual(spamFence,{attempt_generation:1,retryable:1});
    console.log('PASS signed org cancellation spam: one DELETE, released unused capacity, durable typed count and content closure, D1 projection, no false settlement or replay duplication');

    // The DELETE is applied on GitHub, but its answer is lost.
    const unknown=await fixture();
    await emulator.fault({method:'DELETE',path:`/orgs/${unknown.org}/invitations/{id}`,times:1,label:'lost delete',action:{kind:'apply_then_lose',status:502}});
    const beforeUnknown=await unknown.budget();
    await http(`${ingress}/InvitationLink/${unknown.link}/revoke`,unknown.revoke);
    await eventually(()=>deletes(unknown),n=>n===1);
    await replay(unknown);
    assert.equal((await unknown.call('status')).settlement,null,'absence after uncertain DELETE is not cancellation proof');
    assert.equal(await deletes(unknown),1);assert.equal((await unknown.budget()).content_used_minute,beforeUnknown.content_used_minute+1);
    const waiting=await http(`${ingress}/InvitationLink/${unknown.link}/cascade_progress`,{link_id:unknown.link,admin:unknown.revoke.admin});
    assert.equal(waiting.waiting_on_github,1);assert.equal(waiting.done_deliveries,0);
    // The requester joins anyway (invited again elsewhere, and accepts).
    await github.api('PUT',`/orgs/${unknown.org}/memberships/${unknown.login}`,{role:'member'},{installation:unknown.installation});
    await acceptOnGithub(unknown);
    await unknown.call('drive_cancellation');
    assert.equal((await unknown.call('status')).settlement.state,'accepted','fresh positive acceptance can resolve uncertain cancellation');
    assert.equal(await deletes(unknown),1);
    console.log('PASS signed org uncertain DELETE: retained charge/fence, no repeat DELETE or false cancellation; positive acceptance wins');

    const accepted=await fixture();await acceptOnGithub(accepted);
    const beforeAccepted=await accepted.budget();
    await http(`${ingress}/InvitationLink/${accepted.link}/revoke`,accepted.revoke);
    await eventually(()=>accepted.call('status'),v=>v?.settlement?.state==='accepted');
    assert.equal(await deletes(accepted),0);assert.equal((await accepted.budget()).content_used_minute,beforeAccepted.content_used_minute);
    console.log('PASS signed org acceptance before cancel: fresh membership settles accepted without DELETE or budget reservation');

    const expiring=await fixture({lifetime:3600});
    await expiring.call('tick_own_expiry',{generation:1,upstream_id:expiring.upstream});
    assert.equal(await deletes(expiring),0,'early timer cannot cancel');
    const due=Date.parse(expiring.snapshot.create.outcome.created_at)+3601000;
    await setTime(due);
    await expiring.call('tick_own_expiry',{generation:99,upstream_id:expiring.upstream});
    assert.equal(await deletes(expiring),0,'stale timer cannot cancel');
    await expiring.call('tick_own_expiry',{generation:1,upstream_id:expiring.upstream});
    const expired=await eventually(()=>expiring.call('status'),v=>v?.settlement?.state==='cancelled');
    assert.equal(expired.settlement.event.metadata.reason,'own_expiry');
    assert.deepEqual(expired.create,expiring.snapshot.create);
    await replay(expiring);assert.equal(await deletes(expiring),1);
    const expiryQuery={link_id:expiring.link,request_id:expiring.request,requester_id:expiring.user};
    const expiredRequest=await eventually(()=>http(`${ingress}/InvitationRequest/${expiring.request}/request_status`,expiryQuery),v=>v?.state==='lapsed');
    assert.equal(expiredRequest.decision.decision_id,`request.approved/${expiring.request}`);
    await eventually(()=>db.prepare('SELECT state FROM invitation_requests WHERE id=?').bind(expiring.request).first(),v=>v?.state==='lapsed');
    const retryInput={...expiring.admissionInput,operation_id:id()};
    const retry=await http(`${ingress}/InvitationLink/${expiring.link}/admit`,retryInput);
    assert.equal(retry.result.state,'pending','short-lifetime rerequest requires approval');
    assert.deepEqual(await http(`${ingress}/InvitationLink/${expiring.link}/admit`,retryInput),retry);
    await http(`${ingress}/InvitationRequest/${retry.result.request_id}/decide`,{link_id:expiring.link,request_id:retry.result.request_id,operation_id:id(),admin:expiring.revoke.admin,action:{kind:'decline',reason:null}});
    const limited=await http(`${ingress}/InvitationLink/${expiring.link}/admit`,{...retryInput,operation_id:id()});
    assert.ok(limited.result.reason.rerequest_limited.retry_after,'declining a pending rerequest cannot bypass seven-day cooldown');
    const summary=await http(`${ingress}/InvitationLink/${expiring.link}/link_status`,{link_id:expiring.link,admin:expiring.revoke.admin});
    assert.equal(summary.uses,2);
    console.log('PASS signed org own expiry: early/stale ticks ignored, due tick re-reads and cancels once (fixture clock, no wall-time scheduling claim)');
  } finally {setClock(undefined);}
}
