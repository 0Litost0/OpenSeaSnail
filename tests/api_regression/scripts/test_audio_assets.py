#!/usr/bin/env python3
"""Reject incomplete licensing, corrupt waveform bytes and references rewritten to fit ASR output."""
from pathlib import Path
import hashlib
import json
import shutil
import tempfile
import unittest
from unittest.mock import patch
import check_assets


class AudioIntegrityTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix='seasnail-audio-integrity-')
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        source = check_assets.ROOT
        files = ['assets/manifest.json', 'assets/audio-provenance.json', 'assets/pipeline-audio.json',
                 'assets/quality-rules.v3.json', 'assets/quality-negatives.v3.json',
                 'assets/scenarios/asr.json', 'assets/scenarios/provider.json', 'assets/provider-eval.v1.json']
        manifest = json.loads((source / 'assets/manifest.json').read_text())
        for sample in manifest['samples']:
            files.extend([sample['audio_path'], sample['reference_path']])
        provenance = json.loads((source / 'assets/audio-provenance.json').read_text())
        files += [row['path'] for row in provenance['license_materials']]
        files.append('assets/audio/pipeline-v1.wav')
        for relative in set(files):
            target = self.root / relative
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(source / relative, target)

    def assert_rejected(self):
        with patch.object(check_assets, 'ROOT', self.root):
            with self.assertRaises(AssertionError):
                check_assets.main()

    def test_missing_full_license_prevents_asset_clearance(self):
        (self.root / 'assets/licenses/CC-BY-4.0.txt').unlink()
        self.assert_rejected()

    def test_changed_business_waveform_is_rejected(self):
        path = self.root / 'assets/audio/pipeline-v1.wav'
        content = bytearray(path.read_bytes()); content[-1] ^= 1; path.write_bytes(content)
        self.assert_rejected()

    def test_reference_rewrite_cannot_be_blessed_by_updating_manifest_hash(self):
        path = self.root / 'assets/references/fleurs-zh-v1.json'
        value = json.loads(path.read_text()); value['text'] += '伪造转写结果'
        path.write_text(json.dumps(value, ensure_ascii=False) + '\n')
        manifest_path = self.root / 'assets/manifest.json'
        manifest = json.loads(manifest_path.read_text())
        manifest['samples'][0]['reference_sha256'] = hashlib.sha256(path.read_bytes()).hexdigest()
        manifest_path.write_text(json.dumps(manifest))
        self.assert_rejected()

    def test_quality_rules_cannot_reuse_the_old_corpus_identity(self):
        path = self.root / 'assets/quality-rules.v3.json'
        value = json.loads(path.read_text()); value['dataset_version'] = 'synthetic-v1'
        path.write_text(json.dumps(value))
        self.assert_rejected()


if __name__ == '__main__':
    unittest.main()
