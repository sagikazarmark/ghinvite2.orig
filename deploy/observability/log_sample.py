#!/usr/bin/env python3
"""Select bounded safe fields from complete native/Worker tracing text exports.

The caller must establish export completeness; a file alone cannot prove it.
Never feed untrusted request bodies to this parser as if they were host logs.
"""
import argparse
import json
from pathlib import Path
import re
import time
from collector import MAX_BYTES, MAX_RECORDS, logs, write_atomic

FIELDS = {
 'admission_rpc_completed': ('event','outcome','clock','clock_valid','elapsed_ms','occurred_at'),
 'installation_mint_attempt': ('event','app','installation_id','cause','occurred_at'),
 'installation_api_call': ('event','app','installation_id','occurred_at'),
 'webhook_signature_failure': ('event','app','reason','occurred_at'),
 'edge_binding_rejection': ('event','bucket','reason','occurred_at'),
}
TOKEN = re.compile(r'(?<!\S)([a-z_]+)=("[^"\r\n]*"|true|false|[0-9]+)(?=\s|$)')
ANSI = re.compile(r'\x1b\[[0-9;]*m')

def select(raw, start, end, complete, now):
 if len(raw) > MAX_BYTES: raise ValueError('log export overflow')
 records=[]
 for line in raw.decode('utf-8',errors='strict').splitlines():
  line=ANSI.sub('',line)
  tokens=TOKEN.findall(line)
  events=[value for key,value in tokens if key=='event']
  if not events: continue
  if len(events)!=1: raise ValueError('ambiguous event')
  event=json.loads(events[0])
  if event not in FIELDS: continue
  fields={}
  for key,value in tokens:
   if key in FIELDS[event]:
    if key in fields: raise ValueError('duplicate event field')
    fields[key]=json.loads(value)
  records.append(fields)
  if len(records)>MAX_RECORDS: raise ValueError('log record overflow')
 sample={'window_start':start,'window_end':end,'complete':complete,'records':records}
 logs(sample,now) # Apply the same strict enum, freshness and series boundary.
 return sample

def main():
 parser=argparse.ArgumentParser(description=__doc__)
 parser.add_argument('--input',type=Path,required=True)
 parser.add_argument('--output',type=Path,required=True)
 parser.add_argument('--window-start',type=int,required=True)
 parser.add_argument('--window-end',type=int,required=True)
 parser.add_argument('--complete',action='store_true',help='Attest that the trusted host export covers this entire window without truncation')
 args=parser.parse_args()
 try:
  with args.input.open('rb') as f: raw=f.read(MAX_BYTES+1)
  sample=select(raw,args.window_start,args.window_end,args.complete,int(time.time()))
 except (OSError,ValueError,TypeError,KeyError):
  sample={'window_start':args.window_start,'window_end':args.window_end,'complete':False,'records':[]}
 write_atomic(args.output,json.dumps(sample,separators=(',',':'))+'\n')
if __name__=='__main__': main()
