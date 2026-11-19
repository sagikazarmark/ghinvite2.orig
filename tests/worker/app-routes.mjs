import assert from 'node:assert/strict';
import { createHash, createHmac } from 'node:crypto';
import { fileURLToPath } from 'node:url';
import { Miniflare, Response, Log, LogLevel } from 'miniflare';
import { installedWebApp, signinApp } from './app-config.mjs';

const calls = [];
const options = {
  log: new Log(LogLevel.ERROR), modules: true,
  scriptPath: fileURLToPath(new URL('./worker.mjs', import.meta.url)),
  modulesRules: [{type:'CompiledWasm',include:['**/*.wasm']}],
  compatibilityDate:'2024-09-23', compatibilityFlags:['nodejs_compat'],
  kvNamespaces:['SESSIONS', 'AUTHZ_CACHE'],d1Databases:['DB'],
  ratelimits:{RL_IP:{namespace_id:'1001',simple:{limit:30,period:60}},RL_USER:{namespace_id:'1002',simple:{limit:10,period:60}},AUTHZ_RL:{namespace_id:'1003',simple:{limit:10,period:60}}},
  bindings:{...installedWebApp,
    P1_HOST: 'app.test',
    P1_KEYRING:'kekring1;name=p1;current=1;hwm=1;k1=BwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwc.9fdab21f330c89b7',
    RESTATE_INGRESS_URL:'https://restate.invalid',RESTATE_INGRESS_TOKEN: 'synthetic-worker-ingress-key',
    GHINVITE_GITHUB_INSTALL_URL:'https://github.com/apps/dummy/installations/new',
    ...signinApp,
  },
  async outboundService(request) {
    assert.equal(new URL(request.url).origin,'https://restate.invalid','webhooks must not perform GitHub I/O');
    assert.equal(request.headers.get('authorization'),'Bearer synthetic-worker-ingress-key');
    calls.push({url:request.url,body:await request.json()});
    return Response.json({invocationId:'accepted-fixture-id',status:'Accepted'});
  },
};
const installation={id:77,app_id:123,account:{id:9001,login:'acme',type:'Organization'},
  created_at:'2026-05-20T11:50:00Z',updated_at:'2026-05-20T11:50:00Z',suspended_at:null,
  repository_selection:'selected',permissions:{administration:'write',metadata:'read'}};
const payload={action:'created',installation,sender:{id:42,login:'installer'}};
const mf=new Miniflare(options);
const send=(path,secret,event,body,id='unsigned-hint')=>{
  const data=JSON.stringify(body);
  return mf.dispatchFetch(`https://app.test${path}`,{method:'POST',body:data,headers:{
    'content-type':'application/json','x-github-event':event,'x-github-delivery':id,
    'x-hub-signature-256':`sha256=${createHmac('sha256',secret).update(data).digest('hex')}`,
  }});
};
try {
  for(const path of ['/webhooks/github','/webhooks/signin','/webhooks/org'])
    assert.equal((await send(path,'fixture-repo-webhook-current','installation',payload)).status,404);
  assert.equal((await send('/webhooks/repo','wrong-app-secret','installation',payload)).status,401);
  for(const secret of ['fixture-repo-webhook-current','fixture-repo-webhook-previous'])
    assert.equal((await send('/webhooks/repo',secret,'installation',payload,secret)).status,202);
  assert.equal(calls.length,2);
  assert.deepEqual(calls[0],calls[1],'unsigned delivery ID cannot change receipt identity');
  assert.equal(calls[0].url,'https://restate.invalid/Installation/repo:77/onboard_webhook/send');
  assert.equal(calls[0].body.sender_id,42);
  assert.equal(calls[0].body.delivery_id,createHash('sha256').update(JSON.stringify(payload)).digest('hex'));
  for(const event of ['member','repository_invitation'])
    assert.equal((await send('/webhooks/repo','fixture-repo-webhook-current',event,{action:'added'})).status,202);
  assert.equal((await send('/webhooks/repo','fixture-repo-webhook-current','installation',{action:'future_action'})).status,202);
  assert.equal(calls.length,2,'obsolete events dispatch nothing');
  for(const path of ['/setup/github','/setup/signin','/setup/org'])
    assert.equal((await mf.dispatchFetch(`https://app.test${path}?installation_id=77`,{redirect:'manual'})).status,404);
  const setup=await mf.dispatchFetch('https://app.test/setup/repo?installation_id=77',{redirect:'manual'});
  assert.equal(setup.status,303);
  assert.match(setup.headers.get('location'),/^\/login\?return_to=%2Fsetup%2Frepo/);
  console.log('PASS actual Worker app routes: rotated app HMAC, payload-bound identity, unknown apps, setup redirect and obsolete event isolation');
} finally { await mf.dispose(); }
const missing=new Miniflare({...options,bindings:Object.fromEntries(Object.entries(options.bindings).filter(([key])=>key!=='GH_REPO_WEBHOOK_SECRETS'))});
try {
  const response=await missing.dispatchFetch('https://app.test/health');
  assert.equal(response.status,500);
  assert.equal(calls.length,2,'invalid configuration cannot send outbound');
  console.log('PASS actual Worker startup refuses missing installed-app registry credentials');
} finally { await missing.dispose(); }
