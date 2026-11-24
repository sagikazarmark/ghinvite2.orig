import { registerFinalLogScan } from './runtime-output.mjs';
import assert from 'node:assert/strict';
import { readFileSync, readdirSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { Miniflare, Log, LogLevel } from 'miniflare';
let logs='',overflow=false,outbound=0;
const mf=new Miniflare({
  log:new Log(LogLevel.ERROR),modules:true,
  scriptPath:fileURLToPath(new URL('./worker.mjs',import.meta.url)),
  modulesRules:[{type:'CompiledWasm',include:['**/*.wasm']}],
  compatibilityDate:'2024-09-23',compatibilityFlags:['nodejs_compat'],d1Databases:{DB:'email-maintenance'},
  // Projection-only maintenance; daily-digest.mjs covers the sweep watchdog.
  bindings:{GH_SWEEP_WATCHDOG_ENABLED:'false'},
  outboundService(){outbound++;throw new Error('maintenance has no network effects');},
  handleRuntimeStdio(stdout,stderr){for(const stream of [stdout,stderr])stream.on('data',chunk=>{if(Buffer.byteLength(logs)+chunk.length>1024*1024)overflow=true;else logs+=chunk.toString();});},
});
try {
  const db=await mf.getD1Database('DB');
  for(const file of readdirSync(new URL('../../migrations/',import.meta.url)).filter(f=>f.endsWith('.sql')).sort()) {
    await db.exec(readFileSync(new URL('../../migrations/'+file,import.meta.url),'utf8').replace(/^[ \t]*--.*$/gm,'').replaceAll('\n',' '));
  }
  const web=await mf.getWorker();
  const now=Math.floor(Date.now()/1000);
  const ref=n=>n.toString(16).padStart(64,'0');
  const seed=async(n,created,activated=null,retired=null)=>{
    const expiry=retired!==null?retired+86400:activated!==null?null:created+86400;
    await db.prepare('INSERT INTO email_vault(email_ref,batch_id,row_number,account_id,installation_id,ciphertext,created_at,activated_at,retired_at,purge_after) VALUES(?,?,?,?,?,?,?,?,?,?)')
      .bind(ref(n),'01ARZ3NDEKTSV4RRFFQ69G5FAV',1,42,9,'b3BhcXVl',created,activated,retired,expiry).run();
  };
  for(let n=1;n<=101;n++)await seed(n,now-100000);
  await seed(102,now-100000,now-99999); // active queue, no terminal deadline
  await seed(103,now); // staged retry horizon remains open
  await seed(104,now-100000,now-99999,now-1); // terminal retry horizon remains open
  const row=n=>db.prepare('SELECT * FROM email_vault WHERE email_ref=?').bind(ref(n)).first();
  const retained=await Promise.all([102,103,104].map(row));
  const scheduled=()=>web.scheduled({cron:'5 * * * *',scheduledTime:new Date('2001-01-01T00:05:00Z')});
  // Failure of cleanup must not lose the independently committed daily report.
  await db.exec("CREATE TRIGGER cleanup_failure BEFORE UPDATE ON email_vault BEGIN SELECT RAISE(ABORT,'private-mail-maintenance-marker'); END");
  assert.equal((await scheduled()).outcome,'ok');
  assert.equal((await db.prepare('SELECT COUNT(*) AS n FROM blocked_digest_days').first()).n,1);
  assert.equal((await db.prepare('SELECT COUNT(*) AS n FROM email_vault WHERE ciphertext IS NULL').first()).n,0);
  await db.exec('DROP TRIGGER cleanup_failure');
  assert.equal((await scheduled()).outcome,'ok');
  assert.equal((await db.prepare('SELECT COUNT(*) AS n FROM email_vault WHERE ciphertext IS NULL').first()).n,100);
  assert.equal((await scheduled()).outcome,'ok');
  assert.equal((await db.prepare('SELECT COUNT(*) AS n FROM email_vault WHERE ciphertext IS NULL').first()).n,101);
  assert.deepEqual(await Promise.all([102,103,104].map(row)),retained);
  for(let n=1;n<=101;n++)assert.ok((await row(n)).purged_at>=now,'uses actual processing clock, not scheduled event date');
  // Conversely, a digest outage does not postpone eligible protected-data cleanup.
  await seed(105,now-100000);
  await db.exec('DELETE FROM blocked_digest_items; DELETE FROM blocked_digest_days;');
  await db.exec("CREATE TRIGGER digest_failure BEFORE INSERT ON blocked_digest_days BEGIN SELECT RAISE(ABORT,'private-digest-maintenance-marker'); END");
  assert.notEqual((await scheduled()).outcome,'ok');
  assert.equal((await row(105)).ciphertext,null);
  await db.exec('DROP TRIGGER digest_failure');
  assert.equal((await scheduled()).outcome,'ok');
  assert.equal((await db.prepare('SELECT COUNT(*) AS n FROM blocked_digest_days').first()).n,1);
  assert.equal(outbound,0);assert.equal(overflow,false);
  assert.ok(logs.includes('email_recovery_cleanup_failed'));
  assert.ok(logs.includes('daily_delivery_digest_failed'));
  assert.ok(!logs.includes('private-mail-maintenance-marker'));
  assert.ok(!logs.includes('private-digest-maintenance-marker'));
  console.log('PASS actual scheduled Worker/D1 email cleanup: bounded100+1, active and retry horizons, processing clock, independent failures, tombstones, zero network and bounded safe logs');
} finally {await mf.dispose();}

registerFinalLogScan(bytes => { for(const marker of ['private-mail-maintenance-marker','private-digest-maintenance-marker'])assert.ok(!bytes.includes(Buffer.from(marker))); });
