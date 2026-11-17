#!/usr/bin/env bash
# Owned local rehearsal only; never registers an endpoint with a remote cluster.
set -euo pipefail
: "${EVIDENCE_ROOT:?set a new evidence directory}"
: "${RESTATE_SERVER_BIN:?set the pinned Restate 1.8.1 executable}"
test ! -e "$EVIDENCE_ROOT"
mkdir -p "$EVIDENCE_ROOT"
EVIDENCE_ROOT="$(cd "$EVIDENCE_ROOT" && pwd)"
export EVIDENCE_ROOT
printf '%s  %s\n' cb4c0890a347d3193d9ab5e04e37c83d555ebdb2d42ff24e5f447e4dcaeaeb0c "$RESTATE_SERVER_BIN" | sha256sum --check > "$EVIDENCE_ROOT/restate-sha.log"
"$RESTATE_SERVER_BIN" --version > "$EVIDENCE_ROOT/restate-version.log"
node --version > "$EVIDENCE_ROOT/node-version.log"
rustc -Vv > "$EVIDENCE_ROOT/rustc-version.log"
node --test tests/native-d1-bridge/cutover-controls.test.mjs tests/native-d1-bridge/runtime-output.test.mjs > "$EVIDENCE_ROOT/controls.log" 2>&1
# Record source bytes before compilation. Cargo dep-info is retained separately;
# this checkout inventory is not a claim about remote registry source capture.
python3 - "$EVIDENCE_ROOT" <<'PY'
import hashlib,json,pathlib,subprocess,sys
out=pathlib.Path(sys.argv[1]);root=pathlib.Path('.').resolve()
paths=subprocess.check_output(['git','ls-files','--cached','--others','--exclude-standard','-z'],cwd=root).decode().split('\0')
inputs={p:hashlib.sha256((root/p).read_bytes()).hexdigest() for p in paths if p and (root/p).is_file()}
(out/'inputs-before.json').write_text(json.dumps(inputs,sort_keys=True,indent=2))
(out/'checkout.txt').write_text(subprocess.check_output(['git','rev-parse','HEAD'],cwd=root).decode())
PY
cargo build --locked -p ghinvite-workflows-server --features bridge-fixture --example native-bridge-endpoint --message-format=json > "$EVIDENCE_ROOT/native-build.jsonl" 2> "$EVIDENCE_ROOT/native-build.log"
# GitHub is the octoemu emulator (libs/octoauth, its own workspace), built into
# this checkout's target directory.
cargo build --locked --manifest-path libs/octoauth/Cargo.toml --target-dir target -p octoemu --bin octoemu --message-format=json > "$EVIDENCE_ROOT/octoemu-build.jsonl" 2> "$EVIDENCE_ROOT/octoemu-build.log"
python3 - "$EVIDENCE_ROOT" <<'PY'
import hashlib,json,pathlib,shutil,sys
out=pathlib.Path(sys.argv[1]);artifacts={}
for log,name,kind in [('native-build','native-bridge-endpoint','example'),('octoemu-build','octoemu','bin')]:
 found=[r for line in (out/(log+'.jsonl')).read_text().splitlines() if (r:=json.loads(line)).get('reason')=='compiler-artifact' and r.get('target',{}).get('name')==name and r.get('executable')]
 assert len(found)==1 and found[0]['target']['kind']==[kind] and not found[0]['profile']['test'], 'exact '+kind+' artifact required'
 source=pathlib.Path(found[0]['executable']);dest=out/name;shutil.copy2(source,dest);artifacts[name]=hashlib.sha256(dest.read_bytes()).hexdigest()
(out/'native-artifacts.json').write_text(json.dumps(artifacts,indent=2))
PY
node tests/worker/build.mjs workflows > "$EVIDENCE_ROOT/worker-build.log" 2>&1
export NATIVE_BRIDGE_ENDPOINT_BIN="$EVIDENCE_ROOT/native-bridge-endpoint"
export OCTOEMU_BIN="$EVIDENCE_ROOT/octoemu"
# Preserve the generated module's relative Wasm import, independent of later builds.
mkdir -p "$EVIDENCE_ROOT/worker/.generated-workflows"
cp tests/worker/workflows-worker.mjs "$EVIDENCE_ROOT/worker/"
cp tests/worker/.generated-workflows/index_bg.wasm "$EVIDENCE_ROOT/worker/.generated-workflows/"
export WORKFLOW_WORKER_MODULE="$EVIDENCE_ROOT/worker/workflows-worker.mjs"
parent_evidence="$EVIDENCE_ROOT"
export EVIDENCE_ROOT="$parent_evidence/runtime"
node tests/native-d1-bridge/cutover.mjs > "$parent_evidence/runtime.log" 2>&1
python3 scripts/verify-native-d1-cutover.py "$EVIDENCE_ROOT" --self-test > "$parent_evidence/verification.json"
python3 - "$parent_evidence" <<'PY'
import hashlib,json,pathlib,sys
out=pathlib.Path(sys.argv[1]);root=pathlib.Path('.').resolve();before=json.loads((out/'inputs-before.json').read_text())
changed=[p for p,h in before.items() if not (root/p).is_file() or hashlib.sha256((root/p).read_bytes()).hexdigest()!=h]
assert not changed, f'captured checkout inputs changed: {changed}'
(out/'inputs-after.json').write_text(json.dumps({'unchanged':True,'inputs':len(before)},indent=2))
artifacts={str(p.relative_to(out)):hashlib.sha256(p.read_bytes()).hexdigest() for p in (out/'worker').rglob('*') if p.is_file()}
(out/'worker-artifacts.json').write_text(json.dumps(artifacts,indent=2))
PY
