import { registerFinalLogScan } from './runtime-output.mjs';
import assert from 'node:assert/strict';
import { createHmac } from 'node:crypto';
import { fileURLToPath } from 'node:url';
import { Miniflare, Log, LogLevel, Response } from 'miniflare';
import { installedWebApp, signinApp } from './app-config.mjs';

// Counts are derived from complete emitted records, never an isolate-local
// aggregate presented as a deployment-wide rate.
const logs=[];
let bytes=0,overflow=false,mints=0,apiCalls=0,concurrent=0;
const byInstallation=new Map();
const TOKEN='PRIVATE_INSTALLATION_TOKEN_MARKER';
const BODY='PRIVATE_WEBHOOK_BODY_MARKER';
const SIGNATURE='PRIVATE_SIGNATURE_MARKER';
const mf=new Miniflare({
  log:new Log(LogLevel.ERROR),modules:true,
  scriptPath:fileURLToPath(new URL('./worker.mjs',import.meta.url)),
  modulesRules:[{type:'CompiledWasm',include:['**/*.wasm']}],
  compatibilityDate:'2024-09-23',compatibilityFlags:['nodejs_compat'],
  kvNamespaces:['SESSIONS','AUTHZ_CACHE'],d1Databases:['DB'],
  ratelimits:{
    RL_IP:{simple:{limit:30,period:60}},
    RL_USER:{simple:{limit:10,period:60}},
    AUTHZ_RL:{simple:{limit:10,period:60}},
  },
  bindings:{...installedWebApp,
    P1_HOST:'app.test',
    P1_KEYRING:'akr1;name=p1;current=1;k1=BwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwc.74c0aab4b913ee29',
    RESTATE_INGRESS_URL:'https://restate.invalid',RESTATE_INGRESS_TOKEN:'PRIVATE_INGRESS_MARKER',
    GHINVITE_GITHUB_INSTALL_URL:'https://github.com/apps/dummy/installations/new',
    ...signinApp,
  },
  handleRuntimeStdio(stdout,stderr) {
    for(const stream of [stdout,stderr]) stream.on('data',chunk=>{
      bytes+=chunk.length;
      if(bytes>256*1024) overflow=true; else logs.push(chunk.toString());
    });
  },
  async outboundService(request) {
    const url=new URL(request.url);
    assert.equal(url.origin,'https://broker.test','no unexpected outbound side effect');
    const mint=url.pathname.match(/^\/app\/installations\/(\d+)\/access_tokens$/);
    if(mint) {
      assert.equal(request.method,'POST');
      const id=Number(mint[1]); mints++;
      byInstallation.set(id,(byInstallation.get(id)??0)+1);
      return Response.json({token:TOKEN+'-'+id+'-'+mints,expires_at:new Date(Date.now()+3600000).toISOString()},{status:201});
    }
    assert.equal(url.pathname,'/installation/repositories');
    assert.equal(request.method,'GET');
    assert.ok(request.headers.get('authorization').startsWith('Bearer '+TOKEN));
    apiCalls++;
    return Response.json(apiCalls===1?{message:'PRIVATE_UPSTREAM_MARKER'}:{repositories:[]},{status:apiCalls===1?401:200});
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
  concurrent=byInstallation.get(201);
  assert.ok(concurrent>=1&&concurrent<=20,'twenty concurrent misses create at most twenty tokens');
  await broker('ensure',201);
  assert.equal(byInstallation.get(201),concurrent,'a later request reuses the isolate-cached token');
  await broker('observed-read',202);
  await broker('observed-read',202);
  assert.equal(byInstallation.get(202),2,'one 401 produces one actual remint');
  assert.equal(apiCalls,3,'includes failed authenticated call and its retry');
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
} finally {await mf.dispose();}
assert.equal(overflow,false,'complete log capture must fit the bound');
const text=logs.join('');
const events=name=>text.split(/\r?\n/).filter(line=>line.includes('event="'+name+'"'));
// gh-toolkit's broker emitted installation_mint_attempt/installation_api_call;
// octotoken's provider and ghinvite-github emit no token or API-call events
// (GM-05). Token creations and API calls are counted at the fixture's outbound.
assert.equal(events('installation_mint_attempt').length,0);
assert.equal(events('installation_api_call').length,0);
assert.equal(mints,concurrent+2);
const signatures=events('webhook_signature_failure');
assert.equal(signatures.length,2);
assert.ok(signatures.every(line=>line.includes('app="repo"')&&/occurred_at=\d+/.test(line)));
assert.equal(signatures.filter(line=>line.includes('reason="missing"')).length,1);
assert.equal(signatures.filter(line=>line.includes('reason="invalid"')).length,1);
const limits=events('edge_binding_rejection');
assert.equal(limits.length,3);
for(const bucket of ['ip','user','authority']) assert.equal(limits.filter(line=>line.includes('bucket="'+bucket+'"')&&line.includes('reason="limited"')).length,1);
for(const marker of [TOKEN,BODY,SIGNATURE,'PRIVATE_UPSTREAM_MARKER','PRIVATE_INGRESS_MARKER','PRIVATE_CLIENT_SECRET_MARKER','PRIVATE_DELIVERY_ID_MARKER','192.0.2.17','repo:9876','734967412345']) assert.ok(!text.includes(marker),'private marker leaked: '+marker);
console.log('PASS actual Worker diagnostic events: '+concurrent+' concurrent token creation(s), one 401 replacement/three API calls at the outbound, two app signature failures, three binding rejections; complete logs contain no private markers');

registerFinalLogScan(bytes => { for(const marker of [TOKEN,BODY,SIGNATURE,'PRIVATE_UPSTREAM_MARKER','PRIVATE_INGRESS_MARKER','PRIVATE_CLIENT_SECRET_MARKER','PRIVATE_DELIVERY_ID_MARKER','192.0.2.17','repo:9876','734967412345'])assert.ok(!bytes.includes(Buffer.from(marker))); });
