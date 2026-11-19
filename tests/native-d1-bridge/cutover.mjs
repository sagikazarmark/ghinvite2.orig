// Owned-local signed Restate 1.8 Worker→native cutover over one existing D1 binding.
// Provisioning is fixture-only, before either workflow producer is registered.
// GitHub is the octoemu emulator binary (OCTOEMU_BIN), reached by the Worker and
// the native endpoint through GITHUB_API_BASE (a loopback proxy in front of it
// that collects the minted installation tokens as private markers).
import assert from 'node:assert/strict';
import {createServer as httpServer} from 'node:http';
import {createServer as h2Server,connect as h2connect} from 'node:http2';
import {createServer as tcpServer} from 'node:net';
import {spawn} from 'node:child_process';
import {once} from 'node:events';
import {readFileSync,readdirSync,writeFileSync,mkdirSync} from 'node:fs';
import {randomBytes,randomUUID,createHash} from 'node:crypto';
import {createRequire} from 'node:module';
import {fileURLToPath} from 'node:url';
import {RuntimeOutput} from './runtime-output.mjs';
import {identitySigner} from '../worker/restate-identity.mjs';
import {setupCommand} from '../worker/installation-fixture.mjs';
import {capabilityFor,admitCapability} from '../worker/capability.mjs';
import {frames,fields} from '../worker/protocol.mjs';
import {completeChild,scanPrivate} from './cutover-controls.mjs';
import {schemaQuery,sha256} from '../../native-d1-bridge/bridge.mjs';
const require=createRequire(new URL('../worker/package.json',import.meta.url));
const {Miniflare,Log,LogLevel,supportedCompatibilityDate}=require('miniflare');
// The deployed bridge's date (native-d1-bridge/manifest.py), capped at the newest
// date this workerd supports (4.20260302 falls back with a warning; later releases refuse).
const bridgeCompatibilityDate=['2026-10-01',supportedCompatibilityDate].sort()[0];
const evidence=process.env.EVIDENCE_ROOT;assert.ok(evidence);mkdirSync(evidence,{recursive:true});
for(const key of ['RESTATE_SERVER_BIN','NATIVE_BRIDGE_ENDPOINT_BIN','OCTOEMU_BIN'])assert.ok(process.env[key],key);
const hash=p=>createHash('sha256').update(readFileSync(p)).digest('hex');
const signer=identitySigner(), token=randomBytes(32).toString('hex'), privateKeys='fixture:'+readFileSync(new URL('../../test-fixtures/github-app-rsa.pem',import.meta.url)).toString('base64');
const privatePem=readFileSync(new URL('../../test-fixtures/github-app-rsa.pem',import.meta.url),'utf8');
const markers=new Set([token,privateKeys,privatePem,Buffer.from(privatePem).toString('base64')]);const childReceipts=[];
const children=[],servers=[],sessions=new Set(),traffic=[];let mf,output,db,blocked,held=false,puts=0,nativeUrl,oldWorker,bridgeWorker,unexpectedOutbound=0;
const streams=new Map();
const sleep=ms=>new Promise(r=>setTimeout(r,ms));
async function until(f,p,label,ms=45000){const end=Date.now()+ms;let last;while(Date.now()<end){try{last=await f();if(p(last))return last;}catch(error){last={error:String(error)}}await sleep(50);}throw Error(label+': '+JSON.stringify(last));}
async function http(url,body,method='POST'){const r=await fetch(url,{method,headers:{accept:'application/json',...(body===undefined?{}:{'content-type':'application/json'})},body:body===undefined?undefined:JSON.stringify(body),signal:AbortSignal.timeout(45000)});const text=await r.text();assert.ok(r.ok,`${method} ${new URL(url).pathname} ${r.status}: ${text.slice(0,300)}`);return text?JSON.parse(text):null;}
async function port(){const s=tcpServer();await new Promise(r=>s.listen(0,'127.0.0.1',r));const n=s.address().port;await new Promise(r=>s.close(r));return n;}
function child(name,bin,args,env){const c=spawn(bin,args,{env:{...process.env,...env},stdio:['ignore','pipe','pipe']});const done=new Promise((resolve,reject)=>{c.once('error',reject);c.once('close',(code,signal)=>resolve({code,signal}));});const logs=[[],[]];let n=0;[c.stdout,c.stderr].forEach((s,i)=>s.on('data',b=>{n+=b.length;if(n>32*1024*1024)c.kill('SIGKILL');else logs[i].push(b);}));const owned={name,c,done,logs,bytes:()=>n};children.push(owned);return owned;}
async function listen(s){servers.push(s);await new Promise(r=>s.listen(0,'127.0.0.1',r));return `http://127.0.0.1:${s.address().port}`;}
async function nativeFetch(path,method,headers,body){const client=h2connect(nativeUrl);client.on('error',()=>{});try{const r=client.request({':path':path,':method':method,...headers},{endStream:false});const response=once(r,'response');const chunks=[];r.on('data',b=>chunks.push(b));const done=once(r,'end');r.end(body);const [[h]]=await Promise.all([response,done]);return {status:h[':status'],headers:Object.fromEntries(Object.entries(h).filter(([k])=>!k.startsWith(':'))),body:Buffer.concat(chunks)};}finally{client.destroy();}}
function proxy(kind){const s=h2Server(async(req,res)=>{try{
 if(kind==='native'){
  // Native Restate uses bidirectional streaming. Waiting for request EOF before
  // forwarding deadlocks the protocol; observe only its bounded Start frame.
  const client=h2connect(nativeUrl);sessions.add(client);client.once('close',()=>sessions.delete(client));
  const headers={...Object.fromEntries(Object.entries(req.headers).filter(([k])=>!k.startsWith(':')&&!['host','connection','transfer-encoding','content-length'].includes(k))),...signer.headers(req.url)};
  const upstream=client.request({':path':req.url,':method':req.method,...headers},{endStream:false});
  const event={kind,path:req.url,invocation:null,at:Date.now()};traffic.push(event);let prefix=Buffer.alloc(0),observed=!req.url.startsWith('/invoke/');
  req.on('data',chunk=>{if(observed)return;prefix=Buffer.concat([prefix,chunk]);if(prefix.length>1024*1024){upstream.destroy(Error('bounded Start frame exceeded'));return;}if(prefix.length>=8&&prefix.length>=8+prefix.readUInt32BE(4)){const start=frames(prefix.subarray(0,8+prefix.readUInt32BE(4)))[0];assert.equal(start.type,0);event.invocation=fields(start.payload).get(2)?.toString();observed=true;prefix=Buffer.alloc(0);}});
  const failed=error=>{streams.set('proxy-error-'+streams.size,String(error));if(!res.headersSent)res.writeHead(503);res.end();client.destroy();};
  client.on('error',failed);upstream.on('error',failed);req.on('error',failed);
  upstream.once('response',h=>res.writeHead(h[':status'],Object.fromEntries(Object.entries(h).filter(([k])=>!k.startsWith(':')&&!['connection','transfer-encoding','keep-alive'].includes(k)))));
  upstream.once('end',()=>client.close());res.once('close',()=>client.close());
  req.pipe(upstream);upstream.pipe(res);return;
 }
const chunks=[];let n=0;for await(const b of req){n+=b.length;assert.ok(n<=32*1024*1024);chunks.push(b);}const body=Buffer.concat(chunks);const incoming=req.url.startsWith('/invoke/')?frames(body):[];const invocation=incoming[0]?.type===0?fields(incoming[0].payload).get(2)?.toString():null;traffic.push({kind,path:req.url,invocation,at:Date.now()});if(kind==='worker'&&invocation&&invocation===blocked){res.writeHead(503);res.end();return;}
 const headers={...Object.fromEntries(Object.entries(req.headers).filter(([k])=>!k.startsWith(':')&&!['host','connection','transfer-encoding','content-length'].includes(k))),...signer.headers(req.url)};
 let result;if(kind==='worker'){const r=await oldWorker.fetch('http://workflow'+req.url,{method:req.method,headers,body:body.length?body:undefined});result={status:r.status,headers:Object.fromEntries(r.headers),body:Buffer.from(await r.arrayBuffer())};}else result=await nativeFetch(req.url,req.method,headers,body);
 if(kind==='worker'&&!held&&puts===1&&req.url.endsWith('/RepositoryDelivery/create')){assert.ok(invocation);held=true;blocked=invocation;res.writeHead(503);res.end();return;}
 res.writeHead(result.status,Object.fromEntries(Object.entries(result.headers).filter(([k])=>!['transfer-encoding','connection','keep-alive'].includes(k))));res.end(result.body);
 }catch(error){if(!res.destroyed){res.writeHead(503);res.end();}streams.set('proxy-error-'+streams.size,String(error));}});s.on('session',x=>{sessions.add(x);x.once('close',()=>sessions.delete(x));});return s;}
const id=()=> '01'+randomBytes(12).toString('hex').toUpperCase();
// GitHub's side: acme (100, owned by user-7) with acme/api (10) and acme/web (11),
// the requesters user-91 and user-92, and the repository App 123 (the Worker's and
// the native endpoint's fixture key) installed on all of acme as 1.
const githubSeed={users:[{id:7,login:'user-7'},{id:91,login:'user-91'},{id:92,login:'user-92'}],
 orgs:[{id:100,login:'acme',created_at:'2020-01-01T00:00:00Z'}],memberships:[{org:'acme',user:'user-7',role:'admin',state:'active'}],
 repos:[{id:10,owner:'acme',name:'api',private:true},{id:11,owner:'acme',name:'web',private:true}],
 apps:[{id:123,slug:'p1-repo',client_id:'Iv.fixture-repo',client_secret:'fixture-repo-secret',public_keys:[readFileSync(new URL('../../libs/octoauth/crates/octoemu/tests/fixtures/rsa2048.pub.pem',import.meta.url),'utf8')],permissions:{administration:'write',metadata:'read'}}],
 installations:[{id:1,app_id:123,account:'acme',repository_selection:'all',created_at:'2026-01-01T00:00:00Z'}]};
// The provider writes as the old stub's ledger recorded them, from the emulator's call
// log: method, path without the query, the requested permission and the status.
async function providerCalls(admin){const {calls}=await http(admin+'/calls',undefined,'GET');const requests=calls.map(c=>({method:c.method,path:c.path.split('?')[0],permission:c.body_fields?.permission??null,status:c.status}));return {count:requests.length,requests};}
let raw,report,primary;
try{
 const seedPath=evidence+'/github-seed.json';writeFileSync(seedPath,JSON.stringify(githubSeed));
 const gh=child('github',process.env.OCTOEMU_BIN,['--port','0','--seed',seedPath],{});
 const started=await until(()=>Buffer.concat(gh.logs[0]).toString().split('\n')[0],line=>line.endsWith('}'),'GitHub emulator readiness').then(JSON.parse);
 const upstreamGithub=started.api_base,githubAdmin=started.admin_base;
 const githubUrl=await listen(httpServer(async(req,res)=>{try{const chunks=[];for await(const b of req)chunks.push(b);const r=await fetch(upstreamGithub+req.url,{method:req.method,headers:Object.entries(req.headers).filter(([n])=>n!=='content-length'),body:['GET','HEAD'].includes(req.method)?undefined:Buffer.concat(chunks),signal:AbortSignal.timeout(10000)});const body=Buffer.from(await r.arrayBuffer());if(/\/app\/installations\/\d+\/access_tokens$/.test(req.url)&&r.ok){const minted=JSON.parse(body);assert.ok(typeof minted.token==='string'&&minted.token.length>0);markers.add(minted.token);}res.writeHead(r.status,Object.fromEntries([...r.headers].filter(([k])=>!['transfer-encoding','connection','content-length'].includes(k))));res.end(body);}catch{res.writeHead(503);res.end();}}));
 const [nodePort,adminPort,ingressPort,metricsPort]=await Promise.all([port(),port(),port(),port()]);
 const data=evidence+'/restate';mkdirSync(data);
 const rs=child('restate',process.env.RESTATE_SERVER_BIN,[],{RESTATE_LISTEN_MODE:'tcp',RESTATE_BASE_DIR:data,RESTATE_NODE_NAME:'native-d1-cutover',RESTATE_AUTO_PROVISION:'true',RESTATE_LOG_FILTER:'warn',RESTATE_BIND_ADDRESS:`127.0.0.1:${nodePort}`,RESTATE_ADVERTISED_ADDRESS:`http://127.0.0.1:${nodePort}`,RESTATE_ADMIN__BIND_ADDRESS:`127.0.0.1:${adminPort}`,RESTATE_INGRESS__BIND_ADDRESS:`127.0.0.1:${ingressPort}`,RESTATE_METRICS__BIND_ADDRESS:`127.0.0.1:${metricsPort}`});
 const admin=`http://127.0.0.1:${adminPort}`,ingress=`http://127.0.0.1:${ingressPort}`;
 await until(()=>fetch(admin+'/health'),r=>r.ok,'Restate readiness');
 const databaseId=randomUUID();output=new RuntimeOutput({limit:32*1024*1024});output.install();
 const source=readFileSync(new URL('../../native-d1-bridge/bridge.mjs',import.meta.url),'utf8').replaceAll('export ','');
 const bridgeScript=source+`\nlet authority;export default {async fetch(r,e){if(new URL(r.url).pathname==='/__configure'){authority=await r.json();return new Response('ok')}return createBridge(authority).fetch(r,e)}};`;
 mf=new Miniflare({log:new Log(LogLevel.NONE),handleRuntimeStdio:(a,b)=>output.capture(a,b),workers:[{name:'bridge',modules:true,script:bridgeScript,compatibilityDate:bridgeCompatibilityDate,d1Databases:{DB:databaseId},bindings:{D1_BRIDGE_TOKEN:token}},{name:'workflow',modules:true,scriptPath:process.env.WORKFLOW_WORKER_MODULE??fileURLToPath(new URL('../worker/workflows-worker.mjs',import.meta.url)),modulesRules:[{type:'CompiledWasm',include:['**/*.wasm']}],compatibilityDate:'2024-09-23',compatibilityFlags:['nodejs_compat'],d1Databases:{DB:databaseId},bindings:{RESTATE_IDENTITY_KEYS:signer.key,P1_HOST:'production.test',GH_REPO_APP_ID:'123',GH_REPO_CLIENT_ID:'Iv.fixture-repo',GH_REPO_PRIVATE_KEYS:privateKeys,GH_BULK_ENABLED:'true',GITHUB_API_BASE:githubUrl},async outboundService(r){if(new URL(r.url).origin!==githubUrl){unexpectedOutbound++;return new Response('disabled',{status:502});}if(r.method==='PUT')puts++;return fetch(r.url,{method:r.method,headers:[...r.headers].filter(([n])=>n!=='content-length'),body:['GET','HEAD'].includes(r.method)?undefined:Buffer.from(await r.arrayBuffer()),signal:AbortSignal.timeout(10000)});}}]});
 db=await mf.getD1Database('DB','bridge');oldWorker=await mf.getWorker('workflow');bridgeWorker=await mf.getWorker('bridge');
 const migrations=readdirSync(new URL('../../migrations/',import.meta.url)).filter(n=>n.endsWith('.sql')).sort().map(name=>({name,sql:readFileSync(new URL('../../migrations/'+name,import.meta.url),'utf8')}));
 for(const m of migrations)await db.exec(m.sql.replace(/^[ \t]*--.*$/gm,'').replaceAll('\n',' '));
 await db.exec('CREATE TABLE d1_migrations(id INTEGER PRIMARY KEY AUTOINCREMENT,name TEXT NOT NULL UNIQUE,applied_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP)');
 for(const m of migrations)await db.prepare('INSERT INTO d1_migrations(name) VALUES(?)').bind(m.name).run();
 const manifest={protocol:1,database_id:databaseId,migrations:migrations.map(m=>({name:m.name,sha256:createHash('sha256').update(m.sql).digest('hex')})),schema_sha256:await sha256(JSON.stringify((await db.prepare(schemaQuery).all()).results))};
 await bridgeWorker.fetch('http://bridge/__configure',{method:'POST',body:JSON.stringify(manifest)});
 // Provision identities before registering either real producer; no DDL/adoption after this point.
 for(const user of [7,91])await db.prepare("INSERT INTO users(user_id,login,avatar_url,last_seen_at) VALUES(?,?,NULL,'2026-01-01T00:00:00Z')").bind(user,`user-${user}`).run();
 const ledgerBefore=(await db.prepare('SELECT * FROM d1_migrations ORDER BY id').all()).results;
 const bridgeUrl=await listen(httpServer(async(req,res)=>{try{const bytes=[];let n=0;for await(const b of req){n+=b.length;assert.ok(n<=32*1024*1024);bytes.push(b);}const r=await bridgeWorker.fetch('http://bridge'+req.url,{method:req.method,headers:req.headers,body:Buffer.concat(bytes)});res.writeHead(r.status,Object.fromEntries(r.headers));res.end(Buffer.from(await r.arrayBuffer()));}catch{res.writeHead(503);res.end();}}));
 const manifestPath=evidence+'/authority.json';writeFileSync(manifestPath,JSON.stringify(manifest));
 const nativePort=await port();nativeUrl=`http://127.0.0.1:${nativePort}`;
 child('native',process.env.NATIVE_BRIDGE_ENDPOINT_BIN,[],{P1_HOST:'production.test',GH_REPO_APP_ID:'123',GH_REPO_CLIENT_ID:'Iv.fixture-repo',GH_REPO_PRIVATE_KEYS:privateKeys,GH_BULK_ENABLED:'true',RESTATE_IDENTITY_KEYS:signer.key,GHINVITE_STORAGE:'d1-bridge',GHINVITE_D1_BRIDGE_URL:bridgeUrl+'/v1/d1',GHINVITE_D1_BRIDGE_TOKEN:token,GHINVITE_D1_AUTHORITY_MANIFEST:manifestPath,GHINVITE_LISTEN_ADDR:`127.0.0.1:${nativePort}`,GITHUB_API_BASE:githubUrl});
 await until(()=>nativeFetch('/discover','GET',{'accept':'application/vnd.restate.endpointmanifest.v3+json',...signer.headers('/discover')},Buffer.alloc(0)),r=>r.status===200,'native signed readiness');
 const oldUrl=await listen(proxy('worker')),newUrl=await listen(proxy('native'));
 const first=await http(admin+'/deployments',{uri:oldUrl});writeFileSync(evidence+'/old-deployment.json',JSON.stringify(first));
 await http(ingress+'/Installation/repo:1/onboard',setupCommand({app:'repo',installation_id:1,actor_user_id:7,account_id:100,account_login:'acme',account_type:'Organization',selected_repos:'all',installed_at:'2026-01-01T00:00:00Z'}));
 const linkId=id();markers.add(capabilityFor(linkId).secret);const creation={link_id:linkId,secret_sha256:capabilityFor(linkId).sha256,admin:{account_id:100,user_id:7},account_id:100,installation_id:1,description:'Cutover',internal_note:null,expires_at:null,max_uses:1,approval_required:false,targets:{organization:null,teams:[],repos:[{repo_id:10,repo_full_name:'acme/api',grant:'read'}]}};
 await http(`${ingress}/InvitationLink/${linkId}/create`,creation);
 const admitted=await http(`${ingress}/InvitationLink/${linkId}/admit`,{...admitCapability(linkId),link_id:linkId,operation_id:id(),requester_id:91});assert.equal(admitted.result.state,'approved');
 await until(()=>held,Boolean,'actual provider PUT then lost Worker response');
 const query=async sql=>(await http(admin+'/query',{query:sql})).rows;
 const pending=await until(()=>query(`SELECT * FROM sys_invocation WHERE id='${blocked}'`),rows=>rows[0]&&rows[0].status!=='completed','old invocation pending');
 writeFileSync(evidence+'/pending-old.json',JSON.stringify(pending));assert.equal(pending[0].pinned_deployment_id,first.id);
 const fences=(await db.prepare('SELECT * FROM delivery_attempts').all()).results;assert.equal(fences.length,1);assert.equal(fences[0].retryable,0);assert.equal(fences[0].attempt_generation,1);const command=JSON.parse(fences[0].command);assert.equal(puts,1);
 const second=await http(admin+'/deployments',{uri:newUrl});assert.notEqual(second.id,first.id);writeFileSync(evidence+'/new-deployment.json',JSON.stringify(second));
 const budget={app:'repo',account_id:200,revision:1,observed_at:new Date().toISOString(),confirmed_established:true,buckets:[],content_used_minute:1,content_used_hour:1,next_content_write_at:null};
 await http(ingress+'/InviteBudgetProjection/repo:200/apply',budget);
 const newInvocation=(await query("SELECT * FROM sys_invocation WHERE target_service_name='InviteBudgetProjection' AND target_service_key='repo:200'"))[0];assert.equal(newInvocation.pinned_deployment_id,second.id);
 const oldInvocation=blocked;blocked=null;
 await until(()=>query(`SELECT * FROM sys_invocation WHERE id='${oldInvocation}'`),rows=>rows[0]?.status==='completed','old pinned invocation recovery');
 const completed=(await query(`SELECT * FROM sys_invocation WHERE id='${oldInvocation}'`))[0];assert.equal(completed.pinned_deployment_id,first.id);
 const deliveryKey=`${command.request_id}:${command.repo_id}:${command.delivery_generation}`;
 const receipt=await http(`${ingress}/RepositoryDelivery/${deliveryKey}/create`,command);assert.equal(receipt.outcome.kind,'created');assert.deepEqual(receipt.command,command);
 const repeated=await http(`${ingress}/RepositoryDelivery/${deliveryKey}/create`,command);assert.deepEqual(repeated,receipt);
 assert.equal(puts,1);const calls=await providerCalls(githubAdmin);assert.equal(calls.requests.filter(r=>r.method==='PUT').length,1);
 assert.deepEqual((await db.prepare('SELECT * FROM delivery_attempts').all()).results,fences);
 assert.deepEqual((await db.prepare('SELECT * FROM d1_migrations ORDER BY id').all()).results,ledgerBefore);
 assert.ok(traffic.some(t=>t.kind==='worker'&&t.invocation===oldInvocation));assert.ok(!traffic.some(t=>t.kind==='native'&&t.invocation===oldInvocation));assert.ok(traffic.some(t=>t.kind==='native'&&t.invocation===newInvocation.id));assert.equal(unexpectedOutbound,0);
 // A fresh native owner performs an actual provider effect, beyond native SQL projection.
 await db.prepare("INSERT INTO users(user_id,login,avatar_url,last_seen_at) VALUES(92,'user-92',NULL,'2026-01-01T00:00:00Z')").run();
 const nextLink=id();markers.add(capabilityFor(nextLink).secret);
 const next={...creation,link_id:nextLink,secret_sha256:capabilityFor(nextLink).sha256};
 await http(`${ingress}/InvitationLink/${nextLink}/create`,next);
 const acceptedNative=await http(`${ingress}/InvitationLink/${nextLink}/admit`,{...admitCapability(nextLink),link_id:nextLink,operation_id:id(),requester_id:92});assert.equal(acceptedNative.result.state,'approved');
 const nativePlan=await http(`${ingress}/InvitationRequest/${acceptedNative.result.request_id}/approved_plan`,{link_id:nextLink,request_id:acceptedNative.result.request_id,requester_id:92});
 const nativeCommand=nativePlan.commands[0],nativeKey=`${nativeCommand.request_id}:${nativeCommand.repo_id}:${nativeCommand.delivery_generation}`;
 const nativeStatus=await until(()=>http(`${ingress}/RepositoryDelivery/${nativeKey}/status`),v=>v?.create?.outcome?.kind==='created','fresh native provider create');
 const nativeReceipt=await http(`${ingress}/RepositoryDelivery/${nativeKey}/create`,nativeCommand);assert.deepEqual(nativeReceipt,nativeStatus.create);
 const nativeCalls=await providerCalls(githubAdmin);assert.equal(nativeCalls.requests.filter(r=>r.method==='PUT').length,2);writeFileSync(evidence+'/provider-calls.json',JSON.stringify(nativeCalls));
 const nativeInvocation=(await query(`SELECT * FROM sys_invocation WHERE target_service_name='RepositoryDelivery' AND target_service_key='${nativeKey}' AND target_handler_name='create'`)).find(r=>r.pinned_deployment_id===second.id);assert.ok(nativeInvocation);
 const finalFences=(await db.prepare('SELECT * FROM delivery_attempts ORDER BY delivery_key').all()).results;assert.equal(finalFences.length,2);assert.ok(finalFences.every(f=>f.retryable===0&&f.attempt_generation===1));assert.deepEqual(finalFences.find(f=>f.delivery_key===fences[0].delivery_key),fences[0]);
 assert.deepEqual((await db.prepare('SELECT * FROM d1_migrations ORDER BY id').all()).results,ledgerBefore);
 // Each count is evaluated in the same SQL snapshot as its rows; background
 // projection/timer writes may legitimately advance between separate queries.
 const inventory=async(table,columns='*')=>{const rows=await query(`SELECT ${columns},COUNT(*) OVER () AS capture_total FROM ${table} LIMIT 10000`);assert.ok(rows.length>0,`${table} must contain this rehearsal's records`);assert.ok(rows.every(r=>r.capture_total===rows.length),`${table} capture must include every row without truncation`);writeFileSync(`${evidence}/${table==='sys_journal'?'journal':table==='sys_invocation'?'invocations':'state'}.json`,JSON.stringify(rows));return rows;};
 const journal=await inventory('sys_journal','id,index,raw');
 const invocations=await inventory('sys_invocation');
 const state=await inventory('state');
 const currentBudget=JSON.parse(await db.prepare("SELECT snapshot_json FROM invite_budget_projection WHERE app='repo' AND account_id=100").first('snapshot_json'));assert.equal(currentBudget.content_used_hour,2);assert.equal(currentBudget.content_used_minute,2);

 report={passed:true,old_deployment:first.id,new_deployment:second.id,old_invocation:oldInvocation,new_invocation:newInvocation.id,old_pinned_completed:true,provider_puts:2,old_provider_puts:1,native_provider_puts:1,native_delivery_invocation:nativeInvocation.id,fence_unchanged:true,ledger_unchanged:true,budget_native:true,receipt_replay_exact:true,journal_rows:journal.length,state_rows:state.length,invocation_rows:invocations.length,delivery_budget_used:currentBudget.content_used_hour,scope:'Owned local Restate pinning with per-request test-proxy signatures checked by production endpoint verifiers; same actual D1 binding, no hosted deployment, database migration or adoption'};
}catch(error){primary=error;}finally{
 if(db){raw={};try{for(const {name}of(await db.prepare("SELECT name FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%' AND name NOT LIKE '_cf_%' ORDER BY name").all()).results)raw[name]=(await db.prepare('SELECT * FROM "'+name.replaceAll('"','""')+'"').all()).results;writeFileSync(evidence+'/rows.json',JSON.stringify(raw));}catch(error){primary??=error;}}
 for(const s of sessions)s.destroy();for(const s of servers)await new Promise(r=>s.close(r));
 if(mf)try{await output.disposeAndDrain(mf);output.texts().forEach((v,i)=>streams.set('workerd-'+i,v));}catch(error){primary??=error;}
 for(const owned of children.reverse()){
  const alive=owned.c.exitCode===null&&owned.c.signalCode===null;let timedOut=false,timer;
  if(alive)owned.c.kill('SIGTERM');
  try{const result=await Promise.race([owned.done,new Promise(resolve=>{timer=setTimeout(()=>{timedOut=true;owned.c.kill('SIGKILL');owned.done.then(resolve);},5000);})]);
   const receipt={name:owned.name,alive_before_shutdown:alive,closed:true,stdout_eof:owned.c.stdout.readableEnded,stderr_eof:owned.c.stderr.readableEnded,timed_out:timedOut,bytes:owned.bytes(),...result};childReceipts.push(receipt);completeChild(receipt);
  }catch(error){primary??=error;}finally{clearTimeout(timer);owned.logs.forEach((v,i)=>streams.set(owned.name+'-'+i,Buffer.concat(v).toString()));}
 }
 writeFileSync(evidence+'/traffic.json',JSON.stringify(traffic));
 for(const [name,text]of streams)writeFileSync(`${evidence}/${name}.log`,text);
 writeFileSync(evidence+'/child-close.json',JSON.stringify(childReceipts,null,2));
 scanPrivate([...streams.values(),raw??{}],[...markers]);
}
if(primary)throw primary;
const privateScan=scanPrivate([raw,JSON.parse(readFileSync(evidence+'/journal.json','utf8')),JSON.parse(readFileSync(evidence+'/invocations.json','utf8')),JSON.parse(readFileSync(evidence+'/state.json','utf8')),JSON.parse(readFileSync(evidence+'/provider-calls.json','utf8')),...streams.values()],[...markers]);
assert.ok(markers.size>=7,'actual capabilities and minted tokens were observed');report.private_marker_count=markers.size;report.private_scan_values=privateScan;report.children=childReceipts;
report.runtime_streams=streams.size;report.raw_tables=Object.keys(raw).length;report.complete_eof=true;
report.artifacts=Object.fromEntries(['RESTATE_SERVER_BIN','NATIVE_BRIDGE_ENDPOINT_BIN','OCTOEMU_BIN'].map(k=>[k,hash(process.env[k])]));
writeFileSync(evidence+'/results.json',JSON.stringify(report,null,2));console.log(JSON.stringify(report));
