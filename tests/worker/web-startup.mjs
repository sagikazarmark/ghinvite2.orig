import { registerFinalLogScan } from './runtime-output.mjs';
import assert from 'node:assert/strict';
import { fileURLToPath } from 'node:url';
import { Miniflare, Response, Log, LogLevel } from 'miniflare';
import { installedWebApp, signinApp } from './app-config.mjs';

const bindings = {
  ...installedWebApp, P1_HOST: 'startup.test',
  P1_KEYRING: 'akr1;name=p1;current=1;k1=BwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwc.74c0aab4b913ee29',
  RESTATE_INGRESS_URL: 'https://ingress.test', RESTATE_INGRESS_TOKEN: 'INGRESS_SECRET_MARKER',
  ...signinApp,
  GHINVITE_GITHUB_INSTALL_URL: 'https://github.com/apps/fixture/installations/new',
};
const emailRing = 'akr1;name=p1-email;current=1;k1=BwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwc.81678c3e040a46b7';
const org = {P1_APP_STAGE:'repo-and-org',GH_ORG_APP_ID:'456',GH_ORG_CLIENT_ID:'Iv.fixture-org',GH_ORG_PRIVATE_KEYS:installedWebApp.GH_REPO_PRIVATE_KEYS,GH_ORG_WEBHOOK_SECRETS:'org-hook-fixture',GH_ORG_DELIVERY_ENABLED:'true'};
const cases = [
  ['bulk repo',{GH_BULK_ENABLED:'true'},200],
  ['bulk email',{...org,GH_BULK_ENABLED:'true',P1_EMAIL_KEYRING:emailRing},200],
  ['bulk invalid',{GH_BULK_ENABLED:'SECRET_MARKER'},500],
  ['bulk email missing keyring',{...org,GH_BULK_ENABLED:'true'},500],
  ['bulk email wrong ring',{...org,GH_BULK_ENABLED:'true',P1_EMAIL_KEYRING:emailRing.replace('name=p1-email','name=p1')},500],
  ['bulk email malformed ring',{P1_EMAIL_KEYRING:'SECRET_MARKER'},500],
  ['reissue enabled',{GH_REISSUE_ENABLED:'true'},200],
  ['reissue invalid',{GH_REISSUE_ENABLED:'SECRET_MARKER'},500],

  ['valid', {}, 200],
  ['malformed signin client ID',{GH_SIGNIN_CLIENT_ID:'SECRET_MARKER/invalid'},500],
  ['explicit repo',{P1_APP_STAGE:'repo'},200],
  ['repo and org',{P1_APP_STAGE:'repo-and-org',GH_ORG_APP_ID:'456',GH_ORG_CLIENT_ID:'Iv.fixture-org',GH_ORG_PRIVATE_KEYS:installedWebApp.GH_REPO_PRIVATE_KEYS,GH_ORG_WEBHOOK_SECRETS:'org-hook-fixture'},200],
  ['invalid stage',{P1_APP_STAGE:'SECRET_MARKER'},500],
  ['empty stage',{P1_APP_STAGE:''},500],
  ['missing org',{P1_APP_STAGE:'repo-and-org'},500],
  ['partial org',{P1_APP_STAGE:'repo-and-org',GH_ORG_APP_ID:'456',GH_ORG_CLIENT_ID:'Iv.fixture-org',GH_ORG_PRIVATE_KEYS:installedWebApp.GH_REPO_PRIVATE_KEYS},500],
  ...['P1_HOST','P1_KEYRING','RESTATE_INGRESS_URL','RESTATE_INGRESS_TOKEN','GH_REPO_PRIVATE_KEYS',
    'GH_REPO_WEBHOOK_SECRETS','GH_SIGNIN_CLIENT_ID','GH_SIGNIN_CLIENT_SECRET'].map(field => [`missing ${field}`, {[field]:undefined}, 500]),
  ['host URL', {P1_HOST:'https://SECRET_MARKER.test'}, 500],
  ['host normalization', {P1_HOST:'STARTUP.test'}, 500],
  ['HTTP ingress', {RESTATE_INGRESS_URL:'http://127.0.0.1:8080'}, 500],
  ['userinfo', {RESTATE_INGRESS_URL:'https://SECRET_MARKER@ingress.test'}, 500],
  ['query', {RESTATE_INGRESS_URL:'https://ingress.test?SECRET_MARKER'}, 500],
  ['malformed token', {RESTATE_INGRESS_TOKEN:'SECRET_MARKER\r\n'}, 500],
  ['malformed OAuth secret', {GH_SIGNIN_CLIENT_SECRET:'SECRET_MARKER\n'}, 500],
  ['malformed signing key', {GH_REPO_PRIVATE_KEYS:'primary:SECRET_MARKER'}, 500],
  ['legacy aliases', {P1_HOST:undefined,RESTATE_INGRESS_URL:undefined,RESTATE_INGRESS_TOKEN:undefined,
    GHINVITE_BASE_URL:'https://startup.test',GHINVITE_RESTATE_INGRESS:'http://127.0.0.1:8080',
    GHINVITE_RESTATE_AUTH:'local-unauthenticated'}, 500],
];
for (const [label, changes, status] of cases) {
  let outgoing = 0;
  const logs = [];
  let logBytes = 0;
  const mf = new Miniflare({
    log:new Log(LogLevel.ERROR), modules:true,
    scriptPath:fileURLToPath(new URL('./worker.mjs', import.meta.url)),
    modulesRules:[{type:'CompiledWasm',include:['**/*.wasm']}],
    compatibilityDate:'2024-09-23',compatibilityFlags:['nodejs_compat'],
    kvNamespaces:['SESSIONS','AUTHZ_CACHE'], d1Databases:['DB'],
    ratelimits:{RL_IP:{simple:{limit:30,period:60}},RL_USER:{simple:{limit:10,period:60}},AUTHZ_RL:{simple:{limit:10,period:60}}},
    bindings:Object.fromEntries(Object.entries({...bindings,...changes}).filter(([,value])=>value!==undefined)),
    handleRuntimeStdio(stdout,stderr) {
      for(const stream of [stdout,stderr]) stream.on('data',chunk=>{
        logBytes+=chunk.length;
        if(logBytes<=1024*1024) logs.push(chunk.toString());
      });
    },
    outboundService() { outgoing++; return new Response('disabled',{status:502}); },
  });
  try {
    const response = await mf.dispatchFetch('https://startup.test/health');
    assert.equal(response.status,status,label);
    const body=await response.text();
    assert.equal(body,status===200?'ok':'Service configuration unavailable',label);
    assert.equal(outgoing,0,label);
    assert.equal(body.includes('SECRET_MARKER'),false,label);
  } finally { await mf.dispose(); }
  assert.ok(logBytes<=1024*1024,'complete startup logs must fit scan bound');
  assert.equal(logs.join('').includes('SECRET_MARKER'),false,label);
}
console.log(`PASS actual Worker canonical web startup: ${cases.length} cases, no alias/unsigned fallback, no outbound or secret disclosure`);

registerFinalLogScan(bytes => { assert.ok(!bytes.includes(Buffer.from('SECRET_MARKER'))); });
