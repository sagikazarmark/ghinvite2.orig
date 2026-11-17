import { registerFinalLogScan } from './runtime-output.mjs';
import assert from 'node:assert/strict';
import {randomBytes} from 'node:crypto';
import {fileURLToPath} from 'node:url';
import {orgApp} from './github-world.mjs';
import {GITHUB_TOKEN} from './octoemu.mjs';
import {bulkEmailHandoff} from './bulk-email-handoff.mjs';
export const emailRing='kekring1;name=p1-email;current=1;hwm=1;k1=BwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwc.6b3451284885d811';
const MAIL='EMAIL_WORKER_PRIVACY_MARKER+tag@example.com',MAIL2='EMAIL_WORKER_PRIVACY_MARKER+second@example.com';
// GitHub (octoemu): the owner (7) and the organization App; each scenario
// adds its organization `email-N` and installation N+1000.
export const seed=()=>({users:[{id:7,login:'owner'}],apps:[orgApp]});
export function emailVaultWorker(){return {
  name:'email-vault-web',modules:true,scriptPath:fileURLToPath(new URL('./worker.mjs',import.meta.url)),
  modulesRules:[{type:'CompiledWasm',include:['**/*.wasm']}],compatibilityDate:'2024-09-23',compatibilityFlags:['nodejs_compat'],
  d1Databases:{DB:'journey-shared'},bindings:{P1_EMAIL_KEYRING:emailRing},
  outboundService(){throw new Error('email staging must not send HTTP');},
};}
export async function emailDelivery({mf,ingress,admin,http,id,db,eventually,github,logs}) {
  const emulator=github.emulator;
  // GitHub's answer to each organization's invitation POST: 201 (octoemu
  // creates it), else an injected 503 (nothing created) or 422.
  const posts=async account=>(await github.requests('POST',`/orgs/email-${account}/invitations`)).length;
  const scenarios={get:account=>({get posts(){return posts(account);}})};
  const vault=await mf.getWorker('email-vault-web');
  const vaultCall=async(op,binding,extra={})=>{
    const response=await vault.fetch(`https://fixture.test/__fixture/email-vault/${op}`,{method:'POST',headers:{'content-type':'application/json'},body:JSON.stringify({binding,now:Math.floor(Date.now()/1000),...extra})});
    assert.equal(response.status,200,await response.clone().text());return response.json();
  };
  const api=(service,key,handler,input)=>http(`${ingress}/${service}/${key}/${handler}`,input);
  const key=c=>`${c.batch.batch_id}:${c.batch.row}:${c.organization.org_id}`;
  const budget=account=>api('InviteBudget',`org:${account}`,'snapshot');
  const status=c=>api('EmailInviteDelivery',key(c),'status');
  const setup=async(account,code,activate=true,stagedAt=Math.floor(Date.now()/1000))=>{
    const installation=account+1000,at=new Date().toISOString();
    await emulator.seed({orgs:[{id:account,login:`email-${account}`,created_at:'2020-01-01T00:00:00Z'}],memberships:[{org:`email-${account}`,user:'owner',role:'admin',state:'active'}]});
    await emulator.admin('POST','/installations',{installation:{id:installation,app_id:456,account:`email-${account}`,repository_selection:'all',created_at:'2026-01-01T00:00:00Z'}},201);
    if(code!==201)await emulator.fault({method:'POST',path:`/orgs/email-${account}/invitations`,label:`email ${code}`,action:{kind:'respond',status:code,
      body:{message:code===422?'Validation Failed':'Service Unavailable',documentation_url:'https://docs.github.com/rest/orgs/members#create-an-organization-invitation'}}});
    await api('Installation',`org:${installation}`,'onboard',{operation_id:id(),verified:{app:'org',account:{installation_id:installation,account_id:account,account_login:`email-${account}`,account_type:'Organization',selected_repos:[],installed_at:'2026-01-01T00:00:00Z',uninstalled_at:null},suspended_at:null,permissions:{members:'write',metadata:'read'}}});
    const c={batch:{batch_id:id(),row:1},account_id:account,installation_id:installation,organization:{org_id:account,org_login:`email-${account}`,grant:'member'},teams:[],email_ref:randomBytes(32).toString('hex'),approved_by:7,approved_at:at};
    const binding={email_ref:c.email_ref,batch:c.batch,account_id:account,installation_id:installation};
    if(stagedAt!==null)await vaultCall('stage',binding,{email:MAIL,now:stagedAt});if(activate)await vaultCall('activate',binding);
    return c;
  };
  const reconcile=c=>http(`${ingress}/OrganizationReconcile/target`,{account:{app:'org',account_id:c.account_id},target_id:c.organization.org_id,deliveries:[`email:${key(c)}`]});
  const projected=async c=>eventually(()=>db.prepare('SELECT receipt FROM email_deliveries WHERE id=?').bind(`email:${key(c)}`).first(),r=>r!==null);
  try {
    for(const service of ['BulkInviteBatch','BulkProjection','EmailInviteDelivery','EmailDeliveryProjection','InviteBudget','InviteBudgetProjection','OrganizationReconcile'])await http(`${admin}/services/${service}`,{journal_retention:'1h',idempotency_retention:'1h'},'PATCH');
    await db.prepare('INSERT INTO users(user_id,login,avatar_url,last_seen_at) VALUES(7,?,NULL,?)').bind('owner',new Date().toISOString()).run();
    // Real batch Submit activates this staged binding; fixture does not activate it.
    const c=await setup(910,201,false),at=c.approved_at;
    const query={batch_id:c.batch.batch_id,admin:{account_id:910,user_id:7}};
    const review={manifest:{...query,installation_id:c.installation_id,organization_installation_id:null,targets:{organization:c.organization,teams:[],repos:[]},access_lifetime_seconds:null,entries:[{kind:'email',email_ref:c.email_ref}]},rows:[{row:1,evidence:{row:1,resolution:{kind:'email'},all_targets_satisfied:false,observed_at:at,next_budget_slot:null},status:{kind:'email_org_only'}}],reviewed_at:at,revision:1,removed:[],personal_link_rows:[],submitted:null};
    await api('BulkInviteBatch',query.batch_id,'create',{operation_id:id(),review});assert.equal(await scenarios.get(910).posts,0);
    const submission={query,operation_id:id(),expected_revision:1,submission:{submitted_by:7,requests:[],emails:[c],personal_links:[],submitted_at:at}};
    await db.prepare("CREATE TRIGGER fixture_email_retire_failure BEFORE UPDATE OF retired_at ON email_vault BEGIN SELECT RAISE(ABORT,'fixture retirement unavailable'); END").run();
    const accepted=await api('BulkInviteBatch',query.batch_id,'submit',submission);
    const created=await eventually(()=>status(c),s=>s?.outcome.kind==='created');
    // The invitation octoemu created, for the staged address.
    const invited=(await emulator.state()).world.org_invitations.filter(value=>value.org==='email-910');
    assert.deepEqual(invited.map(value=>[value.id,value.email]),[[created.outcome.upstream_id,MAIL]]);
    const queryState=sql=>http(`${admin}/query`,{query:sql});
    const item=async()=>{const r=await queryState(`SELECT value_utf8 FROM state WHERE service_name='BulkInviteBatch' AND service_key='${query.batch_id}' AND key='batch/item/1'`);return r.rows[0]?JSON.parse(r.rows[0].value_utf8):null;};
    await eventually(item,v=>v?.Email?.revision===created.revision);
    const pending=await eventually(()=>queryState(`SELECT id,status FROM sys_invocation WHERE target_service_name='EmailInviteDelivery' AND target_service_key='${key(c)}' AND target_handler_name='create' AND status!='completed'`),r=>r.rows.length===1);
    assert.equal((await db.prepare('SELECT retired_at FROM email_vault WHERE email_ref=?').bind(c.email_ref).first()).retired_at,null);
    const interrupted=pending.rows[0].id;
    await http(`${admin}/invocations/${interrupted}/kill`,undefined,'PATCH');
    await eventually(()=>queryState(`SELECT status FROM sys_invocation WHERE id='${interrupted}'`),r=>r.rows[0]?.status==='completed');
    await db.prepare('DROP TRIGGER fixture_email_retire_failure').run();
    assert.deepEqual(await api('EmailInviteDelivery',key(c),'cancel',c),created);
    const retired=await db.prepare('SELECT retired_at,purge_after FROM email_vault WHERE email_ref=?').bind(c.email_ref).first();
    const confirmed=Math.floor(Date.parse(created.confirmed_at)/1000);
    assert.deepEqual(retired,{retired_at:confirmed,purge_after:confirmed+86400});
    for(let n=0;n<3;n++)await api('BulkInviteBatch',query.batch_id,'on_item_progress',{kind:'email',source:c.batch});
    assert.deepEqual((await item()).Email,created);
    console.log('PASS signed Worker killed email completion drains callback and original-time retirement; one POST and exact owner receipt');

    assert.deepEqual(await api('BulkInviteBatch',query.batch_id,'submit',submission),accepted);
    for(let n=0;n<3;n++)assert.deepEqual(await api('EmailInviteDelivery',key(c),'create',c),created);
    const changed=await fetch(`${ingress}/EmailInviteDelivery/${key(c)}/create`,{method:'POST',headers:{'content-type':'application/json'},body:JSON.stringify({...c,approved_by:9})});assert.equal(changed.status,409);await changed.text();
    assert.deepEqual(await api('EmailInviteDelivery',key(c),'cancel',c),created);
    assert.equal(await scenarios.get(910).posts,1);assert.equal((await budget(910)).content_used_hour,1);
    await projected(c);
    // Still pending on GitHub.
    await reconcile(c);
    const pendingObserved=await eventually(()=>status(c),v=>Boolean(v?.last_observed_at));
    assert.deepEqual(pendingObserved.outcome,created.outcome);
    assert.equal(pendingObserved.confirmed_at,created.confirmed_at);
    assert.equal(pendingObserved.settlement,null);
    const pendingProjection=await eventually(()=>db.prepare('SELECT receipt FROM email_deliveries WHERE id=?').bind(`email:${key(c)}`).first(),r=>r&&JSON.parse(r.receipt).last_observed_at===pendingObserved.last_observed_at);
    assert.deepEqual(JSON.parse(pendingProjection.receipt),pendingObserved);
    // The owner cancels it on GitHub: it is no longer pending.
    await github.api('DELETE',`/orgs/email-910/invitations/${created.outcome.upstream_id}`,undefined,{installation:c.installation_id,expected:204});
    await reconcile(c);
    const settled=await eventually(()=>status(c),s=>s?.settlement?.kind==='no_longer_pending');
    assert.deepEqual(settled.outcome,created.outcome);
    assert.ok(Date.parse(settled.last_observed_at)>=Date.parse(pendingObserved.last_observed_at));
    await eventually(item,v=>v?.Email?.revision===settled.revision);
    assert.deepEqual((await item()).Email,settled);
    assert.equal(settled.settlement.kind,'no_longer_pending');
    await eventually(()=>db.prepare('SELECT event_type FROM audit_events WHERE account_id=910 AND event_type=?').bind('email_invitation.no_longer_pending').all(),r=>r.results.length===1);
    console.log('PASS signed Worker standard Bulk activates encrypted email; one POST, exact replay/conflict, cancel preserves confirmed result, neutral shared reconciliation and atomic audit');

    const unknown=await setup(911,503);const lost=await api('EmailInviteDelivery',key(unknown),'create',unknown);assert.equal(lost.outcome.kind,'outcome_unknown');assert.ok(Date.parse(lost.unknown_since)>=Date.parse(unknown.approved_at));assert.equal(lost.last_observed_at,undefined);
    await reconcile(unknown);for(let n=0;n<3;n++)assert.deepEqual(await api('EmailInviteDelivery',key(unknown),'create',unknown),lost);
    assert.deepEqual(await api('EmailInviteDelivery',key(unknown),'cancel',unknown),lost);
    assert.equal(await scenarios.get(911).posts,1);assert.equal((await budget(911)).content_used_hour,1);
    const fence=await db.prepare('SELECT generation,retryable FROM email_delivery_attempts WHERE id=?').bind(`email:${key(unknown)}`).first();assert.deepEqual(fence,{generation:1,retryable:0});
    const unknownAudit=await eventually(()=>db.prepare("SELECT * FROM audit_events WHERE target_id=? AND event_type='delivery.outcome_unknown'").bind(`email:${key(unknown)}`).all(),r=>r.results.length===1);
    assert.equal(unknownAudit.results[0].occurred_at,lost.unknown_since);
    assert.equal(JSON.parse(unknownAudit.results[0].metadata).transition_revision,lost.revision);
    console.log('PASS signed Worker ambiguous POST retains one charge/full-command fence; absence/replay/cancel never write or invent acceptance');

    const rejected=await setup(912,422);const failure=await api('EmailInviteDelivery',key(rejected),'create',rejected);assert.equal(failure.outcome.kind,'failed');
    assert.deepEqual(await api('EmailInviteDelivery',key(rejected),'create',rejected),failure);assert.equal(await scenarios.get(912).posts,1);assert.equal((await budget(912)).content_used_hour,0);
    assert.equal((await db.prepare('SELECT retryable FROM email_delivery_attempts WHERE id=?').bind(`email:${key(rejected)}`).first()).retryable,1);
    console.log('PASS signed Worker positive422 releases only exact fence generation and unused budget; immutable failure does not POST again');

    const staged=await setup(913,201,false);const blocked=await api('EmailInviteDelivery',key(staged),'create',staged);assert.equal(blocked.outcome.kind,'blocked');assert.equal(blocked.outcome.reason,'email_recovery_unavailable');assert.equal(await scenarios.get(913).posts,0);assert.equal((await budget(913)).content_used_hour,0);
    const blockedAudit=await eventually(()=>db.prepare("SELECT * FROM audit_events WHERE target_id=? AND event_type='delivery.blocked'").bind(`email:${key(staged)}`).all(),r=>r.results.length===1);
    const firstBlocked=blockedAudit.results[0];
    assert.equal(JSON.parse(firstBlocked.metadata).detail.reason,'email_recovery_unavailable');
    await api('EmailInviteDelivery',key(staged),'create',staged);
    const repeatedBlocked=await db.prepare("SELECT * FROM audit_events WHERE target_id=? AND event_type='delivery.blocked'").bind(`email:${key(staged)}`).all();
    assert.deepEqual(repeatedBlocked.results,[firstBlocked]);
    for(const event of [...unknownAudit.results,...repeatedBlocked.results]){
      const text=JSON.stringify(event); assert.equal(text.includes('email_ref'),false); assert.equal(text.includes(unknown.email_ref),false); assert.equal(text.includes(staged.email_ref),false);
    }
    console.log('PASS signed email original-time Blocked/OutcomeUnknown audit facts, replay dedupe, no mailbox or vault reference');
    const stopped=await setup(914,201,false);const aborted=await api('EmailInviteDelivery',key(stopped),'cancel',stopped);assert.deepEqual(aborted.outcome,{kind:'aborted',reason:'batch_cancelled'});assert.deepEqual(await api('EmailInviteDelivery',key(stopped),'create',stopped),aborted);assert.equal(await scenarios.get(914).posts,0);assert.equal((await budget(914)).content_used_hour,0);
    console.log('PASS signed Worker staged-only recovery is denied/refunded; preinitialization cancellation prevents later POST');
    // Exercise the raw D1 boundary independently of Rust projection validation.
    const guardBatch=id(),guardId=`email:${guardBatch}:1:910`;
    const guard={command:{...c,batch:{batch_id:guardBatch,row:1},approved_at:'2026-01-01T00:00:00.000000001Z'},revision:1,outcome:{kind:'planned'}};
    const insert=()=>db.prepare('INSERT INTO email_deliveries(id,batch_id,row_number,account_id,receipt) VALUES(?,?,1,910,?)').bind(guardId,guardBatch,JSON.stringify(guard)).run();
    for(const bad of [4,'invalid','2026-01-01T00:00:00Z','2026-01-01T00:00:00.000000000Z']){guard.unknown_since=bad;await assert.rejects(insert);}
    delete guard.unknown_since;await insert();
    Object.assign(guard,{revision:2,unknown_since:'2026-01-01T00:00:00.000000002Z',last_observed_at:'2026-01-01T00:00:00.000000003Z'});
    const update=v=>db.prepare('UPDATE email_deliveries SET receipt=? WHERE id=?').bind(JSON.stringify(v),guardId).run();await update(guard);
    for(const [field,bad] of [['unknown_since',null],['unknown_since','2026-01-01T00:00:00.000000003Z'],['last_observed_at',null],['last_observed_at','2026-01-01T00:00:00.000000002Z'],['confirmed_at','2026-01-01T00:00:00.000000004Z']])await assert.rejects(()=>update({...guard,revision:3,[field]:bad}));
    assert.deepEqual(JSON.parse((await db.prepare('SELECT receipt FROM email_deliveries WHERE id=?').bind(guardId).first()).receipt),guard);
    await update({...guard,revision:3,last_observed_at:'2026-01-01T00:00:00.000000004Z'});
    await db.prepare('DELETE FROM email_deliveries WHERE id=?').bind(guardId).run();
    console.log('PASS signed email pending-observation watermark, immutable first-unknown clock, and actual D1 nanosecond/no-clear raw guards');
    await bulkEmailHandoff({api,setup,vaultCall,status,key,budget,scenarios,db,eventually,http,admin,id,mail2:MAIL2});
    const trace=await github.calls();
    const minted=new Map((await emulator.state()).tokens.map(token=>[token.id,token.permissions.members]));
    const installed=trace.filter(t=>t.identity?.kind==='installation');
    assert.deepEqual([...new Set(installed.map(t=>minted.get(t.identity.token_id)))].sort(),['read','write']);
    for(const t of installed)assert.equal(minted.get(t.identity.token_id),t.method==='GET'?'read':'write',`${t.method} ${t.path}`);
    assert.ok(!trace.some(t=>t.method==='DELETE'||t.path.includes('/memberships/')||t.path.startsWith('/users/')));
    assert.equal((await db.prepare('SELECT COUNT(*) AS n FROM invitation_requests').first()).n,0);
    assert.equal((await db.prepare('SELECT COUNT(*) AS n FROM invitation_links').first()).n,0);

    const markers=[MAIL,MAIL2,'EMAIL_WORKER_PRIVACY_MARKER'];
    const scan=bytes=>{for(const marker of markers)assert.equal(Buffer.from(bytes).includes(Buffer.from(marker)),false,'private email/token marker escaped transient boundary');
      assert.doesNotMatch(Buffer.from(bytes).toString('latin1'),GITHUB_TOKEN,'a GitHub token escaped its transient boundary');};
    registerFinalLogScan(scan);
    const scanValue=value=>{
      if(typeof value==='string'){
        scan(Buffer.from(value));if(/^(?:[a-fA-F0-9]{2})+$/.test(value))scan(Buffer.from(value,'hex'));
        if(/^[A-Za-z0-9+/_-]+={0,2}$/.test(value))scan(Buffer.from(value,'base64'));
      }else if(Array.isArray(value))value.forEach(scanValue);else if(value&&typeof value==='object')Object.values(value).forEach(scanValue);
    };
    const sql=query=>http(`${admin}/query`,{query});
    const journals=await sql('SELECT raw FROM sys_journal LIMIT 100001');assert.ok(journals.rows.length>0&&journals.rows.length<=100000);
    for(const row of journals.rows){assert.match(row.raw,/^(?:[a-fA-F0-9]{2})*$/);scan(Buffer.from(row.raw,'hex'));}
    const states=await sql('SELECT * FROM state LIMIT 100001');assert.ok(states.rows.length>0&&states.rows.length<=100000);states.rows.forEach(scanValue);
    const tables=(await db.prepare("SELECT name FROM sqlite_master WHERE type='table'").all()).results.filter(t=>t.name!=='_cf_METADATA');
    for(const {name} of tables){assert.match(name,/^[a-zA-Z_][a-zA-Z_0-9]*$/);const rows=(await db.prepare(`SELECT * FROM "${name}" LIMIT 100001`).all()).results;assert.ok(rows.length<=100000);rows.forEach(scanValue);}
    logs().forEach(text=>scan(Buffer.from(text)));
    console.log(`PASS complete bounded signed email privacy scan: ${journals.rows.length} journal rows, ${states.rows.length} state rows, all ${tables.length} D1 tables and body-prefix runtime logs (launcher verifies shutdown suffix); no link uses or requester identity`);
  } finally {await emulator.clearFaults();}
}
