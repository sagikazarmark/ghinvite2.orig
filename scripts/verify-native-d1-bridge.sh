#!/usr/bin/env bash
set -euo pipefail
: "${EVIDENCE_ROOT:?set a new evidence directory}"
test ! -e "$EVIDENCE_ROOT"
mkdir -p "$EVIDENCE_ROOT"
EVIDENCE_ROOT="$(cd "$EVIDENCE_ROOT" && pwd)"
export EVIDENCE_ROOT
node --test tests/native-d1-bridge/bridge.test.mjs tests/native-d1-bridge/cutover-controls.test.mjs > "$EVIDENCE_ROOT/js-controls.log" 2>&1
python3 scripts/test-native-fallback.py > "$EVIDENCE_ROOT/packaging-controls.log" 2>&1
cargo test --locked -p ghinvite-storage-d1 --features bridge-test --test native_bridge --no-run --message-format=json > "$EVIDENCE_ROOT/build.jsonl" 2> "$EVIDENCE_ROOT/build.log"
python3 - "$EVIDENCE_ROOT" <<'PY'
import json,pathlib,sys
root=pathlib.Path(sys.argv[1]);found=[]
for line in (root/'build.jsonl').read_text().splitlines():
 r=json.loads(line)
 if r.get('reason')=='compiler-artifact' and r.get('target',{}).get('name')=='native_bridge' and r.get('executable'):
  assert r['profile']['test'] and r['target']['kind']==['test'];found.append(r['executable'])
assert len(found)==1, 'expected exactly one actual Cargo test executable'
(root/'executable').write_text(found[0])
PY
node tests/native-d1-bridge/runtime.mjs "$(cat "$EVIDENCE_ROOT/executable")" > "$EVIDENCE_ROOT/runtime.log" 2>&1
cargo test --locked -p ghinvite-workflows-server --test startup --test bridge_startup > "$EVIDENCE_ROOT/startup.log" 2>&1
