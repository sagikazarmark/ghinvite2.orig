import assert from 'node:assert/strict';
export function completeChild(receipt) {
  assert.equal(receipt.alive_before_shutdown,true,'unexpected child exit before owned shutdown');
  assert.equal(receipt.closed,true,'child close was not observed');
  assert.equal(receipt.stdout_eof,true,'stdout did not reach EOF');
  assert.equal(receipt.stderr_eof,true,'stderr did not reach EOF');
  assert.equal(receipt.timed_out,false,'owned shutdown exceeded deadline');
  assert.ok(receipt.bytes<=32*1024*1024,'child output overflow');
  assert.ok(receipt.code===0||receipt.signal==='SIGTERM','unexpected child exit result');
}
export function scanPrivate(values,markers) {
  assert.ok(markers.length>=4,'actual credentials/capabilities must be tracked');
  const texts=[];
  function visit(value){
    if(typeof value==='string'){
      texts.push(value);
      if(value.length>=8&&value.length%4===0&&/^[A-Za-z0-9+/]*={0,2}$/.test(value))texts.push(Buffer.from(value,'base64').toString('utf8'));
      if(value.length>=8&&value.length%2===0&&/^[0-9a-f]+$/i.test(value))texts.push(Buffer.from(value,'hex').toString('utf8'));
    }else if(Array.isArray(value)){
      if(value.length&&value.every(v=>Number.isInteger(v)&&v>=0&&v<=255))texts.push(Buffer.from(value).toString('utf8'));
      else value.forEach(visit);
    }else if(value&&typeof value==='object')Object.values(value).forEach(visit);
  }
  values.forEach(visit);
  for(const text of texts){
    for(const marker of markers){assert.ok(marker.length>0);assert.ok(!text.includes(marker),'actual private marker disclosure');}
    assert.ok(!/Workers runtime canceled.*hung|RT0007/.test(text),'runtime incomplete');
  }
  return texts.length;
}
