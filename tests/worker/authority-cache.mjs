import assert from 'node:assert/strict';
import {fileURLToPath} from 'node:url';
import {Miniflare,Log,LogLevel} from 'miniflare';
const options={log:new Log(LogLevel.ERROR),modules:true,
  scriptPath:fileURLToPath(new URL('./worker.mjs',import.meta.url)),
  modulesRules:[{type:'CompiledWasm',include:['**/*.wasm']}],
  compatibilityDate:'2024-09-23',compatibilityFlags:['nodejs_compat'],
};
const get=(mf,key='authz:repo:1:2:3')=>mf.dispatchFetch(`http://worker.test/__fixture/authority-cache?${key}`);
const missing=new Miniflare(options);
try {assert.equal((await get(missing)).status,503);}finally{await missing.dispose();}
const mf=new Miniflare({...options,kvNamespaces:['AUTHZ_CACHE']});
try {
  const kv=await mf.getKVNamespace('AUTHZ_CACHE');
  assert.equal((await mf.dispatchFetch('http://worker.test/__fixture/authority-cache-write')).status,200);
  assert.equal((await (await get(mf)).json()).value.allowed,true);
  for(const key of ['authz:org:1:2:3','authz:repo:2:2:3','authz:repo:1:3:3','authz:repo:1:2:4']) assert.equal(await(await get(mf,key)).json(),null);
  const now=Math.floor(Date.now()/1000);
  for(const [key,at] of [['authz:repo:1:2:stale',now-300],['authz:repo:1:2:future',now+120]]) {
    await kv.put(key,JSON.stringify({observed_at:at,value:{allowed:true}}),{expirationTtl:600});
    assert.equal(await(await get(mf,key)).json(),null,'KV retaining evidence cannot extend logical authority');
  }
  assert.equal((await get(mf,'invalid')).status,503);
  console.log('PASS actual Worker authority KV: binding required, persisted read, identity/app partitions, stale/future refusal independent of TTL');
}finally{await mf.dispose();}
