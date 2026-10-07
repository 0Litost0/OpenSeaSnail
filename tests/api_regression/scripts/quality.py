#!/usr/bin/env python3
"""Versioned corpus scoring. Pending rules are observations, never acceptance passes."""
from pathlib import Path
import argparse
import json
import re
import unicodedata

ROOT = Path(__file__).resolve().parents[1]
NORMALIZATION = {
    'unicode': 'NFKC', 'case': 'casefold',
    'punctuation': 'replace with space for words; remove for characters',
    'whitespace': 'collapse', 'orthographic_aliases': {'sea snail': 'seasnail'},
    'semantic_synonyms': False,
}


def validate_rules(rules):
    if rules['version'] != 'quality-v3' or rules['normalization'] != NORMALIZATION:
        raise ValueError('unsupported scorer/normalization version')
    for rule in rules['samples'].values():
        if rule['metric'] not in ('cer', 'wer') or not 0 <= rule['maximum_error_rate'] <= 1:
            raise ValueError('invalid quality rule')
        if not isinstance(rule.get('optional_key_content', []), list) or any(not isinstance(key, str) or not key for key in rule.get('optional_key_content', [])):
            raise ValueError('invalid optional key content')


def normalize(text):
    value = unicodedata.normalize('NFKC', text).casefold()
    # Orthography only: no phonetic/synonym repair and no prefix omission allowance.
    value = re.sub(r'\bsea\s+snail\b', 'seasnail', value)
    return ' '.join(''.join(c if c.isalnum() or c.isspace() else ' ' for c in value).split())


def edit_distance(left, right):
    previous = list(range(len(right) + 1))
    for i, lchar in enumerate(left, 1):
        current = [i]
        for j, rchar in enumerate(right, 1):
            current.append(min(current[-1] + 1, previous[j] + 1, previous[j - 1] + (lchar != rchar)))
        previous = current
    return previous[-1]


def score(reference, actual, rule):
    expected, observed = normalize(reference['text']), normalize(actual)
    ec, oc = ''.join(expected.split()), ''.join(observed.split())
    ew, ow = expected.split(), observed.split()
    cer = edit_distance(ec, oc) / max(1, len(ec))
    wer = edit_distance(ew, ow) / max(1, len(ew))
    def contains(key):
        normalized = normalize(key)
        if reference['language'] == 'en':
            tokens = normalized.split()
            return any(ow[i:i + len(tokens)] == tokens for i in range(len(ow) - len(tokens) + 1))
        return ''.join(normalized.split()) in oc
    missing = [key for key in reference['key_content'] if not contains(key)]
    optional = rule.get('optional_key_content', [])
    if any(key not in reference['key_content'] for key in optional):
        raise ValueError('unknown optional key content')
    required_missing = [key for key in missing if key not in optional]
    value = cer if rule['metric'] == 'cer' else wer
    meets = bool(oc) and value <= rule['maximum_error_rate'] and (not rule['require_all_key_content'] or not required_missing)
    return {'normalized_actual':observed, 'cer':round(cer, 6), 'wer':round(wer, 6),
            'metric':rule['metric'], 'maximum_error_rate':rule['maximum_error_rate'],
            'missing_key_content':missing, 'missing_required_key_content':required_missing, 'meets_proposed_rule':meets}


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--sample', required=True)
    parser.add_argument('--text-file', required=True, type=Path)
    args = parser.parse_args()
    rules = json.loads((ROOT / 'assets/quality-rules.v3.json').read_text())
    validate_rules(rules)
    reference = json.loads((ROOT / 'assets/references' / (args.sample + '.json')).read_text())
    result = score(reference, args.text_file.read_text(), rules['samples'][args.sample])
    eligible = rules['status'] == 'approved' and bool(rules['approval'])
    result.update({'rule_version':rules['version'], 'quality_gate_eligible':eligible,
                   'gate':'incomplete' if not eligible else 'passed' if result['meets_proposed_rule'] else 'failed'})
    print(json.dumps(result, ensure_ascii=False, indent=2))
    return 2 if not eligible else 0 if result['meets_proposed_rule'] else 1


if __name__ == '__main__':
    raise SystemExit(main())
