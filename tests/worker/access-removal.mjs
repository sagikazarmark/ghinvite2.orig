import assert from 'node:assert/strict';
import { orgApp } from './github-world.mjs';

// GitHub (octoemu): the owner (7) and the organization App; each scenario
// adds its organization (with the team `devs`), requester and installation.
export const seed = () => ({ users:[{id:7,login:'owner'}], apps:[orgApp] });

// Real standard composition, signed ingress, D1, budget and delayed sends.
// GitHub (octoemu) is the sole external substitute: the requester accepts on
// GitHub, the owner promotes them; a refused or lost DELETE is a fault rule.
// No retained state is fabricated.
export async function accessRemoval({ingress,http,id,db,eventually,github}) {
  const emulator=github.emulator; let next=940;
  const calls=async s=>(await github.calls()).filter(call=>call.path.startsWith(`/orgs/${s.org}/`)).map(call=>({method:call.method,path:call.path.split('?')[0]}));
  const writeMints=async s=>(await github.calls()).filter(call=>call.path===`/app/installations/${s.installation}/access_tokens`&&call.body_fields?.permissions?.members==='write').length;
  // The owner makes the requester an organization admin (their membership is then not ghinvite's to remove).
  const promote=s=>github.api('PUT',`/orgs/${s.org}/memberships/${s.login}`,{role:'admin'},{installation:s.installation});
  const teamDelete=s=>`/orgs/${s.org}/teams/devs/memberships/${s.login}`;
  const loseAnswer=s=>emulator.fault({method:'DELETE',path:teamDelete(s),times:1,label:'lost removal answer',action:{kind:'apply_then_lose',status:502}});
  const refuseOnce=s=>emulator.fault({method:'DELETE',path:teamDelete(s),times:1,label:'refused once',action:{kind:'respond',status:401,body:{message:'Bad credentials',documentation_url:'https://docs.github.com/rest'}}});
  const fixture=async(lifetime=null)=>{
    const account=next++,installation=account*10+2,user=account+1000,team=account*10+5;
    const s={account,installation,user,team,org:`access-${account}`,login:`access-user-${user}`};
    await emulator.seed({users:[{id:user,login:s.login}],orgs:[{id:account,login:s.org,created_at:'2020-01-01T00:00:00Z'}],
      teams:[{id:team,org:s.org,slug:'devs',name:'Developers'}],memberships:[{org:s.org,user:'owner',role:'admin',state:'active'}]});
    await emulator.admin('POST','/installations',{installation:{id:installation,app_id:456,account:s.org,repository_selection:'all',created_at:'2026-01-01T00:00:00Z'}},201);
    for(const [uid,login] of [[7,'owner'],[user,s.login]]) await db.prepare('INSERT OR IGNORE INTO users(user_id,login,avatar_url,last_seen_at) VALUES (?, ?, NULL, ?)').bind(uid,login,new Date().toISOString()).run();
    await http(`${ingress}/Installation/org:${installation}/onboard`,{operation_id:id(),verified:{app:'org',account:{installation_id:installation,account_id:account,account_login:s.org,account_type:'Organization',selected_repos:[],installed_at:'2026-01-01T00:00:00Z',uninstalled_at:null},suspended_at:null,permissions:{members:'write',metadata:'read'}}});
    s.link=id();s.admin={account_id:account,user_id:7};
    const creation={link_id:s.link,secret_sha256:'4c'.repeat(32),account_id:account,installation_id:installation,admin:s.admin,description:'Accepted access removal',max_uses:5,approval_required:false,access_lifetime_seconds:lifetime,targets:{organization:{org_id:account,org_login:s.org,grant:'member'},teams:[{team_id:team,team_slug:'devs',grant:'member'}],repos:[]}};
    assert.ok((await http(`${ingress}/InvitationLink/${s.link}/create`,creation)).Ok);
    s.admissionInput={link_id:s.link,secret_sha256:creation.secret_sha256,secret_generation:1,scope_evidence:[],operation_id:id(),requester_id:user};
    s.admission=await http(`${ingress}/InvitationLink/${s.link}/admit`,s.admissionInput);
    assert.equal(s.admission.result.kind,'accepted');s.request=s.admission.result.request_id;s.key=`${s.request}:${account}:1`;
    s.call=(handler,input)=>http(`${ingress}/OrganizationDelivery/${s.key}/${handler}`,input);
    s.created=await eventually(()=>s.call('status'),v=>v?.create.outcome.kind==='created');
    // The requester accepts the invitation (and so joins the team) on GitHub.
    await emulator.act(s.login,`orgs/${s.org}/accept`);
    if(lifetime!==null) await promote(s);
    await s.call('observe_listed',{account_id:account,installation_id:installation,org_id:account,org_login:s.org,observation_started_at:new Date().toISOString(),observed_at:new Date().toISOString(),pending:[],failed:[]});
    s.original=await eventually(()=>s.call('status'),v=>v?.settlement?.state==='accepted');
    s.prepare={link_id:s.link,admin:s.admin,operation_id:id()};
    s.grantKey=`${s.request}:team:${team}`;
    s.grantQuery={request_id:s.request,target:{kind:'team',org_id:account,org_login:s.org,team_id:team,slug:'devs'},delivery_generation:1};
    s.grantStatus=()=>http(`${ingress}/AccessGrant/${s.grantKey}/status`,s.grantQuery);
    if(lifetime===null) {
      s.preview=await http(`${ingress}/InvitationLink/${s.link}/prepare_access_removal`,s.prepare);
      assert.equal(s.preview.grants.length,2);
      s.grant=s.preview.grants.find(g=>g.target.kind==='team');
    } else { s.grant=(await eventually(s.grantStatus,v=>v?.command)).command; }
    return s;
  };
  const deletes=async s=>(await calls(s)).filter(t=>t.method==='DELETE');
  const revoke=s=>http(`${ingress}/InvitationLink/${s.link}/revoke`,{...s.prepare,cascade:{decline_pending_requests:false,cancel_pending_invitations:false,remove_granted_access:true}});
  try {
    const owned=await fixture();await promote(owned);
    assert.equal((await deletes(owned)).length,0);
    assert.deepEqual(await http(`${ingress}/InvitationLink/${owned.link}/prepare_access_removal`,owned.prepare),owned.preview);
    await new Promise(r=>setTimeout(r,1100));
    const receipt=await revoke(owned);assert.equal(receipt.Ok.addressed_access_grants,2);
    const progress=await eventually(()=>http(`${ingress}/InvitationLink/${owned.link}/cascade_progress`,{link_id:owned.link,admin:owned.admin}),v=>v?.done_access_grants===2);
    assert.equal(progress.addressed_access_grants,2);assert.equal((await deletes(owned)).length,1);
    assert.ok((await deletes(owned))[0].path.includes('/teams/'));
    assert.deepEqual(await revoke(owned),receipt);assert.deepEqual(await owned.call('status'),owned.original);
    const rows=await eventually(()=>db.prepare('SELECT receipt FROM access_grants WHERE link_id=?').bind(owned.link).all(),v=>v.results.length===2&&v.results.every(r=>['removed','removal_skipped'].includes(JSON.parse(r.receipt).outcome.kind)));
    assert.equal(rows.results.filter(r=>JSON.parse(r.receipt).outcome.kind==='removed').length,1);
    const fence=await db.prepare("SELECT attempt_generation AS generation,retryable FROM delivery_attempts WHERE delivery_key=? AND step=?").bind(`access:${owned.grantKey}:1`,`remove:team:${owned.team}`).first();
    assert.equal(fence.generation,1);assert.equal(fence.retryable,0);
    assert.equal((await http(`${ingress}/InviteBudget/org:${owned.account}/snapshot`)).content_used_hour,2);
    console.log('PASS signed AccessGrant preview/revoke: exact set, role skip, one DELETE, separate D1 fence, budget and immutable receipt');

    const unknown=await fixture();await promote(unknown);await loseAnswer(unknown);
    await new Promise(r=>setTimeout(r,1100));await revoke(unknown);
    await eventually(unknown.grantStatus,v=>v?.outcome.kind==='outcome_unknown');
    for(let n=0;n<3;n++) await http(`${ingress}/AccessGrant/${unknown.grantKey}/remove_now`,{grant:unknown.grant,reason:'link_revoked'});
    assert.equal((await deletes(unknown)).length,1);assert.equal((await unknown.grantStatus()).outcome.kind,'outcome_unknown');
    const uncertain=await http(`${ingress}/InvitationLink/${unknown.link}/cascade_progress`,{link_id:unknown.link,admin:unknown.admin});
    assert.equal(uncertain.done_access_grants,1);assert.equal(uncertain.waiting_on_github,1);
    assert.equal((await http(`${ingress}/InviteBudget/org:${unknown.account}/snapshot`)).content_used_hour,2);
    console.log('PASS signed lost access-removal ACK remains charged and uncertain through three replays');

    for(const loseAck of [true,false]) {
      const reminted=await fixture();await promote(reminted);await refuseOnce(reminted);if(loseAck)await loseAnswer(reminted);
      await new Promise(r=>setTimeout(r,1100));await revoke(reminted);
      const expected=loseAck?'outcome_unknown':'removed';
      const result=await eventually(reminted.grantStatus,v=>v?.outcome.kind===expected);
      for(let n=0;n<3;n++)await http(`${ingress}/AccessGrant/${reminted.grantKey}/remove_now`,{grant:reminted.grant,reason:'link_revoked'});
      assert.deepEqual(await reminted.grantStatus(),result);
      assert.equal((await deletes(reminted)).length,2);assert.equal(await writeMints(reminted),2);
      const fence=await db.prepare('SELECT attempt_generation,retryable FROM delivery_attempts WHERE delivery_key=? AND step=?').bind(`access:${reminted.grantKey}:1`,`remove:team:${reminted.team}`).first();
      assert.deepEqual(fence,{attempt_generation:2,retryable:0});
      assert.equal((await http(`${ingress}/InviteBudget/org:${reminted.account}/snapshot`)).content_used_hour,2);
      assert.deepEqual(await reminted.call('status'),reminted.original);
      console.log(`PASS signed access removal401 remints once: second ${loseAck?'lost ACK stays uncertain':'204 stays removed'}, generation2 fenced and charged`);
    }

    const expiry=await fixture(3);
    await eventually(expiry.grantStatus,v=>v?.outcome.kind==='removed');
    assert.equal((await deletes(expiry)).length,1);assert.equal((await expiry.grantStatus()).outcome.reason,'access_expiry');
    assert.deepEqual(await expiry.call('status'),expiry.original);
    console.log('PASS actual Restate delayed access expiry in signed Worker: no manual tick, one team DELETE, promoted owner skipped');
  } finally {await emulator.clearFaults();}
}
