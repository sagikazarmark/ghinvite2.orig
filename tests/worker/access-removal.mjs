import assert from 'node:assert/strict';
import { Response } from 'miniflare';

// Real standard composition, signed ingress, D1, budget and delayed sends. The
// HTTP stub is the sole external substitute; no retained state is fabricated.
export async function accessRemoval({ingress,http,id,db,eventually,observe}) {
  const scenarios=new Map(); let next=940;
  const invitation=s=>({id:s.upstream,login:s.login,email:null,role:'direct_member',created_at:s.createdAt,inviter:{id:7,login:'owner'},team_count:1});
  observe(async request=>{
    const path=new URL(request.url).pathname, method=request.method;
    const body=['GET','HEAD'].includes(method)?null:await request.json().catch(()=>null);
    const install=path.match(/^\/app\/installations\/(\d+)/)?.[1];
    const org=path.match(/^\/orgs\/access-(\d+)/)?.[1];
    const user=path.match(/^\/(?:user\/|users\/access-user-)(\d+)$/)?.[1];
    const token=request.headers.get('authorization')?.match(/ghs_access_(\d+)_/)?.[1];
    const s=[...scenarios.values()].find(s=>install?String(s.installation)===install:org?String(s.account)===org:user?String(s.user)===user:String(s.installation)===token);
    assert.ok(s,`unexpected access HTTP ${method} ${path}`);
    s.trace.push({method,path,body});
    if(install) {
      assert.equal(JSON.parse(Buffer.from(request.headers.get('authorization').split('.')[1],'base64url').toString()).iss,'Iv.fixture-org');
      if(path.endsWith('/access_tokens')) {
        assert.ok(['read','write'].includes(body.permissions.members));
        assert.deepEqual(body,{permissions:{members:body.permissions.members,metadata:'read'}});
        const generation=++s.mints[body.permissions.members];
        return Response.json({token:`ghs_access_${s.installation}_${body.permissions.members}_${generation}`,expires_at:'2099-01-01T00:00:00Z'},{status:201});
      }
      return Response.json({id:s.installation,app_id:456,account:{id:s.account,login:s.org,type:'Organization'},repository_selection:'all',permissions:{members:'write',metadata:'read'},created_at:'2026-01-01T00:00:00Z',updated_at:'2026-01-01T00:00:00Z',suspended_at:null});
    }
    assert.match(request.headers.get('authorization'),new RegExp(`^Bearer ghs_access_${s.installation}_${method==='GET'?'read':'write'}_[0-9]+$`));
    if(path==='/user/7'||path==='/users/owner') return Response.json({id:7,login:'owner'});
    if(user) return Response.json({id:s.user,login:s.login});
    if(path===`/orgs/${s.org}`) return Response.json({id:s.account,login:s.org,created_at:'2020-01-01T00:00:00Z'});
    if(path===`/orgs/${s.org}/memberships/owner`) return Response.json({role:'admin',state:'active',user:{id:7,login:'owner'},organization:{id:s.account,login:s.org}});
    if(path===`/orgs/${s.org}/memberships/${s.login}`) {
      if(method==='DELETE') {assert.ok(s.accepted);s.accepted=false;s.teamActive=false;return new Response(null,{status:204});}
      return s.pending||s.accepted?Response.json({role:s.role,state:s.accepted?'active':'pending',user:{id:s.user,login:s.login},organization:{id:s.account,login:s.org}}):Response.json({},{status:404});
    }
    if(path===`/orgs/${s.org}/teams/devs`) return Response.json({id:s.team,slug:'devs',name:'Developers'});
    if(path===`/orgs/${s.org}/teams/devs/memberships/${s.login}`) {
      if(method==='DELETE') {assert.ok(s.teamActive);if(s.refuseOnce){s.refuseOnce=false;return Response.json({message:'Bad credentials'},{status:401});}s.teamActive=false;return s.loseAck?Response.json({message:'lost upstream response'},{status:502}):new Response(null,{status:204});}
      return s.teamActive?Response.json({role:'member',state:'active'}):Response.json({},{status:404});
    }
    if(path===`/orgs/${s.org}/invitations/${s.upstream}/teams`) return Response.json([{id:s.team,slug:'devs',name:'Developers'}]);
    if(path===`/orgs/${s.org}/failed_invitations`) return Response.json([]);
    if(path===`/orgs/${s.org}/invitations`) {
      if(method==='GET') return Response.json(s.pending?[invitation(s)]:[]);
      assert.equal(method,'POST');assert.equal(s.pending,false);
      assert.deepEqual(body,{invitee_id:s.user,role:'direct_member',team_ids:[s.team]});
      s.pending=true;s.createdAt=new Date().toISOString();return Response.json(invitation(s),{status:201});
    }
    assert.fail(`unexpected access HTTP ${method} ${path}`);
  });
  const fixture=async(lifetime=null)=>{
    const account=next++,installation=account*10+2,user=account+1000,team=account*10+5;
    const s={account,installation,user,team,org:`access-${account}`,login:`access-user-${user}`,upstream:account*100+1,pending:false,accepted:false,teamActive:false,role:'member',trace:[],mints:{read:0,write:0}};
    scenarios.set(account,s);
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
    s.pending=false;s.accepted=true;s.teamActive=true;if(lifetime!==null)s.role='admin';
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
  const deletes=s=>s.trace.filter(t=>t.method==='DELETE');
  const revoke=s=>http(`${ingress}/InvitationLink/${s.link}/revoke`,{...s.prepare,cascade:{decline_pending_requests:false,cancel_pending_invitations:false,remove_granted_access:true}});
  try {
    const owned=await fixture();owned.role='admin';
    assert.equal(deletes(owned).length,0);
    assert.deepEqual(await http(`${ingress}/InvitationLink/${owned.link}/prepare_access_removal`,owned.prepare),owned.preview);
    await new Promise(r=>setTimeout(r,1100));
    const receipt=await revoke(owned);assert.equal(receipt.Ok.addressed_access_grants,2);
    const progress=await eventually(()=>http(`${ingress}/InvitationLink/${owned.link}/cascade_progress`,{link_id:owned.link,admin:owned.admin}),v=>v?.done_access_grants===2);
    assert.equal(progress.addressed_access_grants,2);assert.equal(deletes(owned).length,1);
    assert.ok(deletes(owned)[0].path.includes('/teams/'));
    assert.deepEqual(await revoke(owned),receipt);assert.deepEqual(await owned.call('status'),owned.original);
    const rows=await eventually(()=>db.prepare('SELECT receipt FROM access_grants WHERE link_id=?').bind(owned.link).all(),v=>v.results.length===2&&v.results.every(r=>['removed','removal_skipped'].includes(JSON.parse(r.receipt).outcome.kind)));
    assert.equal(rows.results.filter(r=>JSON.parse(r.receipt).outcome.kind==='removed').length,1);
    const fence=await db.prepare("SELECT attempt_generation AS generation,retryable FROM delivery_attempts WHERE delivery_key=? AND step=?").bind(`access:${owned.grantKey}:1`,`remove:team:${owned.team}`).first();
    assert.equal(fence.generation,1);assert.equal(fence.retryable,0);
    assert.equal((await http(`${ingress}/InviteBudget/org:${owned.account}/snapshot`)).content_used_hour,2);
    console.log('PASS signed AccessGrant preview/revoke: exact set, role skip, one DELETE, separate D1 fence, budget and immutable receipt');

    const unknown=await fixture();unknown.role='admin';unknown.loseAck=true;
    await new Promise(r=>setTimeout(r,1100));await revoke(unknown);
    await eventually(unknown.grantStatus,v=>v?.outcome.kind==='outcome_unknown');
    for(let n=0;n<3;n++) await http(`${ingress}/AccessGrant/${unknown.grantKey}/remove_now`,{grant:unknown.grant,reason:'link_revoked'});
    assert.equal(deletes(unknown).length,1);assert.equal((await unknown.grantStatus()).outcome.kind,'outcome_unknown');
    const uncertain=await http(`${ingress}/InvitationLink/${unknown.link}/cascade_progress`,{link_id:unknown.link,admin:unknown.admin});
    assert.equal(uncertain.done_access_grants,1);assert.equal(uncertain.waiting_on_github,1);
    assert.equal((await http(`${ingress}/InviteBudget/org:${unknown.account}/snapshot`)).content_used_hour,2);
    console.log('PASS signed lost access-removal ACK remains charged and uncertain through three replays');

    for(const loseAck of [true,false]) {
      const reminted=await fixture();reminted.role='admin';reminted.refuseOnce=true;reminted.loseAck=loseAck;
      await new Promise(r=>setTimeout(r,1100));await revoke(reminted);
      const expected=loseAck?'outcome_unknown':'removed';
      const result=await eventually(reminted.grantStatus,v=>v?.outcome.kind===expected);
      for(let n=0;n<3;n++)await http(`${ingress}/AccessGrant/${reminted.grantKey}/remove_now`,{grant:reminted.grant,reason:'link_revoked'});
      assert.deepEqual(await reminted.grantStatus(),result);
      assert.equal(deletes(reminted).length,2);assert.equal(reminted.mints.write,2);
      const fence=await db.prepare('SELECT attempt_generation,retryable FROM delivery_attempts WHERE delivery_key=? AND step=?').bind(`access:${reminted.grantKey}:1`,`remove:team:${reminted.team}`).first();
      assert.deepEqual(fence,{attempt_generation:2,retryable:0});
      assert.equal((await http(`${ingress}/InviteBudget/org:${reminted.account}/snapshot`)).content_used_hour,2);
      assert.deepEqual(await reminted.call('status'),reminted.original);
      console.log(`PASS signed access removal401 remints once: second ${loseAck?'lost ACK stays uncertain':'204 stays removed'}, generation2 fenced and charged`);
    }

    const expiry=await fixture(3);expiry.role='admin';
    await eventually(expiry.grantStatus,v=>v?.outcome.kind==='removed');
    assert.equal(deletes(expiry).length,1);assert.equal((await expiry.grantStatus()).outcome.reason,'access_expiry');
    assert.deepEqual(await expiry.call('status'),expiry.original);
    console.log('PASS actual Restate delayed access expiry in signed Worker: no manual tick, one team DELETE, promoted owner skipped');
  } finally {observe(null);}
}
