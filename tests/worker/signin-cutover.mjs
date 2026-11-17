import { registerFinalLogScan } from './runtime-output.mjs';
// Actual sign-in routes (token-free, the only sign-in path), two keyring generations, and the real KV/D1 stores.
import assert from 'node:assert/strict';
import {hkdfSync,createHash} from 'node:crypto';
import {readFileSync,readdirSync} from 'node:fs';
import {fileURLToPath} from 'node:url';
import {Miniflare,Log,LogLevel} from 'miniflare';
import {GITHUB_TOKEN,signinAppSeed,startOctoemu} from './octoemu.mjs';
import {installedWebApp} from './app-config.mjs';
const verifier='CUTOVER_PRIVATE_PKCE_'.padEnd(64,'x');
// kekring v1 descriptor (libs/kekring/docs/kekring-format.md): the kid is HKDF(root, kekring/v1/kid ‖ len ‖ name ‖ u32 version).
const ring=(current,roots)=>`kekring1;name=p1;current=${current};hwm=${Math.max(...roots.map(([v])=>v))};${roots.map(([v,b])=>{const version=Buffer.alloc(4);version.writeUInt32BE(v);const root=Buffer.alloc(32,b);const kid=Buffer.from(hkdfSync('sha256',root,Buffer.from('kekring/v1/salt'),Buffer.concat([Buffer.from('kekring/v1/kid'),Buffer.from([2]),Buffer.from('p1'),version]),8));return `k${v}=${root.toString('base64url')}.${kid.toString('hex')}`;}).join(';')}`;
let logs='',overflow=false,stateMarker='',code='';
// GitHub is octoemu: `operator` (42) authorizes the sign-in App, which calls back to cutover.test.
const github=await startOctoemu({users:[{id:42,login:'operator'}],apps:[signinAppSeed({clientSecret:'fixture-client-secret',hosts:['cutover.test']})]});
const counts=async()=>{const c=await github.count('POST /login/oauth/access_token','GET /user','DELETE /applications/{client_id}/token');return {exchange:c['POST /login/oauth/access_token'],user:c['GET /user'],revoke:c['DELETE /applications/{client_id}/token']};};
const worker=(name,descriptor)=>({name,modules:true,scriptPath:fileURLToPath(new URL('./worker.mjs',import.meta.url)),modulesRules:[{type:'CompiledWasm',include:['**/*.wasm']}],compatibilityDate:'2024-09-23',compatibilityFlags:['nodejs_compat'],kvNamespaces:{SESSIONS:'cutover-sessions',AUTHZ_CACHE:'cutover-cache'},d1Databases:{DB:'cutover-db'},ratelimits:{RL_IP:{simple:{limit:30,period:60}},RL_USER:{simple:{limit:10,period:60}},AUTHZ_RL:{simple:{limit:10,period:60}}},bindings:{...installedWebApp,P1_HOST:'cutover.test',P1_KEYRING:descriptor,P1_OPERATOR_IDS:'42',RESTATE_INGRESS_URL:'https://ingress.test',RESTATE_INGRESS_TOKEN:'fixture-ingress',GHINVITE_GITHUB_INSTALL_URL:'https://github.com/apps/fixture/installations/new',GH_SIGNIN_CLIENT_ID:'Iv.fixture-signin',GH_SIGNIN_CLIENT_SECRET:'fixture-client-secret',...github.bindings}});
const mf=new Miniflare({log:new Log(LogLevel.ERROR),workers:[worker('old',ring(1,[[1,7]])),worker('current',ring(2,[[1,7],[2,8]]))],handleRuntimeStdio(stdout,stderr){for(const stream of[stdout,stderr])stream.on('data',chunk=>{if(Buffer.byteLength(logs)+chunk.length>2*1024*1024)overflow=true;else logs+=chunk.toString();});}});
try {
 const db=await mf.getD1Database('DB','old');for(const file of readdirSync(new URL('../../migrations/',import.meta.url)).filter(f=>f.endsWith('.sql')).sort())await db.exec(readFileSync(new URL(`../../migrations/${file}`,import.meta.url),'utf8').replace(/^[ \t]*--.*$/gm,'').replaceAll('\n',' '));
 const workers={old:await mf.getWorker('old'),current:await mf.getWorker('current')};
 const call=(name,path,cookie='',body)=>workers[name].fetch(`https://cutover.test${path}`,{method:body?'POST':'GET',redirect:'manual',headers:{cookie,'CF-Connecting-IP':'192.0.2.41',...(body?{'content-type':'application/json'}:{})},body:body?JSON.stringify(body):undefined});
 let response=await call('old','/login');assert.equal(response.status,303,await response.clone().text());const cookie=response.headers.get('set-cookie').split(';')[0],id=cookie.slice(3),url=new URL(response.headers.get('location'));stateMarker=url.searchParams.get('state');
 response=await call('current','/__fixture/pending-signin-replace','',{session_id:id,pending:{state:stateMarker,verifier,issued_at:Math.floor(Date.now()/1000)}});assert.equal(response.status,204);
 // The browser authorizes as operator; the replaced pending record holds this verifier.
 url.searchParams.set('code_challenge',createHash('sha256').update(verifier).digest('base64url'));code=(await github.authorize(url.href,'operator')).searchParams.get('code');
 const key=createHash('sha256').update('p1:pending-signin:v1\0').update(id).digest('hex');const row=()=>db.prepare('SELECT ciphertext,expires_at FROM pending_signins WHERE session_key=?').bind(key).first();const original=await row();
 const path=`/oauth/callback?state=${encodeURIComponent(stateMarker)}&code=${code}`;response=await call('old',path,cookie);assert.equal(response.status,503,await response.clone().text());assert.equal(response.headers.get('set-cookie'),null);assert.deepEqual(await row(),original);assert.deepEqual(await counts(),{exchange:0,user:0,revoke:0});
 response=await call('current',path,cookie);assert.equal(response.status,303,await response.clone().text());const freshCookie=response.headers.get('set-cookie').split(';')[0];assert.notEqual(freshCookie,cookie);assert.equal(await row(),null);assert.deepEqual(await counts(),{exchange:1,user:1,revoke:1});
 response=await call('current',`/__fixture/session-cutover-inspect?${freshCookie.slice(3)}`);assert.equal(response.status,200);const inspected=await response.json();assert.equal(inspected.credential_field,false);assert.equal(inspected.user_id,42);
 response=await call('current','/console/keyring',freshCookie);assert.equal(response.status,200);
 console.log('PASS actual normal callback returns503 on lagging pending generation, preserves exact pending operation, then one exchange/read/revoke succeeds after rollout with a rotated token-free session that authorizes the operator');
 const persisted=[];let records=0;for(const name of['SESSIONS','AUTHZ_CACHE']){const kv=await mf.getKVNamespace(name,'old');const listed=await kv.list();assert.equal(listed.list_complete,true,'bounded complete KV listing');for(const key of listed.keys){records++;persisted.push(await kv.get(key.name));}}
 const tables=(await db.prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' AND name NOT LIKE '_cf_%'").all()).results;for(const {name}of tables){assert.match(name,/^[a-z_]+$/);const rows=(await db.prepare(`SELECT * FROM ${name} LIMIT 5001`).all()).results;records+=rows.length;persisted.push(JSON.stringify(rows));}
 const stored=persisted.join('\n');assert.ok(records<=5000,'bounded application record scan');assert.ok(Buffer.byteLength(stored)<=16*1024*1024,'bounded application byte scan');
 for(const marker of[code,verifier,stateMarker])for(const needle of[marker,Buffer.from(marker).toString('base64'),encodeURIComponent(marker)])assert.ok(!stored.includes(needle));assert.doesNotMatch(stored,GITHUB_TOKEN);
} finally {await mf.dispose();await github.stop();}
assert.equal(overflow,false);for(const marker of[code,verifier,stateMarker])for(const needle of[marker,Buffer.from(marker).toString('base64'),encodeURIComponent(marker)])assert.ok(!logs.includes(needle));assert.doesNotMatch(logs,GITHUB_TOKEN);
console.log('PASS complete bounded logs and all application D1/KV rows contain no code/state/PKCE marker or GitHub token');

registerFinalLogScan(bytes => { for(const marker of[code,verifier,stateMarker])for(const needle of[marker,Buffer.from(marker).toString('base64'),encodeURIComponent(marker)])assert.ok(!bytes.includes(Buffer.from(needle))); assert.doesNotMatch(bytes.toString('latin1'),GITHUB_TOKEN); });
