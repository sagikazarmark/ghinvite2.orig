import { registerFinalLogScan } from './runtime-output.mjs';
import assert from 'node:assert/strict';
import { hkdfSync, randomBytes } from 'node:crypto';
import { readFileSync, readdirSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { Miniflare, Log, LogLevel } from 'miniflare';
const NOW=1_800_000_000;
const EMAIL='PRIVATE_EMAIL_VAULT_MARKER+tag@example.com';
const ring=(current,keys)=>`akr1;name=p1;current=${current};${keys.map(([n,byte])=>{
  const root=Buffer.alloc(32,byte),version=Buffer.alloc(4);version.writeUInt32BE(n);
  const info=Buffer.concat([Buffer.from('anykms-keyring/v1/kcv'),Buffer.from([2]),Buffer.from('p1'),version]);
  const kcv=Buffer.from(hkdfSync('sha256',root,Buffer.from('anykms-keyring/v1/salt'),info,8));
  return `k${n}=${root.toString('base64url')}.${kcv.toString('hex')}`;
}).join(';')}`;
let logs='',overflow=false;
const worker=(name,keyring)=>({name,modules:true,scriptPath:fileURLToPath(new URL('./worker.mjs',import.meta.url)),modulesRules:[{type:'CompiledWasm',include:['**/*.wasm']}],compatibilityDate:'2024-09-23',compatibilityFlags:['nodejs_compat'],d1Databases:{DB:'email-vault-shared'},bindings:{P1_KEYRING:keyring},outboundService(){throw new Error('vault proof must not send network requests');}});
const mf=new Miniflare({log:new Log(LogLevel.ERROR),workers:[worker('a',ring(1,[[1,7]])),worker('b',ring(1,[[1,7]])),worker('newer',ring(2,[[1,7],[2,8]])),worker('wrong',ring(1,[[1,9]]))],handleRuntimeStdio(stdout,stderr){for(const stream of [stdout,stderr])stream.on('data',chunk=>{if(Buffer.byteLength(logs)+chunk.length>1024*1024)overflow=true;else logs+=chunk.toString();});}});
const binding=()=>({email_ref:randomBytes(32).toString('hex'),batch:{batch_id:'01ARZ3NDEKTSV4RRFFQ69G5FAV',row:1},account_id:42,installation_id:9});
try{
  const db=await mf.getD1Database('DB','a');
  for(const file of readdirSync(new URL('../../migrations/',import.meta.url)).filter(f=>f.endsWith('.sql')).sort()) {
    await db.exec(readFileSync(new URL('../../migrations/'+file,import.meta.url),'utf8').replace(/^[ \t]*--.*$/gm,'').replaceAll('\n',' '));
  }
  const fetchers=Object.fromEntries(await Promise.all(['a','b','newer','wrong'].map(async name=>[name,await mf.getWorker(name)])));
  const call=async(op,b,extra={},name='a',status=200)=>{
    const response=await fetchers[name].fetch('http://fixture.test/__fixture/email-vault/'+op,{method:'POST',headers:{'content-type':'application/json'},body:JSON.stringify({binding:b,now:NOW,...extra})});
    assert.equal(response.status,status,await response.clone().text());return response.json();
  };
  const row=async b=>db.prepare('SELECT * FROM email_vault WHERE email_ref=?').bind(b.email_ref).first();
  let b=binding();await call('stage',b,{email:EMAIL});const original=await row(b);
  assert.equal((await call('load',b,{},'a',412)).error,'email recovery is not active');
  await call('stage',b,{email:EMAIL,now:NOW+1});assert.deepEqual(await row(b),original);
  await call('stage',b,{email:'other@example.com'},'a',409);
  assert.deepEqual(await call('activate',b),{changed:true});
  await call('load',b,{expected:EMAIL,now:NOW+200000});
  await call('load',b,{},'wrong',503);assert.equal((await row(b)).ciphertext,original.ciphertext);
  await call('load',b,{expected:EMAIL});
  console.log('PASS actual D1 encrypted staging/exact retry, active-only load, wrong-key retry and long queue retention');

  b=binding();await call('stage',b,{email:EMAIL,lose_ack:true},'a',503);const lost=await row(b);
  await call('stage',b,{email:EMAIL},'b');assert.deepEqual(await row(b),lost);
  await call('activate',b);await call('load',b,{expected:EMAIL},'b');
  b=binding();await call('stage',b,{email:EMAIL},'newer');await call('activate',b,{},'newer');
  const before=await row(b);await call('load',b,{},'a',503);assert.deepEqual(await row(b),before);
  await call('load',b,{expected:EMAIL},'newer');
  console.log('PASS actual D1 lost committed staging acknowledgement and lagging generation preserve immutable ciphertext');

  const concurrent=binding();
  const results=await Promise.all(Array.from({length:12},async(_,n)=>{
    const response=await fetchers[n%2?'a':'b'].fetch('http://fixture.test/__fixture/email-vault/stage',{method:'POST',headers:{'content-type':'application/json'},body:JSON.stringify({binding:concurrent,now:NOW,email:`PRIVATE_CONCURRENT_${n}@example.com`})});return response.status;
  }));
  assert.equal(results.filter(x=>x===200).length,1);assert.equal(results.filter(x=>x===409).length,11);
  console.log('PASS actual D1 concurrent staging across isolates commits one immutable address');

  // Each component independently changes while the same ciphertext is copied.
  const fields=['email_ref','batch_id','row_number','account_id','installation_id'];
  for(let n=0;n<fields.length;n++){
    const copied={...original};
    // Production SQL must refuse each independent binding mutation.
    const field=fields[n],value=field==='email_ref'?randomBytes(32).toString('hex'):field==='batch_id'?'01ARZ3NDEKTSV4RRFFQ69G5FAW':field==='row_number'?2:99;
    await assert.rejects(db.prepare(`UPDATE email_vault SET ${field}=? WHERE email_ref=?`).bind(value,original.email_ref).run());
    assert.equal((await db.prepare('SELECT ciphertext FROM email_vault WHERE email_ref=?').bind(original.email_ref).first()).ciphertext,copied.ciphertext);
  }
  const copied=binding();await db.prepare('INSERT INTO email_vault(email_ref,batch_id,row_number,account_id,installation_id,ciphertext,created_at,purge_after) VALUES(?,?,?,?,?,?,?,?)').bind(copied.email_ref,copied.batch.batch_id,copied.batch.row,copied.account_id,copied.installation_id,original.ciphertext,NOW,NOW+86400).run();
  await call('activate',copied);await call('load',copied,{},'a',503);
  console.log('PASS actual D1 immutable binding guards and AEAD copied-reference refusal');

  b=binding();await call('stage',b,{email:EMAIL});await call('activate',b);await call('retire',b,{now:NOW+10});await call('retire',b,{now:NOW+100});assert.equal((await row(b)).purge_after,NOW+86410);
  await call('load',b,{},'a',412);await call('prune',b,{now:NOW+86409});assert.ok((await row(b)).ciphertext);
  await call('prune',b,{now:NOW+86410});assert.equal((await row(b)).ciphertext,null);
  assert.deepEqual(await call('activate',b,{now:NOW+86411}),{changed:false});await call('stage',b,{email:EMAIL,now:NOW+86411},'a',410);
  await assert.rejects(db.prepare('DELETE FROM email_vault WHERE email_ref=?').bind(b.email_ref).run());
  const fenced=binding();fenced.batch.row=20;
  const command={batch:fenced.batch,account_id:42,installation_id:9,organization:{org_id:42,org_login:'acme',grant:'member'},teams:[],email_ref:fenced.email_ref,approved_by:7,approved_at:new Date(NOW*1000).toISOString()};
  await call('delivery/claim',fenced,{command},'a',503);
  await call('stage',fenced,{email:EMAIL});await call('activate',fenced);
  const claims=await Promise.all(Array.from({length:8},(_,n)=>call('delivery/claim',fenced,{command},n%2?'a':'b')));
  assert.equal(claims.filter(x=>x.generation===1).length,1);assert.equal(claims.filter(x=>x.generation===null).length,7);
  await call('delivery/reject',fenced,{command,generation:1});
  await call('delivery/claim',fenced,{command,lose_ack:true},'a',503);
  assert.deepEqual(await call('delivery/claim',fenced,{command},'b'),{generation:null});
  await call('delivery/reject',fenced,{command,generation:1});assert.deepEqual(await call('delivery/exists',fenced,{command}),{exists:true});
  for(const changed of [{...command,approved_by:8},{...command,email_ref:randomBytes(32).toString('hex')},{...command,installation_id:10}])await call('delivery/claim',fenced,{command:changed},'b',503);
  let snapshot={command,revision:1,outcome:{kind:'planned'},confirmed_at:null,settlement:null,settled_at:null};
  await call('delivery/project',fenced,{command,snapshot});const planned=structuredClone(snapshot);
  snapshot={...snapshot,revision:2,outcome:{kind:'outcome_unknown'}};await call('delivery/project',fenced,{command,snapshot});
  await call('delivery/project',fenced,{command,snapshot:{...snapshot,revision:3,outcome:{kind:'planned'}}},'b',503);
  snapshot={...snapshot,revision:3,outcome:{kind:'created',upstream_id:77,created_at:command.approved_at},confirmed_at:command.approved_at};
  await db.exec("CREATE TRIGGER email_audit_failure BEFORE INSERT ON audit_events BEGIN SELECT RAISE(ABORT,'private-email-audit-failure'); END");
  await call('delivery/project',fenced,{command,snapshot},'a',503);
  assert.equal((await call('delivery/read',fenced,{command})).snapshot.outcome.kind,'outcome_unknown');
  await db.exec('DROP TRIGGER email_audit_failure');
  await call('delivery/project',fenced,{command,snapshot});
  await call('delivery/project',fenced,{command,snapshot:planned});
  const receipt=await call('delivery/read',fenced,{command});assert.equal(receipt.snapshot.outcome.upstream_id,77);assert.equal(receipt.snapshot.revision,3);assert.equal(receipt.batch.length,1);
  const wrongAccount=await call('delivery/read',fenced,{command:{...command,account_id:99}});assert.equal(wrongAccount.batch.length,0);
  for(const statement of ["UPDATE email_delivery_attempts SET generation=0", "UPDATE email_delivery_attempts SET command='{}'", "DELETE FROM email_delivery_attempts", "UPDATE email_deliveries SET receipt='{}'"])await assert.rejects(db.prepare(statement).run());
  snapshot={...snapshot,revision:4,settlement:{kind:'no_longer_pending'},settled_at:new Date((NOW+1)*1000).toISOString()};
  await call('delivery/project',fenced,{command,snapshot});await call('delivery/project',fenced,{command,snapshot},'b');
  const audit=(await db.prepare('SELECT * FROM audit_events ORDER BY occurred_at').all()).results;
  assert.deepEqual(audit.map(e=>e.event_type),['org_invitation.sent','email_invitation.no_longer_pending']);
  assert.equal(JSON.parse(audit[0].metadata).source.row,20);
  assert.ok(!JSON.stringify(audit).includes(fenced.email_ref));
  assert.ok(!JSON.stringify(audit).includes('email_ref'));
  console.log('PASS actual D1 atomic email receipt/audit rollback, exact replay and neutral no-longer-pending event');
  console.log('PASS actual D1 email fence one concurrent winner, lost acknowledgement, stale generation, full binding, immutable receipts and scoped batch reads');
  const raw=JSON.stringify(await Promise.all(['email_vault','email_delivery_attempts','email_deliveries','audit_events'].map(table=>db.prepare(`SELECT * FROM ${table}`).all())));
  for(const marker of [EMAIL,'PRIVATE_CONCURRENT_','other@example.com'])assert.ok(!raw.includes(marker));
  console.log('PASS actual D1 fixed terminal horizon, ciphertext purge, permanent tombstone and raw row privacy');
}finally{await mf.dispose();}
assert.equal(overflow,false);for(const marker of [EMAIL,'PRIVATE_CONCURRENT_','other@example.com'])assert.ok(!logs.includes(marker));
console.log('PASS complete bounded Worker logs contain no email markers; no outbound sends');

registerFinalLogScan(bytes => { for(const marker of [EMAIL,'PRIVATE_CONCURRENT_','other@example.com'])assert.ok(!bytes.includes(Buffer.from(marker))); });
