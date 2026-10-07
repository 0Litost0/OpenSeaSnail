#!/usr/bin/env python3
"""Validate v1 contracts, references and outcome semantics (not an API runner)."""
from pathlib import Path
import argparse
import copy
import json
import os
import re
import sys
from jsonschema import Draft202012Validator

ROOT = Path(__file__).resolve().parents[1]
DOC = ROOT / 'docs'


def read(path):
    return json.loads(path.read_text())


def require(condition, message):
    if not condition:
        raise ValueError(message)


def schema_check(kind, value):
    schema = read(ROOT / f'contracts/{kind}.schema.json')
    Draft202012Validator.check_schema(schema)
    errors = sorted(Draft202012Validator(schema).iter_errors(value), key=lambda e: str(e.path))
    require(not errors, f'{kind}: schema violation at {list(errors[0].path) if errors else []}')
    # Deliberately do not echo invalid values: contract diagnostics can contain secrets.
    serialized = json.dumps(value, ensure_ascii=False)
    require(not re.search(r'ss_live_[A-Za-z0-9_-]{64}|Bearer\s+[A-Za-z0-9._~+/=-]{16,}|(?:POC|API_TEST)_[A-Z_]*CANARY', serialized, re.I),
            f'{kind}: prohibited credential/canary content')


def extension():
    directory = os.environ.get('API_TEST_EXTENSION')
    if not directory:
        return None
    directory = Path(directory)
    require(directory.is_absolute() and directory.resolve() == directory and directory.is_relative_to(ROOT), 'invalid extension directory')
    value = read(directory / 'extension.json')
    require(set(value) == {'scope', 'cases', 'preconditions', 'assertions', 'spec_files'} and value['scope'] == 'isolated-extension-v1', 'invalid extension scope')
    require(value['cases'] and value['spec_files'], 'empty extension')
    for file in value['spec_files']:
        resolved = (directory / file).resolve()
        require(resolved.is_relative_to(directory) and resolved.is_file() and resolved.name.endswith('.spec.ts'), 'invalid extension spec')
    for case in value['cases']:
        require(not case['required'] and not case['quick'], 'extension cannot change default gate')
        case['asset_refs'] = list(dict.fromkeys(case['asset_refs'] + [str((directory / 'extension.json').relative_to(ROOT))] + [str((directory / file).relative_to(ROOT)) for file in value['spec_files']]))
    return value


def read_catalog():
    value = read(ROOT / 'case-catalog.json')
    extra = extension()
    if extra:
        value['cases'] += extra['cases']
    return value


def catalog_check(value):
    schema_check('catalog', value)
    cases = value['cases']
    ids = [case['case_id'] for case in cases]
    require(len(ids) == len(set(ids)), 'duplicate case ID')
    required = [case['case_id'] for case in cases if case['required']]
    require(len(required) == 22 and sum(case['quick'] for case in cases) == 17, 'required/quick count mismatch')
    extra = extension()
    extra_ids = {case['case_id'] for case in extra['cases']} if extra else set()
    require([case['case_id'] for case in cases if not case['required'] and case['case_id'] not in extra_ids] == ['PROVIDER-001'], 'optional case mismatch')
    snapshot = read(ROOT / 'contracts/acceptance-required.v1.json')
    require(snapshot['catalog_version'] == value['catalog_version'] and snapshot['required_case_ids'] == required,
            'independent acceptance snapshot mismatch')
    pre = read(ROOT / 'contracts/preconditions.v1.json')['preconditions']
    assertions = read(ROOT / 'contracts/assertions.v1.json')['assertions']
    design = (DOC / 'design.md').read_text()
    matrix = re.findall(r'^\| ((?:AUTH|TOKEN|DICT|ASR|CLEAN|RECOVERY|SHERPA|SYSTEM|PROVIDER)-\d{3}) \|', design, re.M)
    require([sid for sid in ids if sid not in extra_ids] == matrix and len(matrix) == 23, 'catalog differs from design matrix')
    if extra:
        require(not extra_ids.intersection(matrix), 'extension shadows default case')
        require(not set(extra['preconditions']).intersection(pre) and not set(extra['assertions']).intersection(assertions), 'extension shadows default registry')
        pre = {**pre, **extra['preconditions']}
        assertions = {**assertions, **extra['assertions']}
    for case in cases:
        require(case['case_id'] in extra_ids or case['quick'] == (case['level'] == 'deterministic') and (not case['quick'] or case['required']), 'quick level/required mismatch')
        require(case['assertion_ref'] in assertions, 'unknown assertion reference')
        require(assertions[case['assertion_ref']]['design_case_id'] == case['case_id'], 'assertion belongs to another case')
        require(assertions[case['assertion_ref']]['business_promise'] == case['purpose'], 'assertion/catalog promise mismatch')
        require(all(ref in pre for ref in case['precondition_refs']), 'unknown precondition reference')
        for ref in case['asset_refs'] + [case['requirement_ref'], case['design_ref']]:
            path = (ROOT / ref).resolve()
            require((path.is_relative_to(ROOT) or path.is_relative_to(DOC)) and path.is_file(), 'missing/out-of-bound asset or document reference')
    return ids


def outcome(value):
    incomplete = bool(value['missing_case_ids'])
    failed = value['report_status'] == 'failed'
    incomplete |= value['report_status'] == 'not_published'
    for error in value['errors']:
        incomplete |= error['category'] in ('environment', 'configuration', 'interrupted')
        failed |= error['category'] not in ('environment', 'configuration', 'interrupted')
    for case in value['cases']:
        attempts = case['attempts']
        failed |= case['flaky']
        for attempt in attempts:
            incomplete |= attempt['execution_status'] in ('not_run', 'environment_error', 'interrupted')
            incomplete |= attempt['teardown_status'] == 'unknown'
            failed |= attempt['execution_status'] == 'failed' or attempt['teardown_status'] == 'failed'
            for error in attempt['errors']:
                incomplete |= error['category'] in ('environment', 'configuration', 'interrupted')
                failed |= error['category'] not in ('environment', 'configuration', 'interrupted')
    return ('incomplete', 2) if incomplete else ('failed', 1) if failed else ('passed', 0)


def config_check(case_id, config, exact_replay=False):
    catalog = read_catalog()
    require(config['catalog_version'] == catalog['catalog_version'], 'unknown configuration catalog version')
    require(case_id in {case['case_id'] for case in catalog['cases']}, 'unknown configuration case')
    require(config['assertion_version'] == case_id + '-v1', 'unknown configuration assertion version')
    if case_id == 'SHERPA-001':
        require(config['runtime']['mode'] == 'sherpa', 'required Sherpa case cannot use deterministic runtime')
        require(config['runtime']['model_id'] == 'sensevoice-small-sherpa-int8', 'required Sherpa model mismatch')
    if exact_replay:
        require(not config['build']['dirty'] or config['build']['source_patch'] is not None, 'dirty exact replay lacks source asset')
    if config['provider']['mode'] == 'remote':
        require(config['credential_refs'], 'remote configuration requires secure credential reference')
    if config['fault']:
        require(config['fault']['base_commit'] == config['build']['commit'], 'fault base build mismatch')
        require(config['fault']['binary_sha256'] == config['build']['binary_sha256'], 'fault binary mismatch')


def result_check(value):
    schema_check('result', value)
    catalog = read_catalog()
    ids = {case['case_id'] for case in catalog['cases']}
    expected_required = read(ROOT / 'contracts/acceptance-required.v1.json')['required_case_ids']
    require(value['catalog_version'] == catalog['catalog_version'], 'unknown catalog version')
    require(value['required_case_ids'] == expected_required, 'required IDs differ from independent snapshot')
    case_ids = [case['case_id'] for case in value['cases']]
    require(len(case_ids) == len(set(case_ids)) and set(case_ids) <= ids, 'duplicate or unknown result case')
    selection = value['selection']
    require(set(selection['requested_case_ids']) <= ids, 'unknown selected case')
    actual = {case['case_id'] for case in value['cases'] if any(a['execution_status'] != 'not_run' for a in case['attempts'])}
    require(set(selection['actual_case_ids']) == actual, 'actual selection differs from attempts')
    if selection['mode'] == 'acceptance':
        require(set(selection['requested_case_ids']) == set(expected_required), 'acceptance selection must include independent required set')
        require(set(value['missing_case_ids']) == set(expected_required) - actual, 'acceptance missing set mismatch')
    else:
        require(set(case_ids) == set(selection['requested_case_ids']), 'selected case result missing')
        require(not value['missing_case_ids'], 'subset missing IDs belong in not_run attempts')
    for case in value['cases']:
        attempts = case['attempts']
        require([a['retry_index'] for a in attempts] == list(range(len(attempts))), 'attempt indices must retain chronological history')
        require(len({a['attempt_id'] for a in attempts}) == len(attempts), 'duplicate attempt ID')
        flaky = attempts[-1]['execution_status'] == 'passed' and any(a['execution_status'] == 'failed' for a in attempts[:-1])
        require(case['flaky'] == flaky, 'flaky flag differs from attempt history')
        for attempt in attempts:
            config_check(case['case_id'], attempt['config'])
            require(len({s['step_id'] for s in attempt['steps']}) == len(attempt['steps']), 'duplicate step ID')
            if attempt['execution_status'] in ('failed', 'environment_error', 'interrupted') or attempt['teardown_status'] == 'failed':
                require(attempt['errors'], 'failed attempt requires diagnostics')
            if attempt['execution_status'] == 'passed':
                require(attempt['steps'], 'passed attempt must contain executed business steps')
                require(all(s['status'] == 'passed' for s in attempt['steps']), 'passed attempt contains nonpassed step')
    require((value['gate'], value['exit_code']) == outcome(value), 'gate/exit code differs from strict outcome')


def replay_check(value):
    schema_check('replay', value)
    config_check(value['case_id'], value, exact_replay=True)


def check_examples():
    validators = {'result': result_check, 'replay': replay_check}
    valid_files = sorted((ROOT / 'contracts/examples/valid').glob('*.json'))
    for path in valid_files:
        validators['result' if path.name.startswith('result-') else 'replay'](read(path))
    invalid = read(ROOT / 'contracts/examples/invalid/index.json')
    for example in invalid:
        try:
            validators[example['contract']](read(ROOT / 'contracts/examples/invalid' / example['file']))
        except ValueError:
            continue
        raise ValueError('invalid contract example unexpectedly accepted: ' + example['file'])
    # Catalog mutations prove referential checking, rather than just counting entries.
    original = read(ROOT / 'case-catalog.json')
    for field, new in [('case_id', original['cases'][1]['case_id']), ('assertion_ref', 'unknown'),
                       ('precondition_refs', ['unknown']), ('asset_refs', ['assets/nonexistent.json'])]:
        changed = copy.deepcopy(original)
        changed['cases'][0][field] = new
        try:
            catalog_check(changed)
        except ValueError:
            continue
        raise ValueError('invalid catalog mutation accepted: ' + field)
    return len(valid_files), len(invalid)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('command', choices=['catalog', 'examples', 'all', 'result', 'replay'])
    parser.add_argument('file', nargs='?', type=Path)
    args = parser.parse_args()
    try:
        if args.command in ('catalog', 'examples', 'all'):
            ids = catalog_check(read_catalog())
            print(f'catalog: {len(ids)} unique / 22 required / 17 quick / {len(ids)-22} optional; references valid')
        if args.command in ('examples', 'all'):
            valid, invalid = check_examples()
            print(f'contracts: {valid} valid examples accepted; {invalid} invalid examples rejected; 4 catalog mutations rejected')
        if args.command in ('result', 'replay'):
            require(args.file is not None, 'input file required')
            (result_check if args.command == 'result' else replay_check)(read(args.file))
            print(args.command + ': valid')
    except (ValueError, OSError) as error:
        # OSError messages can include user-controlled paths; keep safe diagnostics.
        print(str(error) if isinstance(error, ValueError) else 'contract input unavailable', file=sys.stderr)
        return 2
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
