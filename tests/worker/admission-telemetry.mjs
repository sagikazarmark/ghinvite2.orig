import { registerFinalLogScan } from './runtime-output.mjs';
import assert from 'node:assert/strict';
import { fileURLToPath } from 'node:url';
import { writeFileSync } from 'node:fs';
import { Miniflare, Log, LogLevel, Response } from 'miniflare';
const logs=[];let bytes=0,overflow=false,calls=0;
const mf=new Miniflare({log:new Log(LogLevel.ERROR),modules:true,
 scriptPath:fileURLToPath(new URL('./worker.mjs',import.meta.url)),
 modulesRules:[{type:'CompiledWasm',include:['**/*.wasm']}],
 compatibilityDate:'2024-09-23',compatibilityFlags:['nodejs_compat'],
 bindings:{GHINVITE_RESTATE_INGRESS:'https://telemetry.test'},
 handleRuntimeStdio(stdout,stderr){for(const stream of [stdout,stderr])stream.on('data',chunk=>{bytes+=chunk.length;if(bytes>256*1024)overflow=true;else logs.push(chunk.toString());});},
 async outboundService(request){
  assert.equal(request.method,'POST');assert.equal(new URL(request.url).origin,'https://telemetry.test');
  const body=await request.json();assert.equal(body.justification,'PRIVATE_JUSTIFICATION');
  calls++;
  await new Promise(resolve=>setTimeout(resolve,60));
  if(calls===3)return new Response('PRIVATE_UPSTREAM_ERROR',{status:503});
  return Response.json({decided_at:'2026-10-17T00:00:00Z',result:calls===1?{kind:'accepted',request_id:'01ARZ3NDEKTSV4RRFFQ69G5FAV',state:'pending',decision_deadline:null}:{kind:'rejected',reason:'paused'}});
 },
});
try{
 for(let i=0;i<3;i++){
  const response=await mf.dispatchFetch('https://app.test/__fixture/telemetry-admit');
  assert.equal(response.status,i===2?503:200,await response.text());
 }
}finally{await mf.dispose();}
assert.equal(overflow,false);assert.equal(calls,3);
const text=logs.join('');const events=text.split(/\r?\n/).filter(line=>line.includes('event="admission_rpc_completed"'));
assert.equal(events.length,3,text);
for(const outcome of ['accepted','refused','unknown'])assert.equal(events.filter(line=>line.includes(`outcome="${outcome}"`)).length,1,text);
for(const line of events){assert.ok(line.includes('clock="worker_host"')&&line.includes('clock_valid=true'),line);assert.ok(Number(line.match(/elapsed_ms=(\d+)/)?.[1])>=50,line);}
for(const marker of ['PRIVATE_','01ARZ3NDEKTSV4RRFFQ69G5FAV','01ARZ3NDEKTSV4RRFFQ69G5FAW','91919191','2a2a2a','secret_sha256','/InvitationLink/'])assert.ok(!text.includes(marker),'private marker leaked');
if(process.env.P1_TELEMETRY_LOG)writeFileSync(process.env.P1_TELEMETRY_LOG,text);
console.log('PASS actual Worker admission timing: three HTTP calls, accepted/refused/unknown, finite host clocks, delayed elapsed time, exact ConsoleWriter output and private marker absence');

registerFinalLogScan(bytes => { for(const marker of ['PRIVATE_','01ARZ3NDEKTSV4RRFFQ69G5FAV','01ARZ3NDEKTSV4RRFFQ69G5FAW','91919191','2a2a2a','secret_sha256','/InvitationLink/'])assert.ok(!bytes.includes(Buffer.from(marker))); });
