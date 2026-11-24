import { registerFinalLogScan } from './runtime-output.mjs';
import assert from 'node:assert/strict';
import { hkdfSync, randomBytes } from 'node:crypto';
import { readFileSync, readdirSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { Miniflare, Log, LogLevel } from 'miniflare';
const NOW=Math.floor(Date.now()/1000), MAIL='REVIEW_ESCROW_PRIVATE_MARKER+tag@example.com';
// kekring v1 descriptor (libs/kekring/docs/kekring-format.md).
const ring=(current,keys)=>`kekring1;name=p1-email;current=${current};hwm=${Math.max(...keys.map(([n])=>n))};${keys.map(([n,byte])=>{
 const key=Buffer.alloc(32,byte),version=Buffer.alloc(4);version.writeUInt32BE(n);
 const name=Buffer.from('p1-email');const info=Buffer.concat([Buffer.from('kekring/v1/kid'),Buffer.from([name.length]),name,version]);
 const kid=Buffer.from(hkdfSync('sha256',key,Buffer.from('kekring/v1/salt'),info,8));
 return `k${n}=${key.toString('base64url')}.${kid.toString('hex')}`;
}).join(';')}`;
let logs='',overflow=false,outbound=0;
// Hourly cleanup only: daily-digest.mjs covers the scheduled sweep watchdog,
// which needs Restate ingress this proof does not model.
const worker=(name,keyring)=>({name,modules:true,scriptPath:fileURLToPath(new URL('./worker.mjs',import.meta.url)),modulesRules:[{type:'CompiledWasm',include:['**/*.wasm']}],compatibilityDate:'2024-09-23',compatibilityFlags:['nodejs_compat'],d1Databases:{DB:'email-review-shared'},bindings:{P1_EMAIL_KEYRING:keyring,GH_SWEEP_WATCHDOG_ENABLED:'false'},outboundService(){outbound++;throw new Error('review proof has no network effects');}});
const mf=new Miniflare({log:new Log(LogLevel.ERROR),workers:[worker('a',ring(1,[[1,7]])),worker('b',ring(1,[[1,7]])),worker('newer',ring(2,[[1,7],[2,8]])),worker('wrong',ring(1,[[1,9]]))],handleRuntimeStdio(stdout,stderr){for(const stream of [stdout,stderr])stream.on('data',chunk=>{if(Buffer.byteLength(logs)+chunk.length>2*1024*1024)overflow=true;else logs+=chunk.toString();});}});
let number=0;
const binding=()=>({email_ref:randomBytes(32).toString('hex'),batch:{batch_id:'01ARZ3NDEKTSV4RRFFQ69G5FAV',row:++number},account_id:42,installation_id:9});
const command=b=>({batch:b.batch,account_id:b.account_id,installation_id:b.installation_id,email_ref:b.email_ref,organization:{org_id:b.account_id,org_login:'acme',grant:'member'},teams:[],approved_by:43,approved_at:new Date(NOW*1000).toISOString()});
try {
 const db=await mf.getD1Database('DB','a');
 for(const file of readdirSync(new URL('../../migrations/',import.meta.url)).filter(f=>f.endsWith('.sql')).sort())await db.exec(readFileSync(new URL('../../migrations/'+file,import.meta.url),'utf8').replace(/^[ \t]*--.*$/gm,'').replaceAll('\n',' '));
 const fetchers=Object.fromEntries(await Promise.all(['a','b','newer','wrong'].map(async name=>[name,await mf.getWorker(name)])));
 const call=async(op,input={},name='a',status=200)=>{
  const response=await fetchers[name].fetch('http://fixture.test/__fixture/email-review/'+op,{method:'POST',headers:{'content-type':'application/json'},body:JSON.stringify({now:NOW,...input})});
  assert.equal(response.status,status,await response.clone().text());return response.json();
 };
 const row=(table,b)=>db.prepare(`SELECT * FROM ${table} WHERE email_ref=?`).bind(b.email_ref).first();
 const a=binding(),removed=binding();await call('retain',{records:[{binding:a,email:MAIL},{binding:removed,email:'REMOVED_ESCROW_MARKER@example.com'}]});const original=await row('email_review',a);
 assert.equal(await row('email_vault',a),null);await call('stage',{commands:[command(a)]},'b');const delivered=await row('email_vault',a);assert.ok(delivered.ciphertext);assert.equal(await row('email_vault',removed),null);
 await call('stage',{commands:[command(a)],now:NOW+1},'b');assert.deepEqual(await row('email_vault',a),delivered);await call('retain',{records:[{binding:a,email:MAIL}],now:NOW+1});assert.deepEqual(await row('email_review',a),original);
 console.log('PASS actual D1 cross-isolate selected-only staging, exact ciphertext retry and immutable one-hour review expiry');
 const good=binding(),expired=binding();await call('retain',{records:[{binding:good,email:MAIL}]});await call('retain',{records:[{binding:expired,email:MAIL}],now:NOW-3600});await call('stage',{commands:[command(good),command(expired)]},'b',410);assert.equal(await row('email_vault',good),null);
 for(const altered of [{...command(good),batch:{...good.batch,row:199}},{...command(good),installation_id:10},{...command(good),account_id:99,organization:{org_id:99,org_login:'acme',grant:'member'}}]){await call('stage',{commands:[command(good),altered]},'b',409);assert.equal(await row('email_vault',good),null);}
 const rollout=binding();await call('retain',{records:[{binding:rollout,email:MAIL}]},'newer');const sealed=await row('email_review',rollout);await call('stage',{commands:[command(rollout)]},'a',503);assert.deepEqual(await row('email_review',rollout),sealed);await call('stage',{commands:[command(rollout)]},'newer');
 await call('stage',{commands:[command(good)]},'wrong',503);assert.equal(await row('email_vault',good),null);
 console.log('PASS actual D1 all-selected-before-first-stage, expiry, context mismatch and lagging/wrong-key preservation');
 const lost=binding();await call('retain',{records:[{binding:lost,email:MAIL}],lose_ack:true},'a',503);const lostRecord=await row('email_review',lost);await call('retain',{records:[{binding:lost,email:MAIL}],now:NOW+2},'b');assert.deepEqual(await row('email_review',lost),lostRecord);
 const race=binding();const results=await Promise.all(Array.from({length:8},async(_,n)=>{const r=await fetchers[n%2?'a':'b'].fetch('http://fixture.test/__fixture/email-review/retain',{method:'POST',headers:{'content-type':'application/json'},body:JSON.stringify({now:NOW,records:[{binding:race,email:`ESCROW_RACE_MARKER_${n}@example.com`}]})});return r.status;}));assert.equal(results.filter(s=>s===200).length,1);assert.equal(results.filter(s=>s===409).length,7);
 console.log('PASS actual D1 committed ACK loss and cross-isolate races keep one immutable review address');
 const copied=binding();await db.prepare('INSERT INTO email_review(email_ref,batch_id,row_number,account_id,installation_id,ciphertext,created_at,expires_at) VALUES(?,?,?,?,?,?,?,?)').bind(copied.email_ref,copied.batch.batch_id,copied.batch.row,42,9,original.ciphertext,NOW,NOW+3600).run();await call('stage',{commands:[command(copied)]},'a',503);
 const crossPurpose=binding();const stagedResponse=await fetchers.a.fetch('http://fixture.test/__fixture/email-vault/stage',{method:'POST',headers:{'content-type':'application/json'},body:JSON.stringify({binding:crossPurpose,email:MAIL,now:NOW})});assert.equal(stagedResponse.status,200);const purposeRow=await row('email_vault',crossPurpose);await db.prepare('INSERT INTO email_review(email_ref,batch_id,row_number,account_id,installation_id,ciphertext,created_at,expires_at) VALUES(?,?,?,?,?,?,?,?)').bind(crossPurpose.email_ref,crossPurpose.batch.batch_id,crossPurpose.batch.row,42,9,purposeRow.ciphertext,NOW,NOW+3600).run();await call('stage',{commands:[command(crossPurpose)]},'a',503);assert.deepEqual(await row('email_vault',crossPurpose),purposeRow);
 for(const [field,value] of [['email_ref',randomBytes(32).toString('hex')],['batch_id','01ARZ3NDEKTSV4RRFFQ69G5FAW'],['row_number',200],['account_id',99],['installation_id',99],['expires_at',NOW+7200]])await assert.rejects(db.prepare(`UPDATE email_review SET ${field}=? WHERE email_ref=?`).bind(value,a.email_ref).run());
 await call('retire',{bindings:[removed,{...a,installation_id:10}]},'a',409);assert.ok((await row('email_review',removed)).ciphertext);
 await call('retire',{bindings:[removed]});const tombstone=await row('email_review',removed);await call('retire',{bindings:[removed],now:NOW+1});assert.deepEqual(await row('email_review',removed),tombstone);assert.equal(tombstone.ciphertext,null);await call('retain',{records:[{binding:removed,email:MAIL}]},'a',410);await assert.rejects(db.prepare('DELETE FROM email_review WHERE email_ref=?').bind(removed.email_ref).run());
 assert.deepEqual(await row('email_vault',a),delivered);
 console.log('PASS actual D1 purpose/context isolation, immutable SQL guards, exact removed-row retirement and permanent tombstones');
 const prior=(await db.prepare('SELECT * FROM email_review').all()).results;
 await call('retire',{bindings:prior.map(r=>({email_ref:r.email_ref,batch:{batch_id:r.batch_id,row:r.row_number},account_id:r.account_id,installation_id:r.installation_id})),now:NOW+2});
 const cleanupRows=Array.from({length:101},(_,i)=>({binding:{email_ref:randomBytes(32).toString('hex'),batch:{batch_id:'01ARZ3NDEKTSV4RRFFQ69G5FAX',row:i+1},account_id:42,installation_id:9},email:MAIL}));await call('retain',{records:cleanupRows,now:NOW-3601});
 const remaining=async()=>Number((await db.prepare('SELECT COUNT(*) AS n FROM email_review WHERE ciphertext IS NOT NULL').first()).n);
 const scheduled=()=>fetchers.a.scheduled({cron:'5 * * * *',scheduledTime:new Date('2001-01-01T00:05:00Z')});
 await db.exec("CREATE TRIGGER review_cleanup_fault BEFORE UPDATE ON email_review BEGIN SELECT RAISE(ABORT,'REVIEW_SQL_PRIVATE_MARKER'); END");assert.equal((await scheduled()).outcome,'ok');assert.equal(await remaining(),101);assert.equal((await db.prepare('SELECT COUNT(*) AS n FROM blocked_digest_days').first()).n,1);await db.exec('DROP TRIGGER review_cleanup_fault');
 assert.equal((await scheduled()).outcome,'ok');assert.equal(await remaining(),1);assert.equal((await scheduled()).outcome,'ok');assert.equal(await remaining(),0);
 for(const r of (await db.prepare('SELECT * FROM email_review WHERE batch_id=?').bind('01ARZ3NDEKTSV4RRFFQ69G5FAX').all()).results)assert.ok(r.cleared_at>=NOW);
 assert.deepEqual(await row('email_vault',a),delivered);
 const tables=(await db.prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' AND name NOT LIKE '_cf_%'").all()).results;
 let raw='';for(const {name} of tables){assert.match(name,/^[A-Za-z0-9_]+$/);raw+=JSON.stringify((await db.prepare(`SELECT * FROM ${name}`).all()).results);assert.ok(raw.length<4*1024*1024);}
 for(const marker of [MAIL,'REMOVED_ESCROW_MARKER','ESCROW_RACE_MARKER_'])assert.ok(!raw.includes(marker));
 console.log(`PASS actual scheduled Worker review cleanup100+1 with independent failure/recovery, processing clock, untouched delivery vault and full ${tables.length}-table privacy scan`);
} finally {await mf.dispose();}
assert.equal(outbound,0);assert.equal(overflow,false);for(const marker of [MAIL,'REMOVED_ESCROW_MARKER','ESCROW_RACE_MARKER_','REVIEW_SQL_PRIVATE_MARKER'])assert.ok(!logs.includes(marker));
console.log('PASS complete bounded Worker logs contain no review mailbox/SQL markers; zero outbound calls');

registerFinalLogScan(bytes => { for(const marker of [MAIL,'REMOVED_ESCROW_MARKER','ESCROW_RACE_MARKER_','REVIEW_SQL_PRIVATE_MARKER'])assert.ok(!bytes.includes(Buffer.from(marker))); });
