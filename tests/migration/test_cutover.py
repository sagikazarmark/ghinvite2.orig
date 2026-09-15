"""Operator CLI contract, using disposable native SQLite fixtures."""
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import tempfile
import unittest
import urllib.request
import time

ROOT = Path(__file__).resolve().parents[2]
TOOL = ROOT / "scripts/admission-cutover.py"


class CutoverTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.db = Path(os.environ.get("CUTOVER_REHEARSAL_DATABASE", str(Path(self.tmp.name) / "legacy.db"))) if self._testMethodName == "test_prepare_preserves_deadline_and_terminal_history_and_requires_runtime_fence" else Path(self.tmp.name) / "legacy.db"
        self.manifest = Path(self.tmp.name) / "manifest.json"
        with sqlite3.connect(self.db) as db:
            if not db.execute("SELECT name FROM sqlite_master WHERE name='users'").fetchone():
                for path in sorted((ROOT / "migrations").glob("*.sql")):
                    db.executescript(path.read_text())
            db.execute("INSERT INTO installations VALUES (9,100,'acme','Organization','2026-01-01T00:00:00Z',NULL,'all')")
            db.execute("INSERT INTO users VALUES (7,'admin',NULL,'2026-01-01T00:00:00Z')")
            db.execute("INSERT INTO users VALUES (8,'alice',NULL,'2026-01-01T00:00:00Z')")
            db.execute("""INSERT INTO invitation_links
                (id,slug,installation_id,account_id,created_by,created_at,uses_count,permission,approval_required,description)
                VALUES ('01ARZ3NDEKTSV4RRFFQ69G5FAV','testcode12345678',9,100,7,'2026-01-01T00:00:00Z',1,'push',1,'Legacy')""")
            db.execute("INSERT INTO invitation_link_repos VALUES ('01ARZ3NDEKTSV4RRFFQ69G5FAV',10,'acme/api')")
            db.execute("""INSERT INTO invitation_requests
                (id,invitation_link_id,requester_id,state,created_at)
                VALUES ('01ARZ3NDEKTSV4RRFFQ69G5FAW','01ARZ3NDEKTSV4RRFFQ69G5FAV',8,'pending','2026-01-01T00:00:00Z')""")

    def cli(self, *args, ok=True):
        result = subprocess.run(["python3", str(TOOL), *map(str, args)], capture_output=True, text=True)
        if ok:
            self.assertEqual(result.returncode, 0, result.stderr)
        else:
            self.assertNotEqual(result.returncode, 0)
        return result

    def test_inventory_reports_conflicts_missing_parents_and_delivery_evidence_without_deleting_history(self):
        with sqlite3.connect(self.db) as db:
            db.execute("UPDATE invitation_links SET uses_count=4")
            for rid, state, user in [("01ARZ3NDEKTSV4RRFFQ69G5FAX", "approved", 8),
                                     ("01ARZ3NDEKTSV4RRFFQ69G5FAY", "cancelled", 99),
                                     ("01ARZ3NDEKTSV4RRFFQ69G5FAZ", "expired", 8)]:
                db.execute("INSERT INTO invitation_requests (id,invitation_link_id,requester_id,state,created_at) VALUES (?, '01ARZ3NDEKTSV4RRFFQ69G5FAV',?,?,'2026-01-01T00:00:00Z')", (rid,user,state))
            for iid, state, upstream in [("01ARZ3NDEKTSV4RRFFQ69G5FB0", "sent", 1234), ("01ARZ3NDEKTSV4RRFFQ69G5FB1", "sending", None)]:
                db.execute("INSERT INTO github_invitations VALUES (?,'01ARZ3NDEKTSV4RRFFQ69G5FAX',10,?,?,NULL,'2026-01-01T00:00:00Z','2026-01-01T00:00:00Z')", (iid,upstream,state))
        self.cli("inventory", "--database", self.db, "--output", self.manifest)
        link = json.loads(self.manifest.read_text())["links"][0]
        self.assertIn("conflicting_blockers:8", link["unresolved"])
        self.assertIn("missing_user_parent:01ARZ3NDEKTSV4RRFFQ69G5FAY", link["unresolved"])
        self.assertNotIn("counter_mismatch", link["unresolved"])
        self.assertEqual([r["state"] for r in link["source"]["requests"]], ["pending", "approved", "cancelled", "expired"])
        self.assertEqual(link["source"]["invitations"][0]["github_invitation_id"], 1234)
        self.assertEqual(link["source"]["invitations"][1]["state"], "sending")

    def test_inventory_keeps_unproven_pending_link_closed(self):
        self.cli("inventory", "--database", self.db, "--output", self.manifest)
        manifest = json.loads(self.manifest.read_text())
        self.assertEqual(manifest["version"], 1)
        link = manifest["links"][0]
        self.assertEqual(link["source"]["link"]["uses_count"], 1)
        self.assertIn("missing_deadline:01ARZ3NDEKTSV4RRFFQ69G5FAW", link["unresolved"])
        self.assertIn("missing_legacy_input:01ARZ3NDEKTSV4RRFFQ69G5FAW", link["unresolved"])
        self.assertEqual(len(manifest["checksum"]), 64)
        self.cli("verify", "--manifest", self.manifest)
        manifest["links"][0]["source"]["link"]["uses_count"] = 0
        self.manifest.write_text(json.dumps(manifest))
        self.cli("verify", "--manifest", self.manifest, ok=False)

    def test_fence_rejects_late_revoke_and_decision_and_cannot_be_removed_after_activation(self):
        self.cli("fence", "--database", self.db, "--migration-id", "rehearsal")
        with sqlite3.connect(self.db) as db:
            with self.assertRaisesRegex(sqlite3.IntegrityError, "obsolete writer"):
                db.execute("UPDATE invitation_links SET revoked_at='2026-02-01T00:00:00Z',revoked_by=7")
            with self.assertRaisesRegex(sqlite3.IntegrityError, "obsolete writer"):
                db.execute("UPDATE invitation_requests SET state='approved'")
        self.cli("fence", "--database", self.db, "--migration-id", "rehearsal")
        self.cli("fence", "--database", self.db, "--migration-id", "different", ok=False)
        self.cli("restore-legacy", "--database", self.db, ok=False)

    def test_restored_pre_activation_checkpoint_can_resume_legacy_writes(self):
        self.cli("fence", "--database", self.db, "--migration-id", "rehearsal")
        self.cli("restore-legacy", "--database", self.db, "--restored-coordinated-checkpoint", "fixture-before-activation")
        with sqlite3.connect(self.db) as db:
            db.execute("UPDATE invitation_links SET revoked_at='2026-02-01T00:00:00Z',revoked_by=7")
            self.assertEqual(db.execute("SELECT uses_count FROM invitation_links").fetchone()[0], 1)

    def test_prepare_preserves_deadline_and_terminal_history_and_requires_runtime_fence(self):
        evidence = Path(self.tmp.name) / "evidence.json"
        prepared = Path(self.tmp.name) / "prepared.json"
        with sqlite3.connect(self.db) as db:
            db.execute("UPDATE invitation_links SET uses_count=4")
            for rid, state, user in [("01ARZ3NDEKTSV4RRFFQ69G5FAX", "approved", 9), ("01ARZ3NDEKTSV4RRFFQ69G5FAY", "cancelled", 8), ("01ARZ3NDEKTSV4RRFFQ69G5FAZ", "expired", 8)]:
                if user == 9:
                    db.execute("INSERT INTO users VALUES (9,'bob',NULL,'2026-01-01T00:00:00Z')")
                db.execute("INSERT INTO invitation_requests (id,invitation_link_id,requester_id,state,created_at,decided_at,decided_by) VALUES (?,'01ARZ3NDEKTSV4RRFFQ69G5FAV',?,?,'2026-01-01T00:00:00Z','2026-01-01T01:00:00Z',7)", (rid,user,state))
            db.execute("INSERT INTO github_invitations VALUES ('01ARZ3NDEKTSV4RRFFQ69G5FB0','01ARZ3NDEKTSV4RRFFQ69G5FAX',10,1234,'sent',NULL,'2026-01-01T01:00:00Z','2026-01-01T01:00:00Z')")
        self.cli("fence", "--database", self.db, "--migration-id", "rehearsal")
        self.cli("inventory", "--database", self.db, "--output", self.manifest)
        facts = {"version": 1, "migration_id": "rehearsal", "manifest_checksum": json.loads(self.manifest.read_text())["checksum"],
                 "checkpoint": {"id": "fixture-checkpoint", "database_sha256": "a" * 64, "restate_sha256": "b" * 64},
                 "runtime": {"deployments": [{"id": "dp_fixture"}], "invocations": [], "old_endpoints_isolated": False,
                             "issued_effects_accounted": True, "new_ingress_closed": True},
                 "requests": {"01ARZ3NDEKTSV4RRFFQ69G5FAW": {
                     "input": {"request_id": "01ARZ3NDEKTSV4RRFFQ69G5FAW", "invitation_link_id": "01ARZ3NDEKTSV4RRFFQ69G5FAV", "requester_id": 8, "justification": None, "created_at": "2026-01-01T00:00:00Z"},
                     "deadline": "2026-01-02T00:00:00Z", "evidence_ref": "journal-export:fixture"}}}
        for rid, user in [("01ARZ3NDEKTSV4RRFFQ69G5FAX",9),("01ARZ3NDEKTSV4RRFFQ69G5FAY",8),("01ARZ3NDEKTSV4RRFFQ69G5FAZ",8)]:
            facts["requests"][rid] = {"input": {"request_id":rid,"invitation_link_id":"01ARZ3NDEKTSV4RRFFQ69G5FAV","requester_id":user,"justification":None,"created_at":"2026-01-01T00:00:00Z"},"deadline":"2026-01-02T00:00:00Z","evidence_ref":"journal:terminal"}
        evidence.write_text(json.dumps(facts))
        self.cli("prepare", "--manifest", self.manifest, "--evidence", evidence, "--output", prepared, ok=False)
        facts["runtime"]["old_endpoints_isolated"] = True
        evidence.write_text(json.dumps(facts))
        self.cli("prepare", "--manifest", self.manifest, "--evidence", evidence, "--output", prepared)
        result = json.loads(prepared.read_text())
        link = result["links"][0]
        self.assertEqual(link["unresolved"], [])
        self.assertEqual(link["records"][0]["request"]["decision_deadline"], "2026-01-02T00:00:00Z")
        self.assertEqual(link["records"][0]["legacy_input"]["version"], 0)
        self.assertEqual(link["begin"]["link"]["uses"], 4)
        self.cli("verify", "--manifest", prepared)
        self.cli("adopt-projections", "--database", self.db, "--manifest", prepared)
        self.cli("adopt-projections", "--database", self.db, "--manifest", prepared)
        with sqlite3.connect(self.db) as db:
            self.assertEqual(db.execute("SELECT uses_count,projection_revision FROM invitation_links").fetchone(), (4, 1))
            self.assertEqual(db.execute("SELECT state,decision_deadline FROM invitation_requests").fetchone(), ("pending", "2026-01-02T00:00:00Z"))
            with self.assertRaisesRegex(sqlite3.IntegrityError, "obsolete writer"):
                db.execute("UPDATE invitation_requests SET state='approved'")
        if os.environ.get("CUTOVER_REHEARSAL_INGRESS"):
            self.cli("import", "--database", self.db, "--manifest", prepared, "--ingress", os.environ["CUTOVER_REHEARSAL_INGRESS"])
            self.cli("import", "--database", self.db, "--manifest", prepared, "--ingress", os.environ["CUTOVER_REHEARSAL_INGRESS"], "--activate")
            opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
            def call(handler, payload):
                request = urllib.request.Request(os.environ["CUTOVER_REHEARSAL_INGRESS"] + "/InvitationLinkV1/01ARZ3NDEKTSV4RRFFQ69G5FAV/" + handler,
                    data=json.dumps(payload).encode(), headers={"Content-Type":"application/json"})
                with opener.open(request, timeout=10) as response:
                    return json.load(response)
            for rid, state, user in [("01ARZ3NDEKTSV4RRFFQ69G5FAX","approved",9),("01ARZ3NDEKTSV4RRFFQ69G5FAY","cancelled",8),("01ARZ3NDEKTSV4RRFFQ69G5FAZ","expired",8)]:
                self.assertEqual(call("request_status", {"link_id":"01ARZ3NDEKTSV4RRFFQ69G5FAV","request_id":rid,"requester_id":user})["state"], state)
            result = call("admit", {"version":1,"link_id":"01ARZ3NDEKTSV4RRFFQ69G5FAV","operation_id":"01ARZ3NDEKTSV4RRFFQ69G5FB5","requester_id":8,"justification":None})
            self.assertEqual(result["result"]["kind"], "accepted")
            for _ in range(100):
                with sqlite3.connect(self.db) as db:
                    uses = db.execute("SELECT uses_count FROM invitation_links").fetchone()[0]
                    expired = db.execute("SELECT state FROM invitation_requests WHERE id='01ARZ3NDEKTSV4RRFFQ69G5FAW'").fetchone()[0]
                    outcomes = db.execute("SELECT receipt FROM delivery_outcomes WHERE invitation_id='01ARZ3NDEKTSV4RRFFQ69G5FB0'").fetchone()
                if uses == 5 and expired == "expired" and outcomes:
                    break
                time.sleep(.05)
            self.assertEqual((uses,expired),(5,"expired"))
            self.assertEqual(json.loads(outcomes[0])["outcome"], {"kind":"created","upstream_id":1234})
            self.cli("restore-legacy", "--database", self.db, "--restored-coordinated-checkpoint", "stale", ok=False)
            self.cli("import", "--database", self.db, "--manifest", prepared, "--ingress", os.environ["CUTOVER_REHEARSAL_INGRESS"], "--activate")


if __name__ == "__main__":
    unittest.main()
