#!/usr/bin/env python3
"""Bind reviewed existing database identity/schema, migration source, and Wrangler config.
No Cloudflare calls, database writes, ledger adoption, or DDL execution.
"""
import argparse,hashlib,json,pathlib,re,uuid,shutil
p=argparse.ArgumentParser();p.add_argument('--database-id',required=True);p.add_argument('--schema-sha256',required=True);p.add_argument('--migrations',type=pathlib.Path,required=True);p.add_argument('--output',type=pathlib.Path,required=True);a=p.parse_args()
assert str(uuid.UUID(a.database_id))==a.database_id
assert re.fullmatch('[0-9a-f]{64}',a.schema_sha256)
files=sorted(a.migrations.glob('*.sql'));assert files
manifest={'protocol':1,'database_id':a.database_id,'migrations':[{'name':f.name,'sha256':hashlib.sha256(f.read_bytes()).hexdigest()} for f in files],'schema_sha256':a.schema_sha256}
sources=[pathlib.Path(__file__).resolve().parent/name for name in ('worker.mjs','bridge.mjs')]
assert all(f.is_file() and not f.is_symlink() for f in sources), 'missing or symlinked bridge source'
a.output.mkdir(parents=True,exist_ok=False)
for f in sources: shutil.copy2(f,a.output/f.name)
(a.output/'authority.json').write_text(json.dumps(manifest,separators=(',',':'))+'\n')
(a.output/'wrangler.json').write_text(json.dumps({'name':'ghinvite-private-d1-bridge','main':'worker.mjs','compatibility_date':'2026-10-01','workers_dev':False,'d1_databases':[{'binding':'DB','database_name':'ghinvite','database_id':a.database_id}],'observability':{'enabled':False}},indent=2)+'\n')
