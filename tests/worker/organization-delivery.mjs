import assert from 'node:assert/strict';
import { isDeepStrictEqual } from 'node:util';
import { orgApp } from './github-world.mjs';

const account=840,installation=8402,user=8,team=50;

// GitHub (octoemu): `org-fixture` (840, owner 7) with the team `devs` (50),
// alice (8), the organization App installed as 8402; and, for the spam
// case, `spam-fixture` (841) with `spam-user` (9), installed as 8412.
export const seed = () => ({
  users:[{id:7,login:'owner'},{id:user,login:'alice'},{id:9,login:'spam-user'},{id:6,login:'spam-owner'}],
  orgs:[{id:account,login:'org-fixture',created_at:'2020-01-01T00:00:00Z',two_factor_requirement_enabled:true,default_repository_permission:'read'},
    {id:841,login:'spam-fixture',created_at:'2020-01-01T00:00:00Z'}],
  teams:[{id:team,org:'org-fixture',slug:'devs',name:'Developers'}],
  apps:[orgApp],
  installations:[{id:installation,app_id:456,account:'org-fixture',repository_selection:'all',created_at:'2026-01-01T00:00:00Z'},
    {id:8412,app_id:456,account:'spam-fixture',repository_selection:'all',created_at:'2026-01-01T00:00:00Z'}],
  memberships:[{org:'org-fixture',user:'owner',role:'admin',state:'active'},{org:'spam-fixture',user:'spam-owner',role:'admin',state:'active'}],
});

// The installation token's read of the organization with the plan's seats.
// GitHub shows `plan` only to a token holding `organization_plan`
// (documented), which ghinvite's ORG_READ profile does not ask for, so the
// emulator leaves it out; this injected answer is what such a token would
// see, keeping the NoSeat path reachable (see the GE-09b-2 notes).
const organization=filled=>({id:account,login:'org-fixture',created_at:'2020-01-01T00:00:00Z',
  plan:{name:'enterprise',seats:10,filled_seats:filled},two_factor_requirement_enabled:true,default_repository_permission:'read'});

// Signed Restate -> production Worker composition -> real D1 and budget.
// GitHub is octoemu, with distinct read/write credentials and numeric IDs.
export async function organizationDelivery({ingress,http,id,db,eventually,github}) {
  const emulator=github.emulator;
  const trace=async()=>(await github.calls()).map(call=>({method:call.method,path:call.path.split('?')[0],identity:call.identity}));
  let plan=await emulator.fault({method:'GET',path:'/orgs/org-fixture',label:'plan seats',action:{kind:'respond',status:200,body:organization(10)}});
  try {
    for(const [uid,login] of [[7,'owner'],[user,'alice']]) await db.prepare('INSERT INTO users(user_id,login,avatar_url,last_seen_at) VALUES (?, ?, NULL, ?)').bind(uid,login,new Date().toISOString()).run();
    const onboarding={operation_id:id(),verified:{app:'org',account:{installation_id:installation,account_id:account,account_login:'org-fixture',account_type:'Organization',selected_repos:[],installed_at:'2026-01-01T00:00:00Z',uninstalled_at:null},suspended_at:null,permissions:{members:'write',metadata:'read'}}};
    await http(`${ingress}/Installation/org:${installation}/onboard`,onboarding);
    const facts=()=>http(`${ingress}/OrganizationEligibility/org:${account}/facts`);
    const cached=await facts();
    assert.deepEqual(cached.seats,{capacity:10,used:10});
    assert.equal(cached.two_factor_required,true);assert.equal(cached.base_permission,'read');
    assert.equal(cached.managed_enterprise,null);
    const reads=(await trace()).length;
    assert.deepEqual(await facts(),cached);
    assert.equal((await trace()).length,reads,'same-hour cache does not repeat GitHub reads');
    const link=id(),secret='2a'.repeat(32);
    const creation={link_id:link,secret_sha256:secret,account_id:account,installation_id:installation,budget_share_pct:17,admin:{account_id:account,user_id:7},description:'Signed org delivery',max_uses:10,approval_required:false,targets:{organization:{org_id:account,org_login:'org-fixture',grant:'member'},teams:[{team_id:team,team_slug:'devs',grant:'member'}],repos:[]}};
    assert.deepEqual(await http(`${ingress}/InvitationLink/${link}/create`,creation),{Ok:{link_id:link,secret_generation:1}});
    const requesterPage=await http(`${ingress}/InvitationLink/${link}/requester_page`,{link_id:link,secret_sha256:secret,secret_generation:1,requester_id:user});
    assert.equal(requesterPage.two_factor_required,true);
    assert.equal(requesterPage.can_start_fresh,true);
    for(const key of ['seats','base_permission','installation_id','internal_note','managed_enterprise']) assert.equal(Object.hasOwn(requesterPage,key),false,`requester page must not expose ${key}`);
    const attempt={scope_evidence:[],secret_sha256:secret,secret_generation:1,link_id:link,operation_id:id(),requester_id:user,justification:null};
    const admission=await http(`${ingress}/InvitationLink/${link}/admit`,attempt);
    assert.equal(admission.result.kind,'accepted');
    const request=admission.result.request_id,key=`${request}:${account}:1`;
    const status=()=>http(`${ingress}/OrganizationDelivery/${key}/status`);
    await eventually(status,value=>value?.create.outcome.kind==='blocked'&&value.create.outcome.reason==='no_seat');
    assert.equal((await trace()).filter(t=>t.method==='POST'&&t.path==='/orgs/org-fixture/invitations').length,0);
    const auditRows=async()=> (await db.prepare("SELECT id,event_type,occurred_at,target_kind,target_id,metadata FROM audit_events WHERE request_id=? AND event_type IN ('delivery.blocked','delivery.outcome_unknown','delivery.held','delivery.released') ORDER BY id").bind(request).all()).results;
    const blockedAudits=await eventually(auditRows,rows=>rows.length===1);
    assert.deepEqual(blockedAudits.map(row=>row.target_id).sort(),[`org:${key}`]);
    for(const row of blockedAudits) {
      const fact=JSON.parse(row.metadata);
      assert.equal(row.event_type,'delivery.blocked');
      assert.equal(fact.delivery_generation,1);
      assert.equal(fact.transition.kind,'blocked');
      assert.deepEqual(fact.transition.reason,{kind:'organization',reason:'no_seat'});
      assert.ok(fact.transition_revision>0);
    }
    const blocked=await status();
    for(let replay=0;replay<2;replay++) await http(`${ingress}/OrganizationDelivery/${key}/create`,blocked.create.command);
    assert.deepEqual(await auditRows(),blockedAudits,'repeated NoSeat polls preserve exact audit identity and time');
    await emulator.removeFault(plan);
    plan=await emulator.fault({method:'GET',path:'/orgs/org-fixture',label:'plan seats',action:{kind:'respond',status:200,body:organization(9)}});
    // A verified installation event refreshes facts, but cached facts alone do
    // not bypass the retained daily NoSeat deadline.
    await http(`${ingress}/Installation/org:${installation}/onboard_webhook`,{delivery_id:'ab'.repeat(32),updated_at:new Date().toISOString(),action:'new_permissions_accepted',verified:onboarding.verified,sender_id:7,sender_login:'owner'});
    await eventually(facts,value=>value?.seats?.used===9);
    assert.equal((await status()).create.outcome.reason,'no_seat');
    await eventually(()=>http(`${ingress}/SweepSchedule/org:${account}/status`),value=>value?.tracked?.[account]?.includes(key));
    const capacityHint={account_id:account,installation_id:installation,removed_user_id:999,payload_sha256:Array(32).fill(42)};
    await http(`${ingress}/OrganizationEligibility/org:${account}/member_removed`,capacityHint);
    const receipt=await eventually(status,value=>value?.create.outcome.kind==='created');
    const afterWake=(await trace()).length;
    await http(`${ingress}/OrganizationEligibility/org:${account}/member_removed`,capacityHint);
    assert.equal((await trace()).length,afterWake,'exact account hint replay does not repeat delivery checks');
    const upstream=(await github.api('GET','/orgs/org-fixture/invitations',undefined,{installation})).find(value=>value.login==='alice').id;
    assert.equal(receipt.invitation_timing.upstream_id,upstream);
    assert.equal(Date.parse(receipt.invitation_timing.created_at),Date.parse(receipt.create.outcome.created_at));
    assert.equal(receipt.invitation_timing.owned_lifetime_seconds,604800);
    assert.ok(Date.parse(receipt.invitation_timing.observed_at)>=Date.parse(receipt.invitation_timing.created_at));
    assert.ok(Date.parse(receipt.invitation_timing.observed_at)>=Date.parse(receipt.create.command.approved_at));
    assert.equal(receipt.create.command.installation_id,installation);
    assert.equal(receipt.create.command.budget_share_pct,17);
    assert.equal(receipt.teams[team].create.outcome.kind,'created');
    assert.equal(receipt.teams[team].create.outcome.via.upstream_id,upstream);
    assert.deepEqual(await http(`${ingress}/InvitationLink/${link}/admit`,attempt),admission);
    const query={link_id:link,request_id:request,requester_id:user};
    for(let n=0;n<3;n++) {
      const recovery=await http(`${ingress}/InvitationRequest/${request}/recover`,query);
      assert.deepEqual(recovery.plan.commands,[]);
      assert.deepEqual(recovery.submitted_organization.command,receipt.create.command);
      assert.deepEqual(await status(),receipt);
    }
    assert.deepEqual(await http(`${ingress}/InvitationRequest/${request}/organization_delivery_progress`,query),{org_id:account,team_ids:[team],stage:'submitted',delivery_generation:1});
    await eventually(async()=>{
      const row=await db.prepare("SELECT o.receipt FROM delivery_outcomes o JOIN deliveries d ON d.id=o.delivery_id WHERE d.kind='org' AND d.id=?").bind(`org:${key}`).first();
      return row?JSON.parse(row.receipt):null;
    },value=>isDeepStrictEqual(value,receipt));
    const budget=await http(`${ingress}/InviteBudget/org:${account}/snapshot`);
    const orgBucket=budget.buckets.find(b=>b.bucket.kind==='org');
    assert.equal(orgBucket.used_24h,1,'exact recovery cannot charge another invitation');
    const minted=new Map((await emulator.state()).tokens.map(token=>[token.id,token.permissions.members]));
    const calls=await trace();
    assert.deepEqual([...new Set(calls.filter(t=>t.identity?.kind==='installation').map(t=>minted.get(t.identity.token_id)))].sort(),['read','write']);
    for(const t of calls.filter(t=>t.identity?.kind==='installation')) assert.equal(minted.get(t.identity.token_id),t.method==='GET'?'read':'write',`${t.method} ${t.path} uses the matching profile`);
    const posts=calls.filter(t=>t.method==='POST'&&t.path==='/orgs/org-fixture/invitations');
    assert.equal(posts.length,1);
    assert.equal(calls.filter(t=>t.method==='PUT').length,0,'all selected teams ride the original org POST');
    assert.deepEqual(await auditRows(),blockedAudits,'later confirmation preserves historical NoSeat facts without duplicate transient events');
    assert.equal(receipt.create.audit_version,'canonical_v2');
    const sent=await eventually(async()=> (await db.prepare("SELECT id,target_kind,target_id FROM audit_events WHERE request_id=? AND event_type='org_invitation.sent'").bind(request).all()).results,rows=>rows.length===1);
    assert.equal(sent[0].target_id,`org:${key}`);
    assert.equal(sent[0].target_kind,'organization_delivery');
    const telemetry=await eventually(async()=> (await db.prepare('SELECT step,unknown_since,last_observed_at FROM organization_step_telemetry WHERE delivery_id=? ORDER BY step').bind(`org:${key}`).all()).results,rows=>rows.length===2&&rows.every(row=>row.last_observed_at));
    assert.deepEqual(telemetry.map(row=>row.step),['org',`team:${team}`]);
    assert.ok(telemetry.every(row=>row.unknown_since===null),'positive no-seat refusal must never manufacture unknown onset');
    assert.ok(telemetry.every(row=>Date.parse(row.last_observed_at)>=Date.parse(receipt.create.command.approved_at)));

    console.log('PASS signed Worker organization eligibility cache, installation refresh, NoSeat account wake and duplicate receipt; organization delivery: real onboarding/admission/dispatch/budget/D1, exact ORG_READ/ORG_WRITE mint, one POST with all teams, immutable admission/recovery and requester progress; exact NoSeat audit fact survives confirmation and replays; canonical org invitation audit, per-step observation clocks and minimal requester 2FA advisory');
  } finally { await emulator.removeFault(plan); }
  await organizationSpam({ingress,http,id,db,eventually,github});
}

// A different real account/budget isolates this refusal from the successful
// invitation above. No direct record_limit injection: the HTTP response must
// pass through the owner, retained completion, governor and D1 projector.
async function organizationSpam({ingress,http,id,db,eventually,github}) {
  const account=841,installation=8412,user=9,login='spam-fixture';
  // GitHub flags the organization: its invitation writes answer `spammed`.
  await github.emulator.admin('POST',`/orgs/${login}/spammed`,{spammed:true});
  const posts=async()=>(await github.requests('POST',`/orgs/${login}/invitations`)).length;
  try {
    await db.prepare('INSERT INTO users(user_id,login,avatar_url,last_seen_at) VALUES (?, ?, NULL, ?)').bind(user,'spam-user',new Date().toISOString()).run();
    await http(`${ingress}/Installation/org:${installation}/onboard`,{operation_id:id(),verified:{app:'org',account:{installation_id:installation,account_id:account,account_login:login,account_type:'Organization',selected_repos:[],installed_at:'2026-01-01T00:00:00Z',uninstalled_at:null},suspended_at:null,permissions:{members:'write',metadata:'read'}}});
    const link=id(),secret='2b'.repeat(32);
    assert.deepEqual(await http(`${ingress}/InvitationLink/${link}/create`,{link_id:link,secret_sha256:secret,account_id:account,installation_id:installation,admin:{account_id:account,user_id:7},description:'Signed spam observation',max_uses:10,approval_required:false,targets:{organization:{org_id:account,org_login:login,grant:'member'},teams:[],repos:[]}}),{Ok:{link_id:link,secret_generation:1}});
    const admission=await http(`${ingress}/InvitationLink/${link}/admit`,{scope_evidence:[],secret_sha256:secret,secret_generation:1,link_id:link,operation_id:id(),requester_id:user,justification:null});
    assert.equal(admission.result.kind,'accepted');
    const key=`${admission.result.request_id}:${account}:1`;
    const status=()=>http(`${ingress}/OrganizationDelivery/${key}/status`);
    const receipt=await eventually(status,value=>value?.create.outcome.kind==='throttled');
    const snapshot=()=>http(`${ingress}/InviteBudget/org:${account}/snapshot`);
    const first=await eventually(snapshot,value=>value?.limit_counts?.spammed===1);
    assert.equal(first.limit_counts.rate_limited,0);
    assert.equal(first.limit_counts.future_observations,0);
    assert.ok(first.limit_counts.coverage_started_at);
    const closed=first.buckets.find(bucket=>bucket.bucket.kind==='org').closed_until;
    assert.ok(Date.parse(closed)>Date.now()+23*3600*1000);
    for(let replay=0;replay<3;replay++) await http(`${ingress}/OrganizationDelivery/${key}/create`,receipt.create.command);
    assert.equal(await posts(),1,'positive spam refusal and retained retries must not repeat POST');
    const repeated=await snapshot();
    assert.deepEqual(repeated.limit_counts,first.limit_counts,'same classified refusal is counted once');
    assert.equal(repeated.buckets.find(bucket=>bucket.bucket.kind==='org').closed_until,closed);
    const refused=await http(`${ingress}/InviteBudget/org:${account}/reserve`,{request_key:'independent-after-org-spam',kind:{kind:'org_invite'},link_id:null});
    assert.equal(refused.kind,'retry_at');assert.equal(refused.why,'closed');
    const projected=await eventually(async()=>{
      const row=await db.prepare("SELECT snapshot_json FROM invite_budget_projection WHERE app='org' AND account_id=?").bind(account).first();
      return row?JSON.parse(row.snapshot_json):null;
    },value=>value?.limit_counts?.spammed===1);
    assert.deepEqual(projected.limit_counts,first.limit_counts,'D1 preserves classified refusal counts and coverage');
    assert.equal(projected.buckets.find(bucket=>bucket.bucket.kind==='org').closed_until,closed);
    console.log('PASS signed Worker org spam: one rejected POST, immutable retry refusal, one classified observation, closed org bucket and real D1 count persistence');
  } finally { await github.emulator.admin('POST',`/orgs/${login}/spammed`,{spammed:false}); }
}
