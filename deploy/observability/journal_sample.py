#!/usr/bin/env python3
"""Fetch one bounded count-only retained journal sample from private Restate admin."""
import argparse
import http.client
import ipaddress
import json
import os
from pathlib import Path
import time
from urllib.parse import urlsplit

from collector import JOURNAL_QUERY, MAX_BYTES, bounded_list, integer, write_atomic


def query(url, token=None, timeout=3):
    parsed = urlsplit(url)
    if parsed.username or parsed.password or parsed.query or parsed.fragment or parsed.path not in ("", "/"):
        raise ValueError("admin endpoint must be an origin")
    try:
        loopback = parsed.hostname == "localhost" or ipaddress.ip_address(parsed.hostname).is_loopback
    except ValueError:
        loopback = False
    if parsed.scheme != "https" and not (parsed.scheme == "http" and loopback):
        raise ValueError("admin requires HTTPS or explicit loopback")
    if token is not None and (not token or len(token) > 8192 or any(ord(c) < 33 or ord(c) > 126 for c in token)):
        raise ValueError("invalid admin credential")
    deadline = time.monotonic() + timeout
    factory = http.client.HTTPSConnection if parsed.scheme == "https" else http.client.HTTPConnection
    connection = factory(parsed.hostname, parsed.port, timeout=timeout)
    try:
        headers = {"Accept": "application/json", "Content-Type": "application/json"}
        if token:
            headers["Authorization"] = "Bearer " + token
        connection.request("POST", "/query", json.dumps({"query": JOURNAL_QUERY}), headers)
        sock = connection.sock
        sock.settimeout(max(.001, deadline - time.monotonic()))
        response = connection.getresponse()
        if response.status != 200:  # Never follow redirects with credentials.
            raise ValueError("admin query refused")
        raw = bytearray()
        while True:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise ValueError("admin deadline")
            sock.settimeout(remaining)
            chunk = response.read1(min(8192, MAX_BYTES + 1 - len(raw)))
            if not chunk:
                break
            raw.extend(chunk)
            if len(raw) > MAX_BYTES:
                raise ValueError("admin response overflow")
        body = json.loads(raw)
        rows = bounded_list(body["rows"], 100)
        # Drop every other server field; reject any row shape beyond exact count.
        result = []
        for row in rows:
            if not isinstance(row, dict) or set(row) != {"entries"}:
                raise ValueError("unexpected admin row")
            result.append({"entries": integer(row["entries"], 100_000)})
        return {"observed_at": int(time.time()), "complete": True,
                "selection": "retained_completed", "rows": result}
    finally:
        connection.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    try:
        sample = query(os.environ["RESTATE_ADMIN_URL"], os.environ.get("RESTATE_ADMIN_TOKEN"))
    except (OSError, ValueError, TypeError, KeyError, http.client.HTTPException):
        # No response/error/token/URL is copied to logs or output.
        sample = {"observed_at": int(time.time()), "complete": False,
                  "selection": "retained_completed", "rows": []}
    write_atomic(args.output, json.dumps(sample, separators=(",", ":")) + "\n")


if __name__ == "__main__":
    main()
