import {test} from 'node:test';
import assert from 'node:assert/strict';
import {completeChild,scanPrivate} from './cutover-controls.mjs';
const good={alive_before_shutdown:true,closed:true,stdout_eof:true,stderr_eof:true,timed_out:false,bytes:17,code:null,signal:'SIGTERM'};
test('owned shutdown requires live child and fully drained successful close',()=>{
 completeChild(good);
 for(const delta of [{alive_before_shutdown:false},{closed:false},{stdout_eof:false},{stderr_eof:false},{timed_out:true},{bytes:32*1024*1024+1},{signal:'SIGKILL'},{signal:null,code:1}])assert.throws(()=>completeChild({...good,...delta}));
});
test('actual marker scanner checks raw, binary journal, base64 and late split-stream output',()=>{
 const markers=['CAPABILITY_123','TOKEN_456','PEM_789','BRIDGE_123'];
 assert.ok(scanPrivate([{safe:'receipt'}],markers)>0);
 for(const value of ['CAPABILITY_123',Buffer.from('TOKEN_456').toString('base64'),[...Buffer.from('PEM_789')],Buffer.from('BRIDGE_123').toString('hex'),['safe prefix','TOKEN_','456'].join('')])assert.throws(()=>scanPrivate([value],markers));
 assert.throws(()=>scanPrivate(['RT0007'],markers));
});
