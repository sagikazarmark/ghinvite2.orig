import { registerFinalLogScan } from './runtime-output.mjs';
import assert from 'node:assert/strict';
import { createHmac } from 'node:crypto';
import { fileURLToPath } from 'node:url';
import { Miniflare, Log, LogLevel } from 'miniflare';
import { installedWebApp, signinApp } from './app-config.mjs';
import { GITHUB_TOKEN, startOctoemu } from './octoemu.mjs';
import { brokerApp } from './github-world.mjs';

// Counts are derived from complete emitted records, never an isolate-local
// aggregate presented as a deployment-wide rate.
const logs=[];
let bytes=0,overflow=false,concurrent=0;
// GitHub is octoemu: the broker fixture's App (99001) installed as 201 and
// 202; the first installation read with 202's token is refused (401).
const github=await startOctoemu({orgs:[{id:9201,login:'tenant-201'},{id:9202,login:'tenant-202'}],apps:[brokerApp],
  installations:[{id:201,app_id:99001,account:'tenant-201'},{id:202,app_id:99001,account:'tenant-202'}]});
await github.fault({method:'GET',path:'/installation/repositories',times:1,label:'refused once',action:{kind:'respond',status:401,body:{message:'PRIVATE_UPSTREAM_MARKER'}}});
const mintsFor=async id=>(await github.calls('POST /app/installations/{installation_id}/access_tokens')).filter(call=>call.path===`/app/installations/${id}/access_tokens`).length;
const BODY='PRIVATE_WEBHOOK_BODY_MARKER';
const SIGNATURE='PRIVATE_SIGNATURE_MARKER';
const mf=new Miniflare({
  log:new Log(LogLevel.ERROR),modules:true,
  scriptPath:fileURLToPath(new URL('./worker.mjs',import.meta.url)),
  modulesRules:[{type:'CompiledWasm',include:['**/*.wasm']}],
  compatibilityDate:'2024-09-23',compatibilityFlags:['nodejs_compat'],
  kvNamespaces:['SESSIONS','AUTHZ_CACHE'],d1Databases:['DB'],
  ratelimits:{
    RL_IP:{namespace_id:'1001',simple:{limit:30,period:60}},
    RL_USER:{namespace_id:'1002',simple:{limit:10,period:60}},
    AUTHZ_RL:{namespace_id:'1003',simple:{limit:10,period:60}},
  },
  bindings:{...installedWebApp,
    P1_HOST:'app.test',
    P1_KEYRING:'kekring1;name=p1;current=1;hwm=1;k1=BwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwc.9fdab21f330c89b7',
    RESTATE_INGRESS_URL:'https://restate.invalid',RESTATE_INGRESS_TOKEN:'PRIVATE_INGRESS_MARKER',
    GHINVITE_GITHUB_INSTALL_URL:'https://github.com/apps/dummy/installations/new',
    ...signinApp,...github.bindings,
  },
  handleRuntimeStdio(stdout,stderr) {
    for(const stream of [stdout,stderr]) stream.on('data',chunk=>{
      bytes+=chunk.length;
      if(bytes>256*1024) overflow=true; else logs.push(chunk.toString());
    });
  },
  outboundService(request) {
    if(github.owns(request)) return github.forward(request);
    throw new Error(`no unexpected outbound side effect: ${new URL(request.url).origin}`);
  },
});
const broker=async(operation,id)=>{
  const response=await mf.dispatchFetch('https://app.test/__fixture/broker/'+operation+'?installation='+id);
  assert.equal(response.status,200,await response.text());
};
const webhook=async(path,headers,body)=>mf.dispatchFetch('https://app.test'+path,{
  method:'POST',headers:{'content-type':'application/json','x-github-event':'installation','x-github-delivery':'PRIVATE_DELIVERY_ID_MARKER',...headers},body,
});
try {
  // Workers do not coalesce concurrent misses (PD-4): each may create a token.
  await broker('coalesce',201);
  concurrent=await mintsFor(201);
  assert.ok(concurrent>=1&&concurrent<=20,'twenty concurrent misses create at most twenty tokens');
  await broker('ensure',201);
  assert.equal(await mintsFor(201),concurrent,'a later request reuses the isolate-cached token');
  await broker('observed-read',202);
  await broker('observed-read',202);
  assert.equal(await mintsFor(202),2,'one 401 produces one actual remint');
  assert.equal((await github.calls('GET /installation/repositories')).length,3,'includes failed authenticated call and its retry');
  assert.equal(await mintsFor(201)+await mintsFor(202),concurrent+2);
  const body=JSON.stringify({action:BODY});
  assert.equal((await webhook('/webhooks/repo',{},body)).status,401);
  assert.equal((await webhook('/webhooks/repo',{'x-hub-signature-256':SIGNATURE},body)).status,401);
  const signature='sha256='+createHmac('sha256','fixture-repo-webhook-current').update(body).digest('hex');
  // A verified unknown action is ignored, not a signature failure.
  assert.equal((await webhook('/webhooks/repo',{'x-hub-signature-256':signature},body)).status,202);
  assert.equal((await webhook('/webhooks/unknown',{},body)).status,404);
  for(const [bucket,key,limit] of [['ip','192.0.2.17',30],['user','734967412345',10],['authority','repo:9876',10]]) {
    const request=()=>mf.dispatchFetch('https://app.test/__fixture/rate-limit/'+bucket+(bucket==='ip'?'':'?'+key),{headers:bucket==='ip'?{'CF-Connecting-IP':key}:{}});
    for(let n=0;n<limit;n++) assert.equal((await request()).status,204);
    assert.equal((await request()).status,429);
  }
} finally {await mf.dispose();await github.stop();}
assert.equal(overflow,false,'complete log capture must fit the bound');
const text=logs.join('');
const events=name=>text.split(/\r?\n/).filter(line=>line.includes('event="'+name+'"'));
// gh-toolkit's broker emitted installation_mint_attempt/installation_api_call;
// octotoken's provider and ghinvite-github emit no token or API-call events
// (GM-05). Token creations and API calls are counted in octoemu's call log.
assert.equal(events('installation_mint_attempt').length,0);
assert.equal(events('installation_api_call').length,0);
const signatures=events('webhook_signature_failure');
assert.equal(signatures.length,2);
assert.ok(signatures.every(line=>line.includes('app="repo"')&&/occurred_at=\d+/.test(line)));
assert.equal(signatures.filter(line=>line.includes('reason="missing"')).length,1);
assert.equal(signatures.filter(line=>line.includes('reason="invalid"')).length,1);
const limits=events('edge_binding_rejection');
assert.equal(limits.length,3);
for(const bucket of ['ip','user','authority']) assert.equal(limits.filter(line=>line.includes('bucket="'+bucket+'"')&&line.includes('reason="limited"')).length,1);
for(const marker of [BODY,SIGNATURE,'PRIVATE_UPSTREAM_MARKER','PRIVATE_INGRESS_MARKER','PRIVATE_CLIENT_SECRET_MARKER','PRIVATE_DELIVERY_ID_MARKER','192.0.2.17','repo:9876','734967412345']) assert.ok(!text.includes(marker),'private marker leaked: '+marker);
assert.doesNotMatch(text,GITHUB_TOKEN,'installation token leaked');
console.log('PASS actual Worker diagnostic events: '+concurrent+' concurrent token creation(s), one 401 replacement/three API calls at the outbound, two app signature failures, three binding rejections; complete logs contain no private markers');

registerFinalLogScan(bytes => { for(const marker of [BODY,SIGNATURE,'PRIVATE_UPSTREAM_MARKER','PRIVATE_INGRESS_MARKER','PRIVATE_CLIENT_SECRET_MARKER','PRIVATE_DELIVERY_ID_MARKER','192.0.2.17','repo:9876','734967412345'])assert.ok(!bytes.includes(Buffer.from(marker))); assert.doesNotMatch(bytes.toString('latin1'),GITHUB_TOKEN); });
