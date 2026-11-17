import assert from 'node:assert/strict';
import {randomBytes} from 'node:crypto';

// Exercises the standard signed Bulk owner and D1 vault; no recovery-only endpoint.
export async function bulkEmailHandoff({api,setup,vaultCall,status,key,budget,scenarios,db,eventually,http,admin,id,mail2}) {
  const binding=c=>({email_ref:c.email_ref,batch:c.batch,account_id:c.account_id,installation_id:c.installation_id});
  const batch=async(account,stagedAt)=>{
    const first=await setup(account,201,false,stagedAt);
    const second={...first,batch:{...first.batch,row:2},email_ref:randomBytes(32).toString('hex')};
    if(stagedAt!==null)await vaultCall('stage',binding(second),{email:mail2,now:stagedAt});
    const emails=[first,second],at=first.approved_at;
    const query={batch_id:first.batch.batch_id,admin:{account_id:account,user_id:7}};
    const review={manifest:{...query,installation_id:first.installation_id,organization_installation_id:null,targets:{organization:first.organization,teams:[],repos:[]},access_lifetime_seconds:null,entries:emails.map(c=>({kind:'email',email_ref:c.email_ref}))},rows:emails.map(c=>({row:c.batch.row,evidence:{row:c.batch.row,resolution:{kind:'email'},all_targets_satisfied:false,observed_at:at,next_budget_slot:null},status:{kind:'email_org_only'}})),reviewed_at:at,revision:1,removed:[],personal_link_rows:[],submitted:null};
    await api('BulkInviteBatch',query.batch_id,'create',{operation_id:id(),review});
    return {emails,query,submit:{query,operation_id:id(),expected_revision:1,submission:{submitted_by:7,requests:[],emails,personal_links:[],submitted_at:at}}};
  };
  const record=c=>db.prepare('SELECT * FROM email_vault WHERE email_ref=?').bind(c.email_ref).first();
  const retired=c=>eventually(()=>record(c),r=>r?.retired_at!=null);
  const noFence=async c=>assert.equal((await db.prepare('SELECT COUNT(*) AS n FROM email_delivery_attempts WHERE id=?').bind(`email:${key(c)}`).first()).n,0);
  const sql=query=>http(`${admin}/query`,{query});

  const {emails,query,submit}=await batch(915,Math.floor(Date.now()/1000));
  await db.prepare("CREATE TRIGGER fixture_bulk_email_activation_failure BEFORE UPDATE OF activated_at ON email_vault WHEN OLD.row_number=2 BEGIN SELECT RAISE(ABORT,'fixture activation unavailable'); END").run();
  let original,first;
  try {
    const pending=await api('BulkInviteBatch',query.batch_id,'submit/send',submit);
    assert.ok(pending.invocationId);
    original=await retired(emails[0]);
    first=await status(emails[0]);assert.equal(first.outcome.kind,'created');
    assert.equal((await scenarios.get(915).posts),1);await noFence(emails[1]);
    await eventually(()=>sql(`SELECT value_utf8 FROM state WHERE service_name='BulkInviteBatch' AND service_key='${query.batch_id}' AND key='batch/current'`),r=>r.rows[0]&&JSON.parse(r.rows[0].value_utf8).handoff.emails===1);
    const second=await record(emails[1]);assert.equal(second.activated_at,null);assert.equal(second.retired_at,null);
    await eventually(()=>sql(`SELECT status FROM sys_invocation WHERE id='${pending.invocationId}'`),r=>r.rows[0]&&r.rows[0].status!=='completed');
    await http(`${admin}/invocations/${pending.invocationId}/kill`,undefined,'PATCH');
    await eventually(()=>sql(`SELECT status FROM sys_invocation WHERE id='${pending.invocationId}'`),r=>r.rows[0]?.status==='completed');
  } finally {
    await db.prepare('DROP TRIGGER fixture_bulk_email_activation_failure').run();
  }
  const accepted=await api('BulkInviteBatch',query.batch_id,'submit',submit);
  await retired(emails[1]);
  const second=await status(emails[1]);assert.equal(second.outcome.kind,'created');
  assert.notEqual(second.outcome.upstream_id,first.outcome.upstream_id);
  assert.deepEqual(await status(emails[0]),first);
  assert.deepEqual(await record(emails[0]),original);
  assert.equal((await scenarios.get(915).posts),2);assert.equal((await budget(915)).content_used_hour,2);
  assert.deepEqual(await api('BulkInviteBatch',query.batch_id,'submit',submit),accepted);
  const cancelled=await api('BulkInviteBatch',query.batch_id,'cancel_remaining',{query,operation_id:id()});
  assert.equal(cancelled.already_written,2);
  assert.deepEqual(await status(emails[0]),first);assert.deepEqual(await status(emails[1]),second);
  assert.deepEqual(await record(emails[0]),original);assert.equal((await scenarios.get(915).posts),2);
  console.log('PASS signed Bulk Submit killed after first retired child recovers exact handoff; two POSTs, unchanged receipts and retirement clocks');

  for(const [account,stagedAt] of [[916,null],[917,Math.floor(Date.now()/1000)-86401]]){
    const {emails,query,submit}=await batch(account,stagedAt);
    const accepted=await api('BulkInviteBatch',query.batch_id,'submit',submit);
    for(const c of emails){
      await eventually(()=>status(c),s=>s?.outcome.kind==='blocked'&&s.outcome.reason==='email_recovery_unavailable');
      await noFence(c);
    }
    assert.equal((await scenarios.get(account).posts),0);assert.equal((await budget(account)).content_used_hour,0);
    assert.deepEqual(await api('BulkInviteBatch',query.batch_id,'submit',submit),accepted);
    const cancel={query,operation_id:id()};
    const cancelled=await api('BulkInviteBatch',query.batch_id,'cancel_remaining',cancel);
    assert.equal(cancelled.aborted,2);
    assert.deepEqual(await api('BulkInviteBatch',query.batch_id,'cancel_remaining',cancel),cancelled);
    for(const c of emails)await noFence(c);
    assert.equal((await scenarios.get(account).posts),0);assert.equal((await budget(account)).content_used_hour,0);
  }
  console.log('PASS signed Bulk missing/expired mailbox recovery retains precise Blocked, exact accepted Submit and cancellability without fence, budget charge or POST');
}
