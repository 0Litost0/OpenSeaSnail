#!/usr/bin/env python3
"""Re-score the new licensed corpus's formal-host observations; never reuse old audio baselines."""
from pathlib import Path
import hashlib
import json
from quality import score, validate_rules

ROOT = Path(__file__).resolve().parents[1]


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main():
    observation_path = ROOT / 'docs/baseline-observations.fleurs-v1.json'
    original = json.loads(observation_path.read_text())
    manifest_path = ROOT / 'assets/manifest.json'
    rules_path = ROOT / 'assets/quality-rules.v3.json'
    manifest = json.loads(manifest_path.read_text())
    rules = json.loads(rules_path.read_text()); validate_rules(rules)
    assert original['dataset_sha256'] == sha(manifest_path), 'baseline belongs to a different dataset'
    assert original['rules_sha256'] == sha(rules_path), 'baseline belongs to different quality rules'
    assert rules['status'] == 'approved' and rules['approval']
    assert original['runtime']['mode'] == 'sherpa'
    assert len(original['observations']) == 6
    analysis = {'version':'fleurs-baseline-analysis-v1', 'observation_sha256':sha(observation_path),
                'dataset_sha256':sha(manifest_path), 'rules_sha256':sha(rules_path),
                'scorer_sha256':sha(Path(__file__).with_name('quality.py')),
                'scope':'Rescoring the licensed corpus formal-host observations; not a new inference run',
                'samples':[]}
    for sample in manifest['samples']:
        reference_path = ROOT / sample['reference_path']
        assert sha(reference_path) == sample['reference_sha256']
        assert sha(ROOT / sample['audio_path']) == sample['audio_sha256']
        observations = [item for item in original['observations'] if item['sample_id'] == sample['id']]
        assert [item['repetition'] for item in observations] == [1,2,3]
        assert all(item['scorer_exit_code'] == 0 and item['quality']['gate'] == 'passed' for item in observations)
        reference = json.loads(reference_path.read_text())
        scores = [score(reference, item['actual'], rules['samples'][sample['id']]) for item in observations]
        assert all(result['meets_proposed_rule'] for result in scores)
        analysis['samples'].append({'sample_id':sample['id'], 'scores':scores, 'repetitions':3})
    analysis['quality_gate'] = 'passed'
    output = ROOT / 'artifacts/baseline/baseline-analysis.fleurs-v1.json'
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(json.dumps(analysis,ensure_ascii=False,indent=2)+'\n')
    print('analysis: 6 new-corpus formal-host outputs passed unchanged CER/WER limits; old system-voice baseline not reused')
    for sample in analysis['samples']:
        print(sample['sample_id'], [(s['metric'],s[s['metric']]) for s in sample['scores']])


if __name__ == '__main__':
    main()
