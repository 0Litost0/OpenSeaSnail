import json
from pathlib import Path
import subprocess
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

from scripts.dev_package.phases import fetch_sherpa
from scripts.dev_package.errors import Failure

ROOT=Path(__file__).resolve().parents[3]


class SourcePreparationTests(unittest.TestCase):
    def fixture(self,root):
        scripts=root/'scripts/sherpa'; scripts.mkdir(parents=True)
        for name in ['source-lock.json','artifact-lock.json']:
            (scripts/name).write_bytes((ROOT/'scripts/sherpa'/name).read_bytes())
        (scripts/'fetch-inputs.py').write_text('# isolated loader fixture')

    def test_empty_sources_prepare_both_pinned_checkouts(self):
        with tempfile.TemporaryDirectory() as folder:
            root=Path(folder).resolve(); self.fixture(root); calls=[]
            def checkout(parent,name,row,**kwargs):
                calls.append(name); (parent/name).mkdir(exist_ok=True)
            fetch=SimpleNamespace(checkout=checkout)
            spec=SimpleNamespace(loader=SimpleNamespace(exec_module=lambda _:None))
            with patch.object(fetch_sherpa,'ROOT',root),patch.object(sys,'argv',['fetch']),patch.object(fetch_sherpa,'download'),patch.object(fetch_sherpa.importlib.util,'spec_from_file_location',return_value=spec),patch.object(fetch_sherpa.importlib.util,'module_from_spec',return_value=fetch),patch.object(fetch_sherpa.subprocess,'run',return_value=subprocess.CompletedProcess([],0)):
                fetch_sherpa.prepare()
            self.assertEqual(calls,['sherpa-onnx','sherpa-onnx','onnxruntime','onnxruntime'])
            self.assertTrue((root/'third_party/sherpa/macos-arm64/sources/onnxruntime').is_dir())

    def test_timeout_retains_owned_workspace_until_parent_proves_inactivity(self):
        with tempfile.TemporaryDirectory() as folder:
            root=Path(folder).resolve(); self.fixture(root)
            def checkout(parent,name,row,**kwargs):
                (parent/'partial').write_text('owned partial')
                raise subprocess.TimeoutExpired(['git'],3600)
            fetch=SimpleNamespace(checkout=checkout)
            spec=SimpleNamespace(loader=SimpleNamespace(exec_module=lambda _:None))
            with patch.object(fetch_sherpa,'ROOT',root),patch.object(sys,'argv',['fetch']),patch.object(fetch_sherpa,'download'),patch.object(fetch_sherpa.importlib.util,'spec_from_file_location',return_value=spec),patch.object(fetch_sherpa.importlib.util,'module_from_spec',return_value=fetch):
                with self.assertRaises(Failure) as caught: fetch_sherpa.prepare()
            self.assertEqual(caught.exception.exit_code,4)
            parent=root/'third_party/sherpa/macos-arm64/sources'
            self.assertFalse((parent/'sherpa-onnx').exists())
            self.assertTrue(list(parent.glob('.dev-package-sources/*/.owner.json')))


if __name__=='__main__': unittest.main()
