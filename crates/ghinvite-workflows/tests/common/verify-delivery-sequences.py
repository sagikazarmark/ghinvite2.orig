#!/usr/bin/env python3
"""Independently replay AC-DLV-3 runtime artifacts; no Rust transition helper imports."""
import argparse
import copy
import json
from pathlib import Path

ALPHABET = ['created','no_content','unauthorized','forbidden','not_found','validation','already_exists','spammed','policy','timeout','primary_limit','secondary_limit']
ACTIONS = ['create','replay','status','present','absent','accepted','expired','wait_deadline']
TERMINAL = {'created','already_pending','already_satisfied','upgraded','skipped_custom_role','aborted','failed'}
CONTROL_NAMES = {'missing_outcome','replaced_confirmed_receipt','duplicate_physical_write','competing_settlement','duplicate_durable_settlement'}


def generated(seed):
    current = seed
    def random():
        nonlocal current
        mask = (1 << 64) - 1
        current = (current + 0x9e3779b97f4a7c15) & mask
        value = current
        value = ((value ^ (value >> 30)) * 0xbf58476d1ce4e5b9) & mask
        value = ((value ^ (value >> 27)) * 0x94d049bb133111eb) & mask
        return value ^ (value >> 31)
    replies = [ALPHABET[seed % 12]] + [ALPHABET[random() % 12] for _ in range(5)] + ['created']
    actions = ['create','wait_deadline','create','status'] + [ACTIONS[random() % 8] for _ in range(14)] + ['wait_deadline','create','present','absent','accepted','expired','replay','status']
    return {'seed':seed,'replies':replies,'actions':actions,'lose_run_acks':seed % 12 == 0 and (seed // 12) % 2 == 1}


def check_audits(sample, kind, complete=False):
    owner = sample['snapshot']
    command = owner['create']['command']
    root_id = (f"org:{command['request_id']}:{command['organization']['org_id']}:{command['delivery_generation']}"
               if kind == 'organization' else command['invitation_id'])
    targets = {root_id: ('organization_delivery' if kind == 'organization' else 'github_invitation', owner)}
    for team_id, team in owner.get('teams', {}).items():
        targets[root_id + '/team:' + team_id] = ('team_delivery', team)
    counts = {}
    for event in sample['audit_settlements']:
        target = event['target_id']
        assert target in targets, 'audit for foreign target'
        expected_kind, record = targets[target]
        settlement = record.get('settlement')
        assert event['target_kind'] == expected_kind and settlement is not None
        counts[target] = counts.get(target, 0) + 1
        assert counts[target] <= 1, 'multiple durable settlement facts for one target'
        expected_type = ('org_invitation.accepted' if expected_kind == 'organization_delivery' and settlement['state'] == 'accepted'
                         else 'team_membership.added' if expected_kind == 'team_delivery' and settlement['state'] == 'accepted' and record['create']['outcome'].get('via', {}).get('kind') == 'organization_invitation'
                         else 'invitation.' + settlement['state'])
        assert event['event_type'] == expected_type
        if expected_kind != 'team_delivery':
            assert event['id'] == settlement['event']['id'], 'audit/owner event identities differ'
    if complete:
        assert all(record.get('settlement') is None or counts.get(target, 0) == 1
                   for target, (_, record) in targets.items()), 'target settlement audit never converged'


def check_case(report, kind):
    assert report['status'] == 'passed', 'case is incomplete or failed'
    plan = generated(report['seed'])
    assert report['plan'] == plan, 'seed does not reproduce the actual recorded plan'
    steps = report['steps']
    assert len(steps) in {len(plan['actions']), len(plan['actions']) + 1}, 'missing generated actions'
    assert [s['action'] for s in steps[:len(plan['actions'])]] == plan['actions']
    confirmed, settled, frozen_mutations = {}, {}, {}
    last_revision = 0
    previous = None
    controls = set(report['controls'])
    assert CONTROL_NAMES & controls, 'oracle controls did not execute'
    for index, step in enumerate(steps):
        assert step['step'] == index, 'nonconsecutive trace prefix'
        sample = step['sample']
        owner = sample['snapshot']
        assert owner['revision'] >= last_revision
        last_revision = owner['revision']
        records = {'root': owner}
        records.update({'team:' + key: value for key, value in owner.get('teams', {}).items()})
        for target, record in records.items():
            create = record['create']
            outcome = create['outcome']
            assert isinstance(outcome, dict) and isinstance(outcome.get('kind'), str), 'missing single create outcome'
            name = outcome['kind']
            assert name in TERMINAL | {'planned','held','submitted','blocked','throttled','outcome_unknown'}
            if target in confirmed:
                assert confirmed[target] == create, 'terminal create receipt was replaced'
                assert frozen_mutations[target] == sample['mutations'].get(target, 0), 'write repeated after confirmation'
            elif name in TERMINAL:
                confirmed[target] = copy.deepcopy(create)
                frozen_mutations[target] = sample['mutations'].get(target, 0)
            settlement = record.get('settlement')
            if target in settled:
                assert settled[target] == settlement, 'settlement was replaced or removed'
            elif settlement is not None:
                assert name in {'created','already_pending'}
                assert settlement['state'] in {'accepted','declined','expired','cancelled'}
                settled[target] = copy.deepcopy(settlement)
        consumed = sample['consumed']
        assert consumed == plan['replies'][:len(consumed)], 'actual provider replies differ from generated script'
        assert consumed and consumed[0] == plan['replies'][0], 'forced first response never crossed HTTP'
        assert sum(r in {'created','no_content'} for r in consumed) <= 1
        check_audits(sample, kind)
        calls = sample['counters']['calls']
        if kind == 'repository':
            writes = [c for c in calls if c['method'] in {'PUT','PATCH','DELETE'}]
            assert len(writes) == sample['mutations']['root'] == len(consumed)
            assert [c['fence'] for c in writes] == [[i + 1, 0] for i in range(len(writes))], 'physical generation/retry fence mismatch'
        else:
            writes = [c for c in calls if c['method'] == 'POST' and c['path'] == '/orgs/acme/invitations']
            assert len(writes) == sample['mutations']['root'] == len(consumed)
            team_writes = [c for c in calls if c['method'] in {'PUT','DELETE'} and c['path'] == '/orgs/acme/teams/team-5/memberships/alice']
            assert len(team_writes) == sample['mutations']['team:5']
            budget = sample['counters']['budget']
            assert budget['content_used_hour'] >= sum(r in {'created','no_content','timeout'} for r in consumed)
            assert budget['limit_counts']['spammed'] == consumed.count('spammed')
            assert budget['limit_counts']['rate_limited'] == consumed.count('primary_limit') + consumed.count('secondary_limit')
        if previous is not None and step['action'] == 'absent' and previous['snapshot']['create']['outcome']['kind'] == 'outcome_unknown':
            assert owner['create']['outcome']['kind'] == 'outcome_unknown', 'absence resolved unknown create'
        previous = sample
    final = steps[-1]['sample']
    check_audits(final, kind, complete=True)
    if plan['lose_run_acks']:
        assert final['counters']['lost_run_acknowledgements'] == 3
    return final


def reject_mutations(reports):
    control_count = 0
    kind, original = reports[0]
    changed = copy.deepcopy(original)
    changed['plan']['actions'][0] = 'status'
    candidates = [(kind, changed)]
    for kind, report in reports:
        for index, step in enumerate(report['steps']):
            sample = step['sample']
            if index and sample['snapshot']['create']['outcome']['kind'] in TERMINAL:
                changed = copy.deepcopy(report)
                changed['steps'][index]['sample']['snapshot']['create']['outcome'] = {'kind':'failed','reason':{'kind':'changed_control'}}
                candidates.append((kind, changed))
                changed = copy.deepcopy(report)
                changed['steps'][index]['sample']['mutations']['root'] += 1
                candidates.append((kind, changed))
                break
        settled = next((i for i, step in enumerate(report['steps']) if step['sample']['snapshot'].get('settlement') is not None), None)
        if settled is not None:
            changed = copy.deepcopy(report)
            changed['steps'][settled]['sample']['audit_settlements'] *= 2
            if not changed['steps'][settled]['sample']['audit_settlements']:
                changed['steps'][settled]['sample']['audit_settlements'] = [{'id':'first'}, {'id':'second'}]
            candidates.append((kind, changed))
            for event in report['steps'][settled]['sample']['audit_settlements']:
                changed = copy.deepcopy(report)
                changed['steps'][settled]['sample']['audit_settlements'].append(copy.deepcopy(event))
                candidates.append((kind, changed))
            later = min(settled + 1, len(report['steps']) - 1)
            changed = copy.deepcopy(report)
            changed['steps'][later]['sample']['snapshot']['settlement']['state'] = 'contradictory_control'
            candidates.append((kind, changed))
    for kind, candidate in candidates:
        try:
            check_case(candidate, kind)
        except (AssertionError, KeyError, TypeError, ValueError):
            control_count += 1
        else:
            raise AssertionError('mutated runtime evidence accepted')
    assert control_count >= 5
    return control_count


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('directory', type=Path)
    parser.add_argument('--self-test', action='store_true')
    parser.add_argument('--output', type=Path)
    args = parser.parse_args()
    reports, result = [], {'scope':'Independent replay of generated native owner/HTTP evidence; no live or deployment acceptance claim.','kinds':{}}
    for kind in ['repository','organization']:
        summary = json.loads((args.directory / kind / 'summary.json').read_text())
        assert summary['status'] == 'passed' and summary['kind'] == kind
        actual_alphabet, controls = set(), set()
        settled = multi_response = 0
        for seed in summary['seeds']:
            path = args.directory / kind / ('seed-%016x' % seed)
            report = json.loads((path / 'results.json').read_text())
            assert json.loads((path / 'plan.json').read_text()) == generated(seed)
            final = check_case(report, kind)
            reports.append((kind, report))
            actual_alphabet.update(final['consumed'])
            controls.update(report['controls'])
            settled += final['snapshot'].get('settlement') is not None
            multi_response += len(final['consumed']) > 1
        assert len(summary['seeds']) == len(set(summary['seeds'])) == summary['cases']
        assert actual_alphabet == set(summary['consumed_response_alphabet']) == set(ALPHABET)
        assert CONTROL_NAMES <= controls == set(summary['controls'])
        if kind == 'organization':
            assert {'undercharged_uncertain_or_accepted_write','duplicate_team_durable_settlement'} <= controls
        assert settled == summary['settled_cases'] > 0
        assert multi_response == summary['multi_response_cases'] > 0
        result['kinds'][kind] = {'cases':summary['cases'],'settled_cases':settled,'multi_response_cases':multi_response,'consumed_alphabet':sorted(actual_alphabet),'controls':sorted(controls)}
    if args.self_test:
        result['actual_report_mutation_controls'] = reject_mutations(reports)
    result['status'] = 'passed'
    if args.output:
        args.output.write_text(json.dumps(result, indent=2) + '\n')
    print(json.dumps(result, indent=2))


if __name__ == '__main__':
    main()
