import json
from pathlib import Path
import plistlib
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch
from scripts.dev_package import native,state
from scripts.dev_package.pipeline import Pipeline


class NativeReuseTests(unittest.TestCase):
    def test_completed_ffmpeg_and_sherpa_reuse_records_events_without_building(self):
        with tempfile.TemporaryDirectory() as folder:
            root=Path(folder).resolve()
            paths=['Cargo.lock','apps/desktop/pnpm-lock.yaml','apps/desktop/native/post-paste-monitor/Package.resolved']
            for name in paths:
                path=root/name; path.parent.mkdir(parents=True,exist_ok=True); path.write_text('fixture lock')
            path=root/'packaging/macos/Info.plist'; path.parent.mkdir(parents=True)
            path.write_bytes(plistlib.dumps({'LSMinimumSystemVersion':'13.0'}))
            run=state.Run(root,'build',{'fixture':'native reuse'})
            controller=SimpleNamespace(counter=0,execute=lambda *a,**kw:self.fail('Reuse must not compile'))
            pipeline=Pipeline(root,run,controller,{})
            def identity(_root,_tools,kind): return {'kind':kind},kind+'-fixture'
            with patch.object(native,'receipt_identity',side_effect=identity),patch.object(native,'verify_sherpa',return_value=[]):
                for kind in ['ffmpeg','sherpa']:
                    value,key=identity(root,{},kind)
                    base,artifact,receipt=pipeline.native_workspace(kind,value,key)
                    artifact.mkdir(); (artifact/'ffmpeg').write_bytes(b'fixture artifact')
                    native.create_receipt(receipt,value,artifact,['fixture validated'])
                pipeline.prepare_ffmpeg(); pipeline.prepare_sherpa_native()
            events=[json.loads(line) for line in (run.directory/'events.jsonl').read_text().splitlines()]
            self.assertEqual([row['native_kind'] for row in events if row['event']=='native_reused'],['ffmpeg','sherpa'])
            self.assertEqual(set(pipeline.outputs),{'ffmpeg','sherpa'})


if __name__=='__main__': unittest.main()
