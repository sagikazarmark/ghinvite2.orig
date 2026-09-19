#!/usr/bin/env python3
"""Native, offline-first admission cutover. See docs/admission-cutover.md."""
import argparse
import hashlib
import json
from pathlib import Path
import sqlite3
import sys
from datetime import datetime, timezone
import urllib.request
import urllib.error


def http(base, path, payload=None, method=None):
    headers = {"Accept": "application/json"}
    if payload is not None:
        headers["Content-Type"] = "application/json"
    request = urllib.request.Request(base.rstrip("/") + path,
        data=canonical(payload).encode() if payload is not None else None,
        headers=headers, method=method)
    with urllib.request.build_opener(urllib.request.ProxyHandler({})).open(request, timeout=60) as response:
        body = response.read()
        return json.loads(body) if body else None


def runtime_inventory(args):
    deployments = http(args.admin, "/deployments")
    invocations = http(args.admin, "/query", {"query": "SELECT id, target_service_name, target_service_key, target_handler_name, pinned_deployment_id, status FROM sys_invocation"})
    with open(args.output, "x") as output:
        output.write(canonical({"deployments": deployments, "invocations": invocations}) + "\n")


def run_import(args):
    manifest = load_manifest(args.manifest)
    identity = {"migration_id": manifest["evidence"]["migration_id"], "manifest_checksum": manifest["checksum"]}
    with sqlite3.connect(f"file:{Path(args.database).resolve()}?mode=rw", uri=True) as db:
        db.row_factory = sqlite3.Row
        fence = db.execute("SELECT * FROM admission_cutover").fetchone()
        if not fence or fence["migration_id"] != identity["migration_id"] or fence["manifest_checksum"] != identity["manifest_checksum"]:
            raise ValueError("adopt projections under this fenced manifest before import")
        result = []
        for entry in manifest["links"]:
            link_id = entry["source"]["link"]["id"]
            if entry["unresolved"]:
                result.append({"link_id": link_id, "phase": "unresolved", "reasons": entry["unresolved"]})
                continue
            progress = db.execute("SELECT * FROM admission_import_progress WHERE link_id=?", (link_id,)).fetchone()
            if not progress or progress["manifest_checksum"] != identity["manifest_checksum"]:
                raise ValueError("projection adoption missing")
            path = f"/InvitationLinkV1/{link_id}/"
            begin = {**entry["begin"], **identity}
            status = http(args.ingress, path + "begin_import", begin)
            # The durable link count is the resume cursor. Local progress is a
            # convenience, never permission to overwrite or skip remote state.
            for index in range(status["imported_requests"], len(entry["records"])):
                status = http(args.ingress, path + "import_request", {**identity, "index": index, "record": entry["records"][index]})
            if status["phase"] != "active":
                for index in range(max(1, len(entry["records"]))):
                    actual = http(args.ingress, path + "verify_import", {**identity, "index": index})
                    if actual["link"] != entry["begin"]["link"]:
                        raise ValueError("imported link counts/revisions/guardrails differ")
                    if entry["records"]:
                        record = entry["records"][index]
                        expected_blocker = next((r["request"]["request_id"] for r in entry["records"] if r["request"]["requester_id"] == record["request"]["requester_id"] and r["request"]["state"] in ("pending", "approved")), None)
                        if actual["request"] != record["request"] or actual["blocker"] != expected_blocker or actual["plan"] != record["plan"]:
                            raise ValueError("imported request/deadline/blocker/dispatch differs")
                        r = record["request"]
                        expected_operation = {"input": record["legacy_input"], "receipt": {"decided_at": r["admitted_at"], "result": {
                            "kind": "accepted", "request_id": r["request_id"], "state": "pending" if entry["begin"]["link"]["creation"]["approval_required"] else "approved",
                            "decision_deadline": r["decision_deadline"]}}}
                        if actual["operation"] != expected_operation:
                            raise ValueError("imported legacy replay differs")
                        for receipt in record["receipts"]:
                            actual_receipt = http(args.ingress, f"/GithubCreateV1/{receipt['command']['invitation_id']}/status", method="POST")
                            comparable = dict(actual_receipt) if actual_receipt else None
                            # #64 may have observed an old confirmed outcome on
                            # a prior projection pass. Accept only that one-time
                            # enrichment; command/outcome remain input-bound.
                            if (comparable and "confirmed_at" not in receipt
                                    and receipt["outcome"]["kind"] in ("created", "already_collaborator", "failed")
                                    and comparable.get("recovered") is True
                                    and comparable.get("confirmed_at")
                                    and comparable.get("revision") == receipt["revision"] + 1):
                                timestamp(comparable.pop("confirmed_at"))
                                comparable.pop("recovered")
                                comparable["revision"] = receipt["revision"]
                            if comparable != receipt:
                                raise ValueError("imported receiving outcome differs")
                            http(args.ingress, f"/GithubCreateV1/{receipt['command']['invitation_id']}/project_import", method="POST")
                    # Exercise the production projector against the ADOPTED rows,
                    # including immutable identity comparison and parent checks.
                    http(args.ingress, "/InvitationProjectionV1/apply_transition", {
                        "version": 1, "transition_id": f"migration/{link_id}/{index}", "link": entry["begin"]["link"],
                        "requests": [entry["records"][index]["request"]] if entry["records"] else [], "events": []})
            if args.activate:
                # Persist the conservative no-rollback marker BEFORE any remote
                # activation, including an activation with lost acknowledgement.
                db.execute("UPDATE admission_cutover SET phase='activation-started'")
                db.commit()
                status = http(args.ingress, path + "activate_import", {**identity, "projection_verified": True})
                for index in range(len(entry["records"])):
                    http(args.ingress, path + "handoff_import", {**identity, "index": index})
                db.execute("UPDATE admission_import_progress SET phase='active' WHERE link_id=?", (link_id,))
            else:
                db.execute("UPDATE admission_import_progress SET phase='imported' WHERE link_id=? AND phase != 'active'", (link_id,))
            db.commit()
            result.append({"link_id": link_id, **status})
        print(canonical(result))


def adopt_projections(args):
    manifest = load_manifest(args.manifest)
    with sqlite3.connect(f"file:{Path(args.database).resolve()}?mode=rw", uri=True) as db:
        db.row_factory = sqlite3.Row
        db.execute("BEGIN IMMEDIATE")
        fence = db.execute("SELECT * FROM admission_cutover").fetchone()
        if not fence or fence["migration_id"] != manifest["evidence"]["migration_id"] or fence["manifest_checksum"] not in (None, manifest["checksum"]):
            raise ValueError("persistent fence/manifest conflict")
        db.execute("CREATE TABLE IF NOT EXISTS admission_import_progress (link_id TEXT PRIMARY KEY, manifest_checksum TEXT NOT NULL, phase TEXT NOT NULL)")
        for entry in manifest["links"]:
            if entry["unresolved"]:
                continue
            source, link = entry["source"], entry["begin"]["link"]
            progress = db.execute("SELECT * FROM admission_import_progress WHERE link_id=?", (link["link_id"],)).fetchone()
            if progress:
                if progress["manifest_checksum"] != manifest["checksum"]:
                    raise ValueError("link progress identity conflict")
                continue
            current = dict(db.execute("SELECT * FROM invitation_links WHERE id=?", (link["link_id"],)).fetchone())
            requests = [dict(r) for r in db.execute("SELECT * FROM invitation_requests WHERE invitation_link_id=? ORDER BY id", (link["link_id"],))]
            repos = [dict(r) for r in db.execute("SELECT * FROM invitation_link_repos WHERE invitation_link_id=? ORDER BY repo_id", (link["link_id"],))]
            invitations = [dict(r) for r in db.execute("SELECT g.* FROM github_invitations g JOIN invitation_requests r ON r.id=g.invitation_request_id WHERE r.invitation_link_id=? ORDER BY g.id", (link["link_id"],))]
            if current != source["link"] or requests != source["requests"] or repos != sorted(source["repos"], key=lambda r: r["repo_id"]) or invitations != source["invitations"]:
                raise ValueError("source changed after inventory")
            identity = [link["creation"], link["invitation_code"], link["created_at"]]
            db.execute("""UPDATE invitation_links SET projection_revision=1, projection_content=?,projection_identity=?,created_at=?,expires_at=?,revoked_at=? WHERE id=?""",
                       (canonical(link), canonical(identity), link["created_at"], link["creation"]["expires_at"], link["revoked_at"], link["link_id"]))
            for record in entry["records"]:
                r = record["request"]
                decision = r.get("decision", {})
                identity = [r[k] for k in ("request_id", "link_id", "account_id", "requester_id", "justification", "admitted_at", "decision_deadline")]
                db.execute("""UPDATE invitation_requests SET projection_revision=1,projection_content=?,projection_identity=?,created_at=?,decision_deadline=?,decided_at=? WHERE id=?""",
                           (canonical(r), canonical(identity), r["admitted_at"], r["decision_deadline"], decision.get("effective_at"), r["request_id"]))
            db.execute("INSERT INTO admission_import_progress VALUES (?,?,'projected')", (link["link_id"], manifest["checksum"]))
        db.execute("UPDATE admission_cutover SET manifest_checksum=?", (manifest["checksum"],))
    print("Legacy projections adopted under source identity; audit rows preserved")


def restore_legacy(args):
    if not args.restored_coordinated_checkpoint:
        raise ValueError("Restore the coordinated PRE-ACTIVATION checkpoint first; forward repair is required after authoritative writes")
    with sqlite3.connect(f"file:{Path(args.database).resolve()}?mode=rw", uri=True) as db:
        db.execute("BEGIN IMMEDIATE")
        state = db.execute("SELECT phase,manifest_checksum FROM admission_cutover").fetchone()
        if state != ("fenced", None) or db.execute("SELECT 1 FROM invitation_links WHERE projection_revision IS NOT NULL UNION SELECT 1 FROM invitation_requests WHERE projection_revision IS NOT NULL LIMIT 1").fetchone():
            raise ValueError("not an unadopted pre-activation checkpoint; reverse reconciliation required")
        for (name,) in db.execute("SELECT name FROM sqlite_master WHERE type='trigger' AND name GLOB 'cutover_*'").fetchall():
            db.execute('DROP TRIGGER "' + name.replace('"', '""') + '"')
        db.execute("DELETE FROM admission_cutover")
    print("Restored checkpoint SQL fence removed; verify paired Restate restoration before resuming legacy routing")


def timestamp(value):
    if value is None:
        return None
    parsed = datetime.fromisoformat(value.replace("Z", "+00:00")).astimezone(timezone.utc)
    precision = "seconds" if not parsed.microsecond else "milliseconds" if parsed.microsecond % 1000 == 0 else "microseconds"
    return parsed.isoformat(timespec=precision).replace("+00:00", "Z")


def prepare(args):
    manifest = load_manifest(args.manifest)
    evidence = json.loads(Path(args.evidence).read_text())
    runtime, checkpoint = evidence["runtime"], evidence["checkpoint"]
    if evidence["version"] != 1 or evidence["manifest_checksum"] != manifest["checksum"]:
        raise ValueError("evidence/source identity conflict")
    if not all(runtime.get(k) is True for k in ("old_endpoints_isolated", "issued_effects_accounted", "new_ingress_closed")):
        raise ValueError("old endpoint/issued-effect fencing and closed ingress required")
    if not runtime["deployments"] or any(not i.get("deployment_id") or not i.get("handoff") for i in runtime["invocations"]):
        raise ValueError("pinned invocation/deployment handoff inventory incomplete")
    if not checkpoint["id"] or any(len(checkpoint[k]) != 64 for k in ("database_sha256", "restate_sha256")):
        raise ValueError("coordinated checkpoint hashes required")
    manifest["runtime"], manifest["checkpoint"] = runtime, checkpoint
    manifest["evidence"] = evidence
    for entry in manifest["links"]:
        source = entry["source"]
        old = source["link"]
        unresolved = [u for u in entry["unresolved"] if not u.startswith(("missing_deadline:", "missing_legacy_input:"))]
        if old["projection_revision"] is not None:
            unresolved.append("already_authoritative")
        creation = {"version": 1, "link_id": old["id"], "admin": {"account_id": old["account_id"], "user_id": old["created_by"]},
                    **{k: old[k] for k in ("account_id", "installation_id", "description", "internal_note", "max_uses", "permission")},
                    "expires_at": timestamp(old["expires_at"]), "approval_required": bool(old["approval_required"]),
                    "repos": [{"repo_id": r["repo_id"], "repo_full_name": r["repo_full_name"]} for r in sorted(source["repos"], key=lambda r: r["repo_id"])]}
        link = {"link_id": old["id"], "creation": creation, "invitation_code": old["slug"], "created_at": timestamp(old["created_at"]),
                "uses": old["uses_count"], "revision": 1, "revoked_at": timestamp(old["revoked_at"]), "revoked_by": old["revoked_by"]}
        records = []
        for request in source["requests"]:
            rid = request["id"]
            facts = evidence["requests"].get(rid, {})
            legacy = facts.get("input")
            expected = {"request_id": rid, "invitation_link_id": old["id"], "requester_id": request["requester_id"],
                        "justification": request["justification"], "created_at": request["created_at"]}
            if legacy != expected or not facts.get("evidence_ref"):
                unresolved.append(f"missing_legacy_input:{rid}")
            deadline = request["decision_deadline"] or facts.get("deadline")
            # Manual admissions need their ORIGINAL deadline even when terminal,
            # so the accepted replay receipt does not invent original input.
            if creation["approval_required"] and not deadline:
                unresolved.append(f"missing_deadline:{rid}")
            if request["decision_deadline"] and facts.get("deadline") and timestamp(request["decision_deadline"]) != timestamp(facts["deadline"]):
                unresolved.append(f"conflicting_deadline:{rid}")
            normalized = (request["justification"] or "").strip() or None
            # Preserve the immutable historical SQL input. Do not silently alter
            # it to make a v1 projection identity appear to match.
            if normalized != request["justification"]:
                unresolved.append(f"noncanonical_legacy_input:{rid}")
            snapshot = {"request_id": rid, "link_id": old["id"], "account_id": old["account_id"], "requester_id": request["requester_id"],
                        "justification": normalized, "state": request["state"], "admitted_at": timestamp(request["created_at"]),
                        "decision_deadline": timestamp(deadline), "revision": 1}
            if request["state"] != "pending" and request["decided_at"]:
                snapshot["decision"] = {"decision_id": f"legacy/v0/{rid}/{request['state']}", "decided_by": request["decided_by"],
                                        "effective_at": timestamp(request["decided_at"]), "evaluated_at": timestamp(request["decided_at"]),
                                        "decline_reason": request["decline_reason"]}
            elif request["state"] == "approved":
                unresolved.append(f"missing_approval:{rid}")
            record = {"request": snapshot, "legacy_input": {"version": 0, "link_id": old["id"], "operation_id": rid,
                      "requester_id": request["requester_id"], "justification": normalized}, "plan": None, "receipts": []}
            if request["state"] == "approved" and "decision" in snapshot:
                commands = []
                for repo in creation["repos"]:
                    invitations = [i for i in source["invitations"] if i["invitation_request_id"] == rid and i["repo_id"] == repo["repo_id"]]
                    # Missing fanout identity can live only in a pinned journal.
                    delivery = facts.get("deliveries", {}).get(str(repo["repo_id"]), {})
                    if len(invitations) > 1 or not invitations and not delivery.get("invitation_id"):
                        unresolved.append(f"missing_or_conflicting_delivery:{rid}:{repo['repo_id']}")
                        continue
                    invitation = invitations[0] if invitations else None
                    iid = invitation["id"] if invitation else delivery["invitation_id"]
                    if delivery.get("invitation_id", iid) != iid:
                        unresolved.append(f"delivery_identity_conflict:{iid}")
                    command = {"version": 1, "invitation_id": iid, "link_id": old["id"], "request_id": rid,
                               "approval_id": snapshot["decision"]["decision_id"], "account_id": old["account_id"],
                               "installation_id": old["installation_id"], "requester_id": request["requester_id"],
                               **repo, "permission": old["permission"], "approved_at": snapshot["decision"]["effective_at"]}
                    commands.append(command)
                    outcome = {"kind": "outcome_unknown"}
                    if invitation and invitation["github_invitation_id"]:
                        outcome = {"kind": "created", "upstream_id": invitation["github_invitation_id"]}
                    elif delivery.get("outcome"):
                        outcome = delivery["outcome"]
                    record["receipts"].append({"command": command, "outcome": outcome, "revision": 1})
                record["plan"] = {"dispatch_id": f"v1/dispatch/{rid}", "input": {"version": 1, "request": snapshot,
                    "installation_id": old["installation_id"], "repos": creation["repos"], "permission": old["permission"],
                    "approval_required": creation["approval_required"]}, "commands": commands}
            records.append(record)
        entry.update(unresolved=sorted(set(unresolved)), records=records,
                     begin={"version": 1, "migration_id": evidence["migration_id"], "manifest_checksum": "",
                            "checkpoint": checkpoint["id"], "link": link, "request_checksums": [checksum(r) for r in records]})
    # The prepared manifest's digest excludes its own embedded transport digest.
    manifest.pop("checksum")
    manifest["checksum"] = checksum(manifest)
    with open(args.output, "x") as output:
        output.write(canonical(manifest) + "\n")


def fence(args):
    with sqlite3.connect(f"file:{Path(args.database).resolve()}?mode=rw", uri=True) as db:
        db.execute("BEGIN IMMEDIATE")
        db.execute("CREATE TABLE IF NOT EXISTS admission_cutover (singleton INTEGER PRIMARY KEY CHECK(singleton=1), migration_id TEXT NOT NULL, manifest_checksum TEXT, phase TEXT NOT NULL)")
        old = db.execute("SELECT migration_id FROM admission_cutover").fetchone()
        if old and old[0] != args.migration_id:
            raise ValueError("migration identity conflict")
        db.execute("INSERT OR IGNORE INTO admission_cutover VALUES (1,?,NULL,'fenced')", (args.migration_id,))
        # Compare revisions, rather than trusting a mutable process flag: an old
        # issued UPDATE that reaches SQLite after the checkpoint must abort.
        for table in ("invitation_links", "invitation_requests"):
            for op, condition in (
                ("INSERT", "NEW.projection_revision IS NULL"),
                ("UPDATE", "NEW.projection_revision IS NULL OR NEW.projection_revision IS OLD.projection_revision"),
                ("DELETE", "1"),
            ):
                # Derived queue-index maintenance does not change domain state
                # or advance a projection revision. Fence every domain column,
                # but allow the queue key's triggers to maintain that index.
                operation = op
                if table == "invitation_requests" and op == "UPDATE":
                    columns = [row[1] for row in db.execute("PRAGMA table_info(invitation_requests)")
                               if row[1] != "queue_account_id"]
                    operation = "UPDATE OF " + ",".join('"' + name.replace('"', '""') + '"' for name in columns)
                    db.execute("DROP TRIGGER IF EXISTS cutover_invitation_requests_UPDATE")
                db.execute(f"""CREATE TRIGGER IF NOT EXISTS cutover_{table}_{op} BEFORE {operation} ON {table}
                    WHEN {condition} BEGIN SELECT RAISE(ABORT,'obsolete writer: admission cutover'); END""")
        for op in ("UPDATE", "DELETE"):
            db.execute(f"""CREATE TRIGGER IF NOT EXISTS cutover_repos_{op} BEFORE {op} ON invitation_link_repos
                BEGIN SELECT RAISE(ABORT,'obsolete writer: admission cutover'); END""")
        db.execute("""CREATE TRIGGER IF NOT EXISTS cutover_repos_INSERT BEFORE INSERT ON invitation_link_repos
            WHEN NOT EXISTS (SELECT 1 FROM invitation_links WHERE id=NEW.invitation_link_id AND projection_revision IS NOT NULL)
            BEGIN SELECT RAISE(ABORT,'obsolete writer: admission cutover'); END""")
        db.execute("""CREATE TRIGGER IF NOT EXISTS cutover_audit_INSERT BEFORE INSERT ON audit_events
            WHEN NEW.projection_event_id IS NULL AND (NEW.target_kind IN ('invitation_link','invitation_request'))
            BEGIN SELECT RAISE(ABORT,'obsolete writer: admission cutover'); END""")
    print("Persistent SQL fence installed; issued GitHub calls still require endpoint/egress isolation")


def canonical(value):
    return json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False)


def checksum(value):
    return hashlib.sha256(canonical(value).encode()).hexdigest()


def rows(db, table):
    return [dict(row) for row in db.execute(f"SELECT * FROM {table} ORDER BY 1")]


def inventory(args):
    with sqlite3.connect(f"file:{Path(args.database).resolve()}?mode=ro", uri=True) as db:
        db.row_factory = sqlite3.Row
        db.execute("BEGIN")
        tables = {t: rows(db, t) for t in (
            "installations", "users", "invitation_links", "invitation_link_repos",
            "invitation_requests", "github_invitations", "audit_events", "delivery_attempts", "delivery_outcomes",
        )}
    links = []
    for link in tables["invitation_links"]:
        requests = [r for r in tables["invitation_requests"] if r["invitation_link_id"] == link["id"]]
        ids = {r["id"] for r in requests}
        source = {"link": link, "requests": requests,
                  "repos": [r for r in tables["invitation_link_repos"] if r["invitation_link_id"] == link["id"]],
                  "invitations": [i for i in tables["github_invitations"] if i["invitation_request_id"] in ids]}
        unresolved = []
        if link["uses_count"] != len(requests):
            unresolved.append("counter_mismatch")
        blockers = set()
        users = {u["user_id"] for u in tables["users"]}
        if not any(i["installation_id"] == link["installation_id"] and i["account_id"] == link["account_id"] for i in tables["installations"]):
            unresolved.append("missing_installation_parent")
        if link["created_by"] not in users or link["revoked_by"] is not None and link["revoked_by"] not in users:
            unresolved.append("missing_admin_parent")
        for request in requests:
            rid = request["id"]
            if request["requester_id"] not in users or request["decided_by"] is not None and request["decided_by"] not in users:
                unresolved.append(f"missing_user_parent:{rid}")
            if request["state"] == "pending" and not request["decision_deadline"]:
                unresolved.append(f"missing_deadline:{rid}")
            unresolved.append(f"missing_legacy_input:{rid}")
            if request["state"] in ("pending", "approved"):
                if request["requester_id"] in blockers:
                    unresolved.append(f"conflicting_blockers:{request['requester_id']}")
                blockers.add(request["requester_id"])
        links.append({"source": source, "source_checksum": checksum(source), "unresolved": unresolved})
    manifest = {"version": 1, "database_checksum": checksum(tables), "tables": tables,
                "links": links, "runtime": None, "checkpoint": None}
    manifest["checksum"] = checksum(manifest)
    with open(args.output, "x") as output:
        output.write(canonical(manifest) + "\n")


def load_manifest(path):
    manifest = json.loads(Path(path).read_text())
    expected = manifest.pop("checksum")
    if manifest["version"] != 1 or checksum(manifest) != expected:
        raise ValueError("manifest version/checksum conflict")
    manifest["checksum"] = expected
    for link in manifest["links"]:
        if checksum(link["source"]) != link["source_checksum"]:
            raise ValueError("link source checksum conflict")
    return manifest


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    inv = commands.add_parser("inventory")
    inv.add_argument("--database", required=True)
    inv.add_argument("--output", required=True)
    verify = commands.add_parser("verify")
    verify.add_argument("--manifest", required=True)
    preparing = commands.add_parser("prepare")
    preparing.add_argument("--manifest", required=True)
    preparing.add_argument("--evidence", required=True)
    preparing.add_argument("--output", required=True)
    fencing = commands.add_parser("fence")
    fencing.add_argument("--database", required=True)
    fencing.add_argument("--migration-id", required=True)
    restore = commands.add_parser("restore-legacy")
    restore.add_argument("--database", required=True)
    restore.add_argument("--restored-coordinated-checkpoint")
    adoption = commands.add_parser("adopt-projections")
    adoption.add_argument("--database", required=True)
    adoption.add_argument("--manifest", required=True)
    running = commands.add_parser("import")
    running.add_argument("--database", required=True)
    running.add_argument("--manifest", required=True)
    running.add_argument("--ingress", required=True)
    running.add_argument("--activate", action="store_true")
    runtime = commands.add_parser("runtime-inventory")
    runtime.add_argument("--admin", required=True)
    runtime.add_argument("--output", required=True)
    args = parser.parse_args()
    if args.command == "inventory":
        inventory(args)
    elif args.command == "fence":
        fence(args)
    elif args.command == "prepare":
        prepare(args)
    elif args.command == "adopt-projections":
        adopt_projections(args)
    elif args.command == "import":
        run_import(args)
    elif args.command == "runtime-inventory":
        runtime_inventory(args)
    elif args.command == "restore-legacy":
        restore_legacy(args)
    else:
        load_manifest(args.manifest)
        print("Manifest identity and checksums verified")


if __name__ == "__main__":
    try:
        main()
    except (ValueError, KeyError, OSError, sqlite3.Error) as error:
        print(str(error), file=sys.stderr)
        sys.exit(1)
