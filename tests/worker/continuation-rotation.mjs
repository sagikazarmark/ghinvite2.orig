import { registerFinalLogScan } from './runtime-output.mjs';
import assert from 'node:assert/strict';
import { readFileSync, readdirSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { Miniflare, Log, LogLevel } from 'miniflare';
const markers=['ROTATION_RUNTIME_PRIVATE_MARKER','ROTATION_RUNTIME_BAD_CIPHERTEXT'];
registerFinalLogScan(bytes=>{for(const m of markers)assert.ok(!bytes.includes(Buffer.from(m)));});
let outbound=0;
const mf=new Miniflare({log:new Log(LogLevel.ERROR),modules:true,
 scriptPath:fileURLToPath(new URL('./worker.mjs',import.meta.url)),
 modulesRules:[{type:'CompiledWasm',include:['**/*.wasm'],fallthrough:true}],
 compatibilityDate:'2026-03-02',compatibilityFlags:['nodejs_compat'],
 d1Databases:{DB:'rotation-db'},kvNamespaces:['SESSIONS'],
 bindings:{},
 outboundService(){outbound++;return new Response('unexpected outbound',{status:500});},
});
try {
 const db=await mf.getD1Database('DB');
 for(const file of readdirSync(new URL('../../migrations/',import.meta.url)).filter(f=>f.endsWith('.sql')).sort())await db.exec(readFileSync(new URL('../../migrations/'+file,import.meta.url),'utf8').replace(/^[ \t]*--.*$/gm,'').replaceAll('\n',' '));
 const response=await mf.dispatchFetch('http://fixture.test/__fixture/continuation-rotation',{method:'POST'});
 assert.equal(response.status,200,await response.clone().text());assert.deepEqual(await response.json(),{ok:true});
 const rows=(await db.prepare('SELECT scope,id,binding,payload,expires_at FROM attempt_continuations ORDER BY rowid').all()).results;
 assert.equal(rows.length,6);assert.ok(rows.every(r=>r.expires_at===100));
 assert.ok(rows.every(r=>!r.payload.includes(markers[0])));
 assert.equal(rows.filter(r=>r.payload===markers[1]).length,1,'bad row is retained, not laundered or deleted');
 const tables=(await db.prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' AND name NOT LIKE '_cf_%'").all()).results;
 for(const {name} of tables){assert.match(name,/^[A-Za-z0-9_]+$/);const raw=JSON.stringify((await db.prepare(`SELECT * FROM ${name}`).all()).results);assert.ok(raw.length<4*1024*1024);assert.ok(!raw.includes(markers[0]));}
 // Overflow must roll back the original continuation write and its epoch.
 await db.prepare('UPDATE continuation_rotation_epoch SET revision=9007199254740991 WHERE singleton=1').run();
 await assert.rejects(db.prepare("UPDATE attempt_continuations SET payload='overflow-write' WHERE id='runtime-0'").run());
 assert.deepEqual((await db.prepare('SELECT scope,id,binding,payload,expires_at FROM attempt_continuations ORDER BY rowid').all()).results,rows);
 assert.equal((await db.prepare('SELECT revision FROM continuation_rotation_epoch').first()).revision,9007199254740991);
 console.log('PASS actual D1 complete expired-row rewrap/verify/remove, stale progress/CAS refusal, bad-row preservation, epoch-overflow rollback and all-table plaintext scan');
} finally {await mf.dispose();}
assert.equal(outbound,0);
