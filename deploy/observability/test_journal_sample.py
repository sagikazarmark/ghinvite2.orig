import json
import threading
import time
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from journal_sample import query
from log_sample import select

class SampleTests(unittest.TestCase):
 def server(self, status=200, body=None, delay=0):
  class Handler(BaseHTTPRequestHandler):
   def log_message(self,*args): pass
   def do_POST(self):
    self.server.request=json.loads(self.rfile.read(int(self.headers['Content-Length'])))
    time.sleep(delay)
    self.send_response(status); self.end_headers()
    try:self.wfile.write(json.dumps(body if body is not None else {'rows':[{'entries':5}]}).encode())
    except (BrokenPipeError,ConnectionResetError):pass
  server=ThreadingHTTPServer(('127.0.0.1',0),Handler)
  thread=threading.Thread(target=server.serve_forever,daemon=True);thread.start()
  self.addCleanup(server.server_close); self.addCleanup(server.shutdown)
  return server,'http://127.0.0.1:'+str(server.server_port)
 def test_real_http_count_only_and_exact_query(self):
  server,url=self.server(); sample=query(url,'PRIVATE_ADMIN_TOKEN')
  self.assertEqual(sample['rows'],[{'entries':5}])
  self.assertNotIn('PRIVATE',json.dumps(sample))
  self.assertNotIn('entry_json',server.request['query'])
  self.assertIn("target_handler_name = 'admit'",server.request['query'])
 def test_real_http_deadline_and_redirect_refusal(self):
  _,url=self.server(delay=.3)
  started=time.monotonic()
  with self.assertRaises((OSError,ValueError)):query(url,timeout=.05)
  self.assertLess(time.monotonic()-started,.25)
  _,url=self.server(status=302)
  with self.assertRaises(ValueError):query(url,'PRIVATE_ADMIN_TOKEN')
 def test_overflow_or_identity_fields_refused(self):
  for rows in ([{'entries':1}]*101,[{'entries':5,'id':'PRIVATE'}]):
   _,url=self.server(body={'rows':rows})
   with self.assertRaises(ValueError):query(url)
 def test_remote_cleartext_and_credential_origin_refused(self):
  for url in ('http://example.com','https://user:secret@example.com','https://example.com/path'):
   with self.assertRaises(ValueError):query(url)
 def test_actual_fmt_selection_drops_nonmetric_fields(self):
  raw=b'INFO ghinvite_web: event="admission_rpc_completed" outcome="accepted" clock="native_monotonic" clock_valid=true elapsed_ms=51 occurred_at=100 PRIVATE="secret"\nINFO other: secret="hidden"\n'
  selected=select(raw,90,100,True,100)
  self.assertEqual(selected['records'][0]['elapsed_ms'],51)
  self.assertNotIn('secret',json.dumps(selected))
  with self.assertRaises(ValueError):select(raw,90,100,False,100)
  with self.assertRaises(ValueError):select(raw.replace(b'elapsed_ms=51',b'elapsed_ms=51 elapsed_ms=52'),90,100,True,100)
if __name__=='__main__':unittest.main()
