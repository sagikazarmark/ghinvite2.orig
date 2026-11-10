import {test} from 'node:test';
import assert from 'node:assert/strict';
import {validSql,canonical,createBridge} from '../../native-d1-bridge/bridge.mjs';
test('prepared DML/CTE supports literals but never DDL, ledger writes or second statements',()=>{
 for(const q of ["SELECT ?1, 'CREATE; DROP'",'WITH input AS (SELECT ?1 AS x) INSERT INTO foo SELECT x FROM input RETURNING x','UPDATE foo SET x=?1; -- tail','/* hint */ DELETE FROM foo WHERE x IS NULL'])assert.equal(validSql(q),true,q);
 for(const q of ['DROP TABLE foo','PRAGMA writable_schema=1','SELECT 1; DELETE FROM foo','WITH input AS (SELECT 1) DELETE FROM "d1_migrations"','SELECT * FROM sqlite_schema','SELECT 1 /* incomplete','CREATE TABLE abc(x)',"DELETE FROM 'd1_migrations'","INSERT INTO 'd1_migrations'(name) VALUES('x')","UPDATE 'sqlite_sequence' SET seq=999",'ATTACH DATABASE ? AS x'])assert.equal(validSql(q),false,q);
});
test('manifest comparison is order independent and exact',()=>{
 assert.equal(canonical({a:1,b:[{x:2,y:3}]}),canonical({b:[{y:3,x:2}],a:1}));
 assert.notEqual(canonical({a:1,b:2}),canonical({a:1,b:3}));
});
test('bad auth and wrong identity fail before any D1 access with fixed response',async()=>{
 let accessed=0;const env={D1_BRIDGE_TOKEN:'a'.repeat(64),get DB(){accessed++;throw Error('TOKEN_MARKER');}};
 const bridge=createBridge({protocol:1,database_id:'expected'});
 const send=async(token,authority)=>bridge.fetch(new Request('https://private/v1/d1',{method:'POST',headers:{authorization:'Bearer '+token,'content-type':'application/json'},body:JSON.stringify({protocol:1,authority,batch:[]})}),env);
 assert.equal((await send('b'.repeat(64),{})).status,401);
 const wrong=await send('a'.repeat(64),{protocol:1,database_id:'other'});assert.equal(wrong.status,409);
 assert.equal(accessed,0);assert.equal((await wrong.text()).includes('TOKEN_MARKER'),false);
});
