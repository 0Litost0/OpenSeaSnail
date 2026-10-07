#!/usr/bin/env python3
"""Verify versioned WAVs/references/scenarios and quality-rule negative controls."""
from pathlib import Path
import hashlib
import json
import re
import wave
from quality import normalize, score, validate_rules
from generate_audio import pipeline_wav

ROOT = Path(__file__).resolve().parents[1]


def load(path):
    return json.loads(path.read_text())


def main():
    manifest = load(ROOT / 'assets/manifest.json')
    rules = load(ROOT / 'assets/quality-rules.v3.json')
    validate_rules(rules)
    assert manifest['dataset_version'] == rules['dataset_version']
    assert manifest['reference_version'] == rules['reference_version']
    provenance = load(ROOT / 'assets/audio-provenance.json')
    assert provenance['license'] == 'CC-BY-4.0' and provenance['dataset'] == 'google/fleurs'
    assert re.fullmatch(r'[0-9a-f]{40}', provenance['revision'])
    for material in provenance['license_materials']:
        path = (ROOT / material['path']).resolve()
        assert path.is_relative_to(ROOT / 'assets/licenses') and path.is_file()
        content = path.read_bytes()
        assert len(content) == material['size_bytes'] and hashlib.sha256(content).hexdigest() == material['sha256']
    assert re.search(r'^license:\s*\n- cc-by-4\.0\s*$', (ROOT / 'assets/licenses/FLEURS-README.md').read_text(), re.M)
    pipeline = load(ROOT / 'assets/pipeline-audio.json')
    pipeline_path = (ROOT / pipeline['audio_path']).resolve()
    assert pipeline_path.is_relative_to(ROOT / 'assets/audio')
    assert pipeline_path.read_bytes() == pipeline_wav()
    assert pipeline_path.stat().st_size == pipeline['bytes']
    assert hashlib.sha256(pipeline_path.read_bytes()).hexdigest() == pipeline['audio_sha256']
    assert pipeline['source']['license'] == 'Apache-2.0'
    source_rows = {sample['id']: sample for sample in provenance['samples']}
    ids = [sample['id'] for sample in manifest['samples']]
    assert set(ids) == set(source_rows)
    assert len(ids) == len(set(ids)) == 2
    assert {sample['language'] for sample in manifest['samples']} == {'zh', 'en'}
    assert set(ids) == set(rules['samples'])
    negative = {'version':'quality-negatives-v3', 'rule_version':rules['version'], 'cases':[]}
    for sample in manifest['samples']:
        for path_key, hash_key in [('audio_path', 'audio_sha256'), ('reference_path', 'reference_sha256')]:
            path = (ROOT / sample[path_key]).resolve()
            assert path.is_relative_to(ROOT / 'assets') and path.is_file()
            assert hashlib.sha256(path.read_bytes()).hexdigest() == sample[hash_key]
        audio = ROOT / sample['audio_path']
        assert audio.stat().st_size == sample['bytes']
        with wave.open(str(audio), 'rb') as wav:
            assert (wav.getnchannels(), wav.getsampwidth(), wav.getframerate()) == (1, 2, 16000)
            assert round(wav.getnframes() * 1000 / wav.getframerate()) == sample['duration_ms']
            assert any(wav.readframes(wav.getnframes()))
        reference = load(ROOT / sample['reference_path'])
        assert reference['id'] == sample['id'] and reference['language'] == sample['language']
        assert reference['reference_version'] == manifest['reference_version']
        assert reference['text'] and reference['key_content'] and sample['source']['authorization']
        source = source_rows[sample['id']]
        assert sample['source']['license'] == 'CC-BY-4.0'
        assert sample['source']['revision'] == provenance['revision']
        assert source['output_path'] == sample['audio_path']
        source_record = (ROOT / source['source_record']).read_text().rstrip('\n').split('\t')
        assert int(source_record[0]) == source['dataset_example_id']
        assert source_record[1] == Path(source['archive_member']).name
        assert reference['text'] == source_record[2]  # Unedited upstream reference, never the ASR output.
        assert source['archive_url'].startswith('https://huggingface.co/datasets/google/fleurs/resolve/' + provenance['revision'] + '/')
        assert re.fullmatch(r'[0-9a-f]{64}', source['source_audio_sha256'])
        rule = rules['samples'][sample['id']]
        assert score(reference, reference['text'], rule)['meets_proposed_rule']
        variants = [('', 'empty'), (reference['text'].replace(reference['key_content'][0], ''), 'missing-required-term'),
                    ('天气晴朗。今天吃午饭。' if sample['language'] == 'zh' else 'The weather is sunny. We are eating lunch.', 'wrong-transcription')]
        for text, kind in variants:
            assert not score(reference, text, rule)['meets_proposed_rule']
            negative['cases'].append({'sample_id':sample['id'], 'kind':kind, 'text':text, 'expected_meets_rule':False})
    # Every new key phrase is mandatory. English matching must remain whole-word.
    for sample in manifest['samples']:
        reference = load(ROOT / sample['reference_path'])
        rule = rules['samples'][sample['id']]
        assert not rule.get('optional_key_content')
        for key in reference['key_content']:
            assert not score(reference, reference['text'].replace(key, ''), rule)['meets_proposed_rule']
        if sample['language'] == 'en':
            key = reference['key_content'][0]
            changed = reference['text'].replace(key, key + 'suffix')
            assert key in score(reference, changed, rule)['missing_required_key_content']
    # Controls for normalization: accept orthography, reject missing prefix and keywords.
    assert normalize('Ｓｅａ Snail, API!') == 'seasnail api'
    assert normalize('Snail') != normalize('SeaSnail')
    asr = load(ROOT / 'assets/scenarios/asr.json')
    assert set(asr['scenarios']) == {'asr-success-v1', 'asr-failure-v1', 'asr-retry-v1', 'asr-delay-v1', 'asr-no-speech-v1'}
    for scenario in asr['scenarios'].values():
        assert scenario['after_sequence'] == 'repeat-last' and scenario['responses']
        for response in scenario['responses']:
            assert response['kind'] in ('transcript', 'error', 'no-speech') and response['delay_ms'] >= 0
    provider = load(ROOT / 'assets/scenarios/provider.json')
    assert set(provider['scenarios']) == {'disabled', 'success', 'http-503', 'invalid-json', 'timeout', 'interrupted'}
    evals = load(ROOT / 'assets/provider-eval.v1.json')
    assert evals['version'] == 'v1' and evals['providers']
    reference_pattern = re.compile(r'[a-zA-Z0-9][a-zA-Z0-9._/-]*\Z')
    for provider_id, entry in evals['providers'].items():
        assert provider_id and entry['provider_type'] and entry['model']
        assert reference_pattern.fullmatch(entry['endpoint_ref'])
        assert reference_pattern.fullmatch(entry['credential_ref'])
        assert entry['endpoint_ref'] != entry['credential_ref']
    # Negative corpus is a checked-in asset, not rebuilt implicitly when the checker runs.
    assert load(ROOT / 'assets/quality-negatives.v3.json') == negative
    print('assets: procedural WAV + 2 CC-BY WAV/reference pairs and license evidence verified; 6 negative controls rejected; reference/normalization controls accepted; scenarios and provider-eval registry valid')


if __name__ == '__main__':
    main()
