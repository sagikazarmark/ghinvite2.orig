import { registerFinalLogScan } from './runtime-output.mjs';
import assert from 'node:assert/strict';
import { createHash, hkdfSync, randomBytes } from 'node:crypto';
import { readFileSync, readdirSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { Miniflare, Log, LogLevel } from 'miniflare';
import { GITHUB_TOKEN, signinAppSeed, startOctoemu } from './octoemu.mjs';

const STATE = 'STATE_MARKER_pending_signin_'.padEnd(43,'x');
const VERIFIER = 'PKCE_MARKER_pending_signin_0123456789012345678901234567890123456789';
const DB_DETAIL = 'DATABASE_PRIVATE_MARKER_pending_signin';
// kekring v1 descriptor (libs/kekring/docs/kekring-format.md).
const ring = (current, roots) => `kekring1;name=p1;current=${current};hwm=${Math.max(...roots.map(([generation]) => generation))};${roots.map(([generation, byte]) => {
  const root = Buffer.alloc(32, byte), version = Buffer.alloc(4); version.writeUInt32BE(generation);
  const info = Buffer.concat([Buffer.from('kekring/v1/kid'), Buffer.from([2]), Buffer.from('p1'), version]);
  const kid = Buffer.from(hkdfSync('sha256', root, Buffer.from('kekring/v1/salt'), info, 8));
  return `k${generation}=${root.toString('base64url')}.${kid.toString('hex')}`;
}).join(';')}`;
const session = () => randomBytes(16).toString('base64url');
const lookup = id => createHash('sha256').update('p1:pending-signin:v1\0').update(id).digest('hex');
const now = () => Math.floor(Date.now()/1000);
const issued = () => ({state:STATE, verifier:VERIFIER, issued_at:now()});
// GitHub is octoemu: the sign-in App calls back to the fixture's
// signin.test, and `verified` (9) is the user who authorizes.
const github = await startOctoemu({
  users: [{ id: 9, login: 'verified' }],
  apps: [signinAppSeed({ clientSecret: 'fixture-client-secret', hosts: ['signin.test'] })],
});
// Callbacks race on the D1 take; a slower exchange widens the window.
await github.fault({ method: 'POST', path: '/login/oauth/access_token', action: { kind: 'delay', ms: 20, clock: 'real' } });
const codes = [];
// The browser's part: authorize with the pending PKCE challenge, as `verified`.
const authorized = async () => {
  const challenge = createHash('sha256').update(VERIFIER).digest('base64url');
  const authorize = new URL(`${github.webBase}/login/oauth/authorize`);
  authorize.search = new URLSearchParams({ client_id: 'Iv.fixture-signin', redirect_uri: 'https://signin.test/oauth/callback',
    state: STATE, code_challenge: challenge, code_challenge_method: 'S256' }).toString();
  const back = await github.authorize(authorize.href, 'verified');
  assert.equal(back.searchParams.get('state'), STATE);
  codes.push(back.searchParams.get('code'));
  return back.searchParams.get('code');
};
const counters = async () => {
  const counts = await github.count('POST /login/oauth/access_token', 'GET /user', 'DELETE /applications/{client_id}/token');
  return { exchange: counts['POST /login/oauth/access_token'], user: counts['GET /user'], revoke: counts['DELETE /applications/{client_id}/token'] };
};
let logs = '', overflow = false;
const worker = (name, descriptor) => ({name, modules:true,
  scriptPath:fileURLToPath(new URL('./worker.mjs',import.meta.url)),
  modulesRules:[{type:'CompiledWasm',include:['**/*.wasm']}],
  compatibilityDate:'2024-09-23',compatibilityFlags:['nodejs_compat'],
  d1Databases:{DB:'pending-signin-shared'},bindings:{P1_KEYRING:descriptor,...github.bindings},
});
const mf = new Miniflare({log:new Log(LogLevel.ERROR),
  handleRuntimeStdio(stdout,stderr) {
    for (const stream of [stdout,stderr]) stream.on('data',chunk=> {
      if(Buffer.byteLength(logs)+chunk.length>1024*1024) overflow=true; else logs+=chunk.toString();
    });
  },
  workers:[worker('a',ring(1,[[1,7]])),worker('b',ring(1,[[1,7]])),worker('newer',ring(2,[[1,7],[2,8]])),worker('wrong',ring(1,[[1,9]]))],
});
try {
  const db = await mf.getD1Database('DB','a');
  for(const file of readdirSync(new URL('../../migrations/',import.meta.url)).filter(f=>f.endsWith('.sql')).sort()) {
    const sql=readFileSync(new URL(`../../migrations/${file}`,import.meta.url),'utf8');
    await db.exec(sql.replace(/^[ \t]*--.*$/gm,'').replaceAll('\n',' '));
  }
  const fetchers = Object.fromEntries(await Promise.all(['a','b','newer','wrong'].map(async name=>[name,await mf.getWorker(name)])));
  const call = async (operation,input,expected=200,name='a') => {
    const response = await fetchers[name].fetch(`http://fixture.test/__fixture/pending-signin-${operation}`,{method:'POST',headers:{'content-type':'application/json'},body:JSON.stringify(input)});
    const body=await response.text();
    assert.equal(response.status,expected,`pending ${operation}: ${response.status} ${body}`);
    return expected === 200 && body ? JSON.parse(body) : null;
  };
  const put = async(id,name='a',pending=issued())=>call('replace',{session_id:id,pending},204,name);
  const row = id=>db.prepare('SELECT ciphertext,expires_at FROM pending_signins WHERE session_key=?1').bind(lookup(id)).first();
  const callback = async (id,extra={},name='a',status=200)=>call('callback',{session_id:id,state:STATE,code:await authorized(),...extra},status,name);
  let id=session(); await put(id);
  const raw=await db.prepare('SELECT * FROM pending_signins').all();
  for(const marker of [STATE,VERIFIER,id]) assert.ok(!JSON.stringify(raw).includes(marker));
  const encrypted=await row(id);
  for(const marker of [STATE,VERIFIER]) assert.ok(!Buffer.from(encrypted.ciphertext,'base64').includes(Buffer.from(marker)));
  const code=await authorized();
  const concurrent=await Promise.all(['a','b'].map(async name=>{
    const response=await fetchers[name].fetch('http://fixture.test/__fixture/pending-signin-callback',{method:'POST',headers:{'content-type':'application/json'},body:JSON.stringify({session_id:id,state:STATE,code})});
    return {status:response.status,body:await response.json()};
  }));
  assert.deepEqual(concurrent.map(v=>v.status).sort(),[200,409],JSON.stringify(concurrent));
  assert.deepEqual(concurrent.find(v=>v.status===200).body,{identity:{github_user_id:9,login:'verified'}});
  assert.deepEqual(await counters(),{exchange:1,user:1,revoke:1}); assert.equal(await row(id),null);
  console.log('PASS actual D1 concurrent callbacks across two Worker isolates: one exchange, identity read and revocation');

  id=session(); await put(id); await callback(id,{lose_take_ack:true},'a',503);
  assert.equal(await row(id),null); await callback(id,{},'b',409);
  assert.deepEqual(await counters(),{exchange:1,user:1,revoke:1});
  await put(id);
  await db.exec(`CREATE TRIGGER reject_pending_delete BEFORE DELETE ON pending_signins BEGIN SELECT RAISE(ABORT,'${DB_DETAIL}'); END;`);
  await callback(id,{},'a',503); assert.ok(await row(id));
  assert.deepEqual(await counters(),{exchange:1,user:1,revoke:1});
  await db.exec('DROP TRIGGER reject_pending_delete;'); await callback(id);
  console.log('PASS actual D1 failed delete and lost committed acknowledgement cannot release callback material');

  id=session(); await put(id,'newer'); const before=await row(id);
  await call('take',{session_id:id},503,'a'); assert.deepEqual(await row(id),before);
  assert.deepEqual(await call('take',{session_id:id},200,'newer'),{found:true});
  await put(id); const beforeWrong=await row(id);
  await call('take',{session_id:id},503,'wrong'); assert.deepEqual(await row(id),beforeWrong);
  assert.deepEqual(await call('take',{session_id:id}),{found:true});
  console.log('PASS actual Worker lagging generation (VersionAboveHighWaterMark) and wrong-root key id (KeyIdMismatch) preserve the row for a correct isolate');

  id=session(); await put(id); const old=await row(id); await put(id); const replacement=await row(id);
  assert.notEqual(old.ciphertext,replacement.ciphertext);
  await db.prepare('UPDATE pending_signins SET ciphertext=?1,expires_at=?2 WHERE session_key=?3').bind(old.ciphertext,old.expires_at,lookup(id)).run();
  assert.deepEqual(await call('take',{session_id:id,replace_before_consume:replacement}),{found:false});
  assert.deepEqual(await row(id),replacement);
  assert.deepEqual(await call('take',{session_id:id}),{found:true});
  console.log('PASS actual D1 exact-record consume preserves a concurrently replaced encrypted record');

  id=session(); const pending=issued(); await put(id,'a',pending);
  assert.deepEqual(await call('take',{session_id:id,now:pending.issued_at+1800}),{found:false}); assert.equal(await row(id),null);
  await put(id); await db.prepare('UPDATE pending_signins SET expires_at=expires_at+1 WHERE session_key=?1').bind(lookup(id)).run();
  await call('take',{session_id:id},503); assert.ok(await row(id));
  await put(id); const other=session();
  await db.prepare('INSERT INTO pending_signins SELECT ?1,ciphertext,expires_at FROM pending_signins WHERE session_key=?2').bind(lookup(other),lookup(id)).run();
  await call('take',{session_id:other},503); assert.ok(await row(other));
  assert.deepEqual(await call('take',{session_id:id}),{found:true});
  await call('purpose',{session_id:id},204);
  console.log('PASS actual Worker expiry, authenticated expiry metadata, cross-session and cross-purpose envelope isolation');

  const rows=await db.prepare('SELECT * FROM pending_signins').all();
  assert.equal(overflow,false,'complete runtime logs exceeded cap');
  for(const marker of [STATE,VERIFIER,DB_DETAIL,...codes]) for(const needle of [marker,Buffer.from(marker).toString('base64'),encodeURIComponent(marker)]) {
    assert.ok(!JSON.stringify(rows).includes(needle),'D1 plaintext marker');
    assert.ok(!logs.includes(needle),'runtime log plaintext marker');
  }
  assert.doesNotMatch(JSON.stringify(rows),GITHUB_TOKEN,'D1 GitHub token');
  assert.doesNotMatch(logs,GITHUB_TOKEN,'runtime log GitHub token');
  // A wrong root is a caller-class DecryptFailed with a stable code, never a configuration alert.
  assert.match(logs,/KeyIdMismatch/,'wrong-root key id code logged');
  assert.match(logs,/VersionAboveHighWaterMark/,'lagging-reader code logged');
  assert.match(logs,/fingerprint/,'nonsecret keyring fingerprint emitted');
  console.log('PASS pending-signin raw D1 rows and complete bounded runtime logs contain no state, PKCE, code, token or private DB error markers (codes, state, PKCE and every GitHub token shape)');
} finally { await mf.dispose(); await github.stop(); }

registerFinalLogScan(bytes => {
  for(const marker of [STATE,VERIFIER,DB_DETAIL,...codes])for(const needle of[marker,Buffer.from(marker).toString('base64'),encodeURIComponent(marker)])assert.ok(!bytes.includes(Buffer.from(needle)));
  assert.doesNotMatch(bytes.toString('latin1'),GITHUB_TOKEN);
});
