#!/usr/bin/env python3
"""Correlate local cutover report with retained raw state; no hosted claim."""
import argparse
import copy
import hashlib
import json
from pathlib import Path


def verify(data):
    r = data['results']
    assert r['passed'] is True and r['complete_eof'] is True
    for flag in ('old_pinned_completed', 'fence_unchanged', 'ledger_unchanged', 'budget_native', 'receipt_replay_exact'):
        assert r[flag] is True, flag
    assert r['old_deployment'] != r['new_deployment']
    for name, field in [('journal', 'journal_rows'), ('state', 'state_rows'), ('invocations', 'invocation_rows')]:
        rows = data[name]
        assert rows and len(rows) == r[field] and len(rows) < 10000
        assert all(row['capture_total'] == len(rows) for row in rows), name
    inv = {row['id']: row for row in data['invocations']}
    for name, deployment in [('old_invocation', 'old_deployment'), ('new_invocation', 'new_deployment'), ('native_delivery_invocation', 'new_deployment')]:
        assert inv[r[name]]['status'] == 'completed'
        assert inv[r[name]]['pinned_deployment_id'] == r[deployment]
    pending = next(v for v in data['pending-old'] if v['id'] == r['old_invocation'])
    assert pending['pinned_deployment_id'] == r['old_deployment'] and pending['status'] != 'completed'
    assert not any(v['kind'] == 'native' and v['invocation'] == r['old_invocation'] for v in data['traffic'])
    assert any(v['kind'] == 'native' and v['invocation'] == r['native_delivery_invocation'] for v in data['traffic'])
    calls = data['provider-calls']; assert calls['count'] == len(calls['requests'])
    puts = [c for c in calls['requests'] if c['method'] == 'PUT']
    assert len(puts) == r['provider_puts'] == 2
    assert {c['path'] for c in puts} == {'/repos/acme/api/collaborators/user-91', '/repos/acme/api/collaborators/user-92'}
    assert all(c['status'] == 201 and c['permission'] == 'pull' for c in puts)
    # Every migrated table plus d1_migrations; 0029 adds sweep_digest_days.
    rows = data['rows']; assert len(rows) == r['raw_tables'] == 33
    assert 'sweep_digest_days' in rows
    fences = rows['delivery_attempts']; assert len(fences) == 2
    assert all(f['attempt_generation'] == 1 and f['retryable'] == 0 for f in fences)
    assert {json.loads(f['command'])['requester_id'] for f in fences} == {91, 92}
    deliveries = rows['deliveries']; assert len(deliveries) == 2
    assert all(d['delivery_generation'] == 1 and d['create_outcome'] == 'created' for d in deliveries)
    assert {d['id'] for d in deliveries} == {f['delivery_key'] for f in fences}
    ledger = rows['d1_migrations']; authority = data['authority']
    assert [v['name'] for v in sorted(ledger, key=lambda v: v['id'])] == [m['name'] for m in authority['migrations']]
    budget = json.loads(next(b['snapshot_json'] for b in rows['invite_budget_projection'] if b['app'] == 'repo' and b['account_id'] == 100))
    assert budget['content_used_hour'] == budget['content_used_minute'] == r['delivery_budget_used'] == 2
    children = data['child-close']; assert children == r['children'] and {c['name'] for c in children} == {'native', 'restate', 'github'}
    for c in children:
        assert all(c[f] is True for f in ['alive_before_shutdown', 'closed', 'stdout_eof', 'stderr_eof'])
        assert c['timed_out'] is False and c['bytes'] <= 32 * 1024 * 1024
        assert c['code'] == 0 or c['signal'] == 'SIGTERM'
    assert len(data['streams']) == r['runtime_streams'] == 8
    for text in data['streams'].values():
        assert 'RT0007' not in text and 'Worker\'s code had hung' not in text
    assert r['private_marker_count'] >= 7 and r['private_scan_values'] > len(data['journal'])


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('directory', type=Path)
    parser.add_argument('--self-test', action='store_true')
    args = parser.parse_args(); root = args.directory
    names = ['results', 'journal', 'state', 'invocations', 'rows', 'pending-old', 'traffic', 'provider-calls', 'child-close', 'authority']
    data = {name: json.loads((root / (name + '.json')).read_text()) for name in names}
    data['streams'] = {p.name: p.read_text() for p in root.glob('*.log')}
    verify(data)
    controls = []
    if args.self_test:
        def wrong_pin(d):
            next(i for i in d['invocations'] if i['id'] == d['results']['old_invocation'])['pinned_deployment_id'] = d['results']['new_deployment']
        def early_close(d):
            d['child-close'][0]['alive_before_shutdown'] = False
            d['results']['children'][0]['alive_before_shutdown'] = False
        def duplicate_put(d):
            calls = d['provider-calls']
            calls['requests'].append(copy.deepcopy(next(c for c in calls['requests'] if c['method'] == 'PUT')))
            calls['count'] = len(calls['requests'])
        mutations = {
            'missing-state-row': lambda d: d['state'].pop(),
            'truncated-snapshot-total': lambda d: d['journal'][0].update(capture_total=len(d['journal']) + 1),
            'wrong-old-pin': wrong_pin,
            'duplicate-provider-write': duplicate_put,
            'refunded-fence': lambda d: d['rows']['delivery_attempts'][0].update(retryable=1),
            'changed-ledger': lambda d: d['rows']['d1_migrations'][0].update(name='other.sql'),
            'early-child-exit': early_close,
            'missing-stream': lambda d: d['streams'].pop(next(iter(d['streams']))),
            'late-hung-diagnostic': lambda d: d['streams'].__setitem__(next(iter(d['streams'])), 'RT0007'),
        }
        for name, mutate in mutations.items():
            bad = copy.deepcopy(data); mutate(bad)
            try:
                verify(bad)
            except (AssertionError, KeyError, StopIteration):
                controls.append(name)
            else:
                raise AssertionError('mutation accepted: ' + name)
    print(json.dumps({'passed': True, 'scope': 'Owned-local pinning/raw-row/complete-inventory correlation; actual secret markers are checked inside the runtime before report emission, not reconstructed by this verifier', 'rejected_controls': controls, 'files': {p.name: hashlib.sha256(p.read_bytes()).hexdigest() for p in sorted(root.iterdir()) if p.is_file()}}, indent=2))


if __name__ == '__main__':
    main()
