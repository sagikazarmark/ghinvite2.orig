import assert from 'node:assert/strict';
import { Response } from 'miniflare';

// Actual signed Restate -> workerd handlers; only GitHub responses are scripted.
export async function orgObservation({ ingress,http,id,observe,budget=null }) {
  const account=830,orgInstallation=8302,repoInstallation=8301;
  let mode='available',role='admin',membershipState='active';
  const requests=[],mints=[];
  observe(async request=>{
    const url=new URL(request.url),path=url.pathname,authorization=request.headers.get('authorization');
    requests.push({method:request.method,path});
    if(path===`/app/installations/${orgInstallation}` || path===`/app/installations/${repoInstallation}`) {
      assert.match(authorization,/^Bearer ey/);
      const org=path.endsWith(String(orgInstallation));
      assert.equal(JSON.parse(Buffer.from(authorization.split('.')[1],'base64url').toString()).iss,org?'Iv.fixture-org':'Iv.fixture-repo');
      if(org && mode==='missing') return Response.json({}, {status:404});
      return Response.json({id:org?orgInstallation:repoInstallation,app_id:org?456:123,
        account:{id:org&&mode==='account_mismatch'?999:account,login:'org-fixture',type:'Organization'},
        repository_selection:'all',permissions:org?{members:'write',metadata:'read'}:{administration:'write',metadata:'read'},
        created_at:'2026-01-01T00:00:00Z',updated_at:'2026-01-01T00:00:00Z',
        suspended_at:org&&mode==='suspended'?'2026-01-01T00:00:00Z':null});
    }
    if(path.endsWith('/access_tokens')) {
      const org=path===`/app/installations/${orgInstallation}/access_tokens`;
      if(!org) assert.equal(path,`/app/installations/${repoInstallation}/access_tokens`);
      assert.equal(JSON.parse(Buffer.from(authorization.split('.')[1],'base64url').toString()).iss,org?'Iv.fixture-org':'Iv.fixture-repo');
      const body=await request.json(),permissions=org?{members:'read',metadata:'read'}:{administration:'read',metadata:'read'};
      assert.deepEqual(body,{permissions});mints.push({org,body});
      return Response.json({token:org?'ghs_org_scoped':'ghs_repo_scoped',permissions,expires_at:'2099-01-01T00:00:00Z'},{status:201});
    }
    if(path==='/installation/repositories') {
      assert.equal(authorization,'Bearer ghs_repo_scoped');
      return Response.json({total_count:1,repositories:[{id:10,full_name:'org-fixture/api',private:true}]});
    }
    assert.equal(authorization,'Bearer ghs_org_scoped');
    if(path==='/orgs/org-fixture') {
      if(mode==='failure') return Response.json({}, {status:503});
      return Response.json({id:mode==='org_mismatch'?999:account,login:'org-fixture',created_at:'2020-01-01T00:00:00Z'});
    }
    if(path==='/user/7') return Response.json({id:mode==='user_mismatch'?99:7,login:'current-owner'});
    if(path==='/orgs/org-fixture/memberships/current-owner') {
      if(mode==='not_member') return Response.json({}, {status:404});
      return Response.json({role,state:membershipState,user:{id:7,login:'current-owner'},organization:{id:account,login:'org-fixture'}});
    }
    assert.fail(`unexpected organization observation request ${request.method} ${path}`);
  });
  const owner=(app,handler,body)=>http(`${ingress}/AccountInstallation/${app}:${account}/${handler}`,body);
  try {
    assert.equal(await owner('org','org_authority',7),'absent');
    for(const [app,installation] of [['repo',repoInstallation],['org',orgInstallation]]) {
      await http(`${ingress}/Installation/${app}:${installation}/onboard`,{operation_id:id(),verified:{app,account:{installation_id:installation,account_id:account,account_login:'org-fixture',account_type:'Organization',selected_repos:[],installed_at:'2026-01-01T00:00:00Z',uninstalled_at:null},suspended_at:null,permissions:{}}});
    }
    assert.deepEqual((await owner('org','status')).observation,{kind:'available',repo_ids:[]});
    const repo=(await owner('repo','status')).observation;
    assert.deepEqual(repo,{kind:'available',repo_ids:[10]});
    assert.ok(mints.some(value=>value.org));assert.ok(mints.some(value=>!value.org));
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
      role='member';
      assert.deepEqual(await owner('repo','set_org_cap',{...command,operation_id:id(),confirmed_established:false}),{Err:'not_owner'});
      assert.deepEqual(await owner('repo','set_org_cap',command),confirmed,'original receipt remains recoverable after role loss');
      role='admin';
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
    role='member';assert.equal(await owner('org','org_authority',7),'non_owner');
    assert.equal(await owner('repo','decision_authority',7),true);
    membershipState='pending';assert.equal(await owner('org','org_authority',7),'denied');
    assert.equal(await owner('repo','decision_authority',7),false);
    membershipState='active';role='admin';
    for(const [failure,expected] of [['suspended','unavailable'],['missing','unavailable'],['account_mismatch','unavailable'],['org_mismatch','unavailable'],['failure','unknown']]) {
      mode=failure;await owner('org','refresh',orgInstallation);
      assert.equal((await owner('org','status')).observation.kind,expected,failure);
      assert.deepEqual((await owner('repo','status')).observation,repo);
      if (failure==='failure') {
        const response=await fetch(`${ingress}/AccountInstallation/repo:${account}/decision_authority`,{method:'POST',headers:{'content-type':'application/json'},body:'7'});
        assert.equal(response.status,503);await response.arrayBuffer();
      } else assert.equal(await owner('repo','decision_authority',7),false,failure);
    }
    mode='user_mismatch';assert.equal(await owner('org','org_authority',7),'denied');
    mode='not_member';assert.equal(await owner('org','org_authority',7),'non_owner');
    mode='available';await owner('org','refresh',orgInstallation);
    assert.equal((await owner('org','status')).observation.kind,'available');
    await owner('org','uninstall',{app:'org',installation_id:orgInstallation,uninstalled_at:new Date().toISOString()});
    const before=requests.length;await owner('org','recheck');assert.equal(requests.length,before);
    assert.equal((await owner('org','status')).observation.kind,'unavailable');
    console.log('PASS actual signed Worker org observation: exact app profiles, numeric identity, owner/member/pending, canonical delegation writer, failure isolation and retained uninstall');
  } finally { observe(null); }
}
