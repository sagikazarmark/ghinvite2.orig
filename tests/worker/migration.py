"""Prepare a disposable #58 checkpoint for the Worker/D1 rehearsal.

SQLite here is the offline checkpoint/tooling input. The Node runner imports its
SQL archive into actual local D1 before invoking the CLI's Restate import path.
This is not a live D1 export/adoption implementation.
"""
import importlib.util
import os
from pathlib import Path
import sqlite3
import sys

root = Path(__file__).resolve().parents[2]
# Never let a developer's native rehearsal environment redirect this fixture.
os.environ.pop("CUTOVER_REHEARSAL_DATABASE", None)
os.environ.pop("CUTOVER_REHEARSAL_INGRESS", None)
directory = Path(sys.argv[1])
database = directory / "checkpoint.db"
spec = importlib.util.spec_from_file_location("cutover_fixture", root / "tests/migration/test_cutover.py")
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
case = module.CutoverTest("test_prepare_preserves_deadline_and_terminal_history_and_requires_runtime_fence")
case.setUp()
try:
    case.test_prepare_preserves_deadline_and_terminal_history_and_requires_runtime_fence()
    prepared = Path(case.tmp.name) / "prepared.json"
    (directory / "prepared.json").write_text(prepared.read_text())
    with sqlite3.connect(case.db) as source, sqlite3.connect(database) as target:
        source.backup(target)
        # D1's ordinary account reader expects the portable `all` encoding.
        target.execute("UPDATE installations SET selected_repos='all'")
        target.commit()
        (directory / "adopted.sql").write_text("\n".join(target.iterdump()))
finally:
    case.doCleanups()
    case.tmp.cleanup()
