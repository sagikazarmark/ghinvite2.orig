import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import {fileURLToPath} from 'node:url';
import {Miniflare,Log,LogLevel} from 'miniflare';
import {identitySigner} from './restate-identity.mjs';

for (const [binding,status] of [[undefined,200],['',500],['RUN_FIXTURE_SECRET_MARKER',500],['three-losses-v1',200]]) {
  const signer=identitySigner();
  const mf=new Miniflare({
    modules:true,log:new Log(LogLevel.ERROR),
    scriptPath:fileURLToPath(new URL('./workflows-worker.mjs',import.meta.url)),
    modulesRules:[{type:'CompiledWasm',include:['**/*.wasm']}],
    compatibilityDate:'2024-09-23',compatibilityFlags:['nodejs_compat'],d1Databases:['DB'],
    bindings:{RESTATE_IDENTITY_KEYS:signer.key,P1_HOST:'run-fixture.test',GH_REPO_APP_ID:'123',GH_REPO_CLIENT_ID:'Iv.fixture-repo',
      GH_REPO_PRIVATE_KEYS:`fixture:${readFileSync(new URL('../../test-fixtures/github-app-rsa.pem',import.meta.url)).toString('base64')}`,
      ...(binding===undefined?{}:{P1_DELIVERY_RUN_RETRY_FIXTURE:binding})},
    outboundService(){throw new Error('startup must perform no outbound request');},
  });
  try {
    const response=await mf.dispatchFetch('http://worker.test/discover',{headers:{accept:'application/vnd.restate.endpointmanifest.v3+json',...signer.headers('/discover'),'x-test-delivery-run-retry-fixture':'three-losses-v1'}});
    assert.equal(response.status,status);
    assert.ok(!(await response.text()).includes('RUN_FIXTURE_SECRET_MARKER'));
    if(status===200){
      const losses=await mf.dispatchFetch('http://worker.test/__fixture/delivery-run-losses',{method:'POST',body:'null'});
      assert.equal(await losses.json(),0,'startup does not consume failures; only a confirmed guarded result can');
    }
  }finally{await mf.dispose();}
}
console.log('PASS actual Worker guarded-run fixture startup: optional absent binding, invalid binding refusal, exact preset activation, zero losses before guarded writes');
