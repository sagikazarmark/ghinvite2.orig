import assert from 'node:assert/strict';
import { repoApp, orgApp } from './github-world.mjs';

const account=830,orgInstallation=8302,repoInstallation=8301;

// GitHub (octoemu): `org-fixture` (830) with `org-fixture/api` (10), owned by
// `current-owner` (7) and `second-owner` (6); the repository App installed
// as 8301 and the organization App as 8302.
export const seed = () => ({
  users:[{id:6,login:'second-owner'},{id:7,login:'current-owner'}],
  orgs:[{id:account,login:'org-fixture',created_at:'2020-01-01T00:00:00Z'}],
  repos:[{id:10,owner:'org-fixture',name:'api',private:true}],
  apps:[repoApp,orgApp],
  installations:[
    {id:repoInstallation,app_id:123,account:'org-fixture',repository_selection:'all',created_at:'2026-01-01T00:00:00Z'},
    {id:orgInstallation,app_id:456,account:'org-fixture',repository_selection:'all',created_at:'2026-01-01T00:00:00Z'}],
  memberships:[{org:'org-fixture',user:'second-owner',role:'admin',state:'active'},{org:'org-fixture',user:'current-owner',role:'admin',state:'active'}],
});

// Actual signed Restate -> workerd handlers; GitHub is octoemu. Membership
// changes are GitHub's own (the organization App's API, the invitee's
// acceptance); answers GitHub would not give on its own (an installation
// under another account, a numeric identity mismatch) are fault rules.
export async function orgObservation({ ingress,http,id,github,budget=null }) {
  const emulator=github.emulator;
  const membership='/orgs/org-fixture/memberships/current-owner';
  const role=value=>github.api('PUT',membership,{role:value},{installation:orgInstallation});
  const pending=async()=>{
    await github.api('DELETE',membership,undefined,{installation:orgInstallation,expected:204});
    await github.api('PUT',membership,{role:'admin'},{installation:orgInstallation});
  };
  const active=()=>emulator.act('current-owner','orgs/org-fixture/accept');
  const installation=accountId=>({id:orgInstallation,app_id:456,account:{id:accountId,login:'org-fixture',type:'Organization'},
    repository_selection:'all',permissions:{members:'write',metadata:'read'},events:[],created_at:'2026-01-01T00:00:00Z',updated_at:'2026-01-01T00:00:00Z',suspended_at:null});
  const rules=[];
  const mode=async value=>{
    for(const rule of rules.splice(0)) await emulator.removeFault(rule);
    if(value==='suspended') await emulator.admin('POST',`/installations/${orgInstallation}/suspend`);
    else if((await emulator.state()).world.installations.find(i=>i.id===orgInstallation)?.suspended_at) await emulator.admin('POST',`/installations/${orgInstallation}/unsuspend`);
    const respond=(method,path,status,body)=>emulator.fault({method,path,label:value,action:{kind:'respond',status,body}}).then(rule=>rules.push(rule));
    if(value==='missing') await respond('GET',`/app/installations/${orgInstallation}`,404,{message:'Not Found'});
    if(value==='account_mismatch') await respond('GET',`/app/installations/${orgInstallation}`,200,installation(999));
    if(value==='org_mismatch') await respond('GET','/orgs/org-fixture',200,{id:999,login:'org-fixture',created_at:'2020-01-01T00:00:00Z'});
    if(value==='failure') await respond('GET','/orgs/org-fixture',503,{message:'Service Unavailable'});
    if(value==='user_mismatch') await respond('GET','/user/7',200,{id:99,login:'current-owner',type:'User'});
    if(value==='not_member') await github.api('DELETE',membership,undefined,{installation:orgInstallation,expected:204});
  };
  const mints=async()=>(await github.calls()).filter(call=>call.method==='POST'&&/^\/app\/installations\/\d+\/access_tokens$/.test(call.path))
    .map(call=>({org:call.path===`/app/installations/${orgInstallation}/access_tokens`,body:call.body_fields}));
  const owner=(app,handler,body)=>http(`${ingress}/AccountInstallation/${app}:${account}/${handler}`,body);
  try {
    assert.equal(await owner('org','org_authority',7),'absent');
    for(const [app,installation] of [['repo',repoInstallation],['org',orgInstallation]]) {
      await http(`${ingress}/Installation/${app}:${installation}/onboard`,{operation_id:id(),verified:{app,account:{installation_id:installation,account_id:account,account_login:'org-fixture',account_type:'Organization',selected_repos:[],installed_at:'2026-01-01T00:00:00Z',uninstalled_at:null},suspended_at:null,permissions:{}}});
    }
    assert.deepEqual((await owner('org','status')).observation,{kind:'available',repo_ids:[]});
    const repo=(await owner('repo','status')).observation;
    assert.deepEqual(repo,{kind:'available',repo_ids:[10]});
    const minted=await mints();
    assert.ok(minted.some(value=>value.org));assert.ok(minted.some(value=>!value.org));
    // Each App asks for exactly its read profile.
    for(const {org,body} of minted) assert.deepEqual(body?.permissions,org?{members:'read',metadata:'read'}:{administration:'read',metadata:'read'});
    assert.equal(await owner('org','org_authority',7),'owner');
    assert.equal(await owner('repo','decision_authority',7),true);
    if(budget) {
      const {db,eventually}=budget;
      await db.prepare("INSERT OR IGNORE INTO users(user_id,login,avatar_url,last_seen_at) VALUES (7, 'current-owner', NULL, '2026-01-01T00:00:00Z')").run();
      const command={operation_id:id(),actor_user_id:7,confirmed_established:true};
      const confirmed=await owner('repo','set_org_cap',command);
      assert.equal(confirmed.Ok.account_id,account);
      assert.equal(confirmed.Ok.command.confirmed_established,true);
      assert.deepEqual(await owner('repo','set_org_cap',command),confirmed);
      const status=async()=>http(`${ingress}/InviteBudget/org:${account}/snapshot`);
      let snapshot=await status();
      assert.equal(snapshot.confirmed_established,true);
      assert.equal(snapshot.buckets.find(b=>b.bucket.kind==='org').cap,500);
      let conflict=await fetch(`${ingress}/AccountInstallation/repo:${account}/set_org_cap`,{method:'POST',headers:{'content-type':'application/json'},body:JSON.stringify({...command,confirmed_established:false})});
      assert.equal(conflict.status,409);await conflict.arrayBuffer();
      await role('member');
      assert.deepEqual(await owner('repo','set_org_cap',{...command,operation_id:id(),confirmed_established:false}),{Err:'not_owner'});
      assert.deepEqual(await owner('repo','set_org_cap',command),confirmed,'original receipt remains recoverable after role loss');
      await role('admin');
      const lowered=await owner('repo','set_org_cap',{...command,operation_id:id(),confirmed_established:false});
      assert.equal(lowered.Ok.revision,confirmed.Ok.revision+1);
      assert.equal(lowered.Ok.command.confirmed_established,false);
      assert.deepEqual(await owner('repo','set_org_cap',command),confirmed);
      snapshot=await status();
      assert.equal(snapshot.confirmed_established,false,'historical replay cannot undo later policy');
      assert.equal(snapshot.buckets.find(b=>b.bucket.kind==='org').cap,50);
      await eventually(async()=>{
        const row=await db.prepare("SELECT snapshot_json FROM invite_budget_projection WHERE app='org' AND account_id=?").bind(account).first();
        if(!row)return false;
        const projected=JSON.parse(row.snapshot_json);
        return projected.confirmed_established===false && projected.buckets.some(b=>b.bucket.kind==='org'&&b.cap===50);
      },Boolean,'confirmed organization cap projected to D1');
      await eventually(async()=> (await db.prepare("SELECT COUNT(*) AS n FROM audit_events WHERE account_id=? AND event_type='account.organization_invite_cap_changed'").bind(account).first()).n===2,Boolean,'one cap audit per immutable operation');
      console.log('PASS signed Worker organization cap: real numeric owner authority, canonical repo writer, 500/50 cap projection, exact historical replay, changed-input conflict and member refusal');
    }
    const changed=await owner('repo','set_delegation',{operation_id:id(),actor_user_id:7,on:true});
    assert.deepEqual(changed,{Ok:null});
    assert.equal((await owner('repo','status')).repo_admin_delegation,true);
    assert.deepEqual(await owner('org','set_delegation',{operation_id:id(),actor_user_id:7,on:false}),{Err:'org_app_required'});
    await role('member');assert.equal(await owner('org','org_authority',7),'non_owner');
    assert.equal(await owner('repo','decision_authority',7),true);
    await pending();assert.equal(await owner('org','org_authority',7),'denied');
    assert.equal(await owner('repo','decision_authority',7),false);
    await active();await role('admin');
    for(const [failure,expected] of [['suspended','unavailable'],['missing','unavailable'],['account_mismatch','unavailable'],['org_mismatch','unavailable'],['failure','unknown']]) {
      await mode(failure);await owner('org','refresh',orgInstallation);
      assert.equal((await owner('org','status')).observation.kind,expected,failure);
      assert.deepEqual((await owner('repo','status')).observation,repo);
      if (failure==='failure') {
        const response=await fetch(`${ingress}/AccountInstallation/repo:${account}/decision_authority`,{method:'POST',headers:{'content-type':'application/json'},body:'7'});
        assert.equal(response.status,503);await response.arrayBuffer();
      } else assert.equal(await owner('repo','decision_authority',7),false,failure);
    }
    await mode('user_mismatch');assert.equal(await owner('org','org_authority',7),'denied');
    await mode('not_member');assert.equal(await owner('org','org_authority',7),'non_owner');
    await mode('available');await owner('org','refresh',orgInstallation);
    assert.equal((await owner('org','status')).observation.kind,'available');
    await owner('org','uninstall',{app:'org',installation_id:orgInstallation,uninstalled_at:new Date().toISOString()});
    const before=(await github.calls()).length;await owner('org','recheck');assert.equal((await github.calls()).length,before);
    assert.equal((await owner('org','status')).observation.kind,'unavailable');
    console.log('PASS actual signed Worker org observation: exact app profiles, numeric identity, owner/member/pending, canonical delegation writer, failure isolation and retained uninstall');
  } finally { await mode('available'); }
}
