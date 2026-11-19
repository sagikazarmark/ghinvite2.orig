import { registerFinalLogScan } from './runtime-output.mjs';
// Actual Worker/D1/session composition; explicit HTTP authority fixture, not a Restate proof.
import assert from 'node:assert/strict';
import {readFileSync,readdirSync} from 'node:fs';
import {fileURLToPath} from 'node:url';
import {Miniflare,Response,Log,LogLevel} from 'miniflare';
import {GITHUB_TOKEN,signinAppSeed,startOctoemu} from './octoemu.mjs';
import {installedWebApp} from './app-config.mjs';
import {capabilityFor} from './capability.mjs';
const link='01K00000000000000000000001',capability=capabilityFor(link);
const ADDRESS='PRIVATE_LOCAL_MARKER@example.com', UNMATCHED='PRIVATE_UNMATCHED@other-private.example';
const targets={organization:null,teams:[],repos:[{repo_id:10,repo_full_name:'acme/api',grant:'read'}]};
const rules=[{exact:'example.com'}];
let logs='',overflow=false;
const admissions=[],pendingRows=[],codes=[],states=[];
// GitHub is octoemu. user-8 has a verified address in example.com, another
// verified one elsewhere, an unverified one and a noreply one; user-9 signs
// in while /user/emails fails.
const github=await startOctoemu({users:[
  {id:8,login:'user-8',emails:[{email:ADDRESS,primary:true,verified:true,visibility:'private'},{email:UNMATCHED,verified:true},{email:'unverified@example.com',verified:false},{email:'id@users.noreply.github.com',verified:true}]},
  {id:9,login:'user-9',emails:[{email:'nine@example.com',primary:true,verified:true,visibility:'private'}]}],
  apps:[signinAppSeed({clientSecret:'signin-fixture-secret',hosts:['domains.test']})]});
const counters=async()=>{const c=await github.count('POST /login/oauth/access_token','GET /user','GET /user/emails','DELETE /applications/{client_id}/token');
 return {exchange:c['POST /login/oauth/access_token'],user:c['GET /user'],emails:c['GET /user/emails'],revoke:c['DELETE /applications/{client_id}/token']};};
const mf=new Miniflare({log:new Log(LogLevel.ERROR),modules:true,scriptPath:fileURLToPath(new URL('./worker.mjs',import.meta.url)),modulesRules:[{type:'CompiledWasm',include:['**/*.wasm']}],compatibilityDate:'2024-09-23',compatibilityFlags:['nodejs_compat'],kvNamespaces:['SESSIONS','AUTHZ_CACHE'],d1Databases:['DB'],ratelimits:{RL_IP:{namespace_id:'1001',simple:{limit:30,period:60}},RL_USER:{namespace_id:'1002',simple:{limit:10,period:60}},AUTHZ_RL:{namespace_id:'1003',simple:{limit:10,period:60}}},bindings:{...installedWebApp,P1_HOST:'domains.test',P1_KEYRING:'kekring1;name=p1;current=1;hwm=1;k1=BwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwc.9fdab21f330c89b7',RESTATE_INGRESS_URL:'https://ingress.test',RESTATE_INGRESS_TOKEN:'ingress-fixture',GHINVITE_GITHUB_INSTALL_URL:'https://github.com/apps/fixture/installations/new',GH_SIGNIN_CLIENT_ID:'Iv.fixture-signin',GH_SIGNIN_CLIENT_SECRET:'signin-fixture-secret',...github.bindings},handleRuntimeStdio(stdout,stderr){for(const stream of[stdout,stderr])stream.on('data',chunk=>{if(Buffer.byteLength(logs)+chunk.length>2*1024*1024)overflow=true;else logs+=chunk.toString();});},async outboundService(request){
 if(github.owns(request))return github.forward(request);
 const url=new URL(request.url);
 if(url.hostname==='ingress.test'){
  assert.equal(request.headers.get('authorization'),'Bearer ingress-fixture');const input=await request.json();assert.equal(input.link_id,link);const method=url.pathname.split('/').at(-1);
  if(method==='offer'){assert.equal(input.secret_sha256,capability.sha256);assert.equal(input.secret_generation,1);assert.equal('requester_id'in input,false);return Response.json({link_id:link,account_id:9001,targets,approval_required:true,email_domains:rules,paused:false});}
  if(method==='requester_page'){assert.ok(input.requester_id>0);assert.equal(input.secret_sha256,capability.sha256);if(input.operation_id)return new Response('',{status:404});return Response.json({link_id:link,account_id:9001,description:'Domain workshop',targets,email_domains:rules,approval_required:true,can_start_fresh:true,paused:false,attempt:null,request:null,rerequest_after:null});}
  if(method==='prepare_attempt'){return Response.json({input,receipt:null});}
  if(method==='admit'){admissions.push(input);return Response.json({decided_at:new Date().toISOString(),result:{kind:'accepted',request_id:'01K00000000000000000000002',state:'pending',decision_deadline:new Date(Date.now()+86400000).toISOString()}});}
 }
 throw new Error(`Unexpected outbound ${request.method} ${url.hostname}${url.pathname}`);
}});
let cookie='';
const request=async(path,body)=>{const response=await mf.dispatchFetch(`https://domains.test${path}`,{method:body?'POST':'GET',redirect:'manual',headers:{cookie,'CF-Connecting-IP':'192.0.2.40',...(body?{'content-type':'application/x-www-form-urlencoded'}:{})},body:body?new URLSearchParams(body).toString():undefined});const set=response.headers.get('set-cookie');if(set)cookie=set.split(';')[0];return response;};
const hidden=(html,name)=>{const match=html.match(new RegExp(`name="${name}"[^>]*value="([^"]+)"`));assert.ok(match,`${name} field`);return match[1];};
try{
 const db=await mf.getD1Database('DB');for(const file of readdirSync(new URL('../../migrations/',import.meta.url)).filter(f=>f.endsWith('.sql')).sort())await db.exec(readFileSync(new URL(`../../migrations/${file}`,import.meta.url),'utf8').replace(/^[ \t]*--.*$/gm,'').replaceAll('\n',' '));
 await db.prepare('INSERT INTO link_secrets(secret_sha256,link_id,secret_generation,created_at) VALUES(?,?,?,?)').bind(capability.sha256,link,1,new Date().toISOString()).run();
 let response=await request('/i');let html=await response.text();response=await request('/i',{csrf_token:hidden(html,'csrf_token'),secret:capability.secret});assert.equal(response.status,303);
 response=await request('/join');html=await response.text();assert.equal(response.status,200);assert.match(html,/Verified email domain required/);assert.match(html,/example.com/);assert.match(html,/acme\/api/);assert.equal(html.includes('name="justification"'),false);
 const login=async user=>{const response=await request('/login?return_to=%2Fjoin%3Ffresh%3Dtrue');assert.equal(response.status,303,await response.clone().text());const url=new URL(response.headers.get('location'));assert.equal(url.searchParams.has('scope'),false);assert.equal(url.searchParams.get('code_challenge_method'),'S256');assert.equal((await db.prepare('SELECT COUNT(*) AS n FROM pending_signins').first()).n,1);const state=url.searchParams.get('state');states.push(state);const raw=JSON.stringify((await db.prepare('SELECT * FROM pending_signins').all()).results);assert.equal(raw.includes(state),false);pendingRows.push(raw);const back=await github.authorize(url.href,user);assert.equal(back.origin,'https://domains.test');assert.equal(back.searchParams.get('state'),state);codes.push(back.searchParams.get('code'));return back.pathname+back.search;};
 let callback=await login('user-8');const old=cookie;response=await request(callback);assert.equal(response.status,303,await response.clone().text());assert.notEqual(cookie,old);assert.equal(response.headers.get('location'),'/join?fresh=true');assert.equal((await db.prepare('SELECT COUNT(*) AS n FROM pending_signins').first()).n,0);
 response=await request('/join');html=await response.text();assert.equal(response.status,200);response=await request('/join',{csrf_token:hidden(html,'csrf_token'),operation_id:hidden(html,'operation_id'),justification:'Ready'});assert.equal(response.status,303,await response.clone().text());assert.equal(admissions.length,1);assert.deepEqual(admissions[0].domain_evidence.matched_domains,['example.com']);assert.equal(admissions[0].domain_evidence.link_id,link);assert.equal(admissions[0].requester_id,8);
 await github.fault({method:'GET',path:'/user/emails',action:{kind:'respond',status:503,text:''}});callback=await login('user-9');response=await request(callback);assert.equal(response.status,500);response=await request('/join?fresh=true');html=await response.text();assert.match(html,/Confirm your email domain/);assert.equal(html.includes('name="justification"'),false);assert.equal(admissions.length,1);assert.deepEqual(await counters(),{exchange:2,user:2,emails:2,revoke:2});
 const persisted=[...pendingRows];for(const namespace of['SESSIONS','AUTHZ_CACHE']){const kv=await mf.getKVNamespace(namespace);for(const key of(await kv.list()).keys)persisted.push(await kv.get(key.name));}
 const tables=(await db.prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' AND name NOT LIKE '_cf_%'").all()).results;for(const {name}of tables){assert.match(name,/^[a-z_]+$/);persisted.push(JSON.stringify((await db.prepare(`SELECT * FROM ${name}`).all()).results));}
 assert.equal(overflow,false);for(const marker of[ADDRESS,UNMATCHED,'PRIVATE_LOCAL_MARKER','other-private.example',...codes,...states]){assert.equal(persisted.join('\n').includes(marker),false,marker);assert.equal(JSON.stringify(admissions).includes(marker),false,marker);}
 assert.doesNotMatch(persisted.join('\n'),GITHUB_TOKEN);assert.doesNotMatch(JSON.stringify(admissions),GITHUB_TOKEN);

}finally{await mf.dispose();await github.stop();}

// Include runtime shutdown output before declaring the complete-log scan green.
assert.equal(overflow,false);
for(const marker of[ADDRESS,UNMATCHED,'PRIVATE_LOCAL_MARKER','other-private.example',...codes,...states]) assert.equal(logs.includes(marker),false,marker);
assert.doesNotMatch(logs,GITHUB_TOKEN);
console.log('PASS actual Worker typed domain sign-in: public offer, PKCE + atomic D1 take, verified-only matched domains, token-free session rotation, fresh admission, failed account-switch reauth clears evidence, guaranteed revoke, all app D1/KV and complete-log marker scan');

registerFinalLogScan(bytes => { for(const marker of[ADDRESS,UNMATCHED,'PRIVATE_LOCAL_MARKER','other-private.example',...codes,...states])assert.ok(!bytes.includes(Buffer.from(marker))); assert.doesNotMatch(bytes.toString('latin1'),GITHUB_TOKEN); });
