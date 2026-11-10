#!/usr/bin/env python3
"""Bounded, fail-closed operational snapshots to Prometheus textfile metrics.

Input is an operator-supplied JSON envelope, never an unauthenticated public feed.
Window metrics are gauges, not cumulative counters; do not apply rate() to them.
"""
import argparse
from collections import Counter, defaultdict
from datetime import datetime
import json
import math
import os
from pathlib import Path
import tempfile
import time

MAX_BYTES = 2 * 1024 * 1024
MAX_RECORDS = 10_000
MAX_SERIES = 1_000
MAX_AGE = 300
APPS = {"repo", "org", "signin"}
OUTCOMES = {"accepted", "refused", "invalid", "missing", "conflict", "authorization_unavailable", "unknown", "interrupted"}
CLOCKS = {"native_monotonic", "worker_host"}
REASONS = {"installation_unavailable", "installation_permissions", "requester_identity", "outside_collaborators_restricted", "ip_allow_list", "target_unavailable", "mint_forbidden", "no_seat", "idp_synced_team", "managed_enterprise", "email_recovery_unavailable"}
SOURCES = ("logs", "deliveries", "journals", "budgets", "budget_limits")

# The response contains counts only, never journal raw/entry_json, input or keys.
# Complete invocation journals are short-lived; this is a retained sample only.
JOURNAL_QUERY = """WITH selected AS (
 SELECT id FROM sys_invocation
 WHERE target_service_name = 'InvitationLink' AND target_handler_name = 'admit'
 AND status = 'completed' LIMIT 101
)
SELECT COUNT(j.index) AS entries FROM selected i
LEFT JOIN sys_journal j ON i.id = j.id GROUP BY i.id"""


def integer(value, maximum=2**53-1):
    if type(value) is not int or not 0 <= value <= maximum:
        raise ValueError("invalid bounded integer")
    return value


def member(value, allowed):
    if not isinstance(value, str) or value not in allowed:
        raise ValueError("invalid bounded classification")
    return value


def stamp(value):
    if type(value) is int:
        return integer(value)
    if not isinstance(value, str) or len(value) > 40:
        raise ValueError("invalid timestamp")
    parsed = datetime.fromisoformat(value.replace("Z", "+00:00"))
    if parsed.tzinfo is None:
        raise ValueError("timestamp without timezone")
    return parsed.timestamp()


def fresh(value, now):
    value = stamp(value)
    if not 0 <= now - value <= MAX_AGE:
        raise ValueError("stale or future capture")
    return value


def bounded_list(value, maximum=MAX_RECORDS):
    if not isinstance(value, list) or len(value) > maximum:
        raise ValueError("missing or overflowing list")
    return value


def p95(values):
    return sorted(values)[math.ceil(len(values) * .95) - 1]


def metric(name, value, **labels):
    # Callers supply only hard-coded/enum names or checked numeric identities.
    if not isinstance(value, (int, float)) or not math.isfinite(value):
        raise ValueError("invalid metric value")
    suffix = "{" + ",".join(f'{key}="{val}"' for key, val in sorted(labels.items())) + "}" if labels else ""
    return f"p1_{name}{suffix} {value}\n"


def logs(data, now):
    if data["complete"] is not True:
        raise ValueError("incomplete log window")
    end = fresh(data["window_end"], now)
    start = stamp(data["window_start"])
    if not 0 < end - start <= 3600:
        raise ValueError("invalid log window")
    counts = Counter()
    durations = defaultdict(list)
    for row in bounded_list(data["records"]):
        if not isinstance(row, dict) or not start <= stamp(row["occurred_at"]) <= end:
            raise ValueError("invalid event time")
        event = row["event"]
        if event == "admission_rpc_completed":
            outcome = member(row["outcome"], OUTCOMES)
            clock = member(row["clock"], CLOCKS)
            if type(row["clock_valid"]) is not bool:
                raise ValueError("invalid clock certainty")
            counts[("admission_rpc_window", outcome, clock)] += 1
            if row["clock_valid"]:
                durations[clock].append(integer(row["elapsed_ms"], 60_000) / 1000)
            else:
                counts[("admission_clock_invalid_window", clock)] += 1
        elif event in {"installation_mint_attempt", "installation_api_call"}:
            app = member(row["app"], APPS)
            installation = integer(row["installation_id"])
            if installation == 0:
                raise ValueError("zero installation")
            if event == "installation_mint_attempt":
                cause = member(row["cause"], {"acquire", "installation_unauthorized", "app_jwt_retry"})
                counts[("mint_attempt_window", app, installation, cause)] += 1
            else:
                counts[("installation_api_call_window", app, installation)] += 1
        elif event == "webhook_signature_failure":
            counts[("webhook_signature_failure_window", member(row["app"], APPS), member(row["reason"], {"missing", "invalid"}))] += 1
        elif event == "edge_binding_rejection":
            counts[("edge_binding_rejection_window", member(row["bucket"], {"ip", "user", "authority"}), member(row["reason"], {"limited", "unavailable"}))] += 1
        else:
            raise ValueError("unsupported event in selected feed")
        if len(counts) > MAX_SERIES:
            raise ValueError("series overflow")
    result = metric("log_window_seconds", end-start)
    labels = {"admission_rpc_window": ("outcome", "clock"), "admission_clock_invalid_window": ("clock",),
              "mint_attempt_window": ("app", "installation_id", "cause"), "installation_api_call_window": ("app", "installation_id"),
              "webhook_signature_failure_window": ("app", "reason"), "edge_binding_rejection_window": ("bucket", "reason")}
    for key, count in sorted(counts.items()):
        result += metric(key[0], count, **dict(zip(labels[key[0]], key[1:])))
    for clock, values in sorted(durations.items()):
        result += metric("admission_latency_samples", len(values), clock=clock)
        result += metric("admission_latency_p95_seconds", p95(values), clock=clock)
    pairs = {(key[1], key[2]) for key in counts if key[0] in {"mint_attempt_window", "installation_api_call_window"}}
    for app, installation in sorted(pairs):
        calls = counts[("installation_api_call_window", app, installation)]
        remints = counts[("mint_attempt_window", app, installation, "installation_unauthorized")]
        result += metric("remint_ratio_defined", int(calls > 0), app=app, installation_id=installation)
        if calls:
            result += metric("remint_ratio", remints/calls, app=app, installation_id=installation)
    return result


def deliveries(data, now):
    fresh(data["observed_at"], now)
    if data["incomplete"] is not False:
        raise ValueError("incomplete delivery report")
    # Exact existing /console/operations.json shape. Ignore steps/IDs entirely.
    total = sum(integer(data[name], 10_000) for name in ("repository_deliveries", "organization_deliveries", "team_steps", "email_deliveries"))
    if total > 10_000:
        raise ValueError("delivery overflow")
    result = ""
    for name in ("unknown", "pending"):
        row = data[name]
        size = integer(row["total"], total)
        over = integer(row["over_threshold"], size)
        if integer(row["missing_timestamp"], total) or integer(row["future_timestamp"], total):
            raise ValueError("incomplete timing")
        result += metric("delivery_steps", size, state=name)
        result += metric("delivery_over_threshold", over, state=name)
        age = row["oldest_age_seconds"]
        if size and age is None:
            raise ValueError("missing age")
        if age is not None:
            result += metric("delivery_oldest_age_seconds", integer(age), state=name)
    seen = set()
    for row in bounded_list(data["blocked"], 44):
        kind = member(row["kind"], {"repo", "org", "team", "email"})
        reason = member(row["reason"], REASONS)
        if (kind, reason) in seen:
            raise ValueError("duplicate blocked group")
        seen.add((kind, reason))
        result += metric("blocked_steps", integer(row["count"], total), kind=kind, reason=reason)
    return result


def journals(data, now):
    fresh(data["observed_at"], now)
    if data["complete"] is not True or data["selection"] != "retained_completed":
        raise ValueError("invalid journal selection")
    rows = bounded_list(data["rows"], 100)
    values = [integer(row["entries"], 100_000) for row in rows]
    if not values or any(value == 0 for value in values):
        raise ValueError("journal sample absent or already purged")
    return metric("admit_journal_retained_samples", len(values)) + metric("admit_journal_entries_p95", p95(values))


def budget_rows(data, now):
    fresh(data["observed_at"], now)
    if data["complete"] is not True:
        raise ValueError("incomplete budget capture")
    identities = set()
    rows = bounded_list(data["snapshots"], MAX_SERIES)
    for row in rows:
        fresh(row["observed_at"], now)
        app = member(row["app"], {"repo", "org"})
        account = integer(row["account_id"])
        if not account or (app, account) in identities:
            raise ValueError("invalid budget identity")
        identities.add((app, account))
    return rows


def budgets(data, now):
    result = ""
    for row in budget_rows(data, now):
        app, account = row["app"], row["account_id"]
        closed = sum(1 for bucket in bounded_list(row["buckets"], 16) if bucket["closed_until"] is not None and stamp(bucket["closed_until"]) > now)
        if row.get("content_closed_until") and stamp(row["content_closed_until"]) > now:
            closed += 1
        result += metric("budget_closed_buckets", closed, app=app, account_id=account)
        # A positive observed restriction is useful immediately, before a full
        # day is covered. Never synthesize a zero from incomplete coverage.
        try:
            counts = row["limit_counts"]
            started = stamp(counts["coverage_started_at"])
            if counts["incomplete"] is not False or integer(counts["future_observations"]) or started > now:
                raise ValueError("uncertain restriction observations")
            if integer(counts["spammed"]):
                result += metric("budget_spammed_observed", 1, app=app, account_id=account)
        except (KeyError, TypeError, ValueError, OverflowError):
            pass  # Availability of the full window is reported separately below.
    return result


def budget_limits(data, now):
    result = ""
    for row in budget_rows(data, now):
        app, account = row["app"], row["account_id"]
        counts = row["limit_counts"]
        if counts["incomplete"] is not False or integer(counts["future_observations"]):
            raise ValueError("incomplete budget observations")
        started = stamp(counts["coverage_started_at"])
        if started > now or now - started < 86400:
            raise ValueError("budget window not yet covered")
        for reason in ("rate_limited", "spammed"):
            result += metric("budget_limit_24h", integer(counts[reason]), app=app, account_id=account, reason=reason)
    return result


def collect(envelope, now):
    outputs = metric("collector_observed_at_seconds", now)
    for source in SOURCES:
        try:
            fresh(envelope["generated_at"], now)
            rendered = globals()[source](envelope["budgets" if source == "budget_limits" else source], now)
            if rendered.count("\n") > MAX_SERIES:
                raise ValueError("rendered source series overflow")
            outputs += metric("source_available", 1, source=source) + rendered
        except (ValueError, KeyError, TypeError, OverflowError):
            outputs += metric("source_available", 0, source=source)
    baseline = envelope.get("baseline")
    try:
        if not isinstance(baseline["evidence_sha256"], str) or len(baseline["evidence_sha256"]) != 64 or any(c not in "0123456789abcdef" for c in baseline["evidence_sha256"]):
            raise ValueError("baseline provenance absent")
        clock = member(baseline["clock"], CLOCKS)
        duration = baseline["admission_latency_p95_seconds"]
        if type(duration) not in (int, float) or not math.isfinite(duration) or not 0 < duration <= 60:
            raise ValueError("baseline latency invalid")
        entries = integer(baseline["admit_journal_entries_p95"], 100_000)
        if not entries:
            raise ValueError("baseline journal invalid")
        outputs += metric("baseline_available", 1)
        outputs += metric("baseline_admission_latency_p95_seconds", duration, clock=clock)
        outputs += metric("baseline_admit_journal_entries_p95", entries)
    except (ValueError, KeyError, TypeError):
        outputs += metric("baseline_available", 0)
    # Declare every family once; these are snapshot/window gauges, never counters.
    names = sorted({line.split("{", 1)[0].split(" ", 1)[0] for line in outputs.splitlines()})
    declarations = "".join(f"# HELP {name} Bounded P1 operational snapshot; see source availability and capture time.\n# TYPE {name} gauge\n" for name in names)
    return declarations + outputs


def load(path):
    with path.open("rb") as file:
        raw = file.read(MAX_BYTES + 1)
    if len(raw) > MAX_BYTES:
        raise ValueError("input overflow")
    def unique(pairs):
        result = {}
        for key, value in pairs:
            if key in result:
                raise ValueError("duplicate JSON field")
            result[key] = value
        return result
    value = json.loads(raw, object_pairs_hook=unique, parse_constant=lambda _: (_ for _ in ()).throw(ValueError("nonfinite JSON")))
    if not isinstance(value, dict):
        raise ValueError("invalid envelope")
    return value


def write_atomic(path, text):
    if path.is_symlink():
        raise ValueError("symlink output")
    with tempfile.NamedTemporaryFile(mode="w", dir=path.parent, prefix=".p1-metrics-", delete=False) as output:
        temporary = Path(output.name)
        try:
            output.write(text)
            output.flush()
            os.fsync(output.fileno())
            os.replace(temporary, path)
        finally:
            temporary.unlink(missing_ok=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--input", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    now = int(time.time())
    try:
        envelope = load(args.input)
    except (OSError, ValueError):
        envelope = {}  # Replace stale healthy output, never retain it on failure.
    write_atomic(args.output, collect(envelope, now))


if __name__ == "__main__":
    main()
