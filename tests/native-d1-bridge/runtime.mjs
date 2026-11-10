import assert from 'node:assert/strict';
import {createServer} from 'node:http';
import {spawn} from 'node:child_process';
import {readFileSync,readdirSync,mkdirSync,writeFileSync} from 'node:fs';
import {randomBytes,randomUUID,createHash} from 'node:crypto';
import {createRequire} from 'node:module';
import {fileURLToPath} from 'node:url';
import {RuntimeOutput} from './runtime-output.mjs';
import {schemaQuery,sha256} from '../../native-d1-bridge/bridge.mjs';
const require=createRequire(new URL('../worker/package.json',import.meta.url));
const {Miniflare,Log,LogLevel}=require('miniflare');
const evidence=process.env.EVIDENCE_ROOT;assert.ok(evidence);mkdirSync(evidence,{recursive:true});
const binary=process.argv[2];assert.ok(binary,'Pass built native_bridge test executable');
const bridge=readFileSync(new URL('../../native-d1-bridge/bridge.mjs',import.meta.url),'utf8').replaceAll('export ','');
const migrations=readdirSync(new URL('../../migrations/',import.meta.url)).filter(n=>n.endsWith('.sql')).sort().map(name=>({name,sql:readFileSync(new URL('../../migrations/'+name,import.meta.url),'utf8')}));
let mf,output,config,drop=false,workerCalls=0;const reports=[];const tokens=[];
async function dispose() {
 if(!mf)return;
 const instance=mf;mf=null;
 let raw={},tables=[];
 try {
  const db=await instance.getD1Database('DB');tables=(await db.prepare("SELECT name FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%' AND name NOT LIKE '_cf_%' ORDER BY name").all()).results;
  for(const {name} of tables)raw[name]=(await db.prepare('SELECT * FROM "'+name.replaceAll('"','""')+'"').all()).results;
 } finally { await output.disposeAndDrain(instance); }
 const texts=output.texts();
 const markers=[...(config?[config.token,'Bearer '+config.token]:[]),'NATIVE_BRIDGE_TOKEN_MARKER','ghu_PLAINTEXT','ghr_PLAINTEXT'];
 for(const value of [JSON.stringify(raw),...texts])for(const marker of markers)assert.ok(!value.includes(marker));
 for(const text of texts)assert.ok(!/Workers runtime canceled.*hung|RT0007/.test(text));
 const index=reports.length;writeFileSync(`${evidence}/${index}-rows.json`,JSON.stringify(raw));
 texts.forEach((t,i)=>writeFileSync(`${evidence}/${index}-stream-${i}.log`,t));
 reports.push({index,tables:tables.length,streams:texts.length,worker_calls:workerCalls,complete_eof:true});
}
async function fresh(base) {
 await dispose();workerCalls=0;output=new RuntimeOutput({limit:32*1024*1024});output.install();
 const token=randomBytes(32).toString('hex');tokens.push(token);const id=randomUUID();
 const script=bridge+`\nlet authority; export default {async fetch(request,env){
  if(new URL(request.url).pathname==='/__configure'){authority=await request.json();return new Response('ok')}
  if(new URL(request.url).pathname==='/__retained') {
   const {batch}=await request.json();const session=env.DB.withSession('first-primary');
   return Response.json(await session.batch(batch.map(q=>session.prepare(q.sql).bind(...q.params))));
  }
  return createBridge(authority).fetch(request,env);
 }};`;
 const options={modules:true,script,compatibilityDate:'2026-10-01',d1Databases:{DB:id},bindings:{D1_BRIDGE_TOKEN:token,AUTHORITY:'{}'},log:new Log(LogLevel.NONE),handleRuntimeStdio:(o,e)=>output.capture(o,e)};mf=new Miniflare(options);
 const db=await mf.getD1Database('DB');
 for(const m of migrations)await db.exec(m.sql.replace(/^[ \t]*--.*$/gm,'').replaceAll('\n',' '));
 // Fixture provisioning only. The real bridge never creates/adopts this ledger.
 await db.exec('CREATE TABLE IF NOT EXISTS d1_migrations(id INTEGER PRIMARY KEY AUTOINCREMENT,name TEXT NOT NULL UNIQUE,applied_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP)');
 for(const m of migrations)await db.prepare('INSERT INTO d1_migrations(name) VALUES(?)').bind(m.name).run();
 const manifest={protocol:1,database_id:id,migrations:migrations.map(m=>({name:m.name,sha256:createHash('sha256').update(m.sql).digest('hex')})),schema_sha256:await sha256(JSON.stringify((await db.prepare(schemaQuery).all()).results))};
 await mf.dispatchFetch('http://fixture/__configure',{method:'POST',body:JSON.stringify(manifest)});
 config={url:base+'/v1/d1',token,manifest};return config;
}
const server=createServer(async(req,res)=>{
 try {
  const bytes=[];let n=0;for await(const c of req){n+=c.length;assert.ok(n<=32*1024*1024);bytes.push(c);}const body=Buffer.concat(bytes);
  if(req.url==='/control/new'){res.setHeader('content-type','application/json');res.end(JSON.stringify(await fresh(`http://127.0.0.1:${server.address().port}`)));return;}
  if(req.url==='/control/drop'){drop=true;res.end('{}');return;}
  if(req.url==='/control/worker'){workerCalls++;const r=await mf.dispatchFetch('http://worker.test/__retained',{method:'POST',body});res.statusCode=r.status;res.end(await r.text());return;}
  assert.equal(req.url,'/v1/d1');
  const r=await mf.dispatchFetch('http://bridge.test/v1/d1',{method:'POST',headers:req.headers,body});const text=await r.text();
  if(drop){drop=false;res.destroy();return;} // actual committed response consumed, TCP ACK lost
  res.statusCode=r.status;res.setHeader('content-type','application/json');res.end(text);
 }catch{res.statusCode=500;res.end('fixture unavailable');}
});
await new Promise(resolve=>server.listen(0,'127.0.0.1',resolve));
let status;
try {
 const child=spawn(binary,['--nocapture','--test-threads=1'],{env:{...process.env,BRIDGE_FIXTURE:`http://127.0.0.1:${server.address().port}`},stdio:['ignore','pipe','pipe']});
 const streams=[[],[]];let n=0;for(const [i,s]of [child.stdout,child.stderr].entries())s.on('data',c=>{n+=c.length;if(n>32*1024*1024)child.kill('SIGKILL');streams[i].push(c);});
 status=await new Promise((resolve,reject)=>{child.once('error',reject);child.once('close',resolve);});
 assert.ok(n<=32*1024*1024,'native output exceeded bound');
 for(const [i,s]of streams.entries()){const bytes=Buffer.concat(s);writeFileSync(`${evidence}/native-${i}.log`,bytes);for(const token of tokens)assert.ok(!bytes.toString('utf8').includes(token),'native credential disclosure');}
 assert.equal(status,0,'native bridge target failed; inspect retained bounded logs');
}finally {try {await dispose();} finally {await new Promise(resolve=>server.close(resolve));}}
writeFileSync(`${evidence}/results.json`,JSON.stringify({passed:true,scope:'Actual native D1Storage over TCP bridge and retained local Worker against one D1 binding. No deployed switch or Restate registration claim.',reports},null,2));
