// Private SQL transport only. No migration execution, result logging, or retries.
const LIMIT = 32 * 1024 * 1024;
const encoder = new TextEncoder();
export const schemaQuery = "SELECT type,name,tbl_name,sql FROM sqlite_schema WHERE name NOT LIKE 'sqlite_%' AND name NOT LIKE '_cf_%' ORDER BY name COLLATE BINARY";
export async function sha256(value) {
  return Array.from(new Uint8Array(await crypto.subtle.digest('SHA-256', encoder.encode(value))), b => b.toString(16).padStart(2,'0')).join('');
}
async function authorized(header, token) {
  if (!/^[0-9a-f]{64}$/.test(token ?? '') || !/^Bearer [0-9a-f]{64}$/.test(header ?? '')) return false;
  const key = value => crypto.subtle.importKey('raw',encoder.encode(value),{name:'HMAC',hash:'SHA-256'},false,['sign','verify']);
  const challenge=encoder.encode('ghinvite/private-d1/v1');
  const signature=await crypto.subtle.sign('HMAC',await key(token),challenge);
  return crypto.subtle.verify('HMAC',await key(header.slice(7)),signature,challenge);
}
export function validSql(sql) {
  if (typeof sql !== 'string' || sql.length===0 || sql.length>262144) return false;
  // Tokenize SQL, excluding literals/comments: permits existing prepared DML
  // while excluding maintenance/transaction commands and multiple statements.
  const tokens=[]; let i=0,ended=false;
  while(i<sql.length) {
    const c=sql[i];
    if (/\s/.test(c)) { i++; continue; }
    if (sql.startsWith('--',i)) {const n=sql.indexOf('\n',i);i=n<0?sql.length:n+1;continue;}
    if (sql.startsWith('/*',i)) {const n=sql.indexOf('*/',i+2);if(n<0)return false;i=n+2;continue;}
    if(ended)return false;
    if(c===';'){ended=true;i++;continue;}
    if(c==="'" || c==='"' || c==='`' || c==='[') {
      const close=c==='['?']':c; let v='';i++;let done=false;
      while(i<sql.length){if(sql[i]===close){if(sql[i+1]===close){v+=close;i+=2;continue;}i++;done=true;break;}v+=sql[i++];}
      if(!done)return false;
      // SQLite permits single-quoted identifiers in table-name positions.
      // Conservatively reject protected names even when quoted as literals;
      // ordinary values such as 'CREATE; DROP' remain harmless.
      if(c!=="'" || /^(D1_MIGRATIONS|SQLITE_.*|_CF_.*)$/i.test(v))tokens.push(v.toUpperCase());
      continue;
    }
    if(/[A-Za-z_]/.test(c)){let v='';while(i<sql.length&&/[A-Za-z_0-9]/.test(sql[i]))v+=sql[i++];tokens.push(v.toUpperCase());}
    else i++;
  }
  if(!['SELECT','INSERT','UPDATE','DELETE','WITH'].includes(tokens[0]))return false;
  const forbidden=new Set(['CREATE','DROP','ALTER','ATTACH','DETACH','PRAGMA','BEGIN','COMMIT','ROLLBACK','SAVEPOINT','RELEASE','VACUUM','REINDEX','ANALYZE','D1_MIGRATIONS']);
  return !tokens.some(t=>forbidden.has(t)||t.startsWith('SQLITE_')||t.startsWith('_CF_'));
}
async function boundedJson(request) {
  if(request.headers.get('content-type')!=='application/json')throw Error('invalid');
  const reader=request.body?.getReader();if(!reader)throw Error('invalid');
  const chunks=[];let n=0;
  try {for(;;){const r=await reader.read();if(r.done)break;n+=r.value.length;if(n>LIMIT)throw Error('invalid');chunks.push(r.value);}}
  finally {await reader.cancel();}
  const bytes=new Uint8Array(n);let at=0;for(const c of chunks){bytes.set(c,at);at+=c.length;}
  return JSON.parse(new TextDecoder('utf-8',{fatal:true}).decode(bytes));
}
export function canonical(value) {
  if(Array.isArray(value))return '['+value.map(canonical).join(',')+']';
  if(value!==null&&typeof value==='object')return '{'+Object.keys(value).sort().map(k=>JSON.stringify(k)+':'+canonical(value[k])).join(',')+'}';
  return JSON.stringify(value);
}
export function createBridge(manifest) {
  const serialized=canonical(manifest);
  return {async fetch(request,env) {
    const fail=(status)=>new Response('D1 bridge unavailable',{status,headers:{'cache-control':'no-store'}});
    if(new URL(request.url).pathname!=='/v1/d1' || request.method!=='POST')return fail(404);
    if(!await authorized(request.headers.get('authorization'),env.D1_BRIDGE_TOKEN))return fail(401);
    let body;
    try {body=await boundedJson(request);}catch{return fail(400);}
    if(body?.protocol!==1 || canonical(body.authority)!==serialized || !Array.isArray(body.batch) || body.batch.length>100)return fail(409);
    if(body.batch.some(q=>!validSql(q.sql)||!Array.isArray(q.params)||q.params.some(p=>!(p===null||typeof p==='string'||Number.isSafeInteger(p)))))return fail(400);
    // Authenticate exact current ledger/schema without adopting, creating, or
    // changing anything. These names already belong to Wrangler's ledger.
    try {
      const ledger=await env.DB.withSession('first-primary').prepare('SELECT name FROM d1_migrations ORDER BY name COLLATE BINARY').all();
      if(JSON.stringify(ledger.results.map(r=>r.name))!==JSON.stringify(manifest.migrations.map(m=>m.name)))return fail(409);
      const schema=await env.DB.withSession('first-primary').prepare(schemaQuery).all();
      if(await sha256(JSON.stringify(schema.results))!==manifest.schema_sha256)return fail(409);
    }catch{return fail(503);}
    try {
      // A NEW first-primary session for the actual batch. The schema read must
      // not consume this query's FirstPrimary constraint.
      const session=env.DB.withSession('first-primary');
      const results=body.batch.length?await session.batch(body.batch.map(q=>session.prepare(q.sql).bind(...q.params))):[];
      const value={protocol:1,authority:manifest,results:results.map(r=>({success:r.success,results:r.results??[],meta:{changes:r.meta?.changes}})),error:null};
      const encoded=JSON.stringify(value);
      if(encoder.encode(encoded).length>LIMIT)return fail(503); // committed but ACK unavailable
      return new Response(encoded,{headers:{'content-type':'application/json','cache-control':'no-store'}});
    }catch(error){
      // Preserve only fixed classification vocabulary; upstream text may contain
      // SQL/parameters and is never returned or logged.
      const message=String(error?.message??'');
      const code=message.includes('projection_dependency')?'dependency':
        message.includes('projection_changed')?'changed':
        ['projection_invariant','audit_events.account_id','delivery outcome conflict','delivery_legacy_invitation_identity','foreign tracking stop conflict','settled invitation writer conflict'].some(x=>message.includes(x))?'invariant':
        message.includes('UNIQUE constraint failed')?(message.includes('installations.account_id')?'unique_account':'unique'):
        message.includes('FOREIGN KEY constraint failed')?'foreign_key':'unavailable';
      return Response.json({protocol:1,authority:manifest,results:[],error:code},{headers:{'cache-control':'no-store'}});
    }
  }};
}
