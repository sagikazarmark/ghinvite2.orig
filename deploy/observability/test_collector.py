import copy
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest

spec = importlib.util.spec_from_file_location("collector", Path(__file__).with_name("collector.py"))
c = importlib.util.module_from_spec(spec)
spec.loader.exec_module(c)
NOW = 2_000_000_000


def envelope():
    age = {"total": 1, "over_threshold": 1, "oldest_age_seconds": 8000, "missing_timestamp": 0, "future_timestamp": 0}
    return {"generated_at": NOW, "logs": {"complete": True, "window_start": NOW-60, "window_end": NOW, "records": []},
            "deliveries": {"observed_at": NOW, "incomplete": False, "repository_deliveries": 2, "organization_deliveries": 0, "team_steps": 0, "email_deliveries": 0,
                           "unknown": age, "pending": age, "blocked": [{"kind": "repo", "reason": "installation_unavailable", "count": 1}], "steps": [{"id": "PRIVATE_DELIVERY"}]},
            "journals": {"observed_at": NOW, "complete": True, "selection": "retained_completed", "rows": [{"entries": 20}]},
            "budgets": {"observed_at": NOW, "complete": True, "snapshots": [{"app": "org", "account_id": 8, "observed_at": NOW, "buckets": [{"closed_until": NOW+20}],
                        "limit_counts": {"coverage_started_at": NOW-86400, "incomplete": False, "future_observations": 0, "rate_limited": 2, "spammed": 1}}]}}


class Collector(unittest.TestCase):
    def test_actual_shape_and_safe_output(self):
        value = envelope()
        value["logs"]["records"] = [{"event": "admission_rpc_completed", "occurred_at": NOW, "outcome": "accepted", "clock": "native_monotonic", "clock_valid": True, "elapsed_ms": 125,
                                     "justification": "PRIVATE_BODY", "link_id": "PRIVATE_LINK"}]
        out = c.collect(value, NOW)
        for source in c.SOURCES:
            self.assertIn(f'p1_source_available{{source="{source}"}} 1', out)
        self.assertIn('p1_admission_latency_p95_seconds{clock="native_monotonic"} 0.125', out)
        self.assertIn('p1_budget_limit_24h{account_id="8",app="org",reason="spammed"} 1', out)
        self.assertNotIn("PRIVATE", out)
        self.assertIn('p1_baseline_available 0', out)

    def test_remint_denominator_and_zero_unknown(self):
        value = envelope()
        mint = {"event": "installation_mint_attempt", "occurred_at": NOW, "app": "repo", "installation_id": 9, "cause": "installation_unauthorized"}
        value["logs"]["records"] = [mint]
        out = c.collect(value, NOW)
        self.assertIn('p1_remint_ratio_defined{app="repo",installation_id="9"} 0', out)
        self.assertNotIn('p1_remint_ratio{', out)
        value["logs"]["records"] += [{"event": "installation_api_call", "occurred_at": NOW, "app": "repo", "installation_id": 9}] * 100
        self.assertIn('p1_remint_ratio{app="repo",installation_id="9"} 0.01', c.collect(value, NOW))

    def test_stale_and_future_capture_unavailable(self):
        for timestamp in (NOW-301, NOW+1):
            value = envelope()
            value["generated_at"] = timestamp
            out = c.collect(value, NOW)
            for source in c.SOURCES:
                self.assertIn(f'p1_source_available{{source="{source}"}} 0', out)
            self.assertNotIn('p1_delivery_steps', out)

    def test_missing_source_is_not_zero_health(self):
        out = c.collect({}, NOW)
        self.assertNotIn('p1_delivery_steps', out)
        self.assertEqual(out.count('p1_source_available{'), 5)

    def test_incomplete_log_window_refused_whole(self):
        value = envelope()
        value["logs"]["complete"] = False
        self.assertIn('p1_source_available{source="logs"} 0', c.collect(value, NOW))

    def test_record_and_series_overflow(self):
        value = envelope()
        event = {"event": "installation_api_call", "occurred_at": NOW, "app": "repo", "installation_id": 1}
        value["logs"]["records"] = [event] * 10_001
        self.assertIn('p1_source_available{source="logs"} 0', c.collect(value, NOW))
        value["logs"]["records"] = [dict(event, installation_id=i) for i in range(1, 1002)]
        self.assertIn('p1_source_available{source="logs"} 0', c.collect(value, NOW))

    def test_derived_ratio_series_also_obey_output_bound(self):
        value = envelope()
        value["logs"]["records"] = [{"event": "installation_api_call", "occurred_at": NOW, "app": "repo", "installation_id": i} for i in range(1, 601)]
        out = c.collect(value, NOW)
        self.assertIn('p1_source_available{source="logs"} 0', out)
        self.assertNotIn('p1_installation_api_call_window{', out)

    def test_private_label_rejected_without_echo(self):
        value = envelope()
        value["logs"]["records"] = [{"event": "webhook_signature_failure", "occurred_at": NOW, "app": "PRIVATE_APP", "reason": "invalid"}]
        out = c.collect(value, NOW)
        self.assertNotIn("PRIVATE", out)
        self.assertIn('p1_source_available{source="logs"} 0', out)

    def test_invalid_clock_explicit_without_fake_latency(self):
        value = envelope()
        value["logs"]["records"] = [{"event": "admission_rpc_completed", "occurred_at": NOW, "outcome": "interrupted", "clock": "worker_host", "clock_valid": False}]
        out = c.collect(value, NOW)
        self.assertIn('p1_admission_clock_invalid_window{clock="worker_host"} 1', out)
        self.assertNotIn('p1_admission_latency_p95_seconds', out)

    def test_delivery_missing_timing_and_overflow_refused(self):
        for field, value in [("missing_timestamp", 1), ("future_timestamp", 1), ("oldest_age_seconds", None)]:
            data = envelope()
            data["deliveries"]["unknown"] = dict(data["deliveries"]["unknown"], **{field: value})
            self.assertIn('p1_source_available{source="deliveries"} 0', c.collect(data, NOW))
        data = envelope()
        data["deliveries"]["repository_deliveries"] = 10_001
        self.assertIn('p1_source_available{source="deliveries"} 0', c.collect(data, NOW))

    def test_journal_sample_absent_purged_and_truncated_refused(self):
        for rows in ([], [{"entries": 0}], [{"entries": 2}] * 101):
            value = envelope()
            value["journals"]["rows"] = rows
            out = c.collect(value, NOW)
            self.assertIn('p1_source_available{source="journals"} 0', out)
            self.assertNotIn('p1_admit_journal_entries_p95', out)

    def test_budget_legacy_partial_and_stale_refused(self):
        for mutate in (lambda row: row.pop('limit_counts'), lambda row: row.update(observed_at=NOW-301), lambda row: row['limit_counts'].update(incomplete=True), lambda row: row['limit_counts'].update(coverage_started_at=NOW-3600)):
            value = envelope()
            mutate(value["budgets"]["snapshots"][0])
            self.assertIn('p1_source_available{source="budget_limits"} 0', c.collect(value, NOW))

    def test_positive_closure_and_spam_are_visible_before_complete_day(self):
        value = envelope()
        value["budgets"]["snapshots"][0]["limit_counts"]["coverage_started_at"] = NOW-60
        out = c.collect(value, NOW)
        self.assertIn('p1_budget_closed_buckets{account_id="8",app="org"} 1', out)
        self.assertIn('p1_budget_spammed_observed{account_id="8",app="org"} 1', out)
        self.assertIn('p1_source_available{source="budget_limits"} 0', out)
        self.assertNotIn('p1_budget_limit_24h{', out)

    def test_baseline_requires_provenance(self):
        value = envelope()
        value['baseline'] = {'evidence_sha256': 'a'*64, 'clock': 'native_monotonic', 'admission_latency_p95_seconds': .5, 'admit_journal_entries_p95': 20}
        self.assertIn('p1_baseline_available 1', c.collect(value, NOW))
        value['baseline']['evidence_sha256'] = 'PRIVATE'
        self.assertIn('p1_baseline_available 0', c.collect(value, NOW))

    def test_duplicate_json_nonfinite_and_body_limit_refused(self):
        with tempfile.TemporaryDirectory() as folder:
            path = Path(folder) / 'input'
            for text in ('{"logs":{},"logs":{}}', '{"generated_at":NaN}', ' ' * (c.MAX_BYTES+1)):
                path.write_text(text)
                with self.assertRaises(ValueError):
                    c.load(path)

    def test_atomic_unavailable_replaces_old_health(self):
        with tempfile.TemporaryDirectory() as folder:
            path = Path(folder) / 'p1.prom'
            c.write_atomic(path, c.collect(envelope(), NOW))
            c.write_atomic(path, c.collect({}, NOW))
            self.assertNotIn('p1_delivery_steps', path.read_text())
            self.assertEqual(path.read_text().count('p1_source_available{'), 5)
            self.assertEqual(list(Path(folder).glob('.p1-metrics-*')), [])


if __name__ == '__main__':
    unittest.main()
